//! Daily cleanup of inactive job directories; never follow directory links.
use crate::db::{with, Db};
use chrono::{DateTime, TimeZone};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Lock order: disk gate, then start/prepare gate; never hold runtime and DB locks together.
pub(crate) static DISK_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn sweep_root(
    db: &Db,
    root: &Path,
    now: SystemTime,
    dry: bool,
    live: &impl Fn() -> HashSet<String>,
) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for mut entry in candidates(root) {
        if entry.reason != Reason::Ready {
            out.push(entry);
            continue;
        }
        // Recheck each candidate after admission is blocked. New jobs cannot start in it.
        let _gate = if dry {
            None
        } else {
            Some(DISK_GATE.write().map_err(|_| "maintenance gate poisoned")?)
        };
        let current_live = live();
        let protected = with(db, |c| protections(c, &current_live))?;
        if let Some(reason) = protected.reason(&entry.path) {
            entry.reason = reason;
        } else {
            entry = inspect(root, &entry.path, now);
            if !dry && entry.reason == Reason::Ready {
                let result = if !safe_candidate(root, &entry.path) {
                    Err("unsafe_path".into())
                } else if entry
                    .path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "wt")
                {
                    crate::worktree::remove_leftover(root, &entry.path)
                } else {
                    std::fs::remove_dir_all(&entry.path).map_err(|e| e.to_string())
                };
                entry.reason = if result.is_ok() {
                    Reason::Removed
                } else {
                    Reason::IoError
                };
                if entry.reason == Reason::Removed {
                    with(db, |c| {
                        let path = entry.path.to_string_lossy();
                        // #2104: a finished job's card waited for this branch; it has no tree now.
                        let mut st = c
                            .prepare("SELECT w.delegation_id FROM delegation_worktrees w JOIN delegations d ON d.id=w.delegation_id
                                      WHERE w.path=?1 AND w.removed_at IS NULL AND d.status='done'")
                            .map_err(|e| e.to_string())?;
                        let rows = st.query_map([&path], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
                        let mut done = Vec::new();
                        for id in rows {
                            done.push(id.map_err(|e| e.to_string())?);
                        }
                        c.execute("UPDATE delegation_worktrees SET removed_at=?2 WHERE path=?1 AND removed_at IS NULL",
                        params![path, crate::diagnostics::now_ms() as i64]).map_err(|e| e.to_string())?;
                        for id in done {
                            crate::delegate::sync_task(c, id)?;
                        }
                        Ok(())
                    })?;
                }
            }
        }
        out.push(entry);
    }
    Ok(out)
}

fn key(p: &Path) -> String {
    crate::projects::path_key(
        &p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy(),
    )
}

#[derive(Default)]
struct Protections {
    jobs: HashSet<i64>,
    agents: HashMap<String, Reason>,
    names: HashMap<String, Reason>,
    paths: Vec<(String, Reason)>,
}

fn protections(c: &Connection, live: &HashSet<String>) -> Result<Protections, String> {
    let mut out = Protections::default();
    let mut st = c
        .prepare("SELECT id,to_agent FROM delegations WHERE status IN ('running','queued')")
        .map_err(|e| e.to_string())?;
    for row in st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
    {
        let (id, agent) = row.map_err(|e| e.to_string())?;
        out.jobs.insert(id);
        out.agents.insert(agent, Reason::ActiveJob);
    }
    let mut st = c
        .prepare("SELECT agent_id FROM tasks WHERE status='doing' AND agent_id IS NOT NULL")
        .map_err(|e| e.to_string())?;
    for row in st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
    {
        out.agents
            .insert(row.map_err(|e| e.to_string())?, Reason::ActiveJob);
    }
    for agent in live {
        out.agents
            .entry(agent.clone())
            .or_insert(Reason::ActiveSession);
    }
    // Keep both stable ids and current slugs; an old branch can use an earlier slug.
    out.names = out.agents.clone();
    let mut st = c
        .prepare("SELECT id,COALESCE(slug,id) FROM agents")
        .map_err(|e| e.to_string())?;
    for row in st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
    {
        let (id, slug) = row.map_err(|e| e.to_string())?;
        if let Some(reason) = out.agents.get(&id) {
            out.names.insert(slug, *reason);
        }
    }
    let mut st = c.prepare("SELECT delegation_id,agent_id,path,branch FROM delegation_worktrees WHERE removed_at IS NULL").map_err(|e| e.to_string())?;
    for row in st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
    {
        let (id, agent, path, branch) = row.map_err(|e| e.to_string())?;
        let reason = if out.jobs.contains(&id) {
            Some(Reason::ActiveJob)
        } else {
            out.agents.get(&agent).copied()
        };
        if let Some(reason) = reason {
            out.paths.push((key(Path::new(&path)), reason));
            if let Some(name) = branch
                .strip_prefix("dex/")
                .and_then(|b| b.split('/').next())
            {
                out.names.insert(name.into(), reason);
            }
        }
    }
    let mut st = c
        .prepare("SELECT agent_id,cwd FROM agent_sessions WHERE cwd IS NOT NULL")
        .map_err(|e| e.to_string())?;
    for row in st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
    {
        let (agent, cwd) = row.map_err(|e| e.to_string())?;
        if let Some(reason) = out.agents.get(&agent) {
            out.paths.push((key(Path::new(&cwd)), *reason));
        }
    }
    Ok(out)
}

