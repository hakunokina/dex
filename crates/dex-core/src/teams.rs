//! Teams + their agents (DEX-7). Same shape as `db::tasks`: pure functions over `&Connection`,
//! `Core` methods that emit `teams-changed`.
//!
//! A main team owns a crew and projects. `cwd` remains the legacy single-project view.
//! A service team has no project; it works in `<data_dir>/teams/<id>/workspace`.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};
pub const TEAM_KINDS: &[&str] = &["main", "service"];
pub const TEAM_PROFILES: &[&str] = &["dev", "edu"];
pub const AGENT_KINDS: &[&str] = &["leader", "worker"];

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub profile: String,
    /// The project folder (main teams only).
    pub cwd: Option<String>,
    pub created_at: i64,
    /// Where the team's Claude sessions start: `cwd` for main, the app-data workspace for service.
    pub workdir: Option<String>,
    /// Memory location (DEX-3 will use it): `<cwd>/.dex/memory` or `<app_data>/teams/<id>/memory`.
    pub memory_dir: Option<String>,
    /// Görev #144: workers' jobs in their own git worktree (no effect outside a git repository).
    pub worktrees: bool,
    pub test_level: String,
    /// Merged worktrees awaiting safe cleanup; a maintenance warning for the UI only.
    pub worktree_cleanup: Vec<String>,
    pub default_project_id: Option<String>,
    pub workspace_root: Option<String>,
    pub suggested_workspace_root: Option<String>,
    /// Memory directory roots; shared/ and agents/<slug>/ are resolved below these.
    pub team_memory_root: Option<String>,
    pub agent_memory_root: Option<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub kind: String,
    pub title: String,
    pub model: String,
    pub shirt: String,
    pub hair: String,
    pub skin: String,
    pub sort: i64,
    pub provider: String,
    pub role_slug: Option<String>,
    /// Catalog name of `role_slug` (None when unset or no longer in the catalog).
    pub role_name: Option<String>,
    /// DEX-26: the CLI the agent runs; only `claude` (#1710 removed Codex).
    pub engine: String,
    /// DEX-30: a bulk change that lands when the running session ends.
    pub pending: Option<PendingModel>,
    /// DEX-17: zincir kapalı mı (ajan başına). Zincir geçişindeki geçici motor/sağlayıcı/model;
    /// `None` = ayarlanan (asıl) değer geçerli.
    pub chain_off: bool,
    pub active_provider: Option<String>,
    pub active_model: Option<String>,
    pub active_engine: Option<String>,
    /// Asıl sağlayıcının tükenme saati doldu; kullanıcı isterse işi bölerek geri dönebilir.
    pub chain_return_ready: bool,
}

/// DEX-30: target of a bulk "all workers" change. `provider` absent → anthropic.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingModel {
    pub engine: String,
    #[serde(default)]
    pub provider: Option<String>,
    pub model: String,
}

