//! Görev #144 (Kernel-4): a worker's job in its own git worktree. Parallel workers no longer
//! share one checkout (half-saved files breaking each other's builds, "commit only your hunks").
//!
//! - Job start (fresh session, main team, team switch on, team folder = a git repository's top):
//!   `git worktree add <root>/.dex/wt/<agent>-<id> -b dex/<agent>/<id> <root HEAD>`; a plan step
//!   also merges its prerequisites' branches into that new branch (never into the team branch).
//!   A conflict there does not start the job — the leader hears it as a failed start.
//! - #888: a job bound to a project runs on its own snapshot: `<root>` is the job's project root
//!   and the branch starts on its pinned target branch (`delegations.merge_branch`), which is also
//!   the only branch it lands in. Prerequisite/fix branches come only from the same repository.
//!   A job from before projects (no target) keeps the team folder's HEAD.
//! - node_modules: `pnpm install --frozen-lockfile --offline` (store hardlinks, seconds); only if
//!   that fails a junction to the root's — removed as a link before the worktree goes
//!   (`git worktree remove` would follow it into the root's node_modules).
//! - Rust: one `CARGO_TARGET_DIR` per agent (`<root>/.dex/target/<agent>`): warm across that
//!   agent's jobs, no lock or locked-exe clash with the others.
//! - Delivery: the worker commits on its branch; the leader merges with `delegation_merge`
//!   (policy files gate, conflicts aborted and listed). After a merge the worktree and branch go;
//!   a failed or unreported job keeps both.
//!
//! Git runs where the core runs, so a remote dexd does the same on its host.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db::{with, Db};
use crate::delegate::{self, Caller, Delegation};
use crate::projects::ProjectError;

#[derive(Debug, Clone, PartialEq)]
pub struct Wt {
    pub delegation_id: i64,
    pub agent_id: String,
    /// The job's project root, the main checkout (before projects: the team folder).
    pub root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// The commit the branch started from (its target's tip).
    pub base: String,
    /// node_modules is a junction to the root's (pnpm install failed).
    pub junction: bool,
    pub merged_at: Option<i64>,
    pub removed_at: Option<i64>,
    /// #888: the job's pinned target branch (`delegations.merge_branch`). `None`: a job from
    /// before projects — the main checkout's current branch, as before.
    pub merge_branch: Option<String>,
}

impl Wt {
    /// What the worker's session gets on top of its usual environment.
    pub fn env(&self) -> Vec<(String, String)> {
        if !self.path.join("Cargo.toml").is_file() {
            return Vec::new();
        }
        // #415: the branch's own name segment (`dex/<slug>/<job>`); an old branch keeps its id's dir.
        let name = self.branch.strip_prefix("dex/").and_then(|b| b.split('/').next()).unwrap_or(&self.agent_id);
        vec![("CARGO_TARGET_DIR".into(), target_dir(&self.root, name).to_string_lossy().into_owned())]
    }
}

/// Starting the job must not go on: the leader gets this as the failed start's summary.
#[derive(Debug, PartialEq)]
pub enum Error {
    /// A prerequisite branch does not merge into the new branch — or (#888) the job's project
    /// binding is unusable (`project_*` code): no fallback to the team folder.
    Conflict(String),
    /// The worktree could not be made; the job may still run in the team folder.
    Other(String),
}

/// #888: a bound job that cannot start where it was pinned.
fn refuse(code: ProjectError, why: &str) -> Error {
    Error::Conflict(format!("{code}: {why}"))
}

const COLS: &str = "w.delegation_id, w.agent_id, w.root, w.path, w.branch, w.base, w.junction, w.merged_at, w.removed_at, d.merge_branch";
const FROM: &str = "delegation_worktrees w LEFT JOIN delegations d ON d.id = w.delegation_id";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Wt> {
    Ok(Wt {
        delegation_id: r.get(0)?,
        agent_id: r.get(1)?,
        root: PathBuf::from(r.get::<_, String>(2)?),
        path: PathBuf::from(r.get::<_, String>(3)?),
        branch: r.get(4)?,
        base: r.get(5)?,
        junction: r.get(6)?,
        merged_at: r.get(7)?,
        removed_at: r.get(8)?,
        merge_branch: r.get(9)?,
    })
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn get(conn: &Connection, delegation_id: i64) -> Result<Option<Wt>, String> {
    conn.query_row(&format!("SELECT {COLS} FROM {FROM} WHERE w.delegation_id = ?1"), [delegation_id], row)
        .optional()
        .map_err(e)
}

/// The worktree the agent's session works in: that of its latest started job, while it exists.
pub fn active(conn: &Connection, agent: &str) -> Result<Option<Wt>, String> {
    let latest: Option<i64> = conn
        .query_row(
            "SELECT id FROM delegations WHERE to_agent = ?1 AND status <> 'queued' ORDER BY id DESC LIMIT 1",
            [agent],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?;
    Ok(match latest {
        Some(id) => get(conn, id)?.filter(|w| w.removed_at.is_none()),
        None => None,
    })
}

pub fn target_dir(root: &Path, agent: &str) -> PathBuf {
    root.join(".dex").join("target").join(safe(agent))
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = hidden(Command::new("git").env("GIT_NO_REPLACE_OBJECTS", "1").arg("-C").arg(dir).args(args))
        .output()
        .map_err(|err| format!("git çalıştırılamadı: {err}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        Err(format!("git {}: {msg}", args.first().copied().unwrap_or("")))
    }
}

pub(crate) fn hidden(c: &mut Command) -> &mut Command {
    c.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What starting `d` in a worktree needs, read under the db lock (git runs without it).
struct Plan {
    root: PathBuf,
    path: PathBuf,
    branch: String,
    /// #888: the job is bound to a project — its pinned root/target are checked, never replaced.
    bound: bool,
    /// The pinned target branch the tree starts on; `None`: the root's HEAD, as before projects.
    target: Option<String>,
    /// The project's repository identity the root must still have.
    repo_key: Option<String>,
    /// Branches of the plan step's prerequisites that have one.
    deps: Vec<String>,
}

fn plan(conn: &Connection, d: &Delegation) -> Result<Option<Plan>, Error> {
    // Görev #286: a review only reads, from the team folder.
    if d.service_team_id.is_some() || crate::review::is_review(conn, d.id).map_err(Error::Other)? {
        return Ok(None);
    }
    let team: Option<(Option<String>, bool, String)> = conn
        .query_row(
            "SELECT t.cwd, t.worktrees, t.kind FROM agents a JOIN teams t ON t.id = a.team_id WHERE a.id = ?1 AND a.kind = 'worker'",
            [&d.to_agent],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|err| Error::Other(e(err)))?;
    let Some((cwd, worktrees, kind)) = team else { return Ok(None) };
    if kind != "main" {
        return Ok(None);
    }
    // #888: a bound job's own snapshot; the team folder/default project is never read for it.
    let (root, worktrees, target, repo_key) = match &d.project_id {
        Some(id) => {
            let p = crate::projects::get(conn, &d.team_id, id).map_err(|err| refuse(err, &format!("#{} işinin projesi yok", d.id)))?;
            if p.archived_at.is_some() {
                return Err(refuse(ProjectError::Unavailable, &format!("#{} işinin projesi arşivde", d.id)));
            }
            match &d.project_root {
                Some(root) => (root.clone(), p.worktrees, d.merge_branch.clone(), p.repo_key),
                // A folderless legacy project: no repository, as before.
                None if p.root_path.is_none() => return Ok(None),
                None => return Err(refuse(ProjectError::Unavailable, &format!("#{} işinin proje kökü kayıtlı değil — yeniden devret", d.id))),
            }
        }
        None => match cwd {
            Some(cwd) => (cwd, worktrees, None, None),
            None => return Ok(None),
        },
    };
    if !worktrees {
        return Ok(None);
    }
    let root = PathBuf::from(root);
    // #415: named after the agent's visible name; recorded paths/branches stay as they were.
    let agent = crate::slug::agent(conn, &d.to_agent).map_err(Error::Other)?;
    Ok(Some(Plan {
        path: root.join(".dex").join("wt").join(format!("{agent}-{}", d.id)),
        branch: format!("dex/{agent}/{}", d.id),
        bound: d.project_id.is_some(),
        target,
        repo_key,
        deps: dep_branches(conn, d)?,
        root,
    }))
}

/// The branches of the prerequisites of the plan step `d` runs (plan_items.depends_on); for a fix
/// job (#286 `fixOf`), the branch of the job it fixes. #888: only of the same repository — another
/// project's prerequisite is ordering and its report only (`plan::dependency_access`).
fn dep_branches(conn: &Connection, d: &Delegation) -> Result<Vec<String>, Error> {
    if let Some(fix) = crate::review::fix_of(conn, d.id).map_err(Error::Other)? {
        // `fixOf` inherits the fixed job's binding; another project's branch is never a base.
        if delegate::get(conn, fix).map_err(Error::Other)?.project_id != d.project_id {
            return Err(refuse(ProjectError::Mismatch, &format!("fixOf #{fix} başka projenin işi")));
        }
        return Ok(get(conn, fix).map_err(Error::Other)?.map(|w| w.branch).into_iter().collect());
    }
    if d.project_id.is_some() {
        let deps = crate::plan::dependency_access(conn, d.id).map_err(Error::Other)?;
        return Ok(deps.into_iter().filter(|a| a.scope == crate::plan::DependencyScope::SameRepository).filter_map(|a| a.git_branch).collect());
    }
    let item: Option<(i64, String)> = conn
        .query_row("SELECT plan_id, depends_on FROM plan_items WHERE delegation_id = ?1", [d.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|err| Error::Other(e(err)))?;
    let Some((plan, deps)) = item else { return Ok(Vec::new()) };
    let keys: Vec<String> = serde_json::from_str(&deps).unwrap_or_default();
    let mut out = Vec::new();
    for key in keys {
        let branch: Option<String> = conn
            .query_row(
                "SELECT w.branch FROM plan_items i JOIN delegation_worktrees w ON w.delegation_id = i.delegation_id
                 WHERE i.plan_id = ?1 AND i.key = ?2",
                params![plan, key.trim()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|err| Error::Other(e(err)))?;
        out.extend(branch);
    }
    Ok(out)
}

/// One preparation at a time: `git worktree add` and pnpm do not like company, and two dispatches
/// of one job must not both make it.
static PREPARE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The job's worktree, made now (or the one it already has). `Ok(None)`: no worktree for this job
/// (service job, switch off, no git repository) — it runs in the team folder as before.
/// #888: a bound job whose root/repository/target is gone is `Conflict` (a failed start).
pub fn prepare(db: &Db, d: &Delegation) -> Result<Option<Wt>, Error> {
    let _disk = crate::maintenance::DISK_GATE.read().map_err(|_| Error::Other("maintenance gate poisoned".into()))?;
    let _one = PREPARE.lock().unwrap_or_else(|p| p.into_inner());
    let (existing, plan) = with(db, |c| Ok((get(c, d.id)?, plan(c, d)))).map_err(Error::Other)?;
    if let Some(w) = existing.filter(|w| w.removed_at.is_none() && w.path.is_dir()) {
        return Ok(Some(w));
    }
    let Some(p) = plan? else { return Ok(None) };
    // #888: the pinned root, repository and target branch must still be there; never another root.
    if p.bound && !p.root.is_dir() {
        return Err(refuse(ProjectError::Unavailable, &format!("#{} işinin proje kökü yok: {}", d.id, p.root.display())));
    }
    if let Some(target) = &p.target {
        let ready = crate::projects::prepare_root(&p.root.to_string_lossy(), Some(target))
            .map_err(|err| refuse(err, &format!("#{} işinin hedef dalı {target} ({}) doğrulanamadı", d.id, p.root.display())))?;
        if p.repo_key.is_some() && ready.repo_key() != p.repo_key.as_deref() {
            return Err(refuse(ProjectError::RootConflict, &format!("{} artık projenin deposu değil", p.root.display())));
        }
    }
    // Only the repository's top folder: a team folder inside a bigger repository is not its own.
    match git(&p.root, &["rev-parse", "--show-toplevel"]) {
        Ok(top) if same_dir(Path::new(&top), &p.root) => {}
        _ => return Ok(None),
    }
    let base = match &p.target {
        Some(target) => git(&p.root, &["rev-parse", "--verify", &format!("refs/heads/{target}")]),
        None => git(&p.root, &["rev-parse", "HEAD"]),
    }
    .map_err(Error::Other)?;
    if let Some(parent) = p.path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| Error::Other(format!("{}: {err}", parent.display())))?;
    }
    let path = p.path.to_string_lossy().into_owned();
    git(&p.root, &["worktree", "add", &path, "-b", &p.branch, &base]).map_err(Error::Other)?;
    for dep in &p.deps {
        // A prerequisite merged and cleaned up already is in the root's HEAD.
        if git(&p.root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{dep}")]).is_err() {
            continue;
        }
        if let Err(err) = git(&p.path, &["merge", "--no-edit", dep]) {
            let files = git(&p.path, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(&p.path, &["merge", "--abort"]);
            discard(&p.root, &path, &p.branch);
            let files = files.lines().collect::<Vec<_>>().join(", ");
            return Err(Error::Conflict(if files.is_empty() {
                format!("önkoşul dalı {dep} {} dalına birleşmedi: {err}", p.branch)
            } else {
                format!("önkoşul dalları çakıştı ({dep} birleşirken): {files} — önkoşul dallarını birleştirip adımı plan_retry ile yeniden başlat")
            }));
        }
    }
    let junction = node_modules(&p.root, &p.path);
    let wt = Wt {
        delegation_id: d.id,
        agent_id: d.to_agent.clone(),
        root: p.root,
        path: p.path,
        branch: p.branch,
        base,
        junction,
        merged_at: None,
        removed_at: None,
        merge_branch: p.target,
    };
    with(db, |c| {
        c.execute(
            "INSERT OR REPLACE INTO delegation_worktrees (delegation_id, agent_id, root, path, branch, base, junction, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![wt.delegation_id, wt.agent_id, wt.root.to_string_lossy(), path, wt.branch, wt.base, wt.junction, now()],
        )
        .map_err(e)
    })
    .map_err(Error::Other)?;
    Ok(Some(wt))
}

/// A worktree that never held work: gone with its branch.
fn discard(root: &Path, path: &str, branch: &str) {
    let _ = git(root, &["worktree", "remove", "--force", path]);
    let _ = git(root, &["branch", "-D", branch]);
}

/// `true`: a junction to the root's node_modules (the fallback).
fn node_modules(root: &Path, wt: &Path) -> bool {
    if wt.join("pnpm-lock.yaml").is_file() && pnpm_install(wt) {
        return false;
    }
    let shared = root.join("node_modules");
    shared.is_dir() && link_dir(&shared, &wt.join("node_modules"))
}

fn pnpm_install(dir: &Path) -> bool {
    let args = ["install", "--frozen-lockfile", "--offline"];
    // pnpm is a .cmd shim on Windows: CreateProcess does not run it by name.
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg("pnpm").args(args);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = Command::new("pnpm");
        c.args(args);
        c
    };
    c.current_dir(dir).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    hidden(&mut c).status().is_ok_and(|s| s.success())
}

/// A directory link that needs no privilege (junction on Windows).
pub(crate) fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        // mklink takes no "/" separators.
        let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
        hidden(Command::new("cmd").args(["/C", "mklink", "/J"]).arg(win(link)).arg(win(target)))
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// Worktree and branch go, junction first and as a link only. A worktree with uncommitted work
/// stays (no `--force`); the error says why.
///
/// #195: on Windows a folder that is a live process's current directory cannot go (the worker's
/// session started there). `git worktree remove` then empties it and drops its registration but
/// fails; the empty folder is left for a later try ([`clean`]) and the branch still goes.
pub fn remove(w: &Wt) -> Result<(), String> {
    let nm = w.path.join("node_modules");
    if w.junction || is_link(&nm) {
        // The link itself: removing "through" it would empty the root's node_modules.
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&nm);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&nm);
    } else if nm.is_dir() {
        // std does not follow pnpm's inner junctions/symlinks; git's delete gives up on them.
        std::fs::remove_dir_all(&nm).map_err(|err| format!("{}: {err}", nm.display()))?;
    }
    let dir = if !w.path.is_dir() {
        let _ = git(&w.root, &["worktree", "prune"]);
        Ok(())
    } else if registered(&w.root, &w.path) {
        git(&w.root, &["worktree", "remove", &w.path.to_string_lossy()]).map(|_| ())
    } else {
        // Left by an earlier try: no longer a worktree, only an empty folder goes (never its content).
        let _ = git(&w.root, &["worktree", "prune"]);
        std::fs::remove_dir(&w.path).map_err(|err| format!("{}: {err}", w.path.display()))
    };
    // A branch still checked out in a registered tree (uncommitted work) refuses; that is right.
    let exists = git(&w.root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", w.branch)]).is_ok();
    // #901: `-d` asks HEAD; a pinned job's branch is checked against its target instead.
    let landed = w.merge_branch.as_deref().is_some_and(|t| git(&w.root, &["merge-base", "--is-ancestor", &w.branch, &format!("refs/heads/{t}")]).is_ok());
    let branch = if exists { git(&w.root, &["branch", if landed { "-D" } else { "-d" }, &w.branch]).map(|_| ()) } else { Ok(()) };
    match (dir, branch) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(a), Ok(())) => Err(format!("{a} (dal silindi)")),
        (Ok(()), Err(b)) => Err(b),
        (Err(a), Err(b)) => Err(format!("{a}; {b}")),
    }
}

/// A symlink or junction (std counts both).
pub(crate) fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| {
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            m.file_attributes() & 0x400 != 0 // All reparse points, including junctions.
        }
        #[cfg(not(windows))] { m.file_type().is_symlink() }
    })
}

