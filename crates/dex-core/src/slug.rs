//! #415: the agent's visible name as a path segment — worktree `.dex/wt/<slug>-<job>`, branch
//! `dex/<slug>/<job>`, memory `.dex/memory/agents/<slug>`. Ids stay in the DB; existing
//! worktrees and branches keep their recorded names. #436: the slug is kept in `agents.slug`.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Lower-case ASCII, words joined by `-`; Turkish letters transliterated (ı→i, ş→s…).
pub fn of(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'ı' | 'i' | 'î' | 'ï' | 'í' | 'ì' => 'i',
            'ş' | 'ś' => 's',
            'ğ' => 'g',
            'ü' | 'û' | 'ú' | 'ù' => 'u',
            'ö' | 'ô' | 'ó' | 'ò' => 'o',
            'ç' | 'ć' => 'c',
            'â' | 'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            c => c,
        };
        // 'İ' lowers to 'i' + U+0307; the mark is no separator.
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "agent".into() } else { out }
}

/// #436: the slug lives in `agents.slug`, set once (creation, or first need for a row that has
/// none) and never changed after: not re-derived from member order, not moved by a rename (#463).
fn taken(conn: &Connection, team_id: &str, except: &str) -> Result<Vec<String>, String> {
    let mut st = conn.prepare("SELECT slug FROM agents WHERE team_id = ?1 AND id <> ?2 AND slug IS NOT NULL").map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params![team_id, except], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// #475: Windows device names — no file or folder can bear them, with or without an extension.
/// A slug has no `.`, so only the bare name can meet one.
fn reserved(slug: &str) -> bool {
    matches!(slug, "con" | "prn" | "aux" | "nul")
        || matches!(slug.as_bytes(), [b'c', b'o', b'm', d] | [b'l', b'p', b't', d] if (b'1'..=b'9').contains(d))
}

/// The first free of `base`, `base-2`, `base-3`…: held by no other member and — when the memory
/// root is known — no `agents/<slug>` folder on disk, so a deleted agent's memory is never handed on.
/// #2463: a worker's private memory also sits in the team's project roots; those count too.
/// A Windows device name becomes `<name>-agent` first (#475).
fn pick(conn: &Connection, team_id: &str, id: &str, name: &str, root: Option<&Path>) -> Result<String, String> {
    let taken = taken(conn, team_id, id)?;
    let base = of(name);
    let base = if reserved(&base) { format!("{base}-agent") } else { base };
    let mut roots: Vec<PathBuf> = root.map(Path::to_path_buf).into_iter().collect();
    if root.is_some() {
        let projects = crate::projects::list(conn, team_id, true).map_err(|e| e.to_string())?;
        roots.extend(projects.into_iter().filter_map(|p| p.root_path).map(|p| Path::new(&p).join(".dex").join("memory")));
    }
    Ok((1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|c| !taken.contains(c) && !roots.iter().any(|r| r.join("agents").join(c).exists()))
        .expect("unbounded"))
}

type Row = (String, String, Option<String>);

/// (team, name, slug) of an agent row.
fn row(conn: &Connection, id: &str) -> Result<Option<Row>, String> {
    conn.query_row("SELECT team_id, name, slug FROM agents WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .optional()
    .map_err(|e| e.to_string())
}

/// The agent's slug, given (and kept) if its row has none yet; an id with no row (a patron pane)
/// is made safe as it is. `root`: the team's memory root for the disk check — `None` where
/// `agents/<id>` folders may be the agents' own (open-time backfill, rows written by plain SQL).
pub fn assign(conn: &Connection, id: &str, root: Option<&Path>) -> Result<String, String> {
    let Some((team, name, slug)) = row(conn, id)? else { return Ok(of(id)) };
    // A device-name slug (kept before #475) never had a folder or branch on Windows: given anew.
    if let Some(s) = slug.filter(|s| !reserved(s)) {
        return Ok(s);
    }
    let s = pick(conn, &team, id, &name, root)?;
    conn.execute("UPDATE agents SET slug = ?2 WHERE id = ?1", params![id, s]).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Every row still without a slug gets one, in `sort` order (open time, after the seed).
pub fn backfill(conn: &Connection) -> Result<(), String> {
    let mut st = conn.prepare("SELECT id FROM agents WHERE slug IS NULL ORDER BY team_id, sort, id").map_err(|e| e.to_string())?;
    let ids = st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    ids.iter().try_for_each(|id| assign(conn, id, None).map(|_| ()))
}

/// Every member's slug in the team, as kept in the DB.
pub fn team(conn: &Connection, team_id: &str) -> Result<Vec<(String, String)>, String> {
    let mut st = conn.prepare("SELECT id FROM agents WHERE team_id = ?1 ORDER BY sort, id").map_err(|e| e.to_string())?;
    let ids = st
        .query_map(params![team_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    ids.into_iter().map(|id| assign(conn, &id, None).map(|s| (id, s))).collect()
}

/// The agent's slug (see [`assign`]).
pub fn agent(conn: &Connection, id: &str) -> Result<String, String> {
    assign(conn, id, None)
}

/// The agent's own memory folder name under the team's memory root: its slug — or, while its
/// `agents/<id>` → `agents/<slug>` move is still pending (#475: it failed, the next open retries),
/// the old `agents/<id>`, so its notes stay in sight and no empty slug folder is made.
pub fn memory_name(conn: &Connection, root: &Path, id: &str) -> Result<String, String> {
    let s = agent(conn, id)?;
    let agents = root.join("agents");
    if s == id || migrated(conn)? || !agents.join(id).is_dir() || agents.join(&s).exists() {
        return Ok(s);
    }
    // As in the migration: a source that is another member's slug on the same memory root is
    // theirs, not this agent's (#486: one in a team on another root takes nothing).
    let Some((team, ..)) = row(conn, id)? else { return Ok(s) };
    let mut st = conn
        .prepare("SELECT t.id, t.kind, t.cwd, t.agent_memory_root FROM agents a JOIN teams t ON t.id = a.team_id WHERE a.slug = ?1 AND a.id <> ?1")
        .map_err(|e| e.to_string())?;
    let holders = st
        .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let own = !holders.iter().any(|(t, kind, cwd, anchor)| *t == team || match anchor {
        Some(anchor) => crate::memory::same_dir(Path::new(anchor), root),
        None => on_root(t, kind, cwd.as_deref(), root),
    });
    Ok(if own { id.to_string() } else { s })
}

/// Whether a team's memory root (as `teams::list_teams` gives it) is `root`.
fn on_root(team_id: &str, kind: &str, cwd: Option<&str>, root: &Path) -> bool {
    if kind == "service" {
        root.ends_with(Path::new("teams").join(team_id).join("memory"))
    } else {
        cwd.is_some_and(|c| Path::new(c).join(".dex").join("memory") == root)
    }
}

/// The agent's own memory folder under the team's memory root (see [`memory_name`]).
pub fn memory_dir(conn: &Connection, root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(root.join("agents").join(memory_name(conn, root, id)?))
}

/// What moving `agents/<from>` to `agents/<to>` did.
#[derive(Debug, PartialEq)]
pub enum Move {
    Moved,
    /// The target already exists: nothing merged, the old folder kept.
    TargetExists,
}

/// Moves memory folders `agents/<from>` → `agents/<to>`; a missing source or `from == to` is
/// skipped. Never merges: a taken target leaves both as they are.
pub fn move_memory(root: &Path, moves: &[(String, String)]) -> Vec<(String, String, Result<Move, String>)> {
    let agents = root.join("agents");
    let mut out = Vec::new();
    for (from, to) in moves {
        let (src, dst) = (agents.join(from), agents.join(to));
        if from == to || !src.is_dir() {
            continue;
        }
        let r = if dst.exists() {
            Ok(Move::TargetExists)
        } else {
            rename(&src, &dst).map(|_| Move::Moved).map_err(|e| e.to_string())
        };
        out.push((from.clone(), to.clone(), r));
    }
    out
}

#[cfg(test)]
thread_local! {
    /// Tests: the next renames fail, as a locked or denied folder would.
    static FAIL_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_RENAME.get() {
        return Err(std::io::Error::other("test: taşıma reddedildi"));
    }
    std::fs::rename(src, dst)
}

/// Logs what [`move_memory`] did (folder names only, no content).
pub fn log_moves(root: &Path, done: &[(String, String, Result<Move, String>)]) {
    for (from, to, r) in done {
        let agents = root.join("agents");
        match r {
            Ok(Move::Moved) => eprintln!("dex: hafıza taşındı: {} → {}", agents.join(from).display(), agents.join(to).display()),
            Ok(Move::TargetExists) => eprintln!(
                "dex: uyarı: hafıza taşınmadı, hedef zaten var ({}); eski klasör korunuyor: {}",
                agents.join(to).display(),
                agents.join(from).display()
            ),
            Err(err) => eprintln!("dex: uyarı: hafıza taşınamadı ({} → {to}): {err}", agents.join(from).display()),
        }
    }
}

/// `settings` key: the one-time `agents/<id>` → `agents/<slug>` move is done (#463).
const MIGRATED: &str = "memory_slug_migrated";

fn migrated(conn: &Connection) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key = ?1)", [MIGRATED], |r| r.get(0)).map_err(|e| e.to_string())
}

/// #415/#463, on Dex start — before the Core exists, so no plan, resume or session can hold a
/// path yet: every team's `agents/<id>` memory folder goes to `agents/<slug>`, once. Done is
/// recorded in `settings`; later opens move nothing. A source that is another member's slug (on
/// the same memory root) is that member's folder and stays, as does one whose target is taken.
/// Returns whether it ran. #475: a failed move is `Err` and leaves it not done — the next open
/// retries (moved folders are gone from their source by then); meanwhile [`memory_dir`] keeps
/// that agent on its old folder.
pub fn migrate_memory(conn: &Connection, data_dir: &Path) -> Result<bool, String> {
    if migrated(conn)? {
        return Ok(false);
    }
    let mut roots: Vec<(PathBuf, Vec<(String, String)>)> = Vec::new();
    for t in crate::teams::list_teams(conn, data_dir)? {
        let Some(root) = t.agent_memory_root.or(t.memory_dir).map(PathBuf::from) else { continue };
        let pairs = team(conn, &t.id)?;
        match roots.iter_mut().find(|(r, _)| crate::memory::same_dir(r, &root)) {
            Some((_, all)) => all.extend(pairs),
            None => roots.push((root, pairs)),
        }
    }
    let mut failed = 0;
    for (root, pairs) in &roots {
        let moves: Vec<(String, String)> =
            pairs.iter().filter(|(id, _)| !pairs.iter().any(|(other, s)| other != id && s == id)).cloned().collect();
        let done = move_memory(root, &moves);
        log_moves(root, &done);
        failed += done.iter().filter(|(_, _, r)| r.is_err()).count();
    }
    if failed > 0 {
        return Err(format!("{failed} hafıza klasörü taşınamadı; sonraki açılışta yeniden denenecek"));
    }
    conn.execute("INSERT INTO settings (key, value) VALUES (?1, 'true') ON CONFLICT(key) DO NOTHING", [MIGRATED])
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_team_uses_explicit_private_anchor_for_migration_and_fallback() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("db")).unwrap();
        let t = crate::teams::create_team(
            &c,
            &f.0,
            crate::teams::NewTeam { profile: None, name: "Category".into(), kind: "main".into(), cwd: None, worker_roles: None, members: None },
        )
        .unwrap();
        let a = crate::teams::list_agents(&c).unwrap().into_iter().find(|a| a.team_id == t.id && a.kind == "worker").unwrap();
        c.execute("UPDATE agents SET slug='stable-private' WHERE id=?1", [&a.id]).unwrap();
        let root = PathBuf::from(t.agent_memory_root.unwrap());
        put(&root.join("agents").join(&a.id), "old.md");
        assert_eq!(memory_dir(&c, &root, &a.id).unwrap(), root.join("agents").join(&a.id));
        assert!(migrate_memory(&c, &f.0).unwrap());
        assert!(root.join("agents/stable-private/old.md").is_file());
        assert!(!migrate_memory(&c, &f.0).unwrap());
    }

    #[test]
    fn turkish_letters_and_separators() {
        assert_eq!(of("Core"), "core");
        assert_eq!(of("Pixel-1"), "pixel-1");
        assert_eq!(of("Işıl Şahin"), "isil-sahin");
        assert_eq!(of("ÇAĞRI Öğüt"), "cagri-ogut");
        assert_eq!(of("  --Ada__  Lovelace!! "), "ada-lovelace");
        assert_eq!(of("İpek"), "ipek");
        assert_eq!(of("???"), "agent");
    }

    /// Seed crew renamed by plain SQL, slugs dropped, then given again as at open.
    fn crew(names: &[(&str, &str)]) -> Connection {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (id, name) in names {
            c.execute("UPDATE agents SET name = ?2, slug = NULL WHERE id = ?1", params![id, name]).unwrap();
        }
        backfill(&c).unwrap();
        c
    }

    fn add(c: &Connection, name: &str) -> String {
        crate::teams::create_agent(c, Path::new("/data"), crate::teams::NewAgent {
            team_id: "default".into(), name: name.into(), kind: "worker".into(), title: String::new(),
            model: "sonnet".into(), provider: None, role_slug: None, engine: None,
        })
        .unwrap()
        .id
    }

    /// The UI's rename: the name changes, nothing else.
    fn rename(c: &Connection, id: &str, name: &str) {
        crate::teams::update_agent(c, id, crate::teams::AgentPatch { name: Some(name.into()), ..Default::default() }).unwrap();
    }

    #[test]
    fn team_slugs_follow_names_and_a_clash_takes_the_next_number() {
        let c = crew(&[("kaan", "Çağrı"), ("ayla", "cagri")]);
        assert_eq!(agent(&c, "timur").unwrap(), "timur");
        assert_eq!(agent(&c, "kaan").unwrap(), "cagri", "the earlier member keeps the bare slug");
        assert_eq!(agent(&c, "ayla").unwrap(), "cagri-2");
        assert_eq!(agent(&c, "Patron Pane").unwrap(), "patron-pane", "no row: the id made safe");
        assert_eq!(memory_dir(&c, Path::new("/m"), "kaan").unwrap(), Path::new("/m").join("agents").join("cagri"));
        // Kept: a later name change by plain SQL moves nothing.
        c.execute("UPDATE agents SET name = 'Zed' WHERE id = 'kaan'", []).unwrap();
        assert_eq!(agent(&c, "ayla").unwrap(), "cagri-2");
    }

    /// Lens (#431): `Pixel-bora` + two `Pixel`s — the suffixed slug is checked again, no two alike.
    #[test]
    fn a_suffixed_slug_never_meets_another_name() {
        let c = crew(&[("kaan", "Pixel-bora"), ("ayla", "Pixel"), ("bora", "Pixel")]);
        let s: Vec<String> = ["kaan", "ayla", "bora"].iter().map(|id| agent(&c, id).unwrap()).collect();
        assert_eq!(s, ["pixel-bora", "pixel", "pixel-2"]);
        // Members created one by one: still all distinct.
        let c = crew(&[]);
        let s: Vec<String> = ["Pixel", "Pixel", "Pixel-2"].iter().map(|n| agent(&c, &add(&c, n)).unwrap()).collect();
        assert_eq!(s, ["pixel", "pixel-2", "pixel-2-2"]);
        // The unique index holds whatever the code does.
        assert!(c.execute("UPDATE agents SET slug = 'pixel' WHERE id = 'bora'", []).is_err());
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-slug-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A team on `cwd`: its memory root is `cwd/.dex/memory`.
    fn on_disk(c: &Connection, cwd: &Path) -> PathBuf {
        let root = cwd.join(".dex").join("memory");
        c.execute("UPDATE teams SET cwd=?1,agent_memory_root=?2,team_memory_root=?2 WHERE id='default'", params![cwd.to_string_lossy(),root.to_string_lossy()]).unwrap();
        root
    }

    #[test]
    fn a_deleted_agents_slug_and_folder_go_to_no_one() {
        let dir = tmp("delete");
        let c = crew(&[("kaan", "Pixel"), ("ayla", "Pixel")]);
        let root = on_disk(&c, &dir);
        assert_eq!((agent(&c, "kaan").unwrap(), agent(&c, "ayla").unwrap()), ("pixel".into(), "pixel-2".into()));
        put(&memory_dir(&c, &root, "kaan").unwrap(), "kaan.md");
        put(&memory_dir(&c, &root, "ayla").unwrap(), "ayla.md");
        crate::teams::delete_agent(&c, "kaan").unwrap();
        // Ayla keeps her own folder; the deleted one's stays unread by her.
        let own = memory_dir(&c, &root, "ayla").unwrap();
        assert!(own.join("ayla.md").is_file() && !own.join("kaan.md").exists());
        // A new `Pixel` skips the folder left on disk (and the held slug).
        let new = crate::teams::create_agent(&c, &dir, crate::teams::NewAgent {
            team_id: "default".into(), name: "Pixel".into(), kind: "worker".into(), title: String::new(),
            model: "sonnet".into(), provider: None, role_slug: None, engine: None,
        })
        .unwrap();
        assert_eq!(agent(&c, &new.id).unwrap(), "pixel-3");
        assert!(!memory_dir(&c, &root, &new.id).unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #463: the slug is given once; a rename — session running or not — moves no folder.
    #[test]
    fn a_rename_keeps_the_slug_and_the_folder() {
        let dir = tmp("rename");
        let c = crew(&[]);
        let root = on_disk(&c, &dir);
        put(&memory_dir(&c, &root, "kaan").unwrap(), "note.md");
        rename(&c, "kaan", "Pixel-1");
        assert_eq!(agent(&c, "kaan").unwrap(), "kaan");
        assert_eq!(memory_dir(&c, &root, "kaan").unwrap(), root.join("agents").join("kaan"));
        assert!(root.join("agents/kaan/note.md").is_file() && !root.join("agents/pixel-1").exists());
        // A later member with that name takes the bare slug; nobody is shifted.
        assert_eq!(agent(&c, &add(&c, "Pixel-1")).unwrap(), "pixel-1");
        assert_eq!(agent(&c, "kaan").unwrap(), "kaan");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn migrated(c: &Connection) -> bool {
        super::migrated(c).unwrap()
    }

    /// #475: a failed move leaves the migration undone; the agent stays on its old folder (no
    /// empty slug folder) and the next open moves it and marks it done.
    #[test]
    fn a_failed_move_is_retried_and_the_agent_keeps_its_old_folder_meanwhile() {
        let dir = tmp("retry");
        let c = crew(&[("timur", "Core"), ("kaan", "Pixel-1")]);
        let root = on_disk(&c, &dir);
        put(&root.join("agents/kaan"), "note.md");
        FAIL_RENAME.set(true);
        assert!(migrate_memory(&c, Path::new("/data")).is_err());
        FAIL_RENAME.set(false);
        assert!(!migrated(&c));
        assert_eq!(memory_dir(&c, &root, "kaan").unwrap(), root.join("agents").join("kaan"));
        assert!(!root.join("agents/pixel-1").exists());
        // No `agents/timur`: nothing pending, the slug folder as usual.
        assert_eq!(memory_dir(&c, &root, "timur").unwrap(), root.join("agents").join("core"));
        // Its session wrote more meanwhile; the next open moves it all.
        put(&root.join("agents/kaan"), "later.md");
        assert!(migrate_memory(&c, Path::new("/data")).unwrap());
        assert!(migrated(&c));
        assert!(root.join("agents/pixel-1/note.md").is_file() && root.join("agents/pixel-1/later.md").is_file());
        assert!(!root.join("agents/kaan").exists());
        assert_eq!(memory_dir(&c, &root, "kaan").unwrap(), root.join("agents").join("pixel-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lens (#481): an agent whose slug is `kaan` in a team on another memory root does not own
    /// `agents/kaan` here — after a failed move Kaan still stays on its old folder, and the retry
    /// moves it (no empty `pixel-1` made meanwhile, so no `TargetExists`).
    #[test]
    fn another_roots_slug_does_not_take_the_old_folder() {
        let dir = tmp("roots");
        let c = crew(&[("kaan", "Pixel-1")]);
        let root = on_disk(&c, &dir.join("a"));
        let other = dir.join("b");
        c.execute("INSERT INTO teams (id, name, kind, cwd, created_at) VALUES ('b', 'B', 'main', ?1, 0)", [other.to_string_lossy()]).unwrap();
        let b = crate::teams::create_agent(&c, Path::new("/data"), crate::teams::NewAgent {
            team_id: "b".into(), name: "Kaan".into(), kind: "worker".into(), title: String::new(),
            model: "sonnet".into(), provider: None, role_slug: None, engine: None,
        })
        .unwrap();
        assert_eq!(agent(&c, &b.id).unwrap(), "kaan");
        put(&root.join("agents/kaan"), "note.md");
        FAIL_RENAME.set(true);
        assert!(migrate_memory(&c, Path::new("/data")).is_err());
        FAIL_RENAME.set(false);
        // Start: the old folder, as the session's memory dir would make it.
        let own = memory_dir(&c, &root, "kaan").unwrap();
        assert_eq!(own, root.join("agents").join("kaan"));
        std::fs::create_dir_all(&own).unwrap();
        assert!(!root.join("agents/pixel-1").exists());
        // The other root's Kaan is on its own folder there.
        let theirs = other.join(".dex").join("memory");
        assert_eq!(memory_dir(&c, &theirs, &b.id).unwrap(), theirs.join("agents").join("kaan"));
        assert!(migrate_memory(&c, Path::new("/data")).unwrap());
        assert!(root.join("agents/pixel-1/note.md").is_file() && !root.join("agents/kaan").exists());
        assert_eq!(memory_dir(&c, &root, "kaan").unwrap(), root.join("agents").join("pixel-1"));
        // On the same root, the holder's claim still stands.
        assert!(on_root("b", "main", Some(&other.to_string_lossy()), &theirs));
        assert!(!on_root("b", "main", Some(&other.to_string_lossy()), &root));
        assert!(on_root("s1", "service", None, &Path::new("/data").join("teams").join("s1").join("memory")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #475: device names get `-agent`, and that candidate is checked for clashes too.
    #[test]
    fn windows_device_names_never_become_slugs() {
        for n in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
            assert!(reserved(n), "{n}");
        }
        for n in ["com", "com0", "com10", "lpt", "console", "con-agent", "con-txt", "nul-2"] {
            assert!(!reserved(n), "{n}");
        }
        let c = crew(&[("kaan", "CON")]);
        assert_eq!(agent(&c, "kaan").unwrap(), "con-agent");
        let s: Vec<String> = ["Con", "con-agent", "AUX", "LPT1", "Com1.txt", "nul"].iter().map(|n| agent(&c, &add(&c, n)).unwrap()).collect();
        assert_eq!(s, ["con-agent-2", "con-agent-3", "aux-agent", "lpt1-agent", "com1-txt", "nul-agent"]);
        // A device-name slug kept before is given anew.
        c.execute("UPDATE agents SET name = 'Ayla', slug = 'prn' WHERE id = 'ayla'", []).unwrap();
        assert_eq!(agent(&c, "ayla").unwrap(), "ayla");
    }

    #[test]
    fn memory_migrates_once_and_a_second_open_moves_nothing() {
        let dir = tmp("migrate");
        let c = crew(&[("timur", "Core"), ("kaan", "Pixel-1")]);
        let root = on_disk(&c, &dir);
        put(&root.join("agents/timur"), "lead.md");
        put(&root.join("agents/kaan"), "kaan.md");
        put(&root.join("agents/ayla"), "ayla.md");
        put(&root.join("agents/pixel-1"), "stray.md");
        assert!(!migrated(&c));
        assert!(migrate_memory(&c, Path::new("/data")).unwrap());
        assert!(root.join("agents/core/lead.md").is_file() && !root.join("agents/timur").exists());
        assert!(root.join("agents/ayla/ayla.md").is_file(), "id = slug: stays");
        // Target taken: nothing merged, both kept (logged).
        assert!(root.join("agents/kaan/kaan.md").is_file() && !root.join("agents/pixel-1/kaan.md").exists());
        assert!(migrated(&c));
        // Second open: done, even with a fresh `agents/<id>` folder on disk.
        put(&root.join("agents/bora"), "x.md");
        c.execute("UPDATE agents SET slug = 'bora-x' WHERE id = 'bora'", []).unwrap();
        assert!(!migrate_memory(&c, Path::new("/data")).unwrap());
        assert!(root.join("agents/bora/x.md").is_file() && !root.join("agents/bora-x").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lens (#458) #1: migrated → worker renamed onto the leader's id name → reopen. The leader's
    /// migration never runs again and the rename moves nothing: each keeps its own folder.
    #[test]
    fn a_rename_after_the_migration_never_hands_a_folder_to_another() {
        let dir = tmp("lens1");
        let c = crew(&[("timur", "Core")]);
        let root = on_disk(&c, &dir);
        c.execute("UPDATE agents SET name = 'Pixel', slug = 'pixel' WHERE id = 'kaan'", []).unwrap();
        put(&root.join("agents/kaan"), "worker.md");
        assert!(migrate_memory(&c, Path::new("/data")).unwrap());
        assert!(root.join("agents/pixel/worker.md").is_file());
        rename(&c, "kaan", "Timur");
        assert!(!migrate_memory(&c, Path::new("/data")).unwrap());
        assert_eq!(memory_dir(&c, &root, "kaan").unwrap(), root.join("agents").join("pixel"));
        assert!(root.join("agents/pixel/worker.md").is_file());
        assert!(!root.join("agents/core").exists() && !root.join("agents/timur").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A source folder named after another member's slug is that member's: not moved.
    #[test]
    fn a_source_that_is_another_members_slug_stays() {
        let dir = tmp("owned");
        let c = crew(&[("timur", "Core"), ("kaan", "Timur")]);
        let root = on_disk(&c, &dir);
        assert_eq!(agent(&c, "kaan").unwrap(), "timur");
        put(&root.join("agents/timur"), "worker.md");
        put(&root.join("agents/kaan"), "old.md");
        migrate_memory(&c, Path::new("/data")).unwrap();
        assert!(root.join("agents/timur/worker.md").is_file() && !root.join("agents/core").exists());
        // Kaan's own `agents/kaan` would go to `timur`: taken — kept, logged.
        assert!(root.join("agents/kaan/old.md").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bulgu 3: `Core::start` moves the folders on the raw connection, before the Core (and so the
    /// plan watch, plan resume and any session start) exists; the next start moves nothing.
    #[test]
    fn core_start_migrates_before_anything_can_start_a_session() {
        let dir = tmp("core");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let root = {
            let c = crate::db::open(&data.join("dex.db")).unwrap();
            c.execute("UPDATE agents SET name = 'Pixel-1', slug = 'pixel-1' WHERE id = 'kaan'", []).unwrap();
            on_disk(&c, &dir.join("proj"))
        };
        put(&root.join("agents/kaan"), "note.md");
        let start = || {
            crate::Core::start(
                crate::Paths { data_dir: data.clone(), config_dir: dir.join("config") },
                crate::Events::new(crate::events::NullSink),
            )
            .unwrap()
        };
        let core = start();
        assert!(root.join("agents/pixel-1/note.md").is_file() && !root.join("agents/kaan").exists());
        assert!(crate::db::with(&core.db, |c| Ok(migrated(c))).unwrap());
        core.shutdown();
        drop(core);
        put(&root.join("agents/kaan"), "later.md");
        let core = start();
        assert!(root.join("agents/kaan/later.md").is_file(), "second start: nothing moved");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn put(dir: &Path, file: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), "x").unwrap();
    }

    #[test]
    fn move_memory_cases() {
        let dir = std::env::temp_dir().join(format!("dex-slug-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.as_path();
        put(&root.join("agents/timur"), "a.md");
        put(&root.join("agents/kaan"), "b.md");
        put(&root.join("agents/pixel-1"), "own.md");
        put(&root.join("agents/pixel-1-old"), "old.md");
        let moves: Vec<(String, String)> = [("timur", "core"), ("kaan", "kaan"), ("ghost", "x"), ("pixel-1-old", "pixel-1")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let done = move_memory(root, &moves);
        // Present: moved with its files.
        assert!(root.join("agents/core/a.md").is_file());
        assert!(!root.join("agents/timur").exists());
        // Same name / missing source: nothing to do, not reported.
        assert!(root.join("agents/kaan/b.md").is_file());
        assert!(!root.join("agents/x").exists());
        // Target taken: no merge, the old folder kept.
        assert!(root.join("agents/pixel-1-old/old.md").is_file());
        assert!(!root.join("agents/pixel-1/old.md").exists());
        assert_eq!(
            done,
            vec![
                ("timur".into(), "core".into(), Ok(Move::Moved)),
                ("pixel-1-old".into(), "pixel-1".into(), Ok(Move::TargetExists)),
            ]
        );
        // Again: idempotent.
        assert!(move_memory(root, &moves).iter().all(|(_, _, r)| r == &Ok(Move::TargetExists)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