/// DEX-30: which workers changed now and which wait for their session to end.
#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkersOutcome {
    pub applied: Vec<String>,
    pub pending: Vec<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewTeam {
    pub name: String,
    pub kind: String,
    /// Absent in older clients; defaults to the development profile.
    #[serde(default)]
    pub profile: Option<String>,
    pub cwd: Option<String>,
    /// Template workers: dev defaults to `DEFAULT_WORKER_ROLES`, edu to one visual-explainer.
    pub worker_roles: Option<Vec<String>>,
    /// When present, replaces the default crew, atomically with the team.
    #[serde(default)]
    pub members: Option<Vec<TeamMemberTemplate>>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberTemplate {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub role_slug: Option<String>,
    #[serde(default)]
    pub chain_off: bool,
    /// Effort belongs to jobs, not agents. Only null is meaningful here.
    #[serde(default)]
    pub effort: Option<crate::effort::Effort>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TeamCloneTemplate {
    pub kind: String,
    pub profile: String,
    pub members: Vec<TeamMemberTemplate>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TeamFolderPreview {
    pub folder_name: String,
    pub workspace_root: String,
    pub main_root: String,
    pub exists: TeamFolderExists,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct TeamFolderExists {
    pub workspace: bool,
    pub main: bool,
}

fn folder_preview(db: &crate::db::Db, name: &str, home: Option<&Path>) -> Result<TeamFolderPreview, String> {
    let (folder_name, _, workspace, main) = planned_team_folders(name, home)?;
    // All filesystem work precedes the DB lock; preview never creates a directory.
    let exists = TeamFolderExists {
        workspace: workspace.try_exists().map_err(|e| e.to_string())?,
        main: main.try_exists().map_err(|e| e.to_string())?,
    };
    let owner = crate::db::with(db, |c| match workspace_owner(c, &workspace, "")? {
        Some(owner) => Ok(Some(owner)),
        // Archived projects still reserve their exact root, including the planned main root.
        None => workspace_owner(c, &main, ""),
    })?;
    Ok(TeamFolderPreview {
        folder_name,
        workspace_root: workspace.to_string_lossy().into_owned(),
        main_root: main.to_string_lossy().into_owned(),
        exists,
        owner,
    })
}

pub fn clone_template(conn: &Connection, team_id: &str) -> Result<TeamCloneTemplate, String> {
    let (kind, profile) = conn
        .query_row("SELECT kind,profile FROM teams WHERE id=?1", [team_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("team_not_found")?;
    let mut st = conn.prepare("SELECT name,kind,title,model,provider,engine,role_slug,chain_off FROM agents WHERE team_id=?1 ORDER BY sort,id").map_err(|e|e.to_string())?;
    let members = st
        .query_map([team_id], |r| {
            Ok(TeamMemberTemplate {
                name: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                model: r.get(3)?,
                provider: r.get(4)?,
                engine: r.get(5)?,
                role_slug: r.get(6)?,
                chain_off: r.get(7)?,
                effort: None,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    Ok(TeamCloneTemplate { kind, profile, members })
}

impl crate::Core {
    pub fn team_folder_preview(&self, name: &str) -> Result<TeamFolderPreview, String> {
        folder_preview(&self.db, name, None)
    }

    pub fn team_clone_template(&self, team_id: &str) -> Result<TeamCloneTemplate, String> {
        crate::db::with(&self.db, |c| clone_template(c, team_id))
    }
}

/// Absent field = unchanged.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TeamPatch {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub cwd: Option<Option<String>>,
    pub worktrees: Option<bool>,
    pub test_level: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub default_project_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub workspace_root: Option<Option<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewAgent {
    pub team_id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model: String,
    /// DEX-13: absent → `anthropic`. Validated against `providers.rs`.
    #[serde(default)]
    pub provider: Option<String>,
    /// Empty `title` + a role → the title is the role's name.
    pub role_slug: Option<String>,
    /// DEX-26: absent → `claude`.
    #[serde(default)]
    pub engine: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentPatch {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    /// DEX-13: absent = unchanged; `"anthropic"` resets. Model, when patched, must fit the
    /// resulting provider.
    pub provider: Option<String>,
    /// `roleSlug: null` clears it. A new role without a `title` also retitles the agent.
    #[serde(default, deserialize_with = "present")]
    pub role_slug: Option<Option<String>>,
    /// DEX-26: absent = unchanged. Like `provider`, a switch sends the model with it.
    pub engine: Option<String>,
    /// DEX-17: absent = unchanged. `Some(true)` kapatır, `Some(false)` açar.
    pub chain_off: Option<bool>,
}

pub const ENGINES: &[&str] = &["claude"];

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn one_of(v: &str, allowed: &[&str], what: &str) -> Result<(), String> {
    allowed.contains(&v).then_some(()).ok_or_else(|| format!("geçersiz {what}: {v}"))
}

/// DEX-13: sağlayıcı adı `providers.rs` tanımlarından biriydi. Trim'li hâli döner.
fn check_provider(v: &str) -> Result<String, String> {
    let t = v.trim();
    crate::providers::get(t).ok_or_else(|| format!("geçersiz sağlayıcı: {t}"))?;
    Ok(t.to_string())
}

/// DEX-13: model, sağlayıcının kataloğundan olmalı (anthropic → sürümlü claude-* + eski takma adlar;
/// glm/muse → sabit liste + sağlayıcının canlı listesi, #1728).
fn check_model(engine: &str, provider: &str, model: &str) -> Result<(), String> {
    one_of(engine, ENGINES, "motor")?;
    let def = crate::providers::get(provider).ok_or_else(|| format!("geçersiz sağlayıcı: {provider}"))?;
    // DEX-25: canlı sağlayıcının kataloğu sunucuda; ad boş değilse sunucu karar verir.
    let ok = if def.live { !model.trim().is_empty() } else { crate::providers::has_model(def, model) };
    ok.then_some(()).ok_or_else(|| format!("geçersiz model: {model}"))
}

fn check_name(s: &str) -> Result<String, String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string()).ok_or_else(|| "ad boş olamaz".into())
}

/// A single portable folder component; casing and Unicode letters are preserved.
pub fn folder_name(name: &str) -> Result<String, String> {
    let name: String = name
        .chars()
        .filter(|c| {
            !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .collect();
    let name = name.trim_end_matches('.');
    let device = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    if name.is_empty()
        || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(device.as_bytes(), [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] if (b'1'..=b'9').contains(d))
    {
        return Err("invalid_folder_name".into());
    }
    Ok(name.into())
}

/// `name` → `slug-xxxxxxxxxxxx`: readable in env vars / layout.json (paths use [`crate::slug`], #415).
/// 48 random bits: crews reuse the same names in every team (core, pixel-1…), so 16 bits collided
/// (~1% of e2e runs, #246). Existing ids keep their short suffix; nothing parses the suffix.
fn os_rng(raw: &mut [u8]) -> Result<(), String> {
    getrandom::fill(raw).map_err(|e| e.to_string())
}

const ID_RANDOM_BYTES: usize = 6;
/// A collision at 48 bits is ~never; the retry is the safety net, not the plan.
const ID_ATTEMPTS: usize = 5;

type Rng<'a> = &'a mut dyn FnMut(&mut [u8]) -> Result<(), String>;

/// The id column (PRIMARY KEY) collided — not another UNIQUE column such as `teams.cwd`.
fn is_id_collision(e: &rusqlite::Error, table: &str) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, Some(m))
        if f.code == rusqlite::ErrorCode::ConstraintViolation && m.ends_with(&format!("{table}.id")))
}

/// Draws an id and runs `insert`; on an id collision draws a fresh one (up to `ID_ATTEMPTS`).
fn insert_with_new_id(
    name: &str,
    table: &str,
    rng: Rng,
    mut insert: impl FnMut(&str) -> rusqlite::Result<()>,
) -> Result<String, String> {
    let mut last = String::new();
    for _ in 0..ID_ATTEMPTS {
        let id = new_id_with(name, rng)?;
        match insert(&id) {
            Ok(()) => return Ok(id),
            Err(e) if is_id_collision(&e, table) => last = e.to_string(),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(last)
}

fn new_id_with(name: &str, rng: Rng) -> Result<String, String> {
    let mut slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ç' => 'c', 'ğ' => 'g', 'ı' => 'i', 'ö' => 'o', 'ş' => 's', 'ü' => 'u',
            c if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    slug = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    slug.truncate(24);
    if slug.is_empty() {
        slug = "x".into();
    }
    let mut raw = [0u8; ID_RANDOM_BYTES];
    rng(&mut raw)?;
    Ok(format!("{slug}-{}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Canonical, existing directory. One folder = one team, so `C:\X`, `c:\x\` and `C:\X\.` must
/// collide on the UNIQUE column; canonicalize also fixes the casing on Windows.
pub fn normalize_dir(raw: &str) -> Result<String, String> {
    let p = std::fs::canonicalize(raw.trim()).map_err(|_| format!("klasör bulunamadı: {raw}"))?;
    if !p.is_dir() {
        return Err(format!("klasör değil: {raw}"));
    }
    let s = p.to_string_lossy().into_owned();
    // Strip Windows verbatim prefixes (`\\?\C:\…`, `\\?\UNC\srv\share`); child processes dislike them.
    Ok(match (s.strip_prefix(r"\\?\UNC\"), s.strip_prefix(r"\\?\")) {
        (Some(unc), _) => format!(r"\\{unc}"),
        (None, Some(rest)) => rest.to_string(),
        _ => s,
    })
}

pub fn service_dir(data_dir: &Path, team_id: &str) -> PathBuf {
    data_dir.join("teams").join(team_id)
}

#[derive(Debug)]
pub(crate) struct TeamFolders {
    workspace_root: PathBuf,
    project_root: PathBuf,
}

pub(crate) fn prepare_team_folders(
    db: &crate::db::Db,
    team: &NewTeam,
    home: Option<&Path>,
) -> Result<Option<TeamFolders>, String> {
    if team.kind != "main" || team.cwd.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return Ok(None);
    }
    let (_, home, workspace, project) = planned_team_folders(&team.name, home)?;
    crate::db::with(db, |c| check_workspace_claim(c, &workspace, ""))?;
    // No files are overwritten, and failed DB commits deliberately leave folders intact.
    std::fs::create_dir_all(&project).map_err(|e| e.to_string())?;
    let workspace_root = PathBuf::from(normalize_dir(&workspace.to_string_lossy())?);
    let project_root = PathBuf::from(normalize_dir(&project.to_string_lossy())?);
    if !workspace_root.starts_with(&home) || !project_root.starts_with(&workspace_root) {
        return Err("project_root_conflict".into());
    }
    Ok(Some(TeamFolders {
        workspace_root,
        project_root,
    }))
}

fn planned_team_folders(name: &str, home: Option<&Path>) -> Result<(String, PathBuf, PathBuf, PathBuf), String> {
    let name = folder_name(name)?;
    let host_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let home = home.or(host_home.as_deref()).ok_or("home_unavailable")?;
    let home = PathBuf::from(normalize_dir(&home.to_string_lossy())?);
    let workspace = home.join(&name);
    let project = workspace.join(format!("{name}Main"));
    // Resolve existing ancestors before touching disk: junctions/symlinks cannot escape home.
    let workspace = planned_dir(&workspace, &home)?;
    let project = planned_dir(&project, &workspace)?;
    Ok((name, home, workspace, project))
}

fn planned_dir(path: &Path, boundary: &Path) -> Result<PathBuf, String> {
    let resolved = if path.try_exists().map_err(|e| e.to_string())? {
        PathBuf::from(normalize_dir(&path.to_string_lossy())?)
    } else {
        let parent = path.parent().ok_or("project_unavailable")?;
        planned_dir(parent, boundary.parent().unwrap_or(boundary))?.join(path.file_name().ok_or("project_unavailable")?)
    };
    if !resolved.starts_with(boundary) {
        return Err("project_root_conflict".into());
    }
    Ok(resolved)
}

fn check_workspace_claim(conn: &Connection, workspace: &Path, except: &str) -> Result<(), String> {
    if workspace_owner(conn, workspace, except)?.is_some() {
        return Err("project_root_conflict".into());
    }
    Ok(())
}

/// Team workspace claims take priority; otherwise return the conflicting project id.
fn workspace_owner(conn: &Connection, workspace: &Path, except: &str) -> Result<Option<String>, String> {
    let key = crate::projects::path_key(&workspace.to_string_lossy());
    let mut st = conn
        .prepare("SELECT id,workspace_root FROM teams WHERE id<>?1 AND workspace_root IS NOT NULL ORDER BY id")
        .map_err(|e| e.to_string())?;
    let roots = st
        .query_map([except], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    for root in roots {
        let (id, root) = root.map_err(|e| e.to_string())?;
        let other = crate::projects::path_key(&root);
        if key == other || key.starts_with(&format!("{other}/")) || other.starts_with(&format!("{key}/")) {
            return Ok(Some(id));
        }
    }
    let mut st = conn
        .prepare("SELECT id,root_key,archived_at FROM projects WHERE root_key IS NOT NULL ORDER BY id")
        .map_err(|e| e.to_string())?;
    let roots = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))
        .map_err(|e| e.to_string())?;
    for root in roots {
        let (id, other, archived) = root.map_err(|e| e.to_string())?;
        if key == other
            || archived.is_none() && (key.starts_with(&format!("{other}/")) || other.starts_with(&format!("{key}/")))
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

// Auto outfit: cycles by position in the team (same muted palette as the seed crew).
const SHIRTS: &[&str] = &["#b3261e", "#4a4f57", "#5c6169", "#33373d", "#7a2a26", "#3f454d"];
const HAIRS: &[&str] = &["#1a1a1a", "#3b2a1e", "#6b3a22", "#2a2a2a", "#8a6a3a", "#4a2f1f"];
const SKINS: &[&str] = &["#d9a878", "#e0b48c", "#f0c8a0", "#c8956a", "#a8784e", "#e8bc94"];

const TEAM_COLS: &str = "id, name, kind, cwd, created_at, worktrees,
    (SELECT json_group_array(path) FROM (
        SELECT path FROM delegation_worktrees WHERE root = teams.cwd AND merged_at IS NOT NULL AND removed_at IS NULL
        UNION SELECT value AS path FROM json_each(COALESCE(
            (SELECT value FROM settings WHERE key = 'worktree_orphans:' || teams.cwd), '[]'))
        ORDER BY path
    )), default_project_id, workspace_root, team_memory_root, agent_memory_root,
    (SELECT root_path FROM projects WHERE id=teams.default_project_id AND team_id=teams.id), test_level, profile";
const AGENT_COLS: &str =
    "a.id, a.team_id, a.name, a.kind, a.title, a.model, a.shirt, a.hair, a.skin, a.sort, a.provider, a.role_slug, r.name, a.engine,
     a.pending_engine, a.pending_provider, a.pending_model, a.chain_off, a.active_provider, a.active_model, a.active_engine,
     CASE WHEN (a.active_provider IS NOT NULL OR a.active_engine IS NOT NULL)
          AND NOT EXISTS (
              SELECT 1 FROM provider_exhausted pe
              WHERE pe.provider = a.provider
                AND pe.reset_at > CAST(strftime('%s','now') AS INTEGER) * 1000
          ) THEN 1 ELSE 0 END";
const AGENT_FROM: &str = "agents a LEFT JOIN roles r ON r.slug = a.role_slug";

/// Every new team starts as 1 `lead` + these 4 workers (roles overridable in the form).
pub const DEFAULT_WORKER_ROLES: [&str; 4] = ["frontend", "backend", "qa", "explorer"];
/// Default agent name per role slug; a role not listed here gets `FALLBACK_NAME`.
const ROLE_NAMES: &[(&str, &str)] = &[
    ("lead", "Core"), ("frontend", "Pixel"), ("backend", "Kernel"), ("qa", "Probe"), ("design", "Canvas"),
    ("devops", "Forge"), ("security", "Shield"), ("explorer", "Scout"), ("data-engineer", "Vector"),
    ("code-review", "Lens"), ("pm", "Compass"), ("marketing", "Signal"), ("seo", "Rank"), ("support", "Relay"),
    ("code-automation", "Bot"), ("n8n-automation", "Bot"), ("mobile", "Droid"),
    ("game-designer", "Dice"), ("technical-artist", "Prism"), ("gameplay-programmer", "Loop"),
    ("teacher", "Sage"), ("visual-explainer", "Chalk"), ("assistant", "Echo"),
];
const FALLBACK_NAME: &str = "Agent";

/// Role template name with the first free number in the team (case-insensitive): workers are always
/// numbered (`Kernel-1`, `Kernel-2`…) so adding a second one never renames the first; the leader is
/// bare `Core` (`Core-2` only if that is taken).
pub fn auto_name(conn: &Connection, team_id: &str, kind: &str, role_slug: Option<&str>) -> Result<String, String> {
    let base = role_slug
        .and_then(|s| ROLE_NAMES.iter().find(|(r, _)| *r == s))
        .map_or(FALLBACK_NAME, |(_, n)| n);
    let mut st = conn.prepare("SELECT lower(name) FROM agents WHERE team_id = ?1").map_err(|e| e.to_string())?;
    let taken: std::collections::HashSet<String> = st
        .query_map([team_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    Ok((1..)
        .map(|n| if n == 1 && kind == "leader" { base.to_string() } else { format!("{base}-{n}") })
        .find(|c| !taken.contains(&c.to_lowercase()))
        .expect("unbounded"))
}

fn team_row(data_dir: &Path) -> impl Fn(&Row) -> rusqlite::Result<Team> + '_ {
    move |r| {
        let id: String = r.get(0)?;
        let kind: String = r.get(2)?;
        let cwd: Option<String> = r.get(3)?;
        let workspace_root: Option<String> = r.get(8)?;
        let team_memory_root: Option<String> = r.get(9)?;
        let agent_memory_root: Option<String> = r.get(10)?;
        let default_root: Option<String> = r.get(11)?;
        let suggested_workspace_root = suggested_workspace(data_dir, &id, &kind, workspace_root.as_deref(), default_root.as_deref());
        let workspace_root = if is_legacy_workspace(data_dir, &id, &kind, workspace_root.as_deref()) {
            None
        } else {
            workspace_root
        };
        let (workdir, memory_dir) = if kind == "service" {
            let d = service_dir(data_dir, &id);
            (Some(d.join("workspace")), Some(d.join("memory")))
        } else {
            (cwd.as_ref().map(PathBuf::from), cwd.as_ref().map(|c| Path::new(c).join(".dex").join("memory")))
        };
        let s = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().into_owned());
        // Preserve an existing repo-memory anchor for slug migration after selection/root moves.
        // New app-data anchors are additive; old clients keep their cwd view until E/G adopt them.
        let legacy_anchor=agent_memory_root.as_ref().filter(|p|Path::new(p).ends_with(Path::new(".dex").join("memory")));
        let memory_dir = legacy_anchor.cloned().or_else(||s(memory_dir));
        let cleanup: String = r.get(6)?;
        let worktree_cleanup = serde_json::from_str(&cleanup)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))?;
        Ok(Team { id, name: r.get(1)?, kind, cwd, created_at: r.get(4)?, workdir: s(workdir), memory_dir, worktrees: r.get(5)?, worktree_cleanup,
            default_project_id:r.get(7)?,workspace_root,suggested_workspace_root,team_memory_root,agent_memory_root,test_level:r.get(12)?,profile:r.get(13)? })
    }
}

/// The v28 backfill wrote `<data_dir>/teams/<id>/workspace` as a placeholder category root for
/// legacy main teams without ever creating it. It is not a real category root: never present it
/// as one — consumers fall back to the same path where the old spelling is load-bearing — and
/// instead suggest the folder the team's projects actually live under.
fn is_legacy_workspace(data_dir: &Path, id: &str, kind: &str, workspace: Option<&str>) -> bool {
    kind == "main"
        && workspace.is_some_and(|w| {
            crate::projects::path_key(w)
                == crate::projects::path_key(&service_dir(data_dir, id).join("workspace").to_string_lossy())
        })
}

fn suggested_workspace(
    data_dir: &Path,
    id: &str,
    kind: &str,
    workspace: Option<&str>,
    root: Option<&str>,
) -> Option<String> {
    if !is_legacy_workspace(data_dir, id, kind, workspace) {
        return None;
    }
    let root = Path::new(root?);
    let parent = root.parent()?;
    parent.file_name()?; // A filesystem root is not a category folder.
    Some(parent.to_string_lossy().into_owned())
}

/// S4: the suggestion a remote client may adopt outside its roots. Only the old
/// `<Category>/<Category>Main` layout grants that; the wider #2137 suggestion (any default
/// project's parent, possibly the home folder) is UI advice, never an authorization.
pub fn remote_adoptable_suggestion(conn: &Connection, data_dir: &Path, id: &str) -> Result<Option<String>, String> {
    let team = get_team(conn, data_dir, id)?;
    let Some(suggested) = team.suggested_workspace_root else { return Ok(None) };
    let root: Option<String> = conn
        .query_row("SELECT root_path FROM projects WHERE id=?1 AND team_id=?2", params![team.default_project_id, id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let main_layout = root.as_deref().map(Path::new).is_some_and(|root| {
        let name = Path::new(&suggested).file_name().and_then(|n| n.to_str());
        name.is_some_and(|name| root.file_name().and_then(|n| n.to_str()) == Some(format!("{name}Main").as_str()))
    });
    Ok(main_layout.then_some(suggested))
}

fn agent_row(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: r.get(0)?,
        team_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        title: r.get(4)?,
        model: r.get(5)?,
        shirt: r.get(6)?,
        hair: r.get(7)?,
        skin: r.get(8)?,
        sort: r.get(9)?,
        provider: r.get(10)?,
        role_slug: r.get(11)?,
        role_name: r.get(12)?,
        engine: r.get(13)?,
        pending: match (r.get::<_, Option<String>>(14)?, r.get::<_, Option<String>>(16)?) {
            (Some(engine), Some(model)) => Some(PendingModel { engine, provider: r.get(15)?, model }),
            _ => None,
        },
        chain_off: r.get::<_, i64>(17)? != 0,
        active_provider: r.get(18)?,
        active_model: r.get(19)?,
        active_engine: r.get(20)?,
        chain_return_ready: r.get::<_, i64>(21)? != 0,
    })
}

pub fn list_teams(conn: &Connection, data_dir: &Path) -> Result<Vec<Team>, String> {
    let mut st = conn
        .prepare(&format!("SELECT {TEAM_COLS} FROM teams ORDER BY created_at, id"))
        .map_err(|e| e.to_string())?;
    let rows = st.query_map([], team_row(data_dir)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub fn get_team(conn: &Connection, data_dir: &Path, id: &str) -> Result<Team, String> {
    let sql = format!("SELECT {TEAM_COLS} FROM teams WHERE id = ?1");
    let mut st = conn.prepare(&sql).or_else(|error| {
        // Migration 29 resolves project contexts through this reader before migration 30.
        // Only that older schema needs a literal default; current reads stay one query.
        let has_test_level: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('teams') WHERE name='test_level')", [], |r| r.get(0),
        )?;
        let has_profile: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('teams') WHERE name='profile')", [], |r| r.get(0),
        )?;
        if has_test_level && has_profile { return Err(error); }
        let sql = if has_test_level { sql.clone() } else { sql.replace(", test_level,", ", 'full',") };
        conn.prepare(&if has_profile { sql } else { sql.replace(", profile FROM", ", 'dev' FROM") })
    }).map_err(|e| e.to_string())?;
    st.query_row([id], team_row(data_dir))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("takım yok: {id}"))
}

/// Normalized folder, or an error naming the team that already owns it.
fn claim_dir(conn: &Connection, raw: &str, except: &str) -> Result<String, String> {
    let cwd = normalize_dir(raw)?;
    let owner: Option<String> = conn
        .query_row("SELECT name FROM teams WHERE cwd = ?1 AND id <> ?2", params![cwd, except], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    match owner {
        Some(name) => Err(format!("bu klasör zaten \"{name}\" takımına ait")),
        None => Ok(cwd),
    }
}

pub fn create_team(conn: &Connection, data_dir: &Path, t: NewTeam) -> Result<Team, String> {
    create_team_with_folders(conn, data_dir, t, None)
}

pub(crate) fn create_team_with_folders(conn: &Connection, data_dir: &Path, t: NewTeam, folders: Option<&TeamFolders>) -> Result<Team, String> {
    let name = check_name(&t.name)?;
    one_of(&t.kind, TEAM_KINDS, "takım türü")?;
    let profile = t.profile.as_deref().unwrap_or("dev");
    one_of(profile, TEAM_PROFILES, "takım profili")?;
    let education = profile == "edu";
    if education {
        if let Some(members) = &t.members {
            if members.len() != 2
                || members.iter().filter(|m| m.kind == "leader" && m.role_slug.as_deref() == Some("teacher")).count() != 1
                || members.iter().filter(|m| m.kind == "worker" && m.role_slug.as_deref() == Some("visual-explainer")).count() != 1
            { return Err("eğitim takımı bir teacher lider ve bir visual-explainer worker ister".into()); }
        }
    }
    let cwd = match (t.kind.as_str(), t.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty())) {
        ("main", Some(raw)) => Some(claim_dir(conn, raw, "")?),
        ("main", None) => folders.map(|f| f.project_root.to_string_lossy().into_owned()),
        (_, Some(_)) => return Err("alt takımın proje klasörü olmaz".into()),
        (_, None) => None,
    };
    let default_roles: &[&str] = if education { &["visual-explainer"] } else { &DEFAULT_WORKER_ROLES };
    let worker_roles = match t.worker_roles.filter(|_| t.members.is_none()) {
        Some(r) if r.len() != default_roles.len() => {
            return Err(format!("takım {} worker ister", default_roles.len()))
        }
        Some(r) if education && r[0] != "visual-explainer" => return Err("eğitim worker rolü visual-explainer olmalı".into()),
        Some(r) => r,
        None => default_roles.iter().map(|s| s.to_string()).collect(),
    };
    // Team and crew land together or not at all.
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    if let Some(f) = folders { check_workspace_claim(&tx, &f.workspace_root, "")?; }
    let id = insert_with_new_id(&name, "teams", &mut os_rng, |id| {
        tx.execute(
            "INSERT INTO teams (id, name, kind, cwd, created_at, profile, worktrees) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, t.kind, cwd, now(), profile, !education],
        )
        .map(|_| ())
    })?;
    if t.kind == "main" {
        if let Some(root)=cwd.as_deref() {
            crate::projects::reserve_root(&tx,root).map_err(|e|e.to_string())?;
        }
        crate::projects::backfill_team(&tx,data_dir,&id,cwd.as_deref(),!education,now(),None)
            .map_err(|e|e.to_string())?;
        if let Some(f) = folders {
            tx.execute("UPDATE teams SET workspace_root=?2 WHERE id=?1", params![id,f.workspace_root.to_string_lossy()]).map_err(|e|e.to_string())?;
            tx.execute("UPDATE projects SET name=?2 WHERE id=(SELECT default_project_id FROM teams WHERE id=?1)",
                params![id,f.project_root.file_name().ok_or("invalid_folder_name")?.to_string_lossy()]).map_err(|e|e.to_string())?;
        }
    } else {
        crate::projects::backfill_service(&tx,data_dir,&id).map_err(|e|e.to_string())?;
    }
    if let Some(members) = t.members {
        for member in members {
            if member.effort.is_some() { return Err("agent_effort_unsupported".into()); }
            let a = create_agent(&tx, data_dir, NewAgent {
                team_id: id.clone(), name: member.name, kind: member.kind, title: member.title,
                model: member.model, provider: member.provider, role_slug: member.role_slug, engine: member.engine,
            })?;
            tx.execute("UPDATE agents SET chain_off=?2 WHERE id=?1", params![a.id,member.chain_off]).map_err(|e|e.to_string())?;
        }
    } else {
        let crew = std::iter::once(("leader", crate::providers::CLAUDE_STRONG, if education { "teacher" } else { "lead" }.to_string()))
            .chain(worker_roles.into_iter().map(|r| ("worker", crate::providers::CLAUDE_MID, r)));
        for (kind, model, role) in crew {
            create_agent(&tx, data_dir, NewAgent {
                team_id: id.clone(), name: String::new(), kind: kind.into(), title: String::new(), model: model.into(), provider: None, role_slug: Some(role), engine: None,
            })?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    get_team(conn, data_dir, &id)
}

pub fn update_team(conn: &Connection, data_dir: &Path, id: &str, p: TeamPatch) -> Result<Team, String> {
    let t = get_team(conn, data_dir, id)?;
    let name = match p.name {
        Some(n) => check_name(&n)?,
        None => t.name,
    };
    let cwd = match p.cwd {
        None => t.cwd.clone(),
        Some(_) if t.kind != "main" => return Err("alt takımın proje klasörü olmaz".into()),
        Some(None) => return Err("proje takımı bir klasör ister".into()),
        Some(Some(raw)) => Some(claim_dir(conn, &raw, id)?),
    };
    let worktrees = p.worktrees.unwrap_or(t.worktrees);
    let test_level = p.test_level.unwrap_or(t.test_level);
    if !matches!(test_level.as_str(), "full" | "light") {
        return Err("invalid_test_level".into());
    }
    let tx=conn.unchecked_transaction().map_err(|e|e.to_string())?;
    if cwd!=t.cwd {
        let projects=crate::projects::list(&tx,id,false).map_err(|e|e.to_string())?;
        if projects.len()!=1 { return Err("project_mismatch".into()); }
        crate::projects::update_legacy_root(&tx,id,&projects[0].id,cwd.as_deref().ok_or("project_mismatch")?)
            .map_err(|e|e.to_string())?;
    }
    // Legacy worktrees changes keep their former semantics; root/archive mutations use the gates.
    tx.execute("UPDATE projects SET worktrees=?2 WHERE id=(SELECT default_project_id FROM teams WHERE id=?1)",params![id,worktrees])
        .map_err(|e|e.to_string())?;
    tx.execute("UPDATE teams SET name = ?2, worktrees = ?3, test_level = ?4 WHERE id = ?1", params![id, name, worktrees, test_level])
        .map_err(|e| e.to_string())?;
    if let Some(selected)=p.default_project_id {
        crate::projects::set_default(&tx,id,selected.as_deref()).map_err(|e|e.to_string())?;
    }
    if let Some(workspace)=p.workspace_root {
        let path=match workspace { Some(ref raw)=>normalize_dir(raw)?,None=>service_dir(data_dir,id).join("workspace").to_string_lossy().into_owned() };
        tx.execute("UPDATE teams SET workspace_root=?2 WHERE id=?1",params![id,path]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e|e.to_string())?;
    get_team(conn, data_dir, id)
}

/// Cascades to the team's agents and tasks. FIX-5: an empty host is valid (a fresh `dexd`
/// starts with no teams), so the last team can go too — clients show an empty state instead.
pub fn delete_team(conn: &Connection, id: &str) -> Result<(), String> {
    // Rollback: any failure drops this transaction and restores every cleared reference.
    // GPU history remains; session pointers into the removed team's projects cannot resume.
    let tx=conn.unchecked_transaction().map_err(|e|e.to_string())?;
    // agent_chain_origins: dead table; old rows must not hold the project FK.
    for table in ["agent_sessions","agent_chain_origins"] {
        tx.execute(&format!("DELETE FROM {table} WHERE agent_id IN (SELECT id FROM agents WHERE team_id=?1)
            OR project_id IN (SELECT id FROM projects WHERE team_id=?1)"),[id]).map_err(|e|e.to_string())?;
    }
    for table in ["gpu_grants","gpu_requests"] {
        tx.execute(&format!("UPDATE {table} SET project_id=NULL WHERE project_id IN (SELECT id FROM projects WHERE team_id=?1)"),[id]).map_err(|e|e.to_string())?;
    }
    if tx.execute("DELETE FROM teams WHERE id = ?1",[id]).map_err(|e|e.to_string())?==0 {
        return Err(format!("takım yok: {id}"));
    }
    tx.commit().map_err(|e|e.to_string())
}

/// Görev #43: what deleting the team would stop — for the confirm dialog.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeleteInfo {
    /// Names of the members whose session runs (leader included).
    pub running: Vec<String>,
    /// Queued / running jobs the team sent, does (service team) or its members work on.
    pub open_delegations: i64,
    pub active_plans: i64,
}

pub fn delete_info(conn: &Connection, id: &str, is_running: impl Fn(&str) -> bool) -> Result<DeleteInfo, String> {
    let running = list_agents(conn)?.into_iter().filter(|a| a.team_id == id && is_running(&a.id)).map(|a| a.name).collect();
    let count = |sql: &str| conn.query_row(sql, [id], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string());
    Ok(DeleteInfo {
        running,
        open_delegations: count(
            "SELECT COUNT(*) FROM delegations WHERE status IN ('queued','running')
               AND (team_id = ?1 OR service_team_id = ?1 OR to_agent IN (SELECT id FROM agents WHERE team_id = ?1))",
        )?,
        active_plans: count("SELECT COUNT(*) FROM plans WHERE team_id = ?1 AND status = 'active'")?,
    })
}

/// Görev #43: before the team's sessions are closed for deletion, nothing may start them again —
/// a session exit starts the member's next queued job and advances active plans. Those plans are
/// cancelled and the queued jobs fail (both go with the team's rows anyway). Returns the ids of
/// the failed jobs.
pub fn freeze_for_delete(conn: &Connection, id: &str) -> Result<Vec<i64>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    conn.execute("UPDATE plans SET status = 'cancelled', updated_at = ?2 WHERE team_id = ?1 AND status = 'active'", params![id, now()])
        .map_err(e)?;
    let queued = "status = 'queued' AND to_agent IN (SELECT id FROM agents WHERE team_id = ?1)";
    let ids = conn
        .prepare(&format!("SELECT id FROM delegations WHERE {queued}"))
        .map_err(e)?
        .query_map([id], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<Vec<i64>, _>>()
        .map_err(e)?;
    conn.execute(
        &format!("UPDATE delegations SET status = 'failed', summary = IFNULL(summary, 'takım silindi; iş başlamadı'), updated_at = ?2 WHERE {queued}"),
        params![id, now()],
    )
    .map_err(e)?;
    Ok(ids)
}

pub fn list_agents(conn: &Connection) -> Result<Vec<Agent>, String> {
    let mut st = conn
        .prepare(&format!("SELECT {AGENT_COLS} FROM {AGENT_FROM} ORDER BY a.team_id, a.sort, a.id"))
        .map_err(|e| e.to_string())?;
    let rows = st.query_map([], agent_row).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub fn get_agent(conn: &Connection, id: &str) -> Result<Agent, String> {
    conn.query_row(&format!("SELECT {AGENT_COLS} FROM {AGENT_FROM} WHERE a.id = ?1"), [id], agent_row)
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ajan yok: {id}"))
}

/// An empty `name` gets the role's template name (`auto_name`). `data_dir` finds the team's memory
/// root: the new slug skips folders left on disk (#436).
pub fn create_agent(conn: &Connection, data_dir: &Path, mut a: NewAgent) -> Result<Agent, String> {
    if a.role_slug.as_deref() == Some("assistant") && a.model.is_empty() { a.model = crate::providers::CLAUDE_MID.into(); }
    one_of(&a.kind, AGENT_KINDS, "ajan türü")?;
    let provider = match a.provider.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("anthropic") => "anthropic".to_string(),
        Some(p) => check_provider(p)?,
    };
    let engine = a.engine.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("claude").to_string();
    check_model(&engine, &provider, &a.model)?;
    let exists: bool = conn
        .query_row("SELECT 1 FROM teams WHERE id = ?1", [&a.team_id], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    if !exists {
        return Err(format!("takım yok: {}", a.team_id));
    }
    let role = a.role_slug.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let name = match a.name.trim() {
        "" => auto_name(conn, &a.team_id, &a.kind, role)?,
        n => n.to_string(),
    };
    let sort: i64 = conn
        .query_row("SELECT COALESCE(MAX(sort) + 1, 0) FROM agents WHERE team_id = ?1", [&a.team_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let i = sort as usize;
    let mut title = a.title.trim().to_string();
    if let Some(slug) = role {
        let role_name = crate::roles::name_of(conn, slug)?;
        if title.is_empty() {
            title = role_name;
        }
    }
    let id = insert_with_new_id(&name, "agents", &mut os_rng, |id| {
        conn.execute(
            "INSERT INTO agents (id, team_id, name, kind, title, model, role_slug, shirt, hair, skin, sort, provider, engine)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id, a.team_id, name, a.kind, title, a.model, role,
                SHIRTS[i % SHIRTS.len()], HAIRS[(i * 5 + 1) % HAIRS.len()], SKINS[(i * 7 + 2) % SKINS.len()], sort,
                provider, engine
            ],
        )
        .map(|_| ())
    })?;
    let root = get_team(conn, data_dir, &a.team_id)?.memory_dir.map(PathBuf::from);
    crate::slug::assign(conn, &id, root.as_deref())?;
    get_agent(conn, &id)
}

pub fn update_agent(conn: &Connection, id: &str, p: AgentPatch) -> Result<Agent, String> {
    let mut a = get_agent(conn, id)?;
    if let Some(v) = p.name {
        a.name = check_name(&v)?;
    }
    if let Some(v) = p.kind {
        one_of(&v, AGENT_KINDS, "ajan türü")?;
        a.kind = v;
    }
    let retitle = p.title.is_none();
    // DEX-30: a hand-picked engine/provider/model overrides a bulk change still waiting.
    let overrides_pending = p.engine.is_some() || p.provider.is_some() || p.model.is_some();
    if let Some(v) = p.title {
        a.title = v.trim().to_string();
    }
    if let Some(v) = p.provider {
        a.provider = check_provider(&v)?;
    }
    if let Some(v) = p.model {
        a.model = v;
    }
    if let Some(v) = p.engine {
        a.engine = v.trim().to_string();
    }
    if let Some(v) = p.chain_off {
        a.chain_off = v;
    }
    // Nihai (motor, provider, model) üçlüsü tutarlı olmalı; yarım patch kötü durum bırakmaz.
    check_model(&a.engine, &a.provider, &a.model)?;
    if let Some(v) = p.role_slug {
        let v = v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let role_name = v.as_deref().map(|s| crate::roles::name_of(conn, s)).transpose()?;
        if retitle && v != a.role_slug {
            if let Some(n) = &role_name {
                a.title = n.clone();
            }
        }
        a.role_slug = v;
        a.role_name = role_name;
    }
    conn.execute(
        "UPDATE agents SET name = ?2, kind = ?3, title = ?4, model = ?5, role_slug = ?6, provider = ?7, engine = ?8, chain_off = ?9 WHERE id = ?1",
        params![id, a.name, a.kind, a.title, a.model, a.role_slug, a.provider, a.engine, i64::from(a.chain_off)],
    )
    .map_err(|e| e.to_string())?;
    if overrides_pending {
        conn.execute(
            "UPDATE agents SET active_engine = NULL, active_provider = NULL, active_model = NULL, chain_return_pending = 0 WHERE id = ?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        a.active_engine = None;
        a.active_provider = None;
        a.active_model = None;
        a.chain_return_ready = false;
    }
    if overrides_pending && a.pending.is_some() {
        set_pending(conn, id, None)?;
        a.pending = None;
    }
    Ok(a)
}

/// DEX-30: `engine` + optional `provider` + `model` → the stored triple, validated.
fn resolve_target(t: &PendingModel) -> Result<PendingModel, String> {
    let engine = t.engine.trim().to_string();
    one_of(&engine, ENGINES, "motor")?;
    let provider = match t.provider.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Some("anthropic".to_string()),
        Some(p) => Some(check_provider(p)?),
    };
    let model = t.model.trim().to_string();
    check_model(&engine, provider.as_deref().unwrap_or("anthropic"), &model)?;
    Ok(PendingModel { engine, provider, model })
}

fn set_pending(conn: &Connection, id: &str, p: Option<&PendingModel>) -> Result<(), String> {
    conn.execute(
        "UPDATE agents SET pending_engine = ?2, pending_provider = ?3, pending_model = ?4 WHERE id = ?1",
        params![id, p.map(|p| &p.engine), p.and_then(|p| p.provider.as_ref()), p.map(|p| &p.model)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Writes the target over the agent's engine/provider/model and clears anything pending.
fn apply_target(conn: &Connection, id: &str, t: &PendingModel) -> Result<(), String> {
    conn.execute(
        "UPDATE agents SET engine = ?2, provider = COALESCE(?3, provider), model = ?4,
             pending_engine = NULL, pending_provider = NULL, pending_model = NULL,
             active_engine = NULL, active_provider = NULL, active_model = NULL WHERE id = ?1",
        params![id, t.engine, t.provider, t.model],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// DEX-30: every `worker` of the team gets `target`; leaders and the personal assistant are never touched. `live(id)` says
/// whose session is open: those can't switch under a running CLI, so theirs is stored as pending
/// and lands via [`apply_pending`] when the session ends. One transaction: all or nothing.
pub fn set_workers_model(
    conn: &Connection,
    team_id: &str,
    target: &PendingModel,
    live: impl Fn(&str) -> bool,
) -> Result<WorkersOutcome, String> {
    let t = resolve_target(target)?;
    conn.query_row("SELECT 1 FROM teams WHERE id = ?1", [team_id], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("takım yok: {team_id}"))?;
    let workers: Vec<Agent> = list_agents(conn)?.into_iter().filter(|a| a.team_id == team_id).collect();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut out = WorkersOutcome::default();
    for a in workers.iter().filter(|a| a.kind == "worker" && a.role_slug.as_deref() != Some("assistant")) {
        let same = a.engine == t.engine && a.model == t.model && t.provider.as_ref().is_none_or(|p| *p == a.provider);
        if !live(&a.id) {
            apply_target(&tx, &a.id, &t)?;
            out.applied.push(a.id.clone());
        } else if same {
            // Already running what was asked: an older pending change is void.
            set_pending(&tx, &a.id, None)?;
        } else {
            set_pending(&tx, &a.id, Some(&t))?;
            out.pending.push(a.id.clone());
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Görev #94: asıl sağlayıcısı `provider` olup şu an zincir geçici hedefinde olan ajanlar
/// (otomatik geri dönüş adayları).
pub fn chain_displaced(conn: &Connection, provider: &str) -> Result<Vec<Agent>, String> {
    Ok(list_agents(conn)?
        .into_iter()
        .filter(|a| {
            a.engine == "claude" && a.provider == provider && (a.active_provider.is_some() || a.active_engine.is_some())
        })
        .collect())
}

/// Görev #94: çalışanın dönüşünü oturum kapanışına ertele (ya da bayrağı kaldır).
pub fn set_chain_return_pending(conn: &Connection, id: &str, pending: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE agents SET chain_return_pending = ?2 WHERE id = ?1",
        params![id, i64::from(pending)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Görev #94: bayrak set miydi (okunur ve temizlenir).
pub fn take_chain_return_pending(conn: &Connection, id: &str) -> Result<bool, String> {
    let was: i64 = conn
        .query_row("SELECT chain_return_pending FROM agents WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    if was != 0 {
        set_chain_return_pending(conn, id, false)?;
    }
    Ok(was != 0)
}

/// DEX-30: lands the agent's pending change (the caller knows its session is closed). `Ok(true)`
/// = something changed. A target the catalog no longer accepts is dropped, with the error.
pub fn apply_pending(conn: &Connection, id: &str) -> Result<bool, String> {
    let Some(p) = get_agent(conn, id)?.pending else { return Ok(false) };
    match resolve_target(&p) {
        Ok(t) => apply_target(conn, id, &t).map(|_| true),
        Err(err) => {
            set_pending(conn, id, None)?;
            Err(format!("bekleyen model değişikliği uygulanamadı: {err}"))
        }
    }
}

/// At startup no session survives: every pending change lands. Returns how many did.
pub fn apply_all_pending(conn: &Connection) -> Result<usize, String> {
    let ids: Vec<String> = list_agents(conn)?.into_iter().filter(|a| a.pending.is_some()).map(|a| a.id).collect();
    let mut n = 0;
    for id in ids {
        match apply_pending(conn, &id) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(err) => eprintln!("dex: {id}: {err}"),
        }
    }
    Ok(n)
}

pub fn delete_agent(conn: &Connection, id: &str) -> Result<(), String> {
    match conn.execute("DELETE FROM agents WHERE id = ?1", [id]) {
        Ok(0) => Err(format!("ajan yok: {id}")),
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn assistant_defaults_to_claude_mid_and_echo() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let a: super::NewAgent = serde_json::from_value(serde_json::json!({"teamId":"default","name":"","kind":"worker","roleSlug":"assistant"})).unwrap();
        let agent = super::create_agent(&c, std::path::Path::new("/data"), a).unwrap();
        assert_eq!((agent.name.as_str(), agent.engine.as_str(), agent.model.as_str(), agent.provider.as_str()), ("Echo-1", "claude", crate::providers::CLAUDE_MID, "anthropic"));
    }
    use super::*;

    #[test]
    fn education_team_has_teacher_and_one_visual_worker_without_worktrees() {
        let f = crate::projects::tests::Fixture::new();
        let root = f.0.join("lessons-team");
        std::fs::create_dir(&root).unwrap();
        let c = mem();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Education","kind":"main","profile":"edu","cwd":root})).unwrap();
        let t = create_team(&c, &f.0, input).unwrap();
        assert_eq!(serde_json::to_value(&t).unwrap()["profile"], "edu");
        assert!(!t.worktrees);
        let crew = crew_of(&c, &t.id);
        let shape: Vec<_> = crew.iter().map(|a| (a.kind.as_str(), a.role_slug.as_deref().unwrap(), a.name.as_str())).collect();
        assert_eq!(shape, [("leader", "teacher", "Sage"), ("worker", "visual-explainer", "Chalk-1")]);
        assert_eq!((crew[0].engine.as_str(), crew[0].model.as_str()), ("claude", crate::providers::CLAUDE_STRONG));
        assert_eq!((crew[1].engine.as_str(), crew[1].provider.as_str(), crew[1].model.as_str()), ("claude", "anthropic", crate::providers::CLAUDE_MID));
        assert!(crate::roles::browser_for_agent(&c, &crew[1].id));
        assert!(crate::skills::skills_for_agent(&c, &crew[0].id).unwrap().is_empty());
        assert!(crate::skills::skills_for_agent(&c, &crew[1].id).unwrap().is_empty());
        let ctx = crate::project_context::resolve(&c, &f.0, crate::project_context::ResolveRequest {
            team_id: &t.id, agent_id: Some(&crew[1].id), delegation_id: None, project_id: None,
            mode: crate::project_context::ContextMode::Selection,
        }).unwrap();
        assert!(!ctx.worktrees);
        assert!(crate::project_context::validate(&ctx).is_ok(), "plain folder must work");
        assert!(!root.join(".git").exists());
        assert_eq!(serde_json::to_value(clone_template(&c, &t.id).unwrap()).unwrap()["profile"], "edu");
    }

    #[test]
    fn education_team_memory_search_and_index_are_isolated_from_default() {
        let f = crate::projects::tests::Fixture::new();
        let root = f.0.join("education");
        std::fs::create_dir(&root).unwrap();
        let c = mem();
        let legacy_memory = f.0.join("default-memory");
        c.execute("UPDATE teams SET team_memory_root=?1,agent_memory_root=?1 WHERE id='default'", [legacy_memory.to_str().unwrap()]).unwrap();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Education","kind":"main","profile":"edu","cwd":root})).unwrap();
        let t = create_team(&c, &f.0, input).unwrap();
        let default = get_team(&c, &f.0, "default").unwrap();
        assert_ne!(t.team_memory_root, default.team_memory_root);
        let resolve = |team, agent| crate::project_context::resolve(&c, &f.0, crate::project_context::ResolveRequest {
            team_id: team, agent_id: Some(agent), delegation_id: None, project_id: None,
            mode: crate::project_context::ContextMode::Selection,
        }).unwrap();
        let crew = crew_of(&c, &t.id);
        let dev_ctx = resolve("default", "timur");
        let edu_ctx = resolve(&t.id, &crew[0].id);
        for (ctx, name) in [(&dev_ctx, "dev-only"), (&edu_ctx, "edu-only")] {
            let scope = crate::memory::SearchScope::for_context(&c, ctx).unwrap();
            for dir in [ctx.team_memory_root.join("shared"), scope.own] {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{name}.md")), format!("---\nname: {name}\ndescription: isolationneedle\nmetadata:\n  type: reference\n---\nisolationneedle\n")).unwrap();
            }
        }
        for (ctx, own, other) in [(&dev_ctx, "dev-only", "edu-only"), (&edu_ctx, "edu-only", "dev-only")] {
            let hits = crate::memory::search(&crate::memory::SearchScope::for_context(&c, ctx).unwrap(), "isolationneedle", 8);
            assert_eq!(hits.len(), 2);
            assert!(hits.iter().all(|h| h.slug == own));
            let index = crate::hooks::memory_index_for_context(&c, ctx, 8192).unwrap();
            assert!(index.contains(own) && !index.contains(other), "{index}");
        }
        for a in &crew {
            let ctx = resolve(&t.id, &a.id);
            if a.kind == "worker" {
                let hits = crate::memory::search(&crate::memory::SearchScope::for_context(&c, &ctx).unwrap(), "isolationneedle", 8);
                assert_eq!(hits.len(), 1, "worker sees its team share, not the teacher's private memory");
                assert_eq!(hits[0].slug, "edu-only");
                let index = crate::hooks::memory_index_for_context(&c, &ctx, 8192).unwrap();
                assert!(index.contains("edu-only") && !index.contains("dev-only"));
            }
            let prompt = crate::rules::system_prompt_for_context(&c, &a.id, &ctx);
            assert!(prompt.contains("## İş/proje bağlamı") && prompt.contains(&t.id));
            println!("EDU_PROMPT_BEGIN:{}\n{prompt}\nEDU_PROMPT_END", a.kind);
        }
    }

    #[test]
    fn education_profile_validates_crew_atomically_and_keeps_kind_independent() {
        let c = mem();
        let before = list_teams(&c, Path::new("/data")).unwrap().len();
        for body in [
            serde_json::json!({"profile":"unknown"}),
            serde_json::json!({"profile":"edu","workerRoles":["qa"]}),
            serde_json::json!({"profile":"edu","workerRoles":["visual-explainer","visual-explainer"]}),
            serde_json::json!({"profile":"edu","members":[]}),
            serde_json::json!({"profile":"edu","members":[{"name":"Sage","kind":"leader","model":"opus","roleSlug":"teacher"}]}),
        ] {
            let mut body = body;
            body["name"] = "Invalid".into();
            body["kind"] = "service".into();
            assert!(create_team(&c, Path::new("/data"), serde_json::from_value(body).unwrap()).is_err());
            assert_eq!(list_teams(&c, Path::new("/data")).unwrap().len(), before);
        }
        let edu = create_team(&c, Path::new("/data"), serde_json::from_value(serde_json::json!({"name":"Edu service","kind":"service","profile":"edu"})).unwrap()).unwrap();
        assert_eq!((edu.kind.as_str(), edu.profile.as_str(), edu.worktrees), ("service", "edu", false));
        assert_eq!(crew_of(&c, &edu.id).len(), 2);
        let template = serde_json::to_value(clone_template(&c, &edu.id).unwrap()).unwrap();
        let copy = create_team(&c, Path::new("/data"), serde_json::from_value(serde_json::json!({"name":"Copy","kind":template["kind"],"profile":template["profile"],"members":template["members"]})).unwrap()).unwrap();
        assert_eq!(copy.profile, "edu");
        assert_eq!(crew_of(&c, &copy.id).len(), 2);
    }

    #[test]
    fn legacy_main_layout_suggests_parent_without_writing() {
        let f = crate::projects::tests::Fixture::new();
        let root = f.0.join("StudioII/StudioIIMain");
        std::fs::create_dir_all(&root).unwrap();
        let c = mem();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Old","kind":"main","cwd":root})).unwrap();
        let t = create_team(&c,&f.0,input).unwrap();
        let before = c.total_changes();
        let loaded = get_team(&c,&f.0,&t.id).unwrap();
        assert_eq!(loaded.suggested_workspace_root, Some(normalize_dir(root.parent().unwrap().to_str().unwrap()).unwrap()));
        assert!(loaded.workspace_root.is_none(), "the backfilled placeholder is not a category root");
        let ctx = crate::project_context::resolve(&c,&f.0,crate::project_context::ResolveRequest {
            team_id:&t.id,agent_id:None,delegation_id:None,project_id:None,mode:crate::project_context::ContextMode::Team,
        }).unwrap();
        assert_eq!(serde_json::to_value(ctx).unwrap()["suggestedWorkspaceRoot"], serde_json::json!(loaded.suggested_workspace_root));
        assert_eq!(before,c.total_changes());
        assert_eq!(remote_adoptable_suggestion(&c,&f.0,&t.id).unwrap(), loaded.suggested_workspace_root, "the old Main layout stays adoptable");
        let changed = update_team(&c,&f.0,&t.id,TeamPatch {workspace_root:Some(Some(root.parent().unwrap().to_string_lossy().into())),..Default::default()}).unwrap();
        assert!(changed.suggested_workspace_root.is_none());
    }

    /// #2137: a legacy main team whose default project is not named `<Category>Main` still gets
    /// its real category suggested (the projects' parent), and the placeholder stays unset.
    #[test]
    fn legacy_team_suggests_project_parent_for_non_main_root_name() {
        let f = crate::projects::tests::Fixture::new();
        let root = f.0.join("JezDev/no-more-clickbait");
        std::fs::create_dir_all(&root).unwrap();
        let c = mem();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Jez Dev","kind":"main","cwd":root})).unwrap();
        let t = create_team(&c,&f.0,input).unwrap();
        let loaded = get_team(&c,&f.0,&t.id).unwrap();
        assert!(loaded.workspace_root.is_none());
        assert_eq!(loaded.suggested_workspace_root, Some(normalize_dir(root.parent().unwrap().to_str().unwrap()).unwrap()));
        assert_eq!(remote_adoptable_suggestion(&c,&f.0,&t.id).unwrap(), None, "UI advice, not a remote grant");
    }

    #[test]
    fn clone_template_only_returns_member_configuration_without_mutation() {
        let c = mem();
        c.execute("UPDATE agents SET chain_off=1 WHERE id='kaan'", []).unwrap();
        let before = c.total_changes();
        let template = serde_json::to_value(clone_template(&c, "default").unwrap()).unwrap();
        assert_eq!(template.as_object().unwrap().len(), 3);
        assert_eq!(template["kind"], "main");
        assert_eq!(template["profile"], "dev");
        assert_eq!(template["members"].as_array().unwrap().len(), list_agents(&c).unwrap().into_iter().filter(|a| a.team_id=="default").count());
        let name = get_agent(&c,"kaan").unwrap().name;
        let kaan = template["members"].as_array().unwrap().iter().find(|a| a["name"]==name).unwrap();
        assert_eq!(kaan["chainOff"], true);
        assert!(kaan["effort"].is_null());
        assert!(kaan.get("id").is_none() && kaan.get("teamId").is_none());
        assert_eq!(before, c.total_changes());
        assert_eq!(clone_template(&c,"missing").unwrap_err(),"team_not_found");
    }

    #[test]
    fn folder_preview_is_read_only_and_uses_creation_paths() {
        let f = crate::projects::tests::Fixture::new();
        let db = crate::db::Db::new(mem());
        let before = crate::db::with(&db, |c| Ok(c.total_changes())).unwrap();
        let preview = folder_preview(&db, " Studio II. ", Some(&f.0)).unwrap();
        assert_eq!(preview.folder_name, "StudioII");
        assert_eq!(Path::new(&preview.workspace_root), f.0.join("StudioII"));
        assert_eq!(Path::new(&preview.main_root), f.0.join("StudioII/StudioIIMain"));
        assert!(!preview.exists.workspace && !preview.exists.main);
        assert!(preview.owner.is_none());
        assert!(!f.0.join("StudioII").exists());
        assert_eq!(before, crate::db::with(&db, |c| Ok(c.total_changes())).unwrap());
        for bad in ["", "   ", "..", "CON", "/:*?"] {
            assert_eq!(folder_preview(&db, bad, Some(&f.0)).unwrap_err(), "invalid_folder_name");
        }
    }

    #[test]
    fn folder_preview_reports_existing_team_and_project_owners() {
        let f = crate::projects::tests::Fixture::new();
        let db = crate::db::Db::new(mem());
        let workspace = f.0.join("Studio");
        std::fs::create_dir(&workspace).unwrap();
        let preview = folder_preview(&db, "Studio", Some(&f.0)).unwrap();
        assert!(preview.exists.workspace && !preview.exists.main);
        std::fs::create_dir_all(workspace.join("StudioMain")).unwrap();
        let preview = folder_preview(&db, "Studio", Some(&f.0)).unwrap();
        assert!(preview.exists.workspace && preview.exists.main);
        assert!(preview.owner.is_none());
        crate::db::with(&db, |c| {
            c.execute("UPDATE teams SET workspace_root=?1 WHERE id='default'", [workspace.to_string_lossy().as_ref()]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        assert_eq!(folder_preview(&db, "Studio", Some(&f.0)).unwrap().owner.as_deref(), Some("default"));
        crate::db::with(&db, |c| {
            c.execute("UPDATE teams SET workspace_root=NULL WHERE id='default'", []).map_err(|e| e.to_string())?;
            let root = workspace.join("StudioMain").to_string_lossy().into_owned();
            c.execute("UPDATE projects SET root_path=?1,root_key=?2 WHERE id='legacy-default'", params![root, crate::projects::path_key(&root)]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        assert_eq!(folder_preview(&db, "Studio", Some(&f.0)).unwrap().owner.as_deref(), Some("legacy-default"));
        let before = crate::db::with(&db, |c| Ok(c.total_changes())).unwrap();
        let value = serde_json::to_value(folder_preview(&db, "Studio", Some(&f.0)).unwrap()).unwrap();
        assert_eq!(value["exists"], serde_json::json!({"workspace":true,"main":true}));
        assert_eq!(value["folderName"], "Studio");
        assert_eq!(before, crate::db::with(&db, |c| Ok(c.total_changes())).unwrap());
        crate::db::with(&db, |c| {
            c.execute("UPDATE projects SET archived_at=1 WHERE id='legacy-default'", []).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        assert_eq!(folder_preview(&db, "Studio", Some(&f.0)).unwrap().owner.as_deref(), Some("legacy-default"), "archived main root remains reserved");
        crate::db::with(&db, |c| {
            let root = workspace.to_string_lossy().into_owned();
            c.execute("UPDATE projects SET root_path=?1,root_key=?2 WHERE id='legacy-default'", params![root, crate::projects::path_key(&root)]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        assert_eq!(folder_preview(&db, "Studio", Some(&f.0)).unwrap().owner.as_deref(), Some("legacy-default"), "archived exact roots remain reserved");
    }

    #[test]
    fn folder_name_preserves_case_and_removes_invalid_characters() {
        assert_eq!(folder_name("Studio II").unwrap(), "StudioII");
        assert_eq!(folder_name(" Çığ /:*?\"<>|\\\tDev. ").unwrap(), "ÇığDev");
        for bad in [" /:*?\"<>|\\", ".", "..", "CON", "nul.txt", "LPT1"] {
            assert!(folder_name(bad).is_err(), "{bad}");
        }
    }

    fn mem() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn template_members_replace_defaults_and_errors_roll_back() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let member = serde_json::json!({"name":"Core","kind":"worker","title":"Backend","model":crate::providers::CLAUDE_MID,"engine":"claude","provider":"anthropic","roleSlug":"backend","chainOff":true,"effort":null});
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Copy","kind":"service","members":[member,member]})).unwrap();
        let t = create_team(&c, &f.0, input).unwrap();
        let agents: Vec<_> = list_agents(&c).unwrap().into_iter().filter(|a| a.team_id == t.id).collect();
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().all(|a| a.name == "Core" && a.chain_off));
        let slugs: Vec<String> = c.prepare("SELECT slug FROM agents WHERE team_id=?1 ORDER BY sort").unwrap().query_map([&t.id], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(slugs, ["core", "core-2"]);
        let before = (list_teams(&c,&f.0).unwrap().len(),list_agents(&c).unwrap().len());
        let bad: NewTeam = serde_json::from_value(serde_json::json!({"name":"Bad copy","kind":"service","members":[member,{"name":"Bad","kind":"worker","model":"invalid","engine":"invalid"}]})).unwrap();
        assert!(create_team(&c,&f.0,bad).is_err());
        assert_eq!(before,(list_teams(&c,&f.0).unwrap().len(),list_agents(&c).unwrap().len()));
        let root = f.dir("failed-copy");
        let project_count = c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_,i64>(0)).unwrap();
        let bad: NewTeam = serde_json::from_value(serde_json::json!({"name":"Bad main","kind":"main","cwd":root,"members":[member,{"name":"Bad","kind":"worker","model":crate::providers::CLAUDE_MID,"roleSlug":"missing-role"}]})).unwrap();
        assert!(create_team(&c,&f.0,bad).is_err());
        assert_eq!(before,(list_teams(&c,&f.0).unwrap().len(),list_agents(&c).unwrap().len()));
        assert_eq!(project_count,c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_,i64>(0)).unwrap());
        let source = serde_json::to_value(clone_template(&c,&t.id).unwrap()).unwrap();
        let copy: NewTeam = serde_json::from_value(serde_json::json!({"name":"Fresh copy","kind":source["kind"],"members":source["members"]})).unwrap();
        let copy = create_team(&c,&f.0,copy).unwrap();
        assert_ne!(copy.team_memory_root,t.team_memory_root);
        assert_ne!(copy.agent_memory_root,t.agent_memory_root);
        assert!(copy.default_project_id.is_none());
        assert_eq!(clone_template(&c,&copy.id).unwrap().members.len(),2);
    }

    #[test]
    fn default_team_folders_are_created_reused_and_claimed_atomically() {
        let f = crate::projects::tests::Fixture::new();
        let db = crate::db::Db::new(crate::db::open(&f.0.join("dex.db")).unwrap());
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Studio II","kind":"main"})).unwrap();
        let folders = prepare_team_folders(&db, &input, Some(&f.0)).unwrap().unwrap();
        assert!(f.0.join("StudioII/StudioIIMain").is_dir());
        std::fs::write(folders.project_root.join("keep.txt"), "keep").unwrap();
        let again = prepare_team_folders(&db, &input, Some(&f.0)).unwrap().unwrap();
        assert_eq!(folders.workspace_root, again.workspace_root);
        let c = db.0.lock().unwrap();
        let t = create_team_with_folders(&c, &f.0, input, Some(&folders)).unwrap();
        assert_eq!(t.workspace_root, Some(folders.workspace_root.to_string_lossy().into()));
        let p = crate::projects::get(&c, &t.id, t.default_project_id.as_deref().unwrap()).unwrap();
        assert_eq!(p.name, "StudioIIMain");
        assert_eq!(p.root_path, t.cwd);
        let raced: NewTeam = serde_json::from_value(serde_json::json!({"name":"Studio II","kind":"main"})).unwrap();
        assert_eq!(create_team_with_folders(&c,&f.0,raced,Some(&folders)).unwrap_err(),"project_root_conflict");
        drop(c);
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Studio II","kind":"main"})).unwrap();
        assert_eq!(prepare_team_folders(&db, &input, Some(&f.0)).unwrap_err(), "project_root_conflict");
        assert_eq!(std::fs::read_to_string(folders.project_root.join("keep.txt")).unwrap(), "keep");
        assert_eq!(db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM teams", [], |r| r.get::<_,i64>(0)).unwrap(), 2);
        let owned = f.0.join("Owned");
        std::fs::create_dir(&owned).unwrap();
        let owner: NewTeam = serde_json::from_value(serde_json::json!({"name":"Other owner","kind":"main","cwd":owned})).unwrap();
        create_team(&db.0.lock().unwrap(),&f.0,owner).unwrap();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Owned","kind":"main"})).unwrap();
        assert_eq!(prepare_team_folders(&db,&input,Some(&f.0)).unwrap_err(),"project_root_conflict");
        assert!(!owned.join("OwnedMain").exists());
    }

    #[cfg(any(windows, unix))]
    fn linked_team_folder_escape_is_rejected(project_link: bool, junction: bool) {
        let f = crate::projects::tests::Fixture::new();
        let home = f.0.join("home");
        // A lexical prefix is not containment: this directory is outside home.
        let outside = f.0.join("home-outside");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        let workspace = home.join("Linked");
        let link = if project_link {
            std::fs::create_dir(&workspace).unwrap();
            workspace.join("LinkedMain")
        } else {
            workspace.clone()
        };
        #[cfg(windows)]
        if junction {
            let output = std::process::Command::new("cmd")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(output.status.success(), "junction creation failed: {output:?}");
        } else if let Err(error) = std::os::windows::fs::symlink_dir(&outside, &link) {
            if matches!(error.raw_os_error(), Some(1314) | Some(50)) {
                eprintln!("directory symlinks unavailable: {error}");
                return;
            }
            panic!("symlink creation failed: {error}");
        }
        #[cfg(unix)]
        {
            assert!(!junction);
            std::os::unix::fs::symlink(&outside, &link).unwrap();
        }
        assert_eq!(normalize_dir(link.to_str().unwrap()).unwrap(), normalize_dir(outside.to_str().unwrap()).unwrap());
        let db = crate::db::Db::new(mem());
        let before = db.0.lock().unwrap().total_changes();
        let input: NewTeam = serde_json::from_value(serde_json::json!({"name":"Linked","kind":"main"})).unwrap();
        assert_eq!(folder_preview(&db, &input.name, Some(&home)).unwrap_err(), "project_root_conflict");
        assert_eq!(db.0.lock().unwrap().total_changes(), before, "preview must not write to the database");
        let entries: Vec<_> = std::fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries, [std::ffi::OsString::from("keep.txt")], "preview must leave the outside target untouched");
        assert_eq!(std::fs::read_to_string(outside.join("keep.txt")).unwrap(), "keep");
        if !project_link {
            assert!(!workspace.join("LinkedMain").exists(), "preview must not create the planned main folder");
        }
        assert_eq!(prepare_team_folders(&db, &input, Some(&home)).unwrap_err(), "project_root_conflict");
        assert_eq!(db.0.lock().unwrap().total_changes(), before);
        let entries: Vec<_> = std::fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries, [std::ffi::OsString::from("keep.txt")], "outside target must remain untouched");
        assert_eq!(std::fs::read_to_string(outside.join("keep.txt")).unwrap(), "keep");
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn workspace_junction_escape_is_rejected_before_writing() {
        linked_team_folder_escape_is_rejected(false, true);
    }

    #[cfg(windows)]
    #[test]
    fn project_junction_escape_is_rejected_before_writing() {
        linked_team_folder_escape_is_rejected(true, true);
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn workspace_symlink_escape_is_rejected_before_writing() {
        linked_team_folder_escape_is_rejected(false, false);
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn project_symlink_escape_is_rejected_before_writing() {
        linked_team_folder_escape_is_rejected(true, false);
    }

    #[test]
    fn project_team_creation_is_atomic_and_preserves_legacy_memory_anchor() {
        let f=crate::projects::tests::Fixture::new();
        let c=crate::db::open(&f.0.join("dex.db")).unwrap();
        let root=f.dir("new-root");
        let t=create_team(&c,&f.0,NewTeam{ profile: None,name:"New".into(),kind:"main".into(),cwd:Some(root.to_string_lossy().into()),worker_roles:None, members: None }).unwrap();
        let p=crate::projects::get(&c,&t.id,t.default_project_id.as_deref().unwrap()).unwrap();
        assert_eq!(p.root_path,t.cwd);
        assert_eq!(t.team_memory_root,Some(service_dir(&f.0,&t.id).join("memory").to_string_lossy().into()));
        let legacy_root=f.dir("legacy-root").to_string_lossy().into_owned();
        c.execute("INSERT INTO teams(id,name,kind,cwd,created_at) VALUES('old','Old','main',?1,1)",[&legacy_root]).unwrap();
        crate::projects::backfill(&c,&f.0).unwrap();
        let old=get_team(&c,&f.0,"old").unwrap();
        let anchor=Path::new(&legacy_root).join(".dex").join("memory").to_string_lossy().into_owned();
        assert_eq!(old.team_memory_root.as_deref(),Some(anchor.as_str()));
        assert_eq!(old.agent_memory_root.as_deref(),Some(anchor.as_str()));
        assert_eq!(old.memory_dir.as_deref(),Some(anchor.as_str()));
        assert!(!Path::new(&anchor).exists(),"backfill moves/creates no memory files");
        update_team(&c,&f.0,"old",TeamPatch{default_project_id:Some(None),..Default::default()}).unwrap();
        let cleared=get_team(&c,&f.0,"old").unwrap();
        assert_eq!(cleared.memory_dir.as_deref(),Some(anchor.as_str()),"slug migration keeps the legacy anchor after selecting team context");
        let before=list_teams(&c,&f.0).unwrap().len();
        let bad=f.dir("bad-root");
        assert!(create_team(&c,&f.0,NewTeam{ profile: None,name:"Bad".into(),kind:"main".into(),cwd:Some(bad.to_string_lossy().into()),worker_roles:Some(vec!["not-a-role".into();4]), members: None }).is_err());
        assert_eq!(list_teams(&c,&f.0).unwrap().len(),before);
    }

    #[test]
    fn project_legacy_team_update_routes_root_and_rejects_busy_or_ambiguous() {
        let f=crate::projects::tests::Fixture::new();
        let c=crate::db::open(&f.0.join("dex.db")).unwrap();
        let t=create_team(&c,&f.0,NewTeam{ profile: None,name:"Category".into(),kind:"main".into(),cwd:None,worker_roles:None, members: None }).unwrap();
        let pid=t.default_project_id.unwrap();
        let root=f.dir("root");
        let changed=update_team(&c,&f.0,&t.id,TeamPatch{cwd:Some(Some(root.to_string_lossy().into())),..Default::default()}).unwrap();
        assert_eq!(crate::projects::get(&c,&t.id,&pid).unwrap().root_path,changed.cwd);
        c.execute("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,project_id,project_root,created_at,updated_at) VALUES('l','w',?1,'x',?2,?3,1,1)",params![t.id,pid,changed.cwd]).unwrap();
        let second=f.dir("second");
        assert_eq!(update_team(&c,&f.0,&t.id,TeamPatch{cwd:Some(Some(second.to_string_lossy().into())),..Default::default()}).unwrap_err(),"project_busy");
        c.execute("UPDATE delegations SET status='done' WHERE team_id=?1",[&t.id]).unwrap();
        let prepared=crate::projects::prepare_root(second.to_str().unwrap(),None).unwrap();
        crate::projects::create(&c,&t.id,crate::projects::NewProject{name:"Second".into(),worktrees:None},&prepared).unwrap();
        assert_eq!(update_team(&c,&f.0,&t.id,TeamPatch{cwd:Some(Some(second.to_string_lossy().into())),..Default::default()}).unwrap_err(),"project_mismatch");
    }

    #[test]
    fn project_team_delete_clears_external_references_atomically() {
        let f=crate::projects::tests::Fixture::new();
        let c=crate::db::open(&f.0.join("dex.db")).unwrap();
        c.execute_batch("INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,created_at,project_id) VALUES(1,'default','timur','kaan','[]','x',1,1,'legacy-default');
            INSERT INTO gpu_grants(id,request_id,team_id,worker,types,budget,created_at,project_id) VALUES(1,1,'default','kaan','[]',1,1,'legacy-default');
            INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id) VALUES('kaan','s','claude',1,'legacy-default');
            INSERT INTO agent_chain_origins(agent_id,engine,project_id) VALUES('kaan','claude','legacy-default');").unwrap();
        delete_team(&c,"default").unwrap();
        assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
        assert_eq!(c.query_row("SELECT COUNT(*) FROM projects",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert!(c.query_row("SELECT project_id FROM gpu_grants WHERE id=1",[],|r|r.get::<_,Option<String>>(0)).unwrap().is_none());
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-teams-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn seq_rng(mut draws: Vec<u8>) -> impl FnMut(&mut [u8]) -> Result<(), String> {
        draws.reverse();
        move |raw| {
            raw.fill(0);
            raw[ID_RANDOM_BYTES - 1] = draws.pop().expect("rng drained");
            Ok(())
        }
    }

    const INS_TEAM: &str = "INSERT INTO teams (id, name, kind, cwd, created_at) VALUES (?1, 'T', 'service', NULL, 0)";

    #[test]
    fn id_has_48_random_bits() {
        let id = new_id_with("Pixel 1", &mut os_rng).unwrap();
        let (slug, hex) = id.rsplit_once('-').unwrap();
        assert_eq!(slug, "pixel-1");
        assert_eq!(hex.len(), 12);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }

    #[test]
    fn same_name_many_agents_never_collide() {
        let c = mem();
        let t = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "T".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let mut ids = std::collections::HashSet::new();
        for _ in 0..2000 {
            let a = create_agent(&c, Path::new("/data"), NewAgent {
                team_id: t.id.clone(), name: "core".into(), kind: "worker".into(), title: String::new(),
                model: crate::providers::CLAUDE_MID.into(), provider: None, role_slug: None, engine: None,
            })
            .unwrap();
            assert!(ids.insert(a.id));
        }
    }

    #[test]
    fn id_collision_retries_with_a_fresh_id() {
        let c = mem();
        c.execute(INS_TEAM, ["core-000000000001"]).unwrap();
        let mut tried = vec![];
        let id = insert_with_new_id("core", "teams", &mut seq_rng(vec![1, 1, 2]), |id| {
            tried.push(id.to_string());
            c.execute(INS_TEAM, [id]).map(|_| ())
        })
        .unwrap();
        assert_eq!(id, "core-000000000002");
        assert_eq!(tried, ["core-000000000001", "core-000000000001", "core-000000000002"]);
    }

    #[test]
    fn id_collision_gives_up_after_the_attempts() {
        let c = mem();
        c.execute(INS_TEAM, ["core-000000000001"]).unwrap();
        let mut n = 0;
        let r = insert_with_new_id("core", "teams", &mut seq_rng(vec![1; ID_ATTEMPTS]), |id| {
            n += 1;
            c.execute(INS_TEAM, [id]).map(|_| ())
        });
        assert!(r.unwrap_err().contains("UNIQUE"));
        assert_eq!(n, ID_ATTEMPTS);
    }

    #[test]
    fn other_unique_violation_is_not_retried() {
        let c = mem();
        let cwd = tmp("cwd-unique").to_string_lossy().to_string();
        c.execute("INSERT INTO teams (id, name, kind, cwd, created_at) VALUES ('a', 'A', 'main', ?1, 0)", [&cwd]).unwrap();
        let mut n = 0;
        let r = insert_with_new_id("b", "teams", &mut seq_rng(vec![1, 2, 3]), |id| {
            n += 1;
            c.execute("INSERT INTO teams (id, name, kind, cwd, created_at) VALUES (?1, 'B', 'main', ?2, 0)", params![id, cwd]).map(|_| ())
        });
        assert!(r.is_err());
        assert_eq!(n, 1);
    }

    #[test]
    fn seed_moves_existing_crew_into_default_team() {
        let c = mem();
        let teams = list_teams(&c, Path::new("/data")).unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!((teams[0].id.as_str(), teams[0].kind.as_str(), teams[0].cwd.as_deref()), ("default", "main", None));
        let agents = list_agents(&c).unwrap();
        let ids: Vec<_> = agents.iter().map(|a| (a.id.as_str(), a.kind.as_str())).collect();
        assert_eq!(ids, [("timur", "leader"), ("kaan", "worker"), ("ayla", "worker"), ("bora", "worker")]);
    }

    #[test]
    fn folder_belongs_to_one_team() {
        let c = mem();
        let d = tmp("unique");
        let data = Path::new("/data");
        let a = create_team(&c, data, NewTeam { profile: None, name: "A".into(), kind: "main".into(), cwd: Some(d.to_string_lossy().into()), worker_roles: None, members: None }).unwrap();
        assert_eq!(a.workdir, a.cwd);
        assert!(a.memory_dir.as_deref().unwrap().ends_with(&format!(".dex{}memory", std::path::MAIN_SEPARATOR)));
        // Same folder spelled differently still collides.
        let alias = format!("{}{}.", d.to_string_lossy(), std::path::MAIN_SEPARATOR);
        let err = create_team(&c, data, NewTeam { profile: None, name: "B".into(), kind: "main".into(), cwd: Some(alias), worker_roles: None, members: None }).unwrap_err();
        assert!(err.contains("\"A\""), "{err}");
        // Raw SQL can't bypass it either.
        assert!(c.execute("INSERT INTO teams VALUES ('x','X','main',?1,0)", [&a.cwd]).is_err());
        // Re-claiming its own folder is fine.
        update_team(&c, data, &a.id, TeamPatch { cwd: Some(a.cwd.clone()), ..Default::default() }).unwrap();
        let category=create_team(&c, data, NewTeam { profile: None, name: "C".into(), kind: "main".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        assert!(category.cwd.is_none());
        assert!(category.default_project_id.is_some());
        assert!(create_team(&c, data, NewTeam { profile: None, name: "C".into(), kind: "main".into(), cwd: Some("Z:/yok/böyle".into()), worker_roles: None, members: None }).is_err());
    }

    #[test]
    fn service_team_lives_in_app_data() {
        let c = mem();
        let data = Path::new("/data");
        let s = create_team(&c, data, NewTeam { profile: None, name: "Araştırma".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        assert!(s.id.starts_with("arastirma-"), "{}", s.id);
        assert_eq!(s.cwd, None);
        assert_eq!(s.workdir.as_deref().map(PathBuf::from), Some(data.join("teams").join(&s.id).join("workspace")));
        assert_eq!(s.memory_dir.as_deref().map(PathBuf::from), Some(data.join("teams").join(&s.id).join("memory")));
        assert!(create_team(&c, data, NewTeam { profile: None, name: "X".into(), kind: "service".into(), cwd: Some(".".into()), worker_roles: None, members: None }).is_err());
        assert!(update_team(&c, data, &s.id, TeamPatch { cwd: Some(Some(".".into())), ..Default::default() }).is_err());
        assert!(create_team(&c, data, NewTeam { profile: None, name: "X".into(), kind: "other".into(), cwd: None, worker_roles: None, members: None }).is_err());
    }

    #[test]
    fn auto_name_takes_the_first_free_number() {
        let c = mem();
        let t = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "T".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let add = |name: &str, role: Option<&str>| {
            create_agent(&c, Path::new("/data"), NewAgent {
                team_id: t.id.clone(), name: name.into(), kind: "worker".into(), title: String::new(), model: "sonnet".into(),
                provider: None, role_slug: role.map(String::from), engine: None,
            })
            .unwrap()
            .name
        };
        // A hand-named "kernel-3" (any case) is skipped; the hole at -2 is filled first.
        assert_eq!(add("kernel-3", Some("backend")), "kernel-3");
        assert_eq!(add("  ", Some("backend")), "Kernel-2");
        assert_eq!(add("", Some("backend")), "Kernel-4");
        assert_eq!(add("Ada", Some("backend")), "Ada", "an explicit name is kept as is");
        assert_eq!(add("", None), "Agent-1");
        assert_eq!(add("", Some("code-automation")), "Bot-1");
        assert_eq!(add("", Some("n8n-automation")), "Bot-2");
        assert_eq!(add("", Some("mobile")), "Droid-1");
        assert_eq!(add("", Some("mobile")), "Droid-2");
        // A second leader: Core is taken.
        assert_eq!(auto_name(&c, &t.id, "leader", Some("lead")).unwrap(), "Core-2");
        // Numbering is per team: U has its own Kernel-1, but no Canvas.
        let u = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "U".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        assert_eq!(auto_name(&c, &u.id, "worker", Some("backend")).unwrap(), "Kernel-2");
        assert_eq!(auto_name(&c, &u.id, "worker", Some("design")).unwrap(), "Canvas-1");
    }

    #[test]
    fn new_team_gets_lead_and_four_workers() {
        let c = mem();
        let data = Path::new("/data");
        let t = create_team(&c, data, NewTeam { profile: None, name: "T".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let crew: Vec<Agent> = list_agents(&c).unwrap().into_iter().filter(|a| a.team_id == t.id).collect();
        let shape: Vec<_> = crew.iter().map(|a| (a.kind.as_str(), a.role_slug.as_deref().unwrap(), a.model.as_str())).collect();
        assert_eq!(shape, [
            ("leader", "lead", "claude-opus-5-5"), ("worker", "frontend", "claude-sonnet-5-5"), ("worker", "backend", "claude-sonnet-5-5"),
            ("worker", "qa", "claude-sonnet-5-5"), ("worker", "explorer", "claude-sonnet-5-5"),
        ]);
        assert_eq!((crew[0].title.as_str(), crew[0].role_name.as_deref()), ("Lead", Some("Lead")));
        let names: Vec<_> = crew.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Core", "Pixel-1", "Kernel-1", "Probe-1", "Scout-1"]);

        let roles = Some(vec!["design".into(), "devops".into(), "security".into(), "pm".into()]);
        let u = create_team(&c, data, NewTeam { profile: None, name: "U".into(), kind: "service".into(), cwd: None, worker_roles: roles, members: None }).unwrap();
        let slugs: Vec<_> = list_agents(&c).unwrap().into_iter().filter(|a| a.team_id == u.id).filter_map(|a| a.role_slug).collect();
        assert_eq!(slugs, ["lead", "design", "devops", "security", "pm"]);

        let qa = Some(vec!["qa".into(), "qa".into(), "seo".into(), "qa".into()]);
        let q = create_team(&c, data, NewTeam { profile: None, name: "Q".into(), kind: "service".into(), cwd: None, worker_roles: qa, members: None }).unwrap();
        let names: Vec<_> = list_agents(&c).unwrap().into_iter().filter(|a| a.team_id == q.id).map(|a| a.name).collect();
        assert_eq!(names, ["Core", "Probe-1", "Probe-2", "Rank-1", "Probe-3"]);

        // Wrong count / unknown role: nothing is left behind.
        let before = list_teams(&c, data).unwrap().len();
        assert!(create_team(&c, data, NewTeam { profile: None, name: "V".into(), kind: "service".into(), cwd: None, worker_roles: Some(vec!["qa".into()]), members: None }).is_err());
        let bad = Some(vec!["qa".into(), "qa".into(), "qa".into(), "yok".into()]);
        assert!(create_team(&c, data, NewTeam { profile: None, name: "W".into(), kind: "service".into(), cwd: None, worker_roles: bad, members: None }).is_err());
        assert_eq!(list_teams(&c, data).unwrap().len(), before);

        // Role change retitles unless a title is given; null clears.
        let w = &crew[1];
        let p: AgentPatch = serde_json::from_str(r#"{"roleSlug":"security"}"#).unwrap();
        let a = update_agent(&c, &w.id, p).unwrap();
        assert_eq!((a.title.as_str(), a.role_slug.as_deref()), ("Security Engineer", Some("security")));
        let p: AgentPatch = serde_json::from_str(r#"{"roleSlug":null}"#).unwrap();
        let a = update_agent(&c, &w.id, p).unwrap();
        assert_eq!((a.title.as_str(), a.role_slug), ("Security Engineer", None));
        assert!(update_agent(&c, &w.id, serde_json::from_str(r#"{"roleSlug":"yok"}"#).unwrap()).is_err());
    }

    #[test]
    fn agents_crud_and_cascade() {
        let c = mem();
        let data = Path::new("/data");
        let s = create_team(&c, data, NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let mk = |name: &str, kind: &str, model: &str| {
            create_agent(&c, Path::new("/data"), NewAgent { team_id: s.id.clone(), name: name.into(), kind: kind.into(), title: " Araştırmacı ".into(), model: model.into(), provider: None, role_slug: None, engine: None })
        };
        let a = mk("Deniz", "leader", "haiku").unwrap();
        let b = mk("Ece", "worker", "sonnet").unwrap();
        assert_eq!((a.sort, b.sort, a.title.as_str()), (5, 6, "Araştırmacı"));
        assert_ne!(a.shirt, b.shirt);
        assert!(mk("X", "boss", "opus").is_err());
        assert!(mk("X", "worker", "gpt").is_err());
        assert!(create_agent(&c, Path::new("/data"), NewAgent { team_id: "yok".into(), name: "X".into(), kind: "worker".into(), title: String::new(), model: "opus".into(), provider: None, role_slug: None, engine: None }).is_err());

        let p: AgentPatch = serde_json::from_str(r#"{"model":"opus"}"#).unwrap();
        assert_eq!(update_agent(&c, &a.id, p).unwrap().model, "opus");
        delete_agent(&c, &b.id).unwrap();
        assert!(delete_agent(&c, &b.id).is_err());

        crate::db::tasks::create(&c, crate::db::tasks::NewTask {
            title: "t".into(), description: String::new(), status: None, agent_id: None, team_id: Some(s.id.clone()),
        }).unwrap();
        delete_team(&c, &s.id).unwrap();
        assert!(get_agent(&c, &a.id).is_err());
        assert!(crate::db::tasks::list(&c, Some(&s.id)).unwrap().is_empty());
        // FIX-5: an empty host is valid — the last team can go too, and an unknown id still fails.
        delete_team(&c, "default").unwrap();
        assert!(list_teams(&c, data).unwrap().is_empty());
        assert!(delete_team(&c, "default").is_err());
    }

    #[test]
    fn providers_validate_model_and_persist() {
        let c = mem();
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let mk = |name: &str, provider: Option<&str>, model: &str| {
            create_agent(&c, Path::new("/data"), NewAgent {
                team_id: s.id.clone(), name: name.into(), kind: "worker".into(), title: String::new(),
                model: model.into(), provider: provider.map(String::from), role_slug: None, engine: None,
            })
        };
        let g = mk("Glmci", Some("glm"), "GLM-5.3").unwrap();
        assert_eq!((g.provider.as_str(), g.model.as_str()), ("glm", "GLM-5.3"));
        assert_eq!(mk("Düz", None, "sonnet").unwrap().provider, "anthropic");
        // Görev #120: sürümlü kimlikler geçerli; eski takma adlar (yukarıda) çalışmaya devam eder.
        assert_eq!(mk("Sürüm", None, "claude-fable-5-1").unwrap().model, "claude-fable-5-1");
        assert!(mk("Uydurma", None, "claude-opus-9-9").is_err());
        assert_eq!(mk("Alias", Some("glm"), "haiku").unwrap().model, "haiku");
        let m = mk("Museci", Some("muse"), "muse-spark-1.3-contributor").unwrap();
        assert!(mk("H", Some("bogus"), "sonnet").is_err());
        assert!(mk("I", Some("muse"), "opus").is_err(), "model muse kataloğunda değil");

        let p: AgentPatch = serde_json::from_str(r#"{"provider":"muse","model":"muse-spark-1.2"}"#).unwrap();
        assert_eq!(update_agent(&c, &m.id, p).unwrap().model, "muse-spark-1.2");
        let p: AgentPatch = serde_json::from_str(r#"{"provider":"anthropic","model":"opus"}"#).unwrap();
        let back = update_agent(&c, &g.id, p).unwrap();
        assert_eq!((back.provider.as_str(), back.model.as_str()), ("anthropic", "opus"));
        // Yarım patch kötü durum bırakmaz: anthropic ajanına (opus) sadece glm taşınamaz.
        assert!(update_agent(&c, &g.id, serde_json::from_str(r#"{"provider":"glm"}"#).unwrap()).is_err());
        assert!(update_agent(&c, &g.id, serde_json::from_str(r#"{"model":"GLM-5.3"}"#).unwrap()).is_err());
    }

    /// DEX-26: engine defaults to claude; #1710: the removed Codex engine is refused.
    #[test]
    fn engine_is_claude_only() {
        let c = mem();
        assert!(list_agents(&c).unwrap().iter().all(|a| a.engine == "claude"), "existing rows stay claude");
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let mk = |engine: Option<&str>, model: &str| {
            create_agent(&c, Path::new("/data"), NewAgent {
                team_id: s.id.clone(), name: "Cx".into(), kind: "worker".into(), title: String::new(),
                model: model.into(), provider: None, role_slug: None, engine: engine.map(String::from),
            })
        };
        assert!(mk(Some("codex"), "gpt-5.5").is_err(), "Codex motoru kaldırıldı");
        assert!(mk(Some("gemini"), "gpt-5.5").is_err());
        assert!(mk(None, "gpt-5.5").is_err(), "claude motoru katalog dışı modeli almaz");
        let x = mk(Some("claude"), "sonnet").unwrap();
        assert_eq!((x.engine.as_str(), x.model.as_str()), ("claude", "sonnet"));
        assert!(update_agent(&c, &x.id, serde_json::from_str(r#"{"engine":"codex","model":"gpt-5.6-sol"}"#).unwrap()).is_err());
        assert_eq!(get_agent(&c, &x.id).unwrap().engine, "claude");
    }

    fn target(engine: &str, provider: Option<&str>, model: &str) -> PendingModel {
        PendingModel { engine: engine.into(), provider: provider.map(String::from), model: model.into() }
    }

    fn crew_of(c: &Connection, team: &str) -> Vec<Agent> {
        list_agents(c).unwrap().into_iter().filter(|a| a.team_id == team).collect()
    }

    /// DEX-30: bulk change hits every worker at once and never the leader.
    #[test]
    fn workers_bulk_change_spares_leader() {
        let c = mem();
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let other = crew_of(&c, "default");
        let out = set_workers_model(&c, &s.id, &target("claude", None, "haiku"), |_| false).unwrap();
        let crew = crew_of(&c, &s.id);
        assert_eq!(out.applied.len(), 4);
        assert!(out.pending.is_empty());
        for a in &crew {
            let want = if a.kind == "leader" { ("claude", "claude-opus-5-5") } else { ("claude", "haiku") };
            assert_eq!((a.engine.as_str(), a.model.as_str()), want, "{}", a.name);
            assert_eq!(a.provider, "anthropic");
        }
        // Other teams are untouched.
        assert_eq!(crew_of(&c, "default").iter().map(|a| &a.model).collect::<Vec<_>>(), other.iter().map(|a| &a.model).collect::<Vec<_>>());

        // To GLM: provider travels with the change.
        set_workers_model(&c, &s.id, &target("claude", Some("glm"), "GLM-5.3"), |_| false).unwrap();
        let w: Vec<_> = crew_of(&c, &s.id).into_iter().filter(|a| a.kind == "worker").collect();
        assert!(w.iter().all(|a| (a.engine.as_str(), a.provider.as_str(), a.model.as_str()) == ("claude", "glm", "GLM-5.3")));
    }

    /// The personal assistant's model changes only from its own member edit, never the bulk one.
    #[test]
    fn workers_bulk_change_spares_assistant() {
        let c = mem();
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let a: NewAgent = serde_json::from_value(serde_json::json!({"teamId": s.id, "name": "", "kind": "worker", "roleSlug": "assistant"})).unwrap();
        let echo = create_agent(&c, Path::new("/data"), a).unwrap();
        let out = set_workers_model(&c, &s.id, &target("claude", Some("glm"), "GLM-5.3"), |_| false).unwrap();
        assert_eq!(out.applied.len(), 4);
        assert!(!out.applied.contains(&echo.id));
        let after = crew_of(&c, &s.id).into_iter().find(|a| a.id == echo.id).unwrap();
        assert_eq!((after.engine.as_str(), after.model.as_str()), ("claude", crate::providers::CLAUDE_MID));
    }

    /// DEX-30: invalid combos fail whole — nothing half-applied.
    #[test]
    fn workers_bulk_change_rejects_invalid_combos() {
        let c = mem();
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let before: Vec<_> = crew_of(&c, &s.id).into_iter().map(|a| (a.engine, a.provider, a.model)).collect();
        assert!(set_workers_model(&c, &s.id, &target("codex", None, "gpt-5.5"), |_| false).is_err(), "Codex motoru kaldırıldı");
        assert!(set_workers_model(&c, &s.id, &target("claude", Some("glm"), "gpt-5.5"), |_| false).is_err());
        assert!(set_workers_model(&c, &s.id, &target("claude", Some("muse"), "opus"), |_| false).is_err());
        assert!(set_workers_model(&c, &s.id, &target("gemini", None, "x"), |_| false).is_err());
        assert!(set_workers_model(&c, "yok", &target("claude", None, "haiku"), |_| false).is_err());
        let after: Vec<_> = crew_of(&c, &s.id).into_iter().map(|a| (a.engine, a.provider, a.model)).collect();
        assert_eq!(before, after);
    }

    /// DEX-30: a running worker keeps its CLI; the change waits and lands when the session ends.
    #[test]
    fn running_workers_get_pending_change() {
        let c = mem();
        let s = create_team(&c, Path::new("/data"), NewTeam { profile: None, name: "S".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let crew = crew_of(&c, &s.id);
        let (lead, busy, idle) = (&crew[0], &crew[1], &crew[2]);
        let live = |id: &str| id == busy.id || id == lead.id;
        let out = set_workers_model(&c, &s.id, &target("claude", None, "haiku"), live).unwrap();
        assert_eq!(out.pending, [busy.id.clone()]);
        assert!(out.applied.contains(&idle.id) && !out.applied.contains(&lead.id));

        let b = get_agent(&c, &busy.id).unwrap();
        assert_eq!((b.engine.as_str(), b.model.as_str()), ("claude", "claude-sonnet-5-5"), "running CLI unchanged");
        assert_eq!(b.pending, Some(target("claude", Some("anthropic"), "haiku")));
        assert!(get_agent(&c, &lead.id).unwrap().pending.is_none(), "leader never gets pending");

        // Session ends → it lands and clears; a second call is a no-op.
        assert!(apply_pending(&c, &busy.id).unwrap());
        let b = get_agent(&c, &busy.id).unwrap();
        assert_eq!((b.engine.as_str(), b.model.as_str(), b.pending.is_none()), ("claude", "haiku", true));
        assert!(!apply_pending(&c, &busy.id).unwrap());

        // A newer bulk change replaces the pending one; asking for what already runs voids it.
        set_workers_model(&c, &s.id, &target("claude", None, "opus"), live).unwrap();
        set_workers_model(&c, &s.id, &target("claude", Some("glm"), "GLM-5.3"), live).unwrap();
        assert_eq!(get_agent(&c, &busy.id).unwrap().pending, Some(target("claude", Some("glm"), "GLM-5.3")));
        set_workers_model(&c, &s.id, &target("claude", None, "haiku"), live).unwrap();
        assert_eq!(get_agent(&c, &busy.id).unwrap().pending, None);

        // A hand edit overrides a waiting bulk change.
        set_workers_model(&c, &s.id, &target("claude", None, "opus"), live).unwrap();
        update_agent(&c, &busy.id, serde_json::from_str(r#"{"engine":"claude","model":"sonnet"}"#).unwrap()).unwrap();
        assert_eq!(get_agent(&c, &busy.id).unwrap().pending, None);

        // Startup: nothing survives, everything pending lands.
        set_workers_model(&c, &s.id, &target("claude", None, "opus"), live).unwrap();
        assert_eq!(apply_all_pending(&c).unwrap(), 1);
        assert_eq!(get_agent(&c, &busy.id).unwrap().model, "opus");
    }
}