/// `path` is one of the root's registered worktrees.
fn registered(root: &Path, path: &Path) -> bool {
    registered_result(root, path).unwrap_or(false)
}

fn registered_result(root: &Path, path: &Path) -> Result<bool, String> {
    let list = git(root, &["worktree", "list", "--porcelain"])?;
    Ok(list.lines().filter_map(|l| l.strip_prefix("worktree ")).any(|p| same_dir(Path::new(p), path)))
}

/// Nightly cleanup retains branches: tracked files can be restored with `git worktree add`.
/// Targets can be rebuilt; untracked/ignored content has no automatic rollback.
pub(crate) fn remove_leftover(root: &Path, path: &Path) -> Result<(), String> {
    let _one = CLEAN.lock().map_err(|_| "worktree clean lock poisoned")?;
    if !crate::maintenance::safe_candidate(root, path) { return Err("unsafe_path".into()); }
    let registered = leftover_registered(root, path)?;
    detach_links(path).map_err(|e| e.to_string())?;
    if registered { git(root, &["worktree", "remove", "--force", &path.to_string_lossy()])?; }
    else { std::fs::remove_dir_all(path).map_err(|e| e.to_string())?; }
    git(root, &["worktree", "prune"])?;
    Ok(())
}

/// Read-only feasibility check: a failed registry must never classify a tree as orphan.
pub(crate) fn leftover_registered(root: &Path, path: &Path) -> Result<bool, String> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    if !same_dir(Path::new(&top), root) { return Err("repository root mismatch".into()); }
    registered_result(root, path)
}

// Git's recursive delete can follow junctions; detach every link, including nested ones.
fn detach_links(root: &Path) -> std::io::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let m = std::fs::symlink_metadata(&path)?;
            if is_link(&path) {
                #[cfg(windows)] {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x10 != 0 { std::fs::remove_dir(&path)?; }
                    else { std::fs::remove_file(&path)?; }
                }
                #[cfg(not(windows))] { std::fs::remove_file(&path)?; }
            } else if m.is_dir() { stack.push(path); }
        }
    }
    Ok(())
}

/// One cleanup at a time: a session exit's retry and the merge's own must not race git.
static CLEAN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// #195: removes the job's worktree and branch unless gone already, and marks it removed. `tries`:
/// a session just closed releases its folder a moment later.
pub fn clean(db: &Db, delegation_id: i64, tries: u32) -> Result<(), String> {
    let _one = CLEAN.lock().unwrap_or_else(|p| p.into_inner());
    let Some(w) = with(db, |c| get(c, delegation_id))? else { return Ok(()) };
    if w.removed_at.is_some() {
        return Ok(());
    }
    let mut result = remove(&w);
    for _ in 1..tries.max(1) {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
        result = remove(&w);
    }
    if let Err(err) = result {
        eprintln!("dex: #{delegation_id} worktree cleanup: {err}");
        return Err(err);
    }
    with(db, |c| c.execute("UPDATE delegation_worktrees SET removed_at = ?2 WHERE delegation_id = ?1", params![delegation_id, now()]).map_err(e))?;
    Ok(())
}

/// #195: merged jobs whose worktree could not go yet (of `agent`, or all).
pub fn pending(conn: &Connection, agent: Option<&str>) -> Result<Vec<Wt>, String> {
    let mut st = conn
        .prepare(&format!(
            "SELECT {COLS} FROM {FROM} WHERE w.merged_at IS NOT NULL AND w.removed_at IS NULL
               AND (?1 IS NULL OR w.agent_id = ?1) ORDER BY w.delegation_id"
        ))
        .map_err(e)?;
    let rows = st.query_map([agent], row).map_err(e)?.collect::<Result<Vec<_>, _>>().map_err(e)?;
    Ok(rows)
}

/// What [`sweep`] could not clean, for logging only; never a blocking leader notification.
#[derive(Debug)]
pub struct Leftover {
    pub delegation: Option<Delegation>,
    pub root: PathBuf,
    pub text: String,
}