impl Protections {
    fn reason(&self, path: &Path) -> Option<Reason> {
        let name = path.file_name()?.to_str()?;
        if path.parent()?.file_name()?.to_str()? == "target" {
            return self.names.get(name).copied();
        }
        if name
            .rsplit_once('-')
            .and_then(|(_, n)| n.parse::<i64>().ok())
            .is_some_and(|id| self.jobs.contains(&id))
        {
            return Some(Reason::ActiveJob);
        }
        let p = key(path);
        if let Some((_, reason)) = self.paths.iter().find(|(cwd, _)| {
            cwd == &p || cwd.starts_with(&format!("{p}/")) || cwd.starts_with(&format!("{p}\\"))
        }) {
            return Some(*reason);
        }
        name.rsplit_once('-')
            .and_then(|(agent, _)| self.names.get(agent).copied())
    }
}

fn candidates(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for bucket in ["wt", "target"] {
        let path = root.join(".dex").join(bucket);
        if crate::worktree::is_link(&root.join(".dex")) || crate::worktree::is_link(&path) {
            out.push(Entry {
                path,
                bytes: 0,
                reason: Reason::UnsafePath,
            });
            continue;
        }
        let dirs = match std::fs::read_dir(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                out.push(Entry {
                    path,
                    bytes: 0,
                    reason: Reason::IoError,
                });
                continue;
            }
        };
        for dir in dirs {
            match dir {
                Ok(d) => match d.file_type() {
                    Ok(t)
                        if t.is_dir() || t.is_symlink() || crate::worktree::is_link(&d.path()) =>
                    {
                        out.push(Entry {
                            path: d.path(),
                            bytes: 0,
                            reason: Reason::Ready,
                        })
                    }
                    Ok(_) => {}
                    Err(_) => out.push(Entry {
                        path: d.path(),
                        bytes: 0,
                        reason: Reason::IoError,
                    }),
                },
                Err(_) => out.push(Entry {
                    path: path.clone(),
                    bytes: 0,
                    reason: Reason::IoError,
                }),
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Read-only: no migration, daily claim, summary write or Git mutation.
pub fn dry_run(
    c: &Connection,
    root: &Path,
    now: SystemTime,
    live: &HashSet<String>,
) -> Result<Vec<Entry>, String> {
    let protected = protections(c, live)?;
    Ok(candidates(root)
        .into_iter()
        .map(|mut e| {
            if e.reason == Reason::Ready {
                if let Some(reason) = protected.reason(&e.path) {
                    e.reason = reason;
                } else {
                    e = inspect(root, &e.path, now);
                }
            }
            e
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Ready,
    Removed,
    Recent,
    ActiveJob,
    ActiveSession,
    UnsafePath,
    IoError,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
    pub reason: Reason,
}

impl Reason {
    fn code(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Removed => "removed",
            Self::Recent => "recent",
            Self::ActiveJob => "active_job",
            Self::ActiveSession => "active_session",
            Self::UnsafePath => "unsafe_path",
            Self::IoError => "io_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub day: String,
    pub started_at_ms: u64,
    pub finished: bool,
    pub deleted: u64,
    pub bytes: u64,
    pub skipped: std::collections::BTreeMap<String, u64>,
}

fn save_summary(c: &Connection, summary: &Summary) -> Result<(), String> {
    c.execute(
        "INSERT INTO settings(key,value) VALUES ('maintenance_summary',?1)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(summary).map_err(|e| e.to_string())?],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub(crate) fn last_summary(c: &Connection) -> Result<Option<Summary>, String> {
    use rusqlite::OptionalExtension;
    c.query_row(
        "SELECT value FROM settings WHERE key='maintenance_summary'",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())?
    .map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    .transpose()
}

fn roots(c: &Connection) -> Result<Vec<PathBuf>, String> {
    let mut st = c
        .prepare(
            "SELECT cwd FROM teams WHERE cwd IS NOT NULL
        UNION SELECT root_path FROM projects WHERE root_path IS NOT NULL
        UNION SELECT root FROM delegation_worktrees WHERE removed_at IS NULL",
        )
        .map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
    {
        let p = PathBuf::from(row.map_err(|e| e.to_string())?);
        if seen.insert(key(&p)) {
            out.push(p);
        }
    }
    Ok(out)
}

fn run_due(
    db: &Db,
    day: &str,
    now: SystemTime,
    live: &impl Fn() -> HashSet<String>,
) -> Result<Option<Summary>, String> {
    let mut summary = Summary {
        day: day.into(),
        started_at_ms: crate::diagnostics::now_ms(),
        finished: false,
        deleted: 0,
        bytes: 0,
        skipped: Default::default(),
    };
    let claimed = with(db, |c| {
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        if !claim_day(&tx, day)? {
            return Ok(false);
        }
        save_summary(&tx, &summary)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    })?;
    if !claimed {
        return Ok(None);
    }
    for root in with(db, roots)? {
        for entry in sweep_root(db, &root, now, false, live)? {
            if entry.reason == Reason::Removed {
                summary.deleted += 1;
                summary.bytes = summary.bytes.saturating_add(entry.bytes);
            } else {
                *summary
                    .skipped
                    .entry(entry.reason.code().into())
                    .or_default() += 1;
            }
        }
    }
    summary.finished = true;
    with(db, |c| save_summary(c, &summary))?;
    crate::diagnostics::event(
        "maintenance",
        serde_json::json!({"deleted":summary.deleted,"bytes":summary.bytes,"skipped":summary.skipped.values().sum::<u64>()}),
    );
    for (reason, count) in &summary.skipped {
        crate::diagnostics::event(
            "maintenance",
            serde_json::json!({"reason":reason,"skipped":count}),
        );
    }
    Ok(Some(summary))
}

impl crate::Core {
    #[cfg(not(test))]
    pub(crate) fn maintenance_watch(&self) {
        let weak = self.weak_self.clone();
        std::thread::spawn(move || {
            let mut checked = None;
            let mut motivated = None;
            loop {
                let now = chrono::Local::now();
                let day = now.date_naive().to_string();
                let Some(core) = weak.upgrade() else { break };
                if core.instance.lock().unwrap().is_none() {
                    break;
                }
                if checked.as_ref() != Some(&day) {
                    let ran = run_due(&core.db, &day, SystemTime::now(), &|| {
                        core.ptys.list().into_iter().map(|p| p.id).collect()
                    });
                    if matches!(ran, Ok(Some(ref s)) if s.deleted > 0) {
                        core.changed("tasks-changed"); // #2104: a removed tree may have closed a card
                    }
                    if ran.is_err() {
                        crate::diagnostics::event(
                            "maintenance",
                            serde_json::json!({"reason":"db_error"}),
                        );
                    }
                    checked = Some(day.clone());
                }
                // #1858: same loop, same per-minute wake; the DB claim keeps it once a day.
                motivation_step(&mut motivated, &day, now.time() >= crate::motivation::due_at(), || {
                    core.motivation_tick(now.naive_local())
                });
                drop(core); // The sleeping watcher does not keep a shutdown Core alive.
                std::thread::sleep(next_wake(chrono::Local::now()));
            }
        });
    }
}

pub(crate) fn safe_candidate(root: &Path, path: &Path) -> bool {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let check = (|| -> std::io::Result<bool> {
        let root = root.canonicalize()?;
        let dex = root.join(".dex");
        let Some(bucket) = path.parent() else {
            return Ok(false);
        };
        if !matches!(
            bucket.file_name().and_then(|n| n.to_str()),
            Some("wt" | "target")
        ) {
            return Ok(false);
        }
        let expected = dex.join(bucket.file_name().unwrap());
        if crate::worktree::is_link(&dex)
            || crate::worktree::is_link(&expected)
            || crate::worktree::is_link(path)
        {
            return Ok(false);
        }
        Ok(dex.canonicalize()? == dex
            && expected.canonicalize()? == expected
            && bucket.canonicalize()? == expected
            && path.canonicalize()?.parent() == Some(expected.as_path())
            && std::fs::symlink_metadata(path)?.is_dir())
    })();
    check.unwrap_or(false)
}

fn inspect(root: &Path, path: &Path, now: SystemTime) -> Entry {
    let mut out = Entry {
        path: path.into(),
        bytes: 0,
        reason: Reason::Ready,
    };
    if !safe_candidate(root, path) {
        out.reason = Reason::UnsafePath;
        return out;
    }
    let result = (|| -> std::io::Result<()> {
        let cutoff = now
            .checked_sub(Duration::from_secs(7200))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut stack = vec![path.to_path_buf()];
        while let Some(p) = stack.pop() {
            let m = std::fs::symlink_metadata(&p)?;
            if m.modified()? >= cutoff || m.created().is_ok_and(|t| t >= cutoff) {
                out.reason = Reason::Recent;
            }
            if crate::worktree::is_link(&p) {
                continue;
            }
            if m.is_dir() {
                for entry in std::fs::read_dir(&p)? {
                    stack.push(entry?.path());
                }
            } else if m.is_file() {
                out.bytes = out.bytes.saturating_add(m.len());
            }
        }
        Ok(())
    })();
    if result.is_err() {
        out.reason = Reason::IoError;
    }
    if out.reason == Reason::Ready
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "wt")
        && crate::worktree::leftover_registered(root, path).is_err()
    {
        out.reason = Reason::IoError;
    }
    out
}

fn claim_day(c: &Connection, day: &str) -> Result<bool, String> {
    c.execute(
        "INSERT INTO settings(key,value) VALUES ('maintenance_day',?1)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE settings.value < excluded.value",
        [serde_json::to_string(day).map_err(|e| e.to_string())?],
    )
    .map(|n| n == 1)
    .map_err(|e| e.to_string())
}

/// #1864: the day is marked only after a tick that did not fail; a transient DB error (e.g.
/// `database is locked`) is retried on the next minute. The DB claim keeps it once a day.
fn motivation_step(motivated: &mut Option<String>, day: &str, due: bool, tick: impl FnOnce() -> Result<bool, String>) {
    if !due || motivated.as_deref() == Some(day) {
        return;
    }
    match tick() {
        Ok(_) => *motivated = Some(day.to_string()),
        Err(_) => crate::diagnostics::event("motivation", serde_json::json!({"reason":"db_error"})),
    }
}

fn next_wake<T: TimeZone>(now: DateTime<T>) -> Duration {
    now.date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|d| now.timezone().from_local_datetime(&d).earliest())
        .and_then(|d| (d - now).to_std().ok())
        .unwrap_or(Duration::from_secs(60))
        .clamp(Duration::from_millis(1), Duration::from_secs(60))
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    fn fixture(name: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "maintenance-{name}-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        std::fs::create_dir_all(&root).unwrap();
        PathBuf::from(
            root.canonicalize()
                .unwrap()
                .to_string_lossy()
                .trim_start_matches(r"\\?\"),
        )
    }

    #[test]
    fn motivation_retries_after_a_db_error_and_still_asks_once() {
        let root = fixture("motivation");
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        let day = "2026-10-15";
        let mut motivated = None;
        let mut calls = 0;
        // Not due yet: no tick.
        motivation_step(&mut motivated, day, false, || { calls += 1; Ok(false) });
        assert_eq!((calls, motivated.as_deref()), (0, None));
        // A transient error leaves the day unmarked.
        motivation_step(&mut motivated, day, true, || { calls += 1; Err("database is locked".into()) });
        assert_eq!((calls, motivated.as_deref()), (1, None));
        // The next minute retries and the real tick asks.
        let mut asked = Vec::new();
        motivation_step(&mut motivated, day, true, || { let r = core.motivation_tick(at("2026-10-15 04:31")); asked.push(r.clone()); r });
        assert_eq!(asked, vec![Ok(true)]);
        assert_eq!(motivated.as_deref(), Some(day));
        // Marked: no more ticks today; a fresh loop (restart) is held off by the DB claim.
        motivation_step(&mut motivated, day, true, || { calls += 1; Ok(true) });
        assert_eq!(calls, 1);
        let mut fresh = None;
        motivation_step(&mut fresh, day, true, || { let r = core.motivation_tick(at("2026-10-15 04:32")); asked.push(r.clone()); r });
        assert_eq!(asked, vec![Ok(true), Ok(false)]);
        let notes: i64 = with(&core.db, |c| c.query_row("SELECT count(*) FROM leader_notes WHERE agent_id='timur' AND text LIKE '[Dex-Motivasyon]%'", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap();
        assert_eq!(notes, 1);
        core.shutdown();
    }

    #[test]
    fn catches_missed_midnight_once_and_survives_reopen() {
        let root = fixture("day");
        let path = root.join("db");
        {
            let c = crate::db::open_bare(&path).unwrap();
            assert!(claim_day(&c, "2026-10-08").unwrap());
            assert!(!claim_day(&c, "2026-10-08").unwrap());
        }
        let c = crate::db::open_bare(&path).unwrap();
        assert!(claim_day(&c, "2026-10-10").unwrap());
        assert!(!claim_day(&c, "2026-10-10").unwrap());
        assert!(!claim_day(&c, "2026-10-09").unwrap());
        let value: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='maintenance_day'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<String>(&value).unwrap(),
            "2026-10-10"
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM settings WHERE key='maintenance_day'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn two_hour_grace_uses_nested_files_and_includes_boundary() {
        let root = fixture("age");
        let dir = root.join(".dex/target/worker");
        let file = dir.join("debug/incremental/deep/object");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "hello").unwrap();
        let now = std::time::SystemTime::now();
        assert_eq!(inspect(&root, &dir, now).reason, Reason::Recent);
        let later = now + Duration::from_secs(10800);
        assert_eq!(
            (
                inspect(&root, &dir, later).reason,
                inspect(&root, &dir, later).bytes
            ),
            (Reason::Ready, 5)
        );
        let set_time = |t| {
            std::fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(t))
                .unwrap()
        };
        set_time(later - Duration::from_secs(7200));
        assert_eq!(inspect(&root, &dir, later).reason, Reason::Recent);
        set_time(later - Duration::from_secs(7201));
        assert_eq!(inspect(&root, &dir, later).reason, Reason::Ready);
    }

    #[test]
    fn rejects_outside_nested_and_linked_roots() {
        let root = fixture("paths");
        let dir = root.join(".dex/target/worker");
        let outside = root.join("outside");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let later = SystemTime::now() + Duration::from_secs(10800);
        for p in [&outside, &root, &dir.join("nested"), &dir.join("../worker")] {
            assert_eq!(
                inspect(&root, p, later).reason,
                Reason::UnsafePath,
                "{}",
                p.display()
            );
        }
        assert_eq!(inspect(&root, &dir, later).reason, Reason::Ready);
        let linked = root.join(".dex/target/link");
        assert!(crate::worktree::link_dir(&outside, &linked));
        assert_eq!(inspect(&root, &linked, later).reason, Reason::UnsafePath);
        let other = fixture("linked-bucket");
        std::fs::create_dir_all(other.join(".dex")).unwrap();
        assert!(crate::worktree::link_dir(
            &root.join(".dex/target"),
            &other.join(".dex/target")
        ));
        assert_eq!(
            inspect(&other, &other.join(".dex/target/worker"), later).reason,
            Reason::UnsafePath
        );
    }

    #[test]
    fn protects_running_queued_jobs_and_live_session_after_report() {
        let root = fixture("active");
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        c.execute("UPDATE agents SET slug='renamed' WHERE id='kaan'", [])
            .unwrap();
        for (id, agent, status) in [
            (71, "kaan", "running"),
            (72, "ayla", "queued"),
            (73, "bora", "done"),
        ] {
            c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(?1,'timur',?2,'default','',?3,1,1)", params![id,agent,status]).unwrap();
        }
        let wt = root.join(".dex/wt/old-name-71");
        c.execute("INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,junction,created_at) VALUES(71,'kaan',?1,?2,'dex/old-name/71','base',0,1)", params![root.to_string_lossy(),wt.to_string_lossy()]).unwrap();
        let session = root.join(".dex/wt/bora-73");
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,cwd) VALUES('bora','s','claude',1,?1)", [session.to_string_lossy()]).unwrap();
        for name in [
            "target/kaan",
            "target/renamed",
            "target/old-name",
            "target/ayla",
            "wt/old-name-71",
            "wt/renamed-71",
            "wt/unknown-72",
            "target/bora",
            "wt/bora-73",
            "target/finished",
        ] {
            std::fs::create_dir_all(root.join(".dex").join(name)).unwrap();
        }
        let live = std::collections::HashSet::from(["bora".to_owned()]);
        let entries = dry_run(
            &c,
            &root,
            SystemTime::now() + Duration::from_secs(10800),
            &live,
        )
        .unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.reason == Reason::ActiveJob)
                .count(),
            7
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.reason == Reason::ActiveSession)
                .count(),
            2
        );
        assert_eq!(
            entries.iter().filter(|e| e.reason == Reason::Ready).count(),
            1
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM settings WHERE key='maintenance_day'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    fn git(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn removes_registered_and_orphan_trees_without_following_junctions() {
        let root = fixture("delete");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "fixture"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(root.join("tracked"), "source").unwrap();
        git(&root, &["add", "tracked"]);
        git(&root, &["commit", "-qm", "fixture"]);
        let registered = root.join(".dex/wt/registered");
        std::fs::create_dir_all(registered.parent().unwrap()).unwrap();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "retained",
                &registered.to_string_lossy(),
            ],
        );
        let orphan = root.join(".dex/wt/orphan");
        let target = root.join(".dex/target/finished");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "sentinel").unwrap();
        for p in [&registered, &orphan, &target] {
            std::fs::create_dir_all(p.join("nested")).unwrap();
            std::fs::write(p.join("cache"), "hello").unwrap();
            assert!(crate::worktree::link_dir(&outside, &p.join("nested/link")));
        }
        let db = crate::db::Db::new(crate::db::open_bare(Path::new(":memory:")).unwrap());
        let later = SystemTime::now() + Duration::from_secs(10800);
        let entries = sweep_root(&db, &root, later, false, &HashSet::new).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.reason == Reason::Removed)
                .count(),
            3,
            "{entries:?}"
        );
        for p in [&registered, &orphan, &target] {
            assert!(!p.exists());
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("keep")).unwrap(),
            "sentinel"
        );
        assert!(root.join("tracked").is_file());
        git(&root, &["rev-parse", "--verify", "retained"]);
    }

    #[test]
    fn unavailable_git_registry_keeps_the_tree() {
        let root = fixture("no-git");
        let path = root.join(".dex/wt/orphan");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("keep"), "sentinel").unwrap();
        let db = Db::new(crate::db::open_bare(Path::new(":memory:")).unwrap());
        let later = SystemTime::now() + Duration::from_secs(10800);
        let entries = with(&db, |c| dry_run(c, &root, later, &HashSet::new())).unwrap();
        assert_eq!(entries[0].reason, Reason::IoError);
        let entries = sweep_root(&db, &root, later, false, &HashSet::new).unwrap();
        assert_eq!(entries[0].reason, Reason::IoError);
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            "sentinel"
        );
    }

    #[cfg(windows)]
    #[test]
    fn daily_run_continues_past_locked_file_and_retries_next_night() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = fixture("locked");
        let locked = root.join(".dex/target/a-locked/cache");
        let free = root.join(".dex/target/z-free/cache");
        for p in [&locked, &free] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "hello").unwrap();
        }
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .unwrap();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        c.execute(
            "UPDATE teams SET cwd=?1 WHERE id='default'",
            [root.to_string_lossy()],
        )
        .unwrap();
        let db = Db::new(c);
        let later = SystemTime::now() + Duration::from_secs(10800);
        let s = run_due(&db, "2026-10-10", later, &HashSet::new)
            .unwrap()
            .unwrap();
        assert!(s.finished);
        assert_eq!(
            (s.deleted, s.bytes, s.skipped.get("io_error")),
            (1, 5, Some(&1))
        );
        assert!(locked.exists() && !free.exists());
        assert!(run_due(&db, "2026-10-10", later, &HashSet::new)
            .unwrap()
            .is_none());
        drop(handle);
        let s = run_due(
            &db,
            "2026-10-11",
            later + Duration::from_secs(86400),
            &HashSet::new,
        )
        .unwrap()
        .unwrap();
        assert_eq!((s.deleted, s.bytes), (1, 5));
        assert!(!locked.exists());
        assert_eq!(with(&db, last_summary).unwrap().unwrap(), s);
    }

    #[test]
    fn timer_wakes_at_local_midnight_and_rechecks_clock_changes() {
        let tz = FixedOffset::east_opt(3 * 3600).unwrap();
        assert_eq!(
            next_wake(tz.with_ymd_and_hms(2026, 10, 10, 23, 59, 59).unwrap()),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            next_wake(tz.with_ymd_and_hms(2026, 10, 10, 23, 0, 0).unwrap()),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            next_wake(tz.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap()),
            std::time::Duration::from_secs(60)
        );
    }
}