/// #195, on Dex start (no session holds a folder then): merged jobs' pending cleanups again, then
/// folders under `<team folder>/.dex/wt/` that are no worktree any more — git's registration
/// pruned, a node_modules link dropped as a link, the folder removed only when empty. A folder
/// still registered stays (a job's tree, or someone's own); nothing is forced or deleted recursively.
pub fn sweep(db: &Db) -> Result<Vec<Leftover>, String> {
    let mut out = Vec::new();
    let mut told = Vec::new();
    for w in with(db, |c| pending(c, None))? {
        if let Err(err) = clean(db, w.delegation_id, 1) {
            told.push(w.path.clone());
            let d = with(db, |c| delegate::get(c, w.delegation_id)).ok();
            out.push(Leftover { delegation: d, root: w.root.clone(), text: format!("#{} worktree temizlenemedi ({}): {err}", w.delegation_id, w.path.display()) });
        }
    }
    // #888: every project's root and the roots of live trees too, team folders first (their
    // spelling keys the team's warning); one scan per root.
    let roots: Vec<String> = with(db, |c| {
        let mut st = c
            .prepare(
                "SELECT cwd FROM teams WHERE cwd IS NOT NULL AND worktrees = 1
                 UNION ALL SELECT root_path FROM projects WHERE root_path IS NOT NULL AND worktrees = 1
                 UNION ALL SELECT root FROM delegation_worktrees WHERE removed_at IS NULL",
            )
            .map_err(e)?;
        let rows: Vec<String> = st.query_map([], |r| r.get(0)).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
        let mut seen = std::collections::HashSet::new();
        Ok(rows.into_iter().filter(|r| seen.insert(crate::projects::path_key(r))).collect())
    })?;
    for root in roots.iter().map(PathBuf::from) {
        let dirs = match std::fs::read_dir(root.join(".dex").join("wt")) {
            Ok(dirs) => Some(dirs),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                eprintln!("dex: worktree leftovers scan: {err}");
                continue; // Keep the last warning when the folder cannot be inspected.
            }
        };
        let mut orphans = Vec::new();
        let _ = git(&root, &["worktree", "prune"]);
        for dir in dirs.into_iter().flatten().flatten().map(|d| d.path()).filter(|p| p.is_dir() && !is_link(p)) {
            if registered(&root, &dir) || told.iter().any(|t| same_dir(t, &dir)) {
                continue;
            }
            let nm = dir.join("node_modules");
            if is_link(&nm) {
                #[cfg(windows)]
                let _ = std::fs::remove_dir(&nm);
                #[cfg(unix)]
                let _ = std::fs::remove_file(&nm);
            }
            if let Err(err) = std::fs::remove_dir(&dir) {
                orphans.push(dir.to_string_lossy().into_owned());
                out.push(Leftover { delegation: None, root: root.clone(), text: format!("worktree artığı kaldırılamadı ({}): {err} — içerik kaldıysa elle bak", dir.display()) });
            }
        }
        // Registered pending trees already live in delegation_worktrees. Only orphan paths
        // need a settings snapshot; replace it on every successful scan, never append notes.
        with(db, |c| c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("worktree_orphans:{}", root.display()), json!(orphans).to_string()],
        ).map_err(e))?;
    }
    Ok(out)
}

/// For the leader's result line: the branch and its commits since the base.
pub fn result_line(w: &Wt) -> String {
    let range = format!("{}..{}", w.base, w.branch);
    let log = git(&w.root, &["log", "--oneline", "--no-decorate", "-n", "5", &range]).unwrap_or_default();
    let count = git(&w.root, &["rev-list", "--count", &range]).unwrap_or_else(|_| "?".into());
    let mut s = format!("dal: {} ({count} commit) — birleştirmek için delegation_merge", w.branch);
    for l in log.lines() {
        s.push_str(&format!("\n    {l}"));
    }
    s
}

/// #286: the branch's tip and the team folder's branch, when the branch has commits of its own.
/// #888: against the job's own target branch; a job from before projects: the root's HEAD.
pub fn tip(w: &Wt) -> Option<(String, String)> {
    let target = w.merge_branch.as_deref().unwrap_or("HEAD");
    let ahead = git(&w.root, &["rev-list", "--count", &format!("{target}..{}", w.branch)]).ok()?;
    if ahead == "0" {
        return None;
    }
    let sha = git(&w.root, &["rev-parse", &w.branch]).ok()?;
    let team = match &w.merge_branch {
        Some(target) => target.clone(),
        None => git(&w.root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?,
    };
    Some((sha, team))
}

/// For the worker's prompt.
pub fn prompt_line(w: &Wt) -> String {
    let short: String = w.base.chars().take(8).collect();
    let mut s = format!(
        "Çalışma ağacın: {} (dal {}, taban {short}). Yalnız bu ağaçta çalış ve bu dala commit'le; ana çalışma ağacına ({}) yazma, \
         dal değiştirme; takım dalına/master'a birleştirme yapma — onu lider yapar. İş metni başka bir dalı istiyorsa onu \
         yalnız bu dala `git merge --no-ff` ile alabilirsin, sorma. Rapor ve hafıza ana ağacın .dex/ klasöründe kalır.",
        w.path.display(),
        w.branch,
        w.root.display()
    );
    if let Some((_, dir)) = w.env().into_iter().find(|(k, _)| k == "CARGO_TARGET_DIR") {
        s.push_str(&format!(" Rust çıktısı CARGO_TARGET_DIR={dir} (sana özel, işler arasında sıcak kalır)."));
    }
    s.push('\n');
    s
}

/// Merged into the root's branch only: policy files the patron approves before they land.
const POLICY_FILES: &[&str] = &[
    "crates/dex-core/src/permit.rs",
    "crates/dex-core/src/hooks.rs",
    "crates/dex-core/src/pty.rs",
    "crates/dex-core/src/merge_gate.rs",
    ".gitleaks.toml",
    "scripts/hooks/",
    "scripts/install-hooks.mjs",
    "scripts/gitleaks.mjs",
    "scripts/commit-policy.mjs",
    "scripts/publish-public.mjs",
    ".github/workflows/",
];

pub fn tools() -> Vec<Value> {
    vec![json!({ "name": "delegation_merge", "description": "YALNIZ LİDER. Bitmiş bir devretmenin worker dalını (kendi git worktree'sinde çalıştıysa) takım dalına `git merge --no-ff` ile alır; başarılıysa worktree ve dal silinir. Çakışmada birleştirme geri alınır ve çakışan dosyalar döner. Yalnız dal ucunun kod incelemesi `onay` ise birleştirir: inceleme sürüyorsa ya da kararı `düzelt` ise merged:false ve önemli bulgular döner; incelenmemiş uca Dex inceleme açar. Takımda code-review üyesi yoksa yalnız inceleme kapısı yoktur. Birleştirme öncesi tüm gelen commitlerin sızıntı taraması ve author/committer e-posta denetimi zorunludur; gitleaks yoksa ya da bulgu varsa reddeder. forcePolicy/forceReview bu denetimi atlamaz. Projeye bağlı iş yalnız kendi projesinin ana checkout'u işin hedef dalındayken birleşir; başka dal/detached HEAD → merged:false, code project_branch_mismatch (otomatik checkout yok). Dal politika dosyalarına (permit/hooks/pty/merge_gate/gitleaks/CI) dokunuyorsa birleştirmez, patron onayı ister; onaydan sonra forcePolicy: true ile tekrar çağır.",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["delegationId"], "properties": {
            "delegationId": { "type": "integer" },
            "forcePolicy": { "type": "boolean", "description": "Politika dosyası değişikliğini patron onayladıysa true" },
            "forceReview": { "type": "boolean", "description": "YALNIZ patron onayıyla: inceleme kapısını atla (patron incelemesiz birleştirmeyi onayladı)" } } } })]
}

/// `delegation_merge`. Errors are for the leader to read; a refused merge is `merged: false`.
/// `release` (#195): before cleanup, closes the worker's session if it still sits in the tree and
/// nothing needs it; `true` when it closed one (its folder is let go a moment later).
/// #286: only a tip reviewed `onay` lands, unless `force_review` (the patron's approval); a refusal
/// with `review.state` `yok` asks the caller to open the review. #287: a team with no code-review
/// member has no gate — the branch lands as before.
pub fn merge(db: &Db, c: &Caller, id: i64, force_policy: bool, force_review: bool, release: &dyn Fn(&Wt) -> bool) -> Result<Value, String> {
    if !c.is_leader() {
        return Err(format!("yetki yok: delegation_merge yalnız takım liderine açık ({} bir worker)", c.name));
    }
    let (d, w) = with(db, |conn| Ok((delegate::get(conn, id)?, get(conn, id)?)))?;
    if d.team_id != c.team_id || d.service_team_id.is_some() {
        return Err(format!("#{id} senin takımının işi değil"));
    }
    let w = w.ok_or_else(|| format!("#{id} kendi worktree'sinde çalışmadı; değişiklikleri zaten takım klasöründe"))?;
    if matches!(d.status.as_str(), "queued" | "running") {
        return Err(format!("#{id} henüz bitmedi ({}); report'tan sonra birleştir", d.status));
    }
    if w.merged_at.is_some() && w.removed_at.is_some() {
        return Ok(json!({ "merged": true, "branch": w.branch, "note": "zaten birleştirildi ve temizlendi" }));
    }
    if w.merged_at.is_some() && d.merge_branch.is_some() {
        // #901: landed in its target already, only the cleanup is left. Never merged again: the
        // main checkout may sit on another branch by now.
        let mut out = json!({ "merged": true, "branch": w.branch, "note": "zaten birleştirildi; bekleyen temizlik yeniden denendi" });
        if let Some(err) = tidy(db, id, &w, release) {
            out["cleanup"] = err.into();
        }
        return Ok(out);
    }
    if let Some(refusal) = landing(db, c, &d, &w)? {
        return Ok(refusal);
    }
    // #901: a pinned job is measured against its target, not whatever HEAD is.
    let into = d.merge_branch.as_deref().map_or_else(|| "HEAD".to_string(), |t| format!("refs/heads/{t}"));
    let root = &w.root;
    let exists = git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", w.branch)]).is_ok();
    if !exists {
        return Err(format!("dal {} yok (elle silinmiş olabilir)", w.branch));
    }
    let sha = git(root, &["rev-parse", &w.branch])?;
    let source_changed = || -> Result<Option<Value>, String> {
        if git(root, &["rev-parse", "--verify", &format!("refs/heads/{}", w.branch)])? != sha {
            return Ok(Some(json!({ "merged": false, "branch": w.branch, "code": "merge_source_changed", "note": "dal ilerledi; yeni dal ucunu yeniden incele/tara ve birleştirmeyi yeniden dene" })));
        }
        Ok(None)
    };
    let target_sha = git(root, &["rev-parse", &into])?;
    if let Some(mut refusal) = crate::merge_gate::check_repository(root, &target_sha)? {
        refusal["branch"] = w.branch.clone().into();
        return Ok(refusal);
    }
    let changed: Vec<String> = git(root, &["diff", "--name-only", &format!("{target_sha}...{sha}")])?
        .lines()
        .map(str::to_string)
        .collect();
    let notes = after_merge_notes(&changed);
    let already = git(root, &["merge-base", "--is-ancestor", &sha, &target_sha]).is_ok();
    if !already {
        if !force_review {
            let gate = with(db, |conn| {
                if !crate::review::gate_on(conn, &d.team_id)? {
                    return Ok(None);
                }
                Ok(Some((crate::review::gate(conn, id, &sha)?, crate::review::reviewer(conn, &d.team_id, &d.to_agent)?)))
            })?;
            if let Some(refusal) = gate.and_then(|(gate, reviewer)| crate::review::refusal(gate, reviewer.is_some(), &w.branch, &sha)) {
                return Ok(refusal);
            }
        }
        let policy: Vec<&String> = changed.iter().filter(|f| POLICY_FILES.iter().any(|p| f.as_str() == *p || (p.ends_with('/') && f.starts_with(p)))).collect();
        if !policy.is_empty() && !force_policy {
            return Ok(json!({
                "merged": false, "branch": w.branch, "policyFiles": policy,
                "note": "politika dosyası değişti — patron onayı gerekir. Diff'i patrona göster (git diff HEAD...<dal> -- <dosya>); onaydan sonra forcePolicy: true ile tekrar çağır.",
            }));
        }
        if let Some(mut refusal) = crate::merge_gate::check(root, &target_sha, &sha)? {
            refusal["branch"] = w.branch.clone().into();
            return Ok(refusal);
        }
        // #901: `git merge` lands in HEAD; the checkout may have left the target since `landing`.
        if let Some(target) = d.merge_branch.as_deref() {
            if git(root, &["symbolic-ref", "--quiet", "HEAD"]).ok().as_deref() != Some(into.as_str()) {
                return Ok(mismatch(&w, target));
            }
        }
        if git(root, &["rev-parse", "HEAD"])? != target_sha {
            return Ok(json!({ "merged": false, "branch": w.branch, "code": "merge_target_changed", "note": "hedef dal tarama sırasında değişti; birleştirmeyi yeniden dene" }));
        }
        if let Some(refusal) = source_changed()? {
            return Ok(refusal);
        }
        if let Err(err) = git(root, &["merge", "--no-ff", "--no-edit", &sha]) {
            let conflicts: Vec<String> = git(root, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            if !conflicts.is_empty() {
                let _ = git(root, &["merge", "--abort"]);
                return Ok(json!({
                    "merged": false, "branch": w.branch, "conflicts": conflicts,
                    "note": "çakışma: birleştirme geri alındı, takım dalı değişmedi. Elle çöz (git merge/cherry-pick) ya da worker'a dalı takım dalına göre rebase etmesini devret.",
                }));
            }
            return Err(format!("birleştirilemedi (takım dalı değişmedi): {err}"));
        }
    }
    // A late commit (including during git merge) must leave the job open for review/scan.
    if let Some(refusal) = source_changed()? {
        return Ok(refusal);
    }
    with(db, |conn| conn.execute("UPDATE delegation_worktrees SET merged_at = ?2 WHERE delegation_id = ?1", params![id, now()]).map_err(e))?;
    with(db, |conn| delegate::sync_task(conn, id))?;
    let mut out = json!({ "merged": true, "branch": w.branch, "files": changed.len(), "notes": notes });
    if let Some(err) = tidy(db, id, &w, release) {
        out["cleanup"] = err.into();
    }
    // #286: the jobs this one fixed are in the team branch now (its branch started on theirs).
    let mut prev = with(db, |conn| crate::review::fix_of(conn, id))?;
    let mut hops = 0;
    while let Some(p) = prev.filter(|_| hops < 20) {
        hops += 1;
        if let Some(pw) = with(db, |conn| get(conn, p))?.filter(|pw| pw.merged_at.is_none()) {
            let exists = git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", pw.branch)]).is_ok();
            if !exists || git(root, &["merge-base", "--is-ancestor", &pw.branch, &into]).is_ok() {
                with(db, |conn| conn.execute("UPDATE delegation_worktrees SET merged_at = ?2 WHERE delegation_id = ?1", params![p, now()]).map_err(e))?;
                with(db, |conn| delegate::sync_task(conn, p))?;
                let closed = release(&pw);
                if let Err(err) = clean(db, p, if closed { 8 } else { 1 }) {
                    out[format!("cleanup#{p}")] = format!("#{p} worktree kaldırılamadı ({}): {err} — bekleyen temizlik", pw.path.display()).into();
                }
            }
        }
        prev = with(db, |conn| crate::review::fix_of(conn, p))?;
    }
    Ok(out)
}

/// #888: a job with a pinned target lands only in that branch, checked out in its own project's
/// main checkout — never switched for it. `Some`: the `merged: false` answer. A job from before
/// projects (no target) lands in the checked-out branch, as before.
fn landing(db: &Db, c: &Caller, d: &Delegation, w: &Wt) -> Result<Option<Value>, String> {
    let Some(target) = d.merge_branch.as_deref() else { return Ok(None) };
    let ctx = with(db, |conn| delegate::resolve_context(conn, Path::new(""), c, None, Some(d.id)))?;
    match crate::project_context::validate_merge(&ctx) {
        Ok(_) => {}
        Err(ProjectError::BranchMismatch) => return Ok(Some(mismatch(w, target))),
        Err(err) => return Err(format!("{err}: #{} işinin projesi doğrulanamadı ({})", d.id, w.root.display())),
    }
    if !ctx.project_root.as_deref().is_some_and(|root| same_dir(root, &w.root)) {
        return Err(format!("{}: #{} worktree'si işin proje kökünde değil", ProjectError::Mismatch, d.id));
    }
    Ok(None)
}

/// The `merged: false` answer for a main checkout off the job's target (or detached).
fn mismatch(w: &Wt, target: &str) -> Value {
    let head = git(&w.root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let on = head.as_deref().map_or_else(|| "detached HEAD".to_string(), |h| format!("{h} dalında"));
    json!({
        "merged": false, "branch": w.branch, "code": ProjectError::BranchMismatch.code(), "mergeBranch": target, "head": head,
        "note": format!("ana checkout ({}) {on}; iş {target} dalına sabit. Otomatik checkout yok: ana checkout {target} dalına alınınca tekrar çağır.", w.root.display()),
    })
}

/// Lets the merged job's tree go (closing its idle session first); `Some`: why it stays pending.
fn tidy(db: &Db, id: i64, w: &Wt, release: &dyn Fn(&Wt) -> bool) -> Option<String> {
    let closed = release(w);
    clean(db, id, if closed { 8 } else { 1 }).err().map(|err| {
        format!("worktree kaldırılamadı ({}): {err} — bekleyen temizlik: worker oturumu kapanınca ve Dex açılışında yeniden denenir", w.path.display())
    })
}

/// What the leader should do after the merge lands.
fn after_merge_notes(changed: &[String]) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if changed.iter().any(|f| f == "package.json" || f == "pnpm-lock.yaml" || f.ends_with("/package.json")) {
        notes.push("package.json/pnpm-lock değişti: takım klasöründe pnpm install gerekebilir");
    }
    if changed.iter().any(|f| f.ends_with(".rs") || f.ends_with("Cargo.toml") || f == "Cargo.lock") {
        notes.push("Rust kodu değişti: dexd/uygulamanın yeni hali için yeniden derleyip yeniden başlat");
    }
    notes
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::merge_gate::{gitleaks_path, GITLEAKS_DIR, GITLEAKS_VERSION};
    use crate::db::Db;
    use crate::delegate::NewDelegation;

    /// A git repository with one commit, identity set (merges commit).
    pub(crate) fn repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dex-wt-{name}-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let root = PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));
        for args in [&["init", "-q", "-b", "team"][..], &["config", "user.email", "test@users.noreply.github.com"], &["config", "user.name", "t"], &["config", "core.autocrlf", "false"]] {
            git(&root, args).unwrap();
        }
        std::fs::write(root.join(".gitignore"), ".dex/\nnode_modules\n").unwrap();
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        std::fs::copy(project.join(".gitleaks.toml"), root.join(".gitleaks.toml")).unwrap();
        let tool = gitleaks_path(project, std::env::var_os("PATH").as_deref()).expect("gitleaks bulunamadı: npm run hooks:install");
        let local = root.join(GITLEAKS_DIR).join(GITLEAKS_VERSION).join(tool.file_name().unwrap());
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::copy(tool, local).unwrap();
        commit(&root, "init");
        root
    }

    pub(crate) fn commit(dir: &Path, msg: &str) {
        git(dir, &["add", "-A"]).unwrap();
        git(dir, &["commit", "-q", "-m", msg]).unwrap();
    }

    pub(crate) fn db_for(root: &Path) -> Db {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [root.to_string_lossy()]).unwrap();
        bind_legacy(&c, true);
        Db::new(c)
    }

    /// #888: the team's single legacy project follows the folder set above, as `update_team`
    /// keeps it; `verify`: as a leader's first validated `delegate` pins its repository/branch.
    fn bind_legacy(c: &Connection, verify: bool) {
        let cwd: Option<String> = c
            .query_row("SELECT cwd FROM teams WHERE id = 'default' AND EXISTS (SELECT 1 FROM projects WHERE id = 'legacy-default' AND root_path IS NULL)", [], |r| r.get(0))
            .optional()
            .unwrap()
            .flatten();
        let Some(cwd) = cwd else { return };
        c.execute("UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", params![cwd, crate::projects::path_key(&cwd)]).unwrap();
        if let Some(ready) = crate::projects::prepare_root(&cwd, None).ok().filter(|_| verify) {
            crate::projects::verify(c, "default", "legacy-default", &ready).unwrap();
        }
    }

    pub(crate) fn job(db: &Db, to: &str) -> Delegation {
        with(db, |c| {
            bind_legacy(c, true);
            let l = delegate::caller(c, "timur")?;
            let d = delegate::insert(c, NewDelegation { from: &l, to_agent: to.into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })?;
            delegate::set_status(c, d.id, "running")?;
            delegate::get(c, d.id)
        })
        .unwrap()
    }

    fn lead(db: &Db) -> Caller {
        with(db, |c| delegate::caller(c, "timur")).unwrap()
    }

    fn finish(db: &Db, id: i64) {
        with(db, |c| delegate::set_status(c, id, "done")).unwrap();
    }

    #[test]
    fn two_jobs_get_their_own_trees_and_branches_from_the_root_head() {
        let root = repo("two");
        let db = db_for(&root);
        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let (a, b) = (job(&db, "kaan"), job(&db, "ayla"));
        let wa = prepare(&db, &a).unwrap().unwrap();
        let wb = prepare(&db, &b).unwrap().unwrap();
        assert_ne!(wa.path, wb.path);
        assert_eq!(wa.path, root.join(".dex").join("wt").join(format!("kaan-{}", a.id)));
        assert_eq!(wb.branch, format!("dex/ayla/{}", b.id));
        assert_eq!(wa.base, head);
        assert!(wa.path.join("a.txt").is_file() && wb.path.join("a.txt").is_file());
        assert_eq!(git(&wa.path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), wa.branch);
        // Edits in one tree do not reach the other or the root.
        std::fs::write(wa.path.join("a.txt"), "kaan\n").unwrap();
        assert_eq!(std::fs::read_to_string(wb.path.join("a.txt")).unwrap(), "a\n");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
        // Asked again (a retried dispatch), the job keeps its tree.
        assert_eq!(prepare(&db, &a).unwrap().unwrap(), wa);
        assert_eq!(with(&db, |c| active(c, "kaan")).unwrap().unwrap().path, wa.path);
        assert!(with(&db, |c| active(c, "bora")).unwrap().is_none());
        assert!(wa.env().is_empty(), "no Cargo.toml: no target dir");
    }

    #[test]
    fn no_tree_outside_a_repository_with_the_switch_off_or_for_a_service_job() {
        let plain = std::env::temp_dir().join(format!("dex-wt-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        let plain = PathBuf::from(crate::teams::normalize_dir(&plain.to_string_lossy()).unwrap());
        let db = db_for(&plain);
        assert_eq!(prepare(&db, &job(&db, "kaan")).unwrap(), None);
        let root = repo("off");
        let db = db_for(&root);
        with(&db, |c| {
            let off = crate::teams::TeamPatch { worktrees: Some(false), ..Default::default() };
            crate::teams::update_team(c, Path::new(""), "default", off).map(|_| ())
        })
        .unwrap();
        assert_eq!(prepare(&db, &job(&db, "kaan")).unwrap(), None);
        // A team folder inside a bigger repository is not that repository.
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let db = db_for(&sub);
        assert_eq!(prepare(&db, &job(&db, "kaan")).unwrap(), None);
    }

    #[test]
    fn merge_lands_the_branch_then_removes_tree_and_branch() {
        let root = repo("merge");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        let line = result_line(&w);
        assert!(line.contains(&w.branch) && line.contains("1 commit") && line.contains("b ekle"), "{line}");
        let err = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap_err();
        assert!(err.contains("bitmedi"), "running jobs are not merged: {err}");
        let kaan = with(&db, |c| delegate::caller(c, "kaan")).unwrap();
        assert!(merge(&db, &kaan, d.id, false, false, &|_| false).unwrap_err().contains("yetki yok"));
        finish(&db, d.id);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert!(out.get("cleanup").is_none(), "{out}");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b\n");
        assert!(!w.path.exists());
        assert!(git(&root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", w.branch)]).is_err());
        let row = with(&db, |c| get(c, d.id)).unwrap().unwrap();
        assert!(row.merged_at.is_some() && row.removed_at.is_some());
        assert_eq!(merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()["merged"], true, "a second call is harmless");
    }

    #[test]
    fn merge_rejects_replace_refs_before_ancestry() {
        let root = repo("merge-replace-ref");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("a.txt"), "replacement fixture\n").unwrap();
        commit(&w.path, "replacement fixture");
        finish(&db, d.id);
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let tip = git(&root, &["rev-parse", &w.branch]).unwrap();
        let replacement = git(&root, &["commit-tree", &format!("{tip}^{{tree}}"), "-m", "replacement root fixture"]).unwrap();
        git(&root, &["replace", &target, &replacement]).unwrap();
        // Rewriting the target changes the diff before the scanner runs.
        git(&root, &["read-tree", &target]).unwrap();
        std::fs::write(root.join("a.txt"), "replacement fixture\n").unwrap();
        let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
        assert_eq!(out["code"], "merge_git_rewrite", "{out}");
        assert_eq!(git(&root, &["show", &format!("{target}:a.txt")]).unwrap(), "a");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), target);
        assert!(w.path.is_dir());
        assert!(with(&db, |c| get(c, d.id)).unwrap().unwrap().merged_at.is_none());
    }

    #[test]
    fn merge_rejects_grafts_before_ancestry() {
        let root = repo("merge-graft");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "clean fixture\n").unwrap();
        commit(&w.path, "graft fixture");
        finish(&db, d.id);
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let tip = git(&root, &["rev-parse", &w.branch]).unwrap();
        // Detach the worker's history; ancestry must not fail before the guard.
        std::fs::write(root.join(".git/info/grafts"), format!("{tip}\n")).unwrap();
        let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
        assert_eq!(out["code"], "merge_git_rewrite", "{out}");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), target);
        assert!(w.path.is_dir());
        assert!(with(&db, |c| get(c, d.id)).unwrap().unwrap().merged_at.is_none());
        let out = crate::merge_gate::check(&w.path, &target, &tip).unwrap().unwrap();
        assert_eq!(out["code"], "merge_git_rewrite", "linked worktree: {out}");
    }

    #[test]
    fn merge_refuses_a_branch_advanced_during_scan() {
        let root = repo("source-tip-race");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "clean\n").unwrap();
        commit(&w.path, "reviewed fixture");
        finish(&db, d.id);
        let before = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let scanned = git(&root, &["rev-parse", &w.branch]).unwrap();
        let worker = w.path.clone();
        crate::merge_gate::before_scan(move |_| {
            std::thread::spawn(move || {
                let secret = format!("{}.{}", "a0".repeat(16), "B1".repeat(8));
                std::fs::write(worker.join("b.txt"), secret).unwrap();
                commit(&worker, "advance during scan fixture");
            }).join().unwrap();
        });
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| panic!("unfinished branch must not be cleaned")).unwrap();
        assert_eq!(out["merged"], false, "{out}");
        assert_eq!(out["code"], "merge_source_changed", "{out}");
        assert!(out["note"].as_str().unwrap().contains("dal ilerledi"));
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
        assert_ne!(git(&root, &["rev-parse", &w.branch]).unwrap(), scanned);
        let row = with(&db, |c| get(c, d.id)).unwrap().unwrap();
        assert!(row.merged_at.is_none() && row.removed_at.is_none());
        assert!(w.path.is_dir());
        // A retry must rescan the new tip, rather than merely clean a "completed" job.
        let retry = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(retry["code"], "merge_leaks", "{retry}");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn merge_rejects_a_secret_deleted_from_the_branch_tip() {
        let root = repo("deleted-secret");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        let secret = format!("{}.{}", "a0".repeat(16), "B1".repeat(8));
        std::fs::write(w.path.join("b.txt"), &secret).unwrap();
        commit(&w.path, "synthetic fixture");
        std::fs::write(w.path.join("b.txt"), "clean\n").unwrap();
        commit(&w.path, "remove fixture");
        finish(&db, d.id);
        let before = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
        assert_eq!(out["merged"], false, "{out}");
        assert_eq!(out["code"], "merge_leaks", "{out}");
        assert!(!out["findings"].as_array().unwrap().is_empty());
        assert!(!out.to_string().contains(&secret));
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
        assert!(w.path.is_dir());
        assert!(with(&db, |c| get(c, d.id)).unwrap().unwrap().merged_at.is_none());
    }

    #[test]
    fn merge_rejects_leaks_in_earlier_commit_messages() {
        for (kind, payload) in [
            ("secret", format!("{}.{}", "a0".repeat(16), "B1".repeat(8))),
            ("personal-email", format!("synthetic-person@{}", "mail.test")),
        ] {
            let root = repo(&format!("message-leak-{kind}"));
            let db = db_for(&root);
            let d = job(&db, "kaan");
            let w = prepare(&db, &d).unwrap().unwrap();
            std::fs::write(w.path.join("b.txt"), "clean\n").unwrap();
            commit(&w.path, &format!("message fixture\n\n{payload}"));
            let leaking = git(&root, &["rev-parse", &w.branch]).unwrap();
            git(&w.path, &["commit", "--allow-empty", "-q", "-m", "clean tip fixture"]).unwrap();
            finish(&db, d.id);
            let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
            let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
            assert_eq!(out["code"], "merge_leaks", "{kind}: {out}");
            assert!(out["findings"].as_array().unwrap().iter().any(|f| f["commit"] == leaking));
            assert!(!out.to_string().contains(&payload));
            assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), target);
            assert!(w.path.is_dir());
            assert!(with(&db, |c| get(c, d.id)).unwrap().unwrap().merged_at.is_none());
        }
    }

    #[test]
    fn merge_rejects_personal_author_and_committer_emails() {
        for role in ["author", "committer"] {
            let root = repo(&format!("private-email-{role}"));
            let db = db_for(&root);
            let d = job(&db, "kaan");
            let w = prepare(&db, &d).unwrap().unwrap();
            std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
            git(&w.path, &["add", "b.txt"]).unwrap();
            let email = "person@example.invalid";
            if role == "author" {
                git(&w.path, &["commit", "-q", "--author", &format!("Person <{email}>"), "-m", "author fixture"]).unwrap();
            } else {
                git(&w.path, &["-c", &format!("user.email={email}"), "commit", "-q", "--author", "Alias <test@users.noreply.github.com>", "-m", "committer fixture"]).unwrap();
            }
            finish(&db, d.id);
            let before = git(&root, &["rev-parse", "HEAD"]).unwrap();
            let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
            assert_eq!(out["merged"], false, "{role}: {out}");
            assert_eq!(out["code"], "merge_identity", "{out}");
            assert!(!out.to_string().contains(email));
            assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
            assert!(w.path.is_dir());
        }
    }

    #[test]
    fn merge_accepts_noreply_identity_without_a_local_email() {
        let root = repo("noreply-email");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        git(&w.path, &["add", "b.txt"]).unwrap();
        git(&w.path, &["-c", "user.email=alias@users.noreply.github.com", "commit", "-q", "-m", "noreply fixture"]).unwrap();
        git(&root, &["config", "--local", "--unset", "user.email"]).unwrap();
        // The merge commit itself still needs an identity; noreply may be worktree-local.
        git(&root, &["config", "extensions.worktreeConfig", "true"]).unwrap();
        git(&root, &["config", "--worktree", "user.email", "alias@users.noreply.github.com"]).unwrap();
        finish(&db, d.id);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
    }

    #[test]
    fn merge_ignores_worker_policy_and_working_tree_overrides() {
        let root = repo("policy-override");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        let secret = format!("{}.{}", "a0".repeat(16), "B1".repeat(8));
        std::fs::write(w.path.join("b.txt"), format!("{secret} # gitleaks:allow\n")).unwrap();
        commit(&w.path, "synthetic fixture");
        let sha = git(&w.path, &["rev-parse", "HEAD"]).unwrap();
        let disabled = "[[rules]]\nid = 'disabled'\nregex = 'never-matches-this-fixture'\n";
        std::fs::write(w.path.join(".gitleaks.toml"), disabled).unwrap();
        std::fs::write(w.path.join(".gitleaksignore"), format!("{sha}:b.txt:dex-zai-api-key:1\n")).unwrap();
        commit(&w.path, "weaken policy fixture");
        std::fs::write(root.join(".gitleaks.toml"), disabled).unwrap();
        std::fs::write(root.join(".gitleaksignore"), format!("{sha}:b.txt:dex-zai-api-key:1\n")).unwrap();
        finish(&db, d.id);
        let before = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let out = merge(&db, &lead(&db), d.id, true, true, &|_| false).unwrap();
        assert_eq!(out["merged"], false, "{out}");
        assert_eq!(out["code"], "merge_leaks", "{out}");
        assert!(!out.to_string().contains(&secret));
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn conflicting_merge_is_aborted_and_lists_the_files() {
        let root = repo("conflict");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("a.txt"), "kaan\n").unwrap();
        commit(&w.path, "kaan a");
        std::fs::write(root.join("a.txt"), "lider\n").unwrap();
        commit(&root, "lider a");
        finish(&db, d.id);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], false);
        assert_eq!(out["conflicts"], json!(["a.txt"]));
        assert!(!root.join(".git").join("MERGE_HEAD").exists(), "the merge was aborted");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "lider\n");
        assert!(w.path.is_dir(), "the tree is kept for resolving");
    }

    #[test]
    fn policy_files_wait_for_the_patron() {
        let root = repo("policy");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::create_dir_all(w.path.join("crates/dex-core/src")).unwrap();
        std::fs::write(w.path.join("crates/dex-core/src/permit.rs"), "// x\n").unwrap();
        std::fs::write(w.path.join("package.json"), "{}\n").unwrap();
        commit(&w.path, "politika");
        finish(&db, d.id);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], false);
        assert_eq!(out["policyFiles"], json!(["crates/dex-core/src/permit.rs"]));
        assert!(!root.join("package.json").exists());
        let out = merge(&db, &lead(&db), d.id, true, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        let notes = out["notes"].to_string();
        assert!(notes.contains("pnpm install") && notes.contains("Rust"), "{notes}");
    }

    #[test]
    fn leak_gate_policy_files_wait_for_the_patron() {
        let root = repo("leak-policy");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        let paths = [
            ".github/workflows/nested/check.yml", ".gitleaks.toml", "crates/dex-core/src/merge_gate.rs",
            "scripts/commit-policy.mjs", "scripts/gitleaks.mjs", "scripts/hooks/nested/pre-commit", "scripts/install-hooks.mjs", "scripts/publish-public.mjs",
        ];
        for path in paths {
            let file = w.path.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "policy fixture\n").unwrap();
        }
        commit(&w.path, "leak gate policy");
        finish(&db, d.id);
        let before = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], false, "{out}");
        assert_eq!(out["policyFiles"], json!(paths), "{out}");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn plan_step_starts_on_its_prerequisites_branches_and_a_conflict_does_not_start() {
        let root = repo("deps");
        let db = db_for(&root);
        let plan = with(&db, |c| {
            let l = delegate::caller(c, "timur")?;
            let p: crate::plan::NewPlan = serde_json::from_value(json!({ "title": "t", "tasks": [
                { "key": "A", "title": "a", "prompt": "a", "agentId": "kaan" },
                { "key": "B", "title": "b", "prompt": "b", "agentId": "ayla" },
                { "key": "C", "title": "c", "prompt": "c", "agentId": "bora", "dependsOn": ["A"] },
                { "key": "D", "title": "d", "prompt": "d", "agentId": "bora", "dependsOn": ["A", "B"] },
            ] }))
            .map_err(|err| err.to_string())?;
            crate::plan::create(c, &l, p)
        })
        .unwrap();
        let item = |key: &str| -> Delegation {
            with(&db, |c| {
                let (id,): (Option<i64>,) = c
                    .query_row("SELECT delegation_id FROM plan_items WHERE plan_id = ?1 AND key = ?2", params![plan, key], |r| Ok((r.get(0)?,)))
                    .map_err(e)?;
                delegate::get(c, id.ok_or("başlamadı")?)
            })
            .unwrap()
        };
        with(&db, |c| crate::plan::step(c).map(|_| ())).unwrap();
        let (a, b) = (item("A"), item("B"));
        let wa = prepare(&db, &a).unwrap().unwrap();
        let wb = prepare(&db, &b).unwrap().unwrap();
        std::fs::write(wa.path.join("a.txt"), "A\n").unwrap();
        commit(&wa.path, "A");
        std::fs::write(wb.path.join("a.txt"), "B\n").unwrap();
        commit(&wb.path, "B");
        for id in [a.id, b.id] {
            let w = with(&db, |c| delegate::caller(c, &delegate::get(c, id)?.to_agent)).unwrap();
            with(&db, |c| delegate::report(c, &w, id, "bitti", None).map(|_| ())).ok();
            finish(&db, id);
        }
        with(&db, |c| crate::plan::step(c).map(|_| ())).unwrap();
        // C sees A's commit — not merged into the team branch.
        let wc = prepare(&db, &item("C")).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(wc.path.join("a.txt")).unwrap(), "A\n");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
        // D needs A and B, which clash on a.txt: no tree, no branch, the reason names the file.
        let c_id = item("C").id;
        with(&db, |c| delegate::set_status(c, c_id, "done")).unwrap();
        with(&db, |c| crate::plan::step(c).map(|_| ())).unwrap();
        let d = item("D");
        match prepare(&db, &d) {
            Err(Error::Conflict(msg)) => assert!(msg.contains("a.txt"), "{msg}"),
            other => panic!("{other:?}"),
        }
        assert!(!root.join(".dex").join("wt").join(format!("bora-{}", d.id)).exists());
        assert!(git(&root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/dex/bora/{}", d.id)]).is_err());
        assert!(with(&db, |c| get(c, d.id)).unwrap().is_none());
    }

    /// #286: a code-review worker in the team.
    fn add_reviewer(db: &Db) {
        with(db, |c| {
            c.execute(
                "INSERT INTO agents (id, team_id, name, kind, title, model, shirt, hair, skin, sort, role_slug)
                 VALUES ('lens', 'default', 'Lens', 'worker', 'Code Reviewer', 'sonnet', '#000', '#000', '#000', 9, 'code-review')",
                [],
            )
            .map_err(e)
        })
        .unwrap();
    }

    /// The review of `d`'s tip as the merge would open it (Core dispatches it; here it stays queued).
    fn open_review(db: &Db, d: &Delegation) -> Delegation {
        let w = with(db, |c| get(c, d.id)).unwrap().unwrap();
        let (sha, team) = tip(&w).unwrap();
        match with(db, |c| crate::review::open(c, &delegate::get(c, d.id)?, &w, &sha, &team)).unwrap() {
            crate::review::Opened::Started(r) => r,
            other => panic!("{other:?}"),
        }
    }

    /// The reviewer reports `text` as its result file.
    fn review_report(db: &Db, root: &Path, review: i64, text: &str) -> Result<Delegation, String> {
        let dir = root.join(".dex").join("results");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{review}.md"));
        std::fs::write(&file, text).unwrap();
        with(db, |c| delegate::report(c, &delegate::caller(c, "lens")?, review, "bitti: inceledim", Some(&file.to_string_lossy())))
    }

    fn state(out: &Value) -> &str {
        out["review"]["state"].as_str().unwrap_or_default()
    }

    #[test]
    fn review_verdict_gates_the_merge_and_binds_to_the_reviewed_commit() {
        let root = repo("review");
        let db = db_for(&root);
        add_reviewer(&db);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        let merge_now = || merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        // No review yet: refused, the caller is asked to open one.
        let out = merge_now();
        assert_eq!((out["merged"].as_bool(), state(&out)), (Some(false), "yok"), "{out}");
        let r1 = open_review(&db, &d);
        assert_eq!(r1.to_agent, "lens");
        assert!(r1.task_id.is_some(), "the board shows the review");
        let sha1 = git(&root, &["rev-parse", &w.branch]).unwrap();
        assert!(r1.prompt.contains(&format!("team...{sha1}")) && r1.prompt.contains("karar: onay") && r1.prompt.contains("Asıl görev"), "{}", r1.prompt);
        assert!(with(&db, |c| prepare_plan_is_none(c, &r1)).unwrap(), "a review has no worktree");
        // Running: refused.
        let out = merge_now();
        assert_eq!((state(&out), out["review"]["id"].as_i64()), ("sürüyor", Some(r1.id)), "{out}");
        // A report without a verdict line is refused; `düzelt` refuses the merge with its findings.
        let err = review_report(&db, &root, r1.id, "inceledim, iyi").unwrap_err();
        assert!(err.contains("karar: onay"), "{err}");
        review_report(&db, &root, r1.id, "karar: düzelt\n\n## önemli\n- b.txt:1 — yanlış harf\n\n## küçük\nyok\n").unwrap();
        let out = merge_now();
        assert_eq!(state(&out), "düzelt", "{out}");
        assert_eq!(out["review"]["important"], "- b.txt:1 — yanlış harf", "{out}");
        assert!(!root.join("b.txt").exists());
        // A new commit is not covered by the older verdict — not even by an `onay`.
        std::fs::write(w.path.join("b.txt"), "B\n").unwrap();
        commit(&w.path, "b düzelt");
        assert_eq!(state(&merge_now()), "yok");
        let r2 = open_review(&db, &d);
        review_report(&db, &root, r2.id, "karar: onay\n## önemli\nyok\n").unwrap();
        std::fs::write(w.path.join("c.txt"), "c\n").unwrap();
        commit(&w.path, "incelemeden sonra");
        let out = merge_now();
        assert_eq!(state(&out), "yok", "onay of an older commit: {out}");
        // `onay` of the tip: merged.
        let r3 = open_review(&db, &d);
        review_report(&db, &root, r3.id, "karar: onay\n").unwrap();
        let out = merge_now();
        assert_eq!(out["merged"], true, "{out}");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "B\n");
    }

    fn prepare_plan_is_none(c: &Connection, d: &Delegation) -> Result<bool, String> {
        Ok(plan(c, d).map_err(|err| format!("{err:?}"))?.is_none())
    }

    #[test]
    fn no_reviewer_merges_directly_until_one_joins() {
        let root = repo("noreviewer");
        let db = db_for(&root);
        let done = |name: &str| {
            let d = job(&db, "kaan");
            let w = prepare(&db, &d).unwrap().unwrap();
            std::fs::write(w.path.join(name), "x
").unwrap();
            commit(&w.path, name);
            finish(&db, d.id);
            d
        };
        // #287: no code-review member, no gate — as before #286.
        let a = done("b.txt");
        let out = merge(&db, &lead(&db), a.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert!(root.join("b.txt").is_file());
        // A reviewer joined: the unreviewed tip is gated, decided at merge time.
        let b = done("c.txt");
        add_reviewer(&db);
        let out = merge(&db, &lead(&db), b.id, false, false, &|_| false).unwrap();
        assert_eq!((out["merged"].as_bool(), state(&out)), (Some(false), "yok"), "{out}");
        assert!(!root.join("c.txt").exists());
    }

    #[test]
    fn fix_job_starts_on_the_reviewed_branch_and_its_merge_takes_the_old_one() {
        let root = repo("fixof");
        let db = db_for(&root);
        add_reviewer(&db);
        let a = job(&db, "kaan");
        let wa = prepare(&db, &a).unwrap().unwrap();
        std::fs::write(wa.path.join("b.txt"), "b\n").unwrap();
        commit(&wa.path, "b ekle");
        finish(&db, a.id);
        let ra = open_review(&db, &a);
        review_report(&db, &root, ra.id, "karar: düzelt\n## önemli\n- b.txt:1 — eksik\n").unwrap();
        let fix = job(&db, "kaan");
        with(&db, |c| { crate::review::check_fix_of(c, "default", a.id)?; crate::review::set_fix_of(c, fix.id, a.id) }).unwrap();
        let wf = prepare(&db, &fix).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(wf.path.join("b.txt")).unwrap(), "b\n", "the fix starts on the reviewed branch");
        std::fs::write(wf.path.join("b.txt"), "b tamam\n").unwrap();
        commit(&wf.path, "b düzelt");
        finish(&db, fix.id);
        // The old branch alone is judged by its own verdict.
        assert_eq!(state(&merge(&db, &lead(&db), a.id, false, false, &|_| false).unwrap()), "düzelt");
        let rf = open_review(&db, &fix);
        assert!(rf.prompt.contains(&format!("Düzeltme işi (#{})", fix.id)) && rf.prompt.contains(&format!("Asıl görev (#{})", a.id)), "{}", rf.prompt);
        review_report(&db, &root, rf.id, "karar: onay\n").unwrap();
        let out = merge(&db, &lead(&db), fix.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b tamam\n");
        // The fixed job's branch is in the team branch: counted merged and cleaned up.
        let old = with(&db, |c| get(c, a.id)).unwrap().unwrap();
        assert!(old.merged_at.is_some() && old.removed_at.is_some(), "{old:?}");
        assert!(!wa.path.exists());
        assert!(git(&root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", wa.branch)]).is_err());
        assert_eq!(merge(&db, &lead(&db), a.id, false, false, &|_| false).unwrap()["merged"], true);
        // fixOf must name an unmerged branch of the team.
        assert!(with(&db, |c| crate::review::check_fix_of(c, "default", a.id)).unwrap_err().contains("zaten birleştirildi"));
    }

    fn rename(db: &Db, id: &str, name: &str) {
        with(db, |c| crate::teams::update_agent(c, id, crate::teams::AgentPatch { name: Some(name.into()), ..Default::default() }).map(|_| ())).unwrap();
    }

    /// As if the agent had been created under `name`: its slug is given anew.
    fn named(db: &Db, id: &str, name: &str) {
        with(db, |c| {
            c.execute("UPDATE agents SET name = ?2, slug = NULL WHERE id = ?1", rusqlite::params![id, name]).map_err(|e| e.to_string())?;
            crate::slug::agent(c, id).map(|_| ())
        })
        .unwrap();
    }

    #[test]
    fn new_trees_and_branches_take_the_visible_name_and_a_clash_the_next_number() {
        let root = repo("slug");
        let db = db_for(&root);
        named(&db, "kaan", "Pixel-1");
        named(&db, "ayla", "Pixel 1");
        let (a, b) = (job(&db, "kaan"), job(&db, "ayla"));
        let wa = prepare(&db, &a).unwrap().unwrap();
        let wb = prepare(&db, &b).unwrap().unwrap();
        assert_eq!(wa.path, root.join(".dex").join("wt").join(format!("pixel-1-{}", a.id)));
        assert_eq!(wa.branch, format!("dex/pixel-1/{}", a.id));
        assert_eq!(wb.branch, format!("dex/pixel-1-2/{}", b.id), "same slug, later member: the next number");
        std::fs::write(wa.path.join("Cargo.toml"), "").unwrap();
        let target = wa.env().into_iter().find(|(k, _)| k == "CARGO_TARGET_DIR").unwrap().1;
        assert!(target.ends_with(&format!("target{}pixel-1", std::path::MAIN_SEPARATOR)), "{target}");
        // A tree recorded under the old name keeps its id's target dir.
        let old = Wt { branch: format!("dex/kaan/{}", a.id), ..wa.clone() };
        assert!(old.env()[0].1.ends_with(&format!("target{}kaan", std::path::MAIN_SEPARATOR)));
    }

    #[test]
    fn fix_job_starts_on_a_branch_made_under_the_old_name() {
        let root = repo("fixold");
        let db = db_for(&root);
        let a = job(&db, "kaan");
        let wa = prepare(&db, &a).unwrap().unwrap();
        assert_eq!(wa.branch, format!("dex/kaan/{}", a.id));
        std::fs::write(wa.path.join("b.txt"), "b\n").unwrap();
        commit(&wa.path, "b ekle");
        finish(&db, a.id);
        rename(&db, "kaan", "Pixel-1");
        let fix = job(&db, "kaan");
        with(&db, |c| crate::review::set_fix_of(c, fix.id, a.id)).unwrap();
        let wf = prepare(&db, &fix).unwrap().unwrap();
        assert_eq!(wf.branch, format!("dex/kaan/{}", fix.id), "#463: a rename keeps the slug");
        assert_eq!(std::fs::read_to_string(wf.path.join("b.txt")).unwrap(), "b\n", "the old-named branch is found in the DB");
        // The old tree stays where it was recorded.
        assert_eq!(with(&db, |c| get(c, a.id)).unwrap().unwrap().path, wa.path);
    }

    /// The worktree-junction-remove trap: the junction goes as a link, the root's folder stays.
    #[test]
    fn node_modules_junction_is_removed_as_a_link() {
        let root = repo("junction");
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::write(root.join("node_modules").join("pkg").join("index.js"), "x").unwrap();
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        assert!(w.junction, "no pnpm-lock: linked to the root's node_modules");
        assert!(w.path.join("node_modules").join("pkg").join("index.js").is_file());
        finish(&db, d.id);
        assert_eq!(merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(!w.path.exists());
        assert!(root.join("node_modules").join("pkg").join("index.js").is_file(), "the root's node_modules is intact");
    }

    /// A process whose current directory is `dir` (the worker's session, as Windows sees it).
    pub(crate) fn hold(dir: &Path) -> std::process::Child {
        #[cfg(windows)]
        let mut c = {
            let mut c = Command::new("ping");
            c.args(["-n", "120", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut c = {
            let mut c = Command::new("sleep");
            c.arg("120");
            c
        };
        c.current_dir(dir).stdout(std::process::Stdio::null());
        hidden(&mut c).spawn().unwrap()
    }

    /// #195: a folder held by a process does not go — the branch still does, the job is a pending
    /// cleanup, and Dex's next start finishes it; `.dex/wt/` leftovers go only when empty.
    #[test]
    fn held_tree_keeps_a_pending_cleanup_that_the_start_sweep_finishes() {
        let root = repo("held");
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::write(root.join("node_modules").join("pkg").join("index.js"), "x").unwrap();
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        let mut held = hold(&w.path);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert!(git(&root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", w.branch)]).is_err(), "the branch goes anyway");
        #[cfg(unix)]
        assert!(out.get("cleanup").is_none(), "Unix permits removing a process's working directory: {out}");
        if cfg!(windows) {
            let cleanup = out["cleanup"].as_str().unwrap_or_default();
            assert!(cleanup.contains("bekleyen temizlik") && cleanup.contains("dal silindi"), "{out}");
            assert!(w.path.is_dir() && !registered(&root, &w.path));
            assert_eq!(with(&db, |c| pending(c, Some("kaan"))).unwrap().len(), 1);
            // Still held: the start sweep returns a maintenance warning.
            let left = sweep(&db).unwrap();
            assert_eq!(left.len(), 1, "{left:?}");
            assert_eq!(left[0].delegation.as_ref().map(|d| d.from_agent.as_str()), Some("timur"));
        }
        held.kill().unwrap();
        held.wait().unwrap();
        // Leftovers: an empty folder, one with a node_modules link (the link goes, the root's
        // node_modules stays), one with content (kept with a UI warning), a live job's tree.
        let wt = root.join(".dex").join("wt");
        std::fs::create_dir_all(wt.join("empty")).unwrap();
        std::fs::create_dir_all(wt.join("linked")).unwrap();
        assert!(link_dir(&root.join("node_modules"), &wt.join("linked").join("node_modules")));
        std::fs::create_dir_all(wt.join("full")).unwrap();
        std::fs::write(wt.join("full").join("x.txt"), "x").unwrap();
        let live = prepare(&db, &job(&db, "ayla")).unwrap().unwrap();
        let left = sweep(&db).unwrap();
        assert!(!w.path.exists());
        let row = with(&db, |c| get(c, d.id)).unwrap().unwrap();
        assert!(row.removed_at.is_some());
        assert!(!wt.join("empty").exists() && !wt.join("linked").exists());
        assert!(root.join("node_modules").join("pkg").join("index.js").is_file(), "the root's node_modules is intact");
        assert!(wt.join("full").join("x.txt").is_file(), "content is never deleted");
        assert!(live.path.join("a.txt").is_file(), "a registered tree is left alone");
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].delegation.is_none() && left[0].root == root && left[0].text.contains("full"), "{left:?}");
        assert!(sweep(&db).unwrap().iter().all(|l| l.delegation.is_none()), "nothing pending any more");
        let warnings = || with(&db, |c| crate::teams::get_team(c, &root, "default")).unwrap().worktree_cleanup;
        assert_eq!(warnings(), vec![wt.join("full").to_string_lossy().into_owned()]);
        std::fs::remove_file(wt.join("full").join("x.txt")).unwrap();
        assert!(sweep(&db).unwrap().is_empty());
        assert!(warnings().is_empty(), "successful cleanup clears the warning");
    }

    /// #888: a second project of the default team on its own repository.
    fn second(db: &Db, name: &str) -> (PathBuf, String) {
        let root = repo(name);
        let ready = crate::projects::prepare_root(&root.to_string_lossy(), None).unwrap();
        let new = crate::projects::NewProject { name: name.into(), worktrees: None };
        let p = with(db, |c| crate::projects::create(c, "default", new, &ready).map_err(|err| err.to_string())).unwrap();
        (root, p.id)
    }

    /// A running job of `to` bound as `delegate` binds it (`project`: explicit; `branch`: pinned).
    fn job_in(db: &Db, to: &str, project: Option<&str>, branch: Option<&str>) -> Delegation {
        with(db, |c| {
            let l = delegate::caller(c, "timur")?;
            let mut ctx = delegate::resolve_context(c, Path::new(""), &l, project, None)?;
            if let Some(b) = branch {
                ctx.merge_branch = Some(b.into());
            }
            let new = NewDelegation { from: &l, to_agent: to.into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None };
            let d = delegate::insert_bound(c, new, &ctx)?;
            delegate::set_status(c, d.id, "running")?;
            delegate::get(c, d.id)
        })
        .unwrap()
    }

    fn has_branch(root: &Path, branch: &str) -> bool {
        git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
    }

    #[test]
    fn two_projects_work_in_parallel_and_each_lands_in_its_own_repository() {
        let one = repo("proj-one");
        let db = db_for(&one);
        let (two, p2) = second(&db, "proj-two");
        // Two bound projects: the old unbound insert has to choose, so it refuses.
        let l = lead(&db);
        let err = with(&db, |c| delegate::insert(c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }))
            .unwrap_err();
        assert!(err.contains("project_required"), "{err}");
        let a = job_in(&db, "kaan", Some("legacy-default"), None);
        let b = job_in(&db, "ayla", Some(&p2), None);
        let wa = prepare(&db, &a).unwrap().unwrap();
        let wb = prepare(&db, &b).unwrap().unwrap();
        assert_eq!((wa.root.as_path(), wb.root.as_path()), (one.as_path(), two.as_path()));
        assert!(wa.path.starts_with(one.join(".dex").join("wt")) && wb.path.starts_with(two.join(".dex").join("wt")), "{wa:?} {wb:?}");
        assert_eq!(wb.base, git(&two, &["rev-parse", "team"]).unwrap());
        assert!(has_branch(&two, &wb.branch) && !has_branch(&one, &wb.branch));
        std::fs::write(wa.path.join("one.txt"), "1\n").unwrap();
        commit(&wa.path, "one");
        std::fs::write(wb.path.join("two.txt"), "2\n").unwrap();
        commit(&wb.path, "two");
        finish(&db, a.id);
        finish(&db, b.id);
        assert_eq!(merge(&db, &lead(&db), b.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(two.join("two.txt").is_file() && !one.join("two.txt").exists());
        assert!(!wb.path.exists() && !has_branch(&two, &wb.branch));
        assert!(wa.path.is_dir(), "the other project's tree is untouched");
        assert_eq!(merge(&db, &lead(&db), a.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(one.join("one.txt").is_file() && !two.join("one.txt").exists());
        // The start sweep sees the second project's root too.
        let left = two.join(".dex").join("wt").join("empty");
        std::fs::create_dir_all(&left).unwrap();
        assert!(sweep(&db).unwrap().is_empty());
        assert!(!left.exists());
    }

    #[test]
    fn merge_refuses_when_the_main_checkout_left_the_target_and_never_switches_it() {
        let root = repo("target-moved");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        assert_eq!(d.merge_branch.as_deref(), Some("team"), "pinned at delegation");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        let merge_now = || merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        git(&root, &["switch", "-q", "-c", "other"]).unwrap();
        let out = merge_now();
        assert_eq!((out["merged"].as_bool(), out["code"].as_str(), out["head"].as_str()), (Some(false), Some("project_branch_mismatch"), Some("other")), "{out}");
        assert_eq!(git(&root, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "other", "no automatic checkout");
        assert!(!root.join("b.txt").exists() && w.path.is_dir());
        git(&root, &["checkout", "-q", "--detach"]).unwrap();
        let out = merge_now();
        assert_eq!((out["code"].as_str(), out["head"].is_null()), (Some("project_branch_mismatch"), true), "{out}");
        git(&root, &["switch", "-q", "team"]).unwrap();
        assert_eq!(merge_now()["merged"], true);
        assert!(root.join("b.txt").is_file());
        assert_ne!(git(&root, &["rev-parse", "other"]).unwrap(), git(&root, &["rev-parse", "team"]).unwrap(), "only the target moved");
    }

    /// #901: landed in its target but the tree stayed (a held folder); the main checkout is on
    /// another branch by the next call — only the cleanup runs, nothing lands in that branch.
    #[test]
    fn a_landed_job_with_a_pending_cleanup_never_merges_into_another_branch() {
        let root = repo("landed-pending");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        git(&root, &["branch", "other"]).unwrap();
        git(&root, &["merge", "--no-ff", "--no-edit", &w.branch]).unwrap();
        with(&db, |c| c.execute("UPDATE delegation_worktrees SET merged_at = ?2 WHERE delegation_id = ?1", params![d.id, now()]).map_err(e)).unwrap();
        git(&root, &["switch", "-q", "other"]).unwrap();
        let other = git(&root, &["rev-parse", "other"]).unwrap();
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert!(out["note"].as_str().unwrap_or_default().contains("zaten birleştirildi"), "{out}");
        assert_eq!(git(&root, &["rev-parse", "other"]).unwrap(), other, "nothing landed in the checked-out branch");
        assert_eq!(git(&root, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "other", "no automatic checkout");
        assert!(!root.join("b.txt").exists());
        let row = with(&db, |c| get(c, d.id)).unwrap().unwrap();
        assert!(row.removed_at.is_some() && !w.path.exists() && !has_branch(&root, &w.branch), "the pending cleanup ran");
    }

    #[test]
    fn the_tree_starts_on_the_pinned_target_and_a_lost_target_does_not_start() {
        let root = repo("target-base");
        let db = db_for(&root);
        let team = git(&root, &["rev-parse", "team"]).unwrap();
        git(&root, &["switch", "-q", "-c", "other"]).unwrap();
        std::fs::write(root.join("o.txt"), "o\n").unwrap();
        commit(&root, "other");
        let d = job(&db, "kaan");
        let w = prepare(&db, &d).unwrap().unwrap();
        assert_eq!((w.base.as_str(), w.merge_branch.as_deref()), (team.as_str(), Some("team")), "the target's tip, not the checkout's HEAD");
        assert!(!w.path.join("o.txt").exists());
        // The target is gone: no tree, no fallback to the checkout's HEAD or the team folder.
        let lost = job(&db, "ayla");
        git(&root, &["branch", "-q", "-m", "team", "renamed"]).unwrap();
        match prepare(&db, &lost) {
            Err(Error::Conflict(msg)) => assert!(msg.starts_with("project_branch_mismatch"), "{msg}"),
            other => panic!("{other:?}"),
        }
        assert!(with(&db, |c| get(c, lost.id)).unwrap().is_none());
        // A pinned root that is gone is a failed start too (never the team folder).
        let gone = with(&db, |c| {
            c.execute(
                "INSERT INTO delegations (from_agent, to_agent, team_id, prompt, status, project_id, project_root, merge_branch, created_at, updated_at)
                 VALUES ('timur', 'bora', 'default', 'iş', 'running', 'legacy-default', ?1, 'renamed', 1, 1)",
                [root.join("yok").to_string_lossy()],
            )
            .map_err(e)?;
            delegate::get(c, c.last_insert_rowid())
        })
        .unwrap();
        match prepare(&db, &gone) {
            Err(Error::Conflict(msg)) => assert!(msg.starts_with("project_unavailable"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_job_from_before_projects_lands_in_the_checked_out_branch_as_before() {
        let root = repo("legacy-head");
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [root.to_string_lossy()]).unwrap();
        bind_legacy(&c, false);
        let db = Db::new(c);
        git(&root, &["switch", "-q", "-c", "other"]).unwrap();
        let d = job(&db, "kaan");
        assert_eq!(d.merge_branch, None, "never verified: no target");
        let w = prepare(&db, &d).unwrap().unwrap();
        assert_eq!(w.base, git(&root, &["rev-parse", "HEAD"]).unwrap());
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        assert_eq!(merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(root.join("b.txt").is_file());
        assert_eq!(git(&root, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "other");
    }

    #[test]
    fn only_same_repository_prerequisites_and_fixes_reach_the_tree() {
        let one = repo("deps-one");
        let db = db_for(&one);
        let (two, p2) = second(&db, "deps-two");
        let plan = with(&db, |c| {
            let l = delegate::caller(c, "timur")?;
            let p: crate::plan::NewPlan = serde_json::from_value(json!({ "title": "t", "project": "legacy-default", "tasks": [
                { "key": "A", "title": "a", "prompt": "a", "agentId": "kaan" },
                { "key": "B", "title": "b", "prompt": "b", "agentId": "ayla", "project": p2 },
                { "key": "C", "title": "c", "prompt": "c", "agentId": "bora", "dependsOn": ["A", "B"] },
            ] }))
            .map_err(|err| err.to_string())?;
            crate::plan::create(c, &l, p)
        })
        .unwrap();
        let item = |key: &str| -> Delegation {
            with(&db, |c| {
                let id: Option<i64> = c.query_row("SELECT delegation_id FROM plan_items WHERE plan_id = ?1 AND key = ?2", params![plan, key], |r| r.get(0)).map_err(e)?;
                delegate::get(c, id.ok_or("başlamadı")?)
            })
            .unwrap()
        };
        with(&db, |c| crate::plan::step(c).map(|_| ())).unwrap();
        let (a, b) = (item("A"), item("B"));
        let wa = prepare(&db, &a).unwrap().unwrap();
        let wb = prepare(&db, &b).unwrap().unwrap();
        assert_eq!(wb.root, two);
        std::fs::write(wa.path.join("a.txt"), "A\n").unwrap();
        commit(&wa.path, "A");
        std::fs::write(wb.path.join("b.txt"), "B\n").unwrap();
        commit(&wb.path, "B");
        // A branch of B's name in the first repository (a clash): still never C's base.
        git(&one, &["branch", &wb.branch]).unwrap();
        let clash = one.join(".dex").join("tmp-clash");
        git(&one, &["worktree", "add", "-q", &clash.to_string_lossy(), &wb.branch]).unwrap();
        std::fs::write(clash.join("clash.txt"), "x\n").unwrap();
        commit(&clash, "clash");
        git(&one, &["worktree", "remove", "--force", &clash.to_string_lossy()]).unwrap();
        for id in [a.id, b.id] {
            finish(&db, id);
        }
        with(&db, |c| crate::plan::step(c).map(|_| ())).unwrap();
        let cd = item("C");
        assert_eq!(cd.project_id.as_deref(), Some("legacy-default"));
        let wc = prepare(&db, &cd).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(wc.path.join("a.txt")).unwrap(), "A\n", "same repository: merged");
        assert!(!wc.path.join("b.txt").exists() && !wc.path.join("clash.txt").exists(), "another project's prerequisite is a report only");
        // fixOf of another project's job (B checks it at delegate; Git refuses it again).
        let fix = job_in(&db, "kaan", Some("legacy-default"), None);
        with(&db, |c| crate::review::set_fix_of(c, fix.id, b.id)).unwrap();
        match prepare(&db, &fix) {
            Err(Error::Conflict(msg)) => assert!(msg.starts_with("project_mismatch"), "{msg}"),
            other => panic!("{other:?}"),
        }
        assert!(with(&db, |c| get(c, fix.id)).unwrap().is_none());
    }

    #[test]
    fn review_opens_with_several_projects_on_the_reviewed_job_binding() {
        let one = repo("review-one");
        let db = db_for(&one);
        add_reviewer(&db);
        let (two, p2) = second(&db, "review-two");
        let d = job_in(&db, "kaan", Some(&p2), None);
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        let out = merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap();
        assert_eq!(state(&out), "yok", "{out}");
        let r = open_review(&db, &d);
        assert_eq!((&r.project_id, &r.project_root, &r.merge_branch), (&d.project_id, &d.project_root, &d.merge_branch));
        assert!(r.prompt.contains(&format!("git -C {} diff team...", two.display())), "{}", r.prompt);
        let task: Option<String> = with(&db, |c| c.query_row("SELECT project_id FROM tasks WHERE id = ?1", [r.task_id.unwrap()], |row| row.get(0)).map_err(e)).unwrap();
        assert_eq!(task.as_deref(), Some(p2.as_str()));
        // Its report belongs to the reviewed job's project.
        assert!(review_report(&db, &one, r.id, "karar: onay\n").unwrap_err().contains("altına yazılmalı"));
        review_report(&db, &two, r.id, "karar: onay\n").unwrap();
        assert_eq!(merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(two.join("b.txt").is_file());
    }

    #[test]
    fn review_and_its_new_tip_follow_the_target_snapshot_not_the_project_of_today() {
        let root = repo("review-pin");
        let db = db_for(&root);
        add_reviewer(&db);
        git(&root, &["branch", "release"]).unwrap();
        // One project whose target is `team`; the job was pinned to `release`.
        let d = job_in(&db, "kaan", None, Some("release"));
        let w = prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        commit(&w.path, "b ekle");
        finish(&db, d.id);
        let r1 = open_review(&db, &d);
        assert_eq!((r1.merge_branch.as_deref(), r1.project_root.as_deref()), (Some("release"), d.project_root.as_deref()));
        assert!(r1.prompt.contains("diff release..."), "{}", r1.prompt);
        review_report(&db, &root, r1.id, "karar: onay\n").unwrap();
        // A new worker commit is not covered: a new review is due.
        std::fs::write(w.path.join("c.txt"), "c\n").unwrap();
        commit(&w.path, "c ekle");
        git(&root, &["switch", "-q", "release"]).unwrap();
        assert_eq!(state(&merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()), "yok");
        let r2 = open_review(&db, &d);
        assert_ne!(r1.id, r2.id);
        review_report(&db, &root, r2.id, "karar: onay\n").unwrap();
        assert_eq!(merge(&db, &lead(&db), d.id, false, false, &|_| false).unwrap()["merged"], true);
        assert!(root.join("c.txt").is_file());
        assert!(git(&root, &["merge-base", "--is-ancestor", "release", "team"]).is_err(), "team did not get it");
    }
}
