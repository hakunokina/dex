//! Delegations (DEX-4): a leader hands work to a worker of its own team, or to a worker of a
//! service team. Pure functions over `&Connection` + the hook side (Stop / UserPromptSubmit /
//! PreToolUse) + what the host commands call. Every change emits `delegations-changed`.
//!
//! Delivery never types into a terminal:
//! - worker offline / idle → the frontend (re)spawns its PTY with the prompt as claude's positional
//!   argument (`dex-delegate-start`, acknowledged via `delegation_ack`);
//! - worker busy → the row stays `queued` and the worker's next Stop hook answers
//!   `decision: block` with the prompt, so Claude continues with it in the same session.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use crate::db::{with, Db};
use crate::events::Events;
use crate::project_context::{self, ContextMode, ProjectContext, ResolveRequest};

#[path = "report_fs.rs"]
mod report_fs;

pub(crate) const ROLE_QUEUE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS delegation_role_queue (
    delegation_id INTEGER PRIMARY KEY REFERENCES delegations(id) ON DELETE CASCADE,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    role_slug TEXT NOT NULL,
    exclude_agent TEXT
);
CREATE INDEX IF NOT EXISTS delegation_role_queue_fifo ON delegation_role_queue(team_id, role_slug, delegation_id);
CREATE TABLE IF NOT EXISTS plan_targets (
    item_id INTEGER PRIMARY KEY REFERENCES plan_items(id) ON DELETE CASCADE,
    role_slug TEXT,
    agent_id TEXT,
    CHECK ((role_slug IS NULL) <> (agent_id IS NULL))
);";

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Delegation {
    pub id: i64,
    pub from_agent: String,
    pub to_agent: String,
    /// Empty `to_agent`: durable role pool, not a worker's own queue.
    pub target_role: Option<String>,
    pub queue_position: Option<i64>,
    pub team_id: String,
    pub service_team_id: Option<String>,
    pub project_id: Option<String>,
    pub project_root: Option<String>,
    pub merge_branch: Option<String>,
    pub prompt: String,
    pub status: String,
    pub summary: Option<String>,
    pub result_path: Option<String>,
    pub task_id: Option<i64>,
    /// Görev #283: the patron's own words behind the job, quoted verbatim (leader's `patronWords`).
    pub patron_words: Option<String>,
    /// #671: the leader's effort for this job (none: the CLI's own default, as before).
    pub effort: Option<crate::effort::Effort>,
    /// #671: the job went into the patron's own session; its effort was not applied.
    pub effort_skipped: bool,
    pub model: Option<String>,
    pub model_approval: Option<String>,
    pub actual_model: Option<String>,
    pub actual_provider: Option<String>,
    pub actual_engine: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// DEX-19: the leader got the result (Stop block / prompt context / SessionStart context).
    pub delivered_at: Option<i64>,
    /// DEX-19: the leader finished the turn the result arrived in.
    pub read_at: Option<i64>,
    /// DEX-19: the patron opened the worker's pane after it finished.
    pub seen_at: Option<i64>,
}

/// Who is calling, resolved from the DB (never from what the caller claims about itself).
#[derive(Debug, Clone)]
pub struct Caller {
    pub id: String,
    pub name: String,
    pub team_id: String,
    pub kind: String,
}

impl Caller {
    pub fn is_leader(&self) -> bool {
        self.kind == "leader"
    }
}

// `notified` stays the delivery flag (plan cancel sets it directly): such rows, and rows from before
// migration 7, read as delivered and read at `updated_at` so they never light up or get pinged.
const COLS: &str = "id, from_agent, to_agent, team_id, service_team_id, prompt, status, summary, result_path, task_id, created_at, updated_at,
    IFNULL(delivered_at, CASE WHEN notified = 1 THEN updated_at END),
    IFNULL(read_at, CASE WHEN notified = 1 AND delivered_at IS NULL THEN updated_at END),
    seen_at, patron_words, effort, effort_skipped, project_id, project_root, merge_branch,
    (SELECT role_slug FROM delegation_role_queue WHERE delegation_id=delegations.id),
    CASE WHEN to_agent='' AND status='queued' THEN
      (SELECT COUNT(*) FROM delegation_role_queue q JOIN delegations ahead ON ahead.id=q.delegation_id
       WHERE q.team_id=delegations.team_id AND q.role_slug=(SELECT role_slug FROM delegation_role_queue WHERE delegation_id=delegations.id)
         AND ahead.to_agent='' AND ahead.status='queued' AND ahead.id<=delegations.id) END,
    model, model_approval, actual_model, actual_provider, actual_engine";

fn row(r: &Row) -> rusqlite::Result<Delegation> {
    Ok(Delegation {
        id: r.get(0)?,
        from_agent: r.get(1)?,
        to_agent: r.get(2)?,
        target_role: r.get(21)?,
        queue_position: r.get(22)?,
        team_id: r.get(3)?,
        service_team_id: r.get(4)?,
        prompt: r.get(5)?,
        status: r.get(6)?,
        summary: r.get(7)?,
        result_path: r.get(8)?,
        task_id: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        delivered_at: r.get(12)?,
        read_at: r.get(13)?,
        seen_at: r.get(14)?,
        patron_words: r.get(15)?,
        effort: r.get(16)?,
        effort_skipped: r.get(17)?,
        project_id: r.get(18)?,
        project_root: r.get(19)?,
        merge_branch: r.get(20)?,
        model: r.get(23)?,
        model_approval: r.get(24)?,
        actual_model: r.get(25)?,
        actual_provider: r.get(26)?,
        actual_engine: r.get(27)?,
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

pub fn caller(conn: &Connection, agent_id: &str) -> Result<Caller, String> {
    conn.query_row("SELECT id, name, team_id, kind FROM agents WHERE id = ?1", [agent_id], |r| {
        Ok(Caller { id: r.get(0)?, name: r.get(1)?, team_id: r.get(2)?, kind: r.get(3)? })
    })
    .optional()
    .map_err(e)?
    .ok_or_else(|| format!("tanınmayan ajan: {agent_id}"))
}

pub fn get(conn: &Connection, id: i64) -> Result<Delegation, String> {
    conn.query_row(&format!("SELECT {COLS} FROM delegations WHERE id = ?1"), [id], row)
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("devretme yok: {id}"))
}

/// Rows the agent sent or received, plus (for leaders) everything in its team. Active first.
pub fn visible_to(conn: &Connection, c: &Caller, limit: i64) -> Result<Vec<Delegation>, String> {
    let lead = c.is_leader();
    let mut st = conn
        .prepare(&format!(
            "SELECT {COLS} FROM delegations
             WHERE from_agent = ?1 OR to_agent = ?1 OR (?2 AND team_id = ?3)
             ORDER BY status NOT IN ('queued','running'), updated_at DESC LIMIT ?4"
        ))
        .map_err(e)?;
    let rows = st.query_map(params![c.id, lead, c.team_id, limit], row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

/// For the UI: active rows + the latest finished ones, all teams (ids are global).
pub fn list_recent(conn: &Connection) -> Result<Vec<Delegation>, String> {
    let mut st = conn
        .prepare(&format!(
            "SELECT {COLS} FROM delegations
             WHERE status IN ('queued','running') OR updated_at > ?1
             ORDER BY id"
        ))
        .map_err(e)?;
    let day_ago = now() - 24 * 3600 * 1000;
    let rows = st.query_map([day_ago], row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

fn active_count(conn: &Connection, agent: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM delegations WHERE to_agent = ?1 AND status IN ('queued','running')",
        [agent],
        |r| r.get(0),
    )
    .map_err(e)
}

/// A worker of `team_id` by id / name / role (slug, role name or title). Several matches → the
/// one with the fewest active delegations (then team order).
pub fn pick_worker(conn: &Connection, team_id: &str, key: Option<&str>) -> Result<String, String> {
    let mut st = conn
        .prepare(
            "SELECT a.id, a.name, IFNULL(a.role_slug,''), IFNULL(r.name,''), a.title
             FROM agents a LEFT JOIN roles r ON r.slug = a.role_slug
             WHERE a.team_id = ?1 AND a.kind = 'worker' AND IFNULL(a.role_slug,'') <> 'assistant' ORDER BY a.sort, a.id",
        )
        .map_err(e)?;
    let all: Vec<[String; 5]> = st
        .query_map([team_id], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    if all.is_empty() {
        return Err("bu takımda worker yok".into());
    }
    let hits: Vec<&[String; 5]> = match key.map(str::trim).filter(|k| !k.is_empty()) {
        None => all.iter().collect(),
        Some(k) => {
            let k = k.to_lowercase();
            let by_id: Vec<_> = all.iter().filter(|a| a[0].to_lowercase() == k || a[1].to_lowercase() == k).collect();
            if by_id.is_empty() {
                all.iter().filter(|a| a[2..].iter().any(|v| v.to_lowercase() == k)).collect()
            } else {
                by_id
            }
        }
    };
    if hits.is_empty() {
        let names: Vec<String> = all.iter().map(|a| format!("{} ({})", a[0], if a[3].is_empty() { &a[4] } else { &a[3] })).collect();
        let k = key.unwrap_or("");
        let catalog = if crate::roles::in_catalog(conn, k) {
            String::new()
        } else {
            format!(" Geçerli roller: {}. Uygun rol yoksa önce patrona sor (escalate).", crate::roles::catalog_text(conn))
        };
        return Err(format!("eşleşen worker yok: {k}. Takımın worker'ları: {}.{catalog}", names.join(", ")));
    }
    let mut best: Option<(i64, &String)> = None;
    for a in hits {
        let n = active_count(conn, &a[0])?;
        if best.is_none_or(|(b, _)| n < b) {
            best = Some((n, &a[0]));
        }
    }
    Ok(best.unwrap().1.clone())
}

/// Resolve aliases once; the persisted pool uses the worker's catalog role, never its name.
pub(crate) fn role_target(
    conn: &Connection,
    team: &str,
    key: &str,
) -> Result<(String, String), String> {
    let normalized = key.trim().to_lowercase();
    if let Some(role) = crate::roles::list(conn)?
        .into_iter()
        .find(|r| r.slug.to_lowercase() == normalized || r.name.to_lowercase() == normalized)
    {
        if role.slug == "assistant" { return Err("assistant patrona doğrudan cevap verir; devretme hedefi olamaz".into()); }
        let worker: Option<String> = conn.query_row(
            "SELECT id FROM agents WHERE team_id=?1 AND kind='worker' AND role_slug=?2 ORDER BY sort,id LIMIT 1",
            params![team,role.slug],|r|r.get(0),
        ).optional().map_err(e)?;
        return Ok((
            role.slug,
            worker.ok_or_else(|| format!("eşleşen worker yok: {key}"))?,
        ));
    }
    let worker = pick_worker(conn, team, Some(key))?;
    let role: Option<String> = conn
        .query_row("SELECT role_slug FROM agents WHERE id=?1", [&worker], |r| {
            r.get(0)
        })
        .map_err(e)?;
    Ok((
        role.filter(|r| !r.is_empty()).ok_or("worker'ın rolü yok")?,
        worker,
    ))
}

/// Caller owns the insertion transaction. The provisional recipient was already authorized by
/// insert_snapshot; no provisional assignment is visible outside this transaction.
pub(crate) fn queue_role(
    conn: &Connection,
    id: i64,
    role: &str,
    exclude: Option<&str>,
) -> Result<(), String> {
    let d = get(conn, id)?;
    let valid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE id=?1 AND team_id=?2 AND kind='worker' AND role_slug=?3 AND (?4 IS NULL OR id<>?4))",
        params![d.to_agent, d.team_id, role, exclude], |r| r.get(0),
    ).map_err(e)?;
    if d.status != "queued" || d.service_team_id.is_some() || !valid {
        return Err("rol kuyruğu hedefi geçersiz".into());
    }
    conn.execute("INSERT INTO delegation_role_queue(delegation_id,team_id,role_slug,exclude_agent) VALUES(?1,?2,?3,?4)",
        params![id,d.team_id,role,exclude]).map_err(e)?;
    conn.execute("UPDATE delegations SET to_agent='' WHERE id=?1", [id])
        .map_err(e)?;
    if let Some(task) = d.task_id {
        conn.execute("UPDATE tasks SET agent_id=NULL WHERE id=?1", [task])
            .map_err(e)?;
    }
    Ok(())
}

fn claim_role_jobs(
    conn: &Connection,
    only_agent: Option<&str>,
    only_id: Option<i64>,
) -> Result<Vec<Delegation>, String> {
    let mut st = conn.prepare(
        "SELECT q.delegation_id,q.team_id,q.role_slug,q.exclude_agent FROM delegation_role_queue q
         JOIN delegations d ON d.id=q.delegation_id WHERE d.to_agent='' AND d.status='queued'
           AND (d.model IS NULL OR d.model_approval='approved') ORDER BY d.id"
    ).map_err(e)?;
    let queued = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(e)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut blocked = std::collections::HashSet::new();
    let mut assigned = Vec::new();
    for (id, team, role, exclude) in queued {
        if blocked.contains(&(team.clone(), role.clone())) {
            continue;
        }
        if only_id.is_some_and(|wanted| wanted != id) {
            blocked.insert((team, role));
            continue;
        }
        let worker: Option<String> = conn.query_row(
            "SELECT a.id FROM agents a WHERE a.team_id=?1 AND a.kind='worker' AND a.role_slug=?2
               AND (?3 IS NULL OR a.id<>?3) AND (?4 IS NULL OR a.id=?4)
               AND NOT EXISTS(SELECT 1 FROM delegations d WHERE d.to_agent=a.id AND d.status IN ('queued','running'))
             ORDER BY a.sort,a.id LIMIT 1", params![team,role,exclude,only_agent], |r| r.get(0)
        ).optional().map_err(e)?;
        let Some(worker) = worker else {
            blocked.insert((team, role));
            continue;
        };
        conn.execute("UPDATE delegations SET to_agent=?2,updated_at=?3 WHERE id=?1 AND to_agent='' AND status='queued'",params![id,worker,now()]).map_err(e)?;
        let d = get(conn, id)?;
        if let Some(task) = d.task_id {
            conn.execute(
                "UPDATE tasks SET agent_id=?2 WHERE id=?1",
                params![task, worker],
            )
            .map_err(e)?;
        }
        if only_id.is_none() {
            crate::plan::note(
                conn,
                &d.from_agent,
                &format!("Devretme #{id} rol kuyruğundan {worker} worker'ına atandı ({role})."),
            )?;
        }
        assigned.push(d);
    }
    Ok(assigned)
}

/// DB mutex + transaction serialize every claim with enqueue/report/session exit.
pub(crate) fn assign_role_jobs(conn: &Connection) -> Result<Vec<Delegation>, String> {
    if !conn.is_autocommit() {
        return claim_role_jobs(conn, None, None);
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    let assigned = claim_role_jobs(&tx, None, None)?;
    tx.commit().map_err(e)?;
    Ok(assigned)
}

/// Immediate admission of this newly inserted row; older pool rows remain event-dispatched.
pub(crate) fn assign_role_job(conn: &Connection, id: i64) -> Result<Delegation, String> {
    claim_role_jobs(conn, None, Some(id))?;
    get(conn, id)
}

/// Bound queue always precedes the pool, regardless of the pool row's age.
pub(crate) fn next_for_worker(
    conn: &Connection,
    agent: &str,
) -> Result<Option<Delegation>, String> {
    if agent.is_empty() {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    let running: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM delegations WHERE to_agent=?1 AND status='running')",
            [agent],
            |r| r.get(0),
        )
        .map_err(e)?;
    if running {
        return Ok(None);
    }
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM delegations WHERE to_agent=?1 AND status='queued' AND (model IS NULL OR model_approval='approved') ORDER BY id LIMIT 1",
            [agent],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?;
    let next = match id {
        Some(id) => Some(get(&tx, id)?),
        None => claim_role_jobs(&tx, Some(agent), None)?.into_iter().next(),
    };
    tx.commit().map_err(e)?;
    Ok(next)
}

pub struct NewDelegation<'a> {
    pub from: &'a Caller,
    pub to_agent: String,
    pub team_id: String,
    pub service_team_id: Option<String>,
    pub prompt: String,
    pub task_id: Option<i64>,
    pub patron_words: Option<String>,
    pub effort: Option<crate::effort::Effort>,
}

/// Internal persisted selection; never deserialize a caller-supplied root as authority.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub project_id: Option<String>,
    pub project_root: Option<String>,
    pub merge_branch: Option<String>,
    pub repo_key: Option<String>,
}

impl From<&ProjectContext> for ProjectSnapshot {
    fn from(c: &ProjectContext) -> Self {
        Self {
            project_id: c.project_id.clone(),
            project_root: c.project_root.as_ref().map(|p| p.to_string_lossy().into_owned()),
            merge_branch: c.merge_branch.clone(),
            repo_key: c.repo_key.clone(),
        }
    }
}

pub(crate) fn resolve_context(
    conn: &Connection,
    data_dir: &Path,
    from: &Caller,
    project: Option<&str>,
    fix_of: Option<i64>,
) -> Result<ProjectContext, String> {
    let from = caller(conn, &from.id)?;
    if !from.is_leader() {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    project_context::resolve(
        conn,
        data_dir,
        ResolveRequest {
            team_id: &from.team_id,
            agent_id: Some(&from.id),
            delegation_id: fix_of,
            project_id: project,
            mode: ContextMode::Selection,
        },
    )
    .map_err(|e| e.to_string())
}

/// Check that the DB binding did not change while filesystem/Git validation ran.
/// Called inside the write transaction. Fix jobs keep their original root/branch.
pub(crate) fn revalidate_context(
    conn: &Connection,
    from: &Caller,
    context: &ProjectContext,
    ready: Option<&crate::projects::PreparedRoot>,
) -> Result<ProjectContext, String> {
    let current = resolve_context(conn, Path::new(""), from, context.project_id.as_deref(), context.delegation_id)?;
    if ProjectSnapshot::from(&current) != ProjectSnapshot::from(context) {
        return Err(crate::projects::ProjectError::Mismatch.to_string());
    }
    verify_context_metadata(conn, from, context, ready)
}

pub(crate) fn verify_context_metadata(
    conn: &Connection,
    from: &Caller,
    context: &ProjectContext,
    ready: Option<&crate::projects::PreparedRoot>,
) -> Result<ProjectContext, String> {
    let mut context = context.clone();
    if let (Some(id), Some(ready)) = (&context.project_id, ready) {
        let project = crate::projects::verify(conn, &from.team_id, id, ready).map_err(|e| e.to_string())?;
        if context.delegation_id.is_none() {
            context.merge_branch = project.default_branch;
            context.repo_key = project.repo_key;
        }
    }
    Ok(context)
}

pub fn insert(conn: &Connection, d: NewDelegation) -> Result<Delegation, String> {
    let context = resolve_context(conn, Path::new(""), d.from, None, None)?;
    insert_bound(conn, d, &context)
}

/// DB-only insertion with a trusted, already resolved/validated binding. Callers own the
/// transaction and start/mutation gate; no request path or UI default supplies authority.
pub fn insert_bound(conn: &Connection, d: NewDelegation, context: &ProjectContext) -> Result<Delegation, String> {
    if context.team_id != d.team_id || context.project_team_id.as_deref().is_some_and(|t| t != d.team_id) {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    insert_snapshot(conn, d, &ProjectSnapshot::from(context))
}

pub(crate) fn insert_snapshot(conn: &Connection, d: NewDelegation, snapshot: &ProjectSnapshot) -> Result<Delegation, String> {
    let from = caller(conn, &d.from.id)?;
    let to = caller(conn, &d.to_agent)?;
    if crate::teams::get_agent(conn, &d.to_agent)?.role_slug.as_deref() == Some("assistant") {
        return Err("assistant patrona doğrudan cevap verir; devretme hedefi olamaz".into());
    }
    if !from.is_leader() || from.team_id != d.team_id || to.team_id != d.service_team_id.as_deref().unwrap_or(&d.team_id) {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    let ts = now();
    conn.execute(
        "INSERT INTO delegations (from_agent, to_agent, team_id, service_team_id, prompt, status, task_id, created_at, updated_at, patron_words, effort, project_id, project_root, merge_branch)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![from.id, d.to_agent, d.team_id, d.service_team_id, d.prompt, d.task_id, ts, patron_words(d.patron_words.as_deref()), d.effort,
            snapshot.project_id, snapshot.project_root, snapshot.merge_branch],
    )
    .map_err(e)?;
    crate::diagnostics::event("delegation", serde_json::json!({"id": conn.last_insert_rowid(), "agent": d.to_agent, "status": "queued", "bytes": d.prompt.len()}));
    get(conn, conn.last_insert_rowid())
}

/// #671: the effort of the worker's running job — a respawn mid-job (provider chain, quota route
/// back, a lost resume) keeps it. None: no running job, or one without effort.
pub fn running_effort(conn: &Connection, agent: &str) -> Result<Option<crate::effort::Effort>, String> {
    conn.query_row(
        "SELECT effort FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
        [agent],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(e)
}

/// #671: the job went into the patron's own session (no respawn, no `/effort`).
pub fn effort_skipped(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("UPDATE delegations SET effort_skipped = 1 WHERE id = ?1 AND effort IS NOT NULL", [id])
        .map(|_| ())
        .map_err(e)
}

pub fn set_status(conn: &Connection, id: i64, status: &str) -> Result<(), String> {
    conn.execute("UPDATE delegations SET status = ?2, updated_at = ?3 WHERE id = ?1", params![id, status, now()])
        .map_err(e)?;
    crate::diagnostics::event("delegation", serde_json::json!({"id": id, "status": status}));
    Ok(())
}

pub fn report(conn: &Connection, c: &Caller, id: i64, summary: &str, result_path: Option<&str>) -> Result<Delegation, String> {
    report_with_content(conn, c, id, summary, result_path, None)
}

pub(crate) const REPORT_CONTENT_LIMIT: usize = 1024 * 1024;

#[cfg(all(test, windows))]
thread_local! {
    static REPORT_IO_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&str, &Path)>>> = std::cell::RefCell::new(None);
}

#[cfg(all(test, windows))]
pub(crate) fn with_report_io_hook<T>(hook: impl FnMut(&str, &Path) + 'static, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { REPORT_IO_HOOK.with(|h| { h.borrow_mut().take(); }); }
    }
    REPORT_IO_HOOK.with(|h| { assert!(h.borrow_mut().replace(Box::new(hook)).is_none()); });
    let _reset = Reset;
    run()
}

#[cfg(all(test, windows))]
fn report_io_hook(stage: &str, path: &Path) {
    REPORT_IO_HOOK.with(|h| { if let Some(hook) = h.borrow_mut().as_mut() { hook(stage, path); } });
}

pub(crate) fn report_with_content(conn: &Connection, c: &Caller, id: i64, summary: &str, result_path: Option<&str>, content: Option<&str>) -> Result<Delegation, String> {
    let d = get(conn, id)?;
    if d.to_agent != c.id {
        return Err(format!("#{id} sana devredilmedi (alıcı: {})", d.to_agent));
    }
    if !matches!(d.status.as_str(), "running" | "queued" | "done_no_report") {
        return Err(format!("#{id} zaten kapandı ({})", d.status));
    }
    if let Some(question) = crate::questions::unanswered(conn, &c.id, id)? {
        return Err(format!("Açık sorun var (#{question}); cevabı bekle, işe devam et ve bitince report ver. Devretme #{id} açık kaldı."));
    }
    let summary = summary.trim();
    if summary.is_empty() {
        return Err("summary boş olamaz".into());
    }
    if content.is_some() && result_path.is_some() {
        return Err("content ve resultPath birlikte verilemez; content yolunu Dex belirler".into());
    }
    let owned_path = if let Some(body) = content {
        if body.is_empty() || body.len() > REPORT_CONTENT_LIMIT {
            return Err(format!("content 1..{REPORT_CONTENT_LIMIT} UTF-8 bayt olmalı"));
        }
        if crate::review::is_review(conn, id)? && crate::review::parse(body).is_none() {
            return Err("content inceleme raporunun ilk satırı `karar: onay` ya da `karar: düzelt` olmalı".into());
        }
        Some(write_report_content(&d, body)?)
    } else { None };
    let result_path = owned_path.as_deref().or_else(|| result_path.map(str::trim).filter(|p| !p.is_empty()));
    // A team without a project folder has nowhere to keep the file: summary only, as before.
    let legacy_dir = if content.is_none() { result_dir(conn, &d)? } else { None };
    if let Some(dir) = legacy_dir {
        let want = dir.join(result_name(&d));
        let p = result_path
            .ok_or_else(|| format!("resultPath gerekli: raporun tamamını önce {} dosyasına yaz, sonra report'ta o yolu ver", want.display()))?;
        if !path_within(Path::new(p), &dir) {
            let whose = if d.service_team_id.is_some() { "alt takım sonucu" } else { "resultPath" };
            return Err(format!("{whose} {} altına yazılmalı (yazılacak dosya: {})", dir.display(), want.display()));
        }
        if !Path::new(p).is_file() {
            return Err(format!("resultPath dosyası yok: {p}; raporu önce bu dosyaya yaz"));
        }
    }
    // Görev #286: a review's report carries its verdict.
    #[cfg(all(test, windows))]
    if content.is_some() { report_io_hook("archived", Path::new(result_path.unwrap())); }
    if content.is_some() {
        crate::review::record_with_content(conn, &d, summary, result_path, content)?;
    } else {
        crate::review::record(conn, &d, summary, result_path)?;
    }
    conn.execute(
        "UPDATE delegations SET status = 'done', summary = ?2, result_path = ?3, notified = 0,
             delivered_at = NULL, read_at = NULL, seen_at = NULL, updated_at = ?4 WHERE id = ?1",
        params![id, summary, result_path, now()],
    )
    .map_err(e)?;
    crate::diagnostics::event("delegation", serde_json::json!({"id": id, "agent": c.id, "status": "done", "bytes": summary.len()}));
    get(conn, id)
}

/// The host chooses the archive path from the job snapshot, never the MCP caller.
/// Create a private sibling exclusively, then replace the report without following a file link.
fn write_report_content(d: &Delegation, content: &str) -> Result<String, String> {
    let stored = d.project_root.as_deref().ok_or("content için işin kayıtlı proje kökü gerekli")?;
    let stored = Path::new(stored);
    if !stored.is_absolute() || stored.components().any(|c| c == std::path::Component::ParentDir) {
        return Err("rapor dosyası için güvenli proje kökü gerekli".into());
    }
    let dirs = if d.service_team_id.is_some() { ["docs", "research"] } else { [".dex", "results"] };
    report_fs::write(stored, dirs, &result_name(d), content)?;
    Ok(stored.join(dirs[0]).join(dirs[1]).join(result_name(d)).to_string_lossy().into_owned())
}

/// Finished rows the leader has not been told about yet; marks them told (`delivered_at`) and
/// returns them as they are now. Callers hold the db lock, so a row is handed out exactly once.
pub fn take_unnotified(conn: &Connection, leader: &str) -> Result<Vec<Delegation>, String> {
    pending_results(conn, leader, None)
}

fn pending_results(conn: &Connection, leader: &str, pending: Option<&crate::hook_delivery::Pending>) -> Result<Vec<Delegation>, String> {
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM delegations
             WHERE from_agent = ?1 AND notified = 0 AND status IN ('done','done_no_report','failed') ORDER BY id",
        )
        .map_err(e)?
        .query_map([leader], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    let ts = now();
    ids.into_iter()
        .map(|id| {
            if let Some(pending) = pending {
                let row = get(conn, id)?;
                pending.0.lock().unwrap().results.push((id, row.updated_at));
                return Ok(row);
            } else {
                conn.execute("UPDATE delegations SET notified = 1, delivered_at = ?2, read_at = NULL WHERE id = ?1", params![id, ts]).map_err(e)?;
            }
            get(conn, id)
        })
        .collect()
}

/// The leader's turn ended: whatever reached it before this Stop has been taken in.
fn mark_read(conn: &Connection, leader: &str) -> Result<Vec<Delegation>, String> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM delegations WHERE from_agent = ?1 AND delivered_at IS NOT NULL AND read_at IS NULL ORDER BY id")
        .map_err(e)?
        .query_map([leader], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    let ts = now();
    ids.into_iter()
        .map(|id| {
            conn.execute("UPDATE delegations SET read_at = ?2 WHERE id = ?1", params![id, ts]).map_err(e)?;
            get(conn, id)
        })
        .collect()
}

/// Özetin ilk satırı, tek-satır başlıklar için (120 karakterde kesik).
pub(crate) fn first_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    let mut out: String = first.chars().take(120).collect();
    if first.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Result lines the leader reads (prompt context, Stop-block reason, SessionStart context).
fn results_text(conn: &Connection, header: &str, done: &[Delegation]) -> String {
    let mut ctx = format!("[{}] {header}:\n", crate::dexmsg::DEVRETME);
    for d in done {
        let who = caller(conn, &d.to_agent).map(|c| c.name).unwrap_or_else(|_| d.to_agent.clone());
        let status = match d.status.as_str() {
            "done" => "bitti",
            "done_no_report" => "RAPORSUZ bitti (worker report çağırmadan durdu; pane'ine bak)",
            _ => "başarısız",
        };
        let mut head = format!("- [{}] {who} #{} · {}", crate::dexmsg::DEVRETME, d.id, status);
        if let Some(first) = d.summary.as_deref().map(first_line).filter(|s| !s.is_empty()) {
            head.push_str(&format!(" — {first}"));
        }
        ctx.push_str(&head);
        ctx.push('\n');
        if let Some(lvl) = d.effort {
            ctx.push_str(&format!(" · effort {}", lvl.as_str()));
            if d.effort_skipped {
                ctx.push_str(" — effort uygulanmadı (patron oturumu)");
            }
        }
        if let Some(s) = &d.summary {
            // İlki başlıkta; yalnız devam satırları ayrıntıda (tekrar yok).
            let rest: Vec<&str> = s.lines().skip(1).collect();
            if !rest.iter().all(|l| l.trim().is_empty()) {
                ctx.push_str(&format!("\n  özet (devamı): {}", rest.join("\n  ")));
            }
        }
        if let Some(w) = crate::worktree::get(conn, d.id).ok().flatten().filter(|w| w.merged_at.is_none()) {
            ctx.push_str(&format!("\n  {}", crate::worktree::result_line(&w)));
        }
        if let Some(r) = crate::review::result_line(conn, d) {
            ctx.push_str(&format!("\n  {r}"));
        }
        if let Some(p) = &d.result_path {
            ctx.push_str(&format!("\n  rapor: {p} — oku"));
        } else if let Some(f) = result_file(conn, d).ok().flatten().filter(|f| f.is_file()) {
            // Written before the session ended without `report`: the file may still hold the work.
            ctx.push_str(&format!("\n  rapor (report'suz kaydedilmiş): {} — oku", f.display()));
        }
        ctx.push('\n');
    }
    ctx
}

fn project_dir(conn: &Connection, team_id: &str) -> Result<Option<PathBuf>, String> {
    let cwd: Option<String> = conn
        .query_row("SELECT cwd FROM teams WHERE id = ?1", [team_id], |r| r.get(0))
        .optional()
        .map_err(e)?
        .flatten();
    Ok(cwd.map(PathBuf::from))
}

/// `<cwd>/docs/research` of a main team (service results land there).
pub fn research_dir(conn: &Connection, team_id: &str) -> Result<PathBuf, String> {
    let cwd = project_dir(conn, team_id)?.ok_or("isteyen takımın proje klasörü yok (Takım → Değiştir…)")?;
    Ok(cwd.join("docs").join("research"))
}

/// Where the worker's report file must lie: the requester's `docs/research` for a service job,
/// else `<cwd>/.dex/results` (beside `.dex/memory`). `None`: the team has no project folder.
pub fn result_dir(conn: &Connection, d: &Delegation) -> Result<Option<PathBuf>, String> {
    // NULL-only historical jobs retain the old administrative compatibility path.
    let root = if d.project_id.is_some() { d.project_root.as_ref().map(PathBuf::from) } else { project_dir(conn, &d.team_id)? };
    Ok(root.map(|root| if d.service_team_id.is_some() { root.join("docs").join("research") } else { root.join(".dex").join("results") }))
}

/// The file the worker is told to write inside [`result_dir`].
fn result_name(d: &Delegation) -> String {
    match d.service_team_id {
        Some(_) => format!("{}-{}.md", d.id, slug(&d.prompt)),
        None => format!("{}.md", d.id),
    }
}

pub(crate) fn result_file(conn: &Connection, d: &Delegation) -> Result<Option<PathBuf>, String> {
    Ok(result_dir(conn, d)?.map(|dir| dir.join(result_name(d))))
}

/// Lexical containment, case-insensitive on Windows, `/` and `\` equal. `..` is rejected.
pub fn path_within(p: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (p, root) = (norm(p), norm(root));
    !p.split('/').any(|seg| seg == "..") && (p == root || p.starts_with(&format!("{root}/")))
}

fn slug(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ç' => 'c', 'ğ' => 'g', 'ı' => 'i', 'ö' => 'o', 'ş' => 's', 'ü' => 'u',
            c if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    out.truncate(40);
    if out.is_empty() { "arastirma".into() } else { out }
}

/// Görev #283: `patronWords` trimmed; blank is none.
pub fn patron_words(w: Option<&str>) -> Option<String> {
    w.map(str::trim).filter(|w| !w.is_empty()).map(String::from)
}

pub const PATRON_RULE: &str =
    "Patronun sözü liderin açıklamasından üstündür. İkisi çelişirse ya da arada kalırsan varsayım yapma, ask_leader ile sor.";

/// Görev #283: the patron's verbatim words (heading only when there are any), then the leader's text.
pub fn patron_section(patron: Option<&str>) -> String {
    match patron_words(patron) {
        Some(w) => format!("## Patronun sözü\n{w}\n\n"),
        None => String::new(),
    }
}

/// The job body the worker reads: patron's words → leader's explanation → the precedence rule.
pub fn job_text(patron: Option<&str>, prompt: &str) -> String {
    format!("{}## Liderin açıklaması\n{}\n\n{PATRON_RULE}", patron_section(patron), prompt.trim_end())
}

/// What the worker actually receives (positional prompt or Stop-block reason).
pub fn worker_prompt(conn: &Connection, d: &Delegation) -> Result<String, String> {
    if d.to_agent.is_empty() { return Err("iş henüz worker'a atanmadı".into()); }
    let from = caller(conn, &d.from_agent).map(|c| c.name).unwrap_or_else(|_| d.from_agent.clone());
    let task = match d.task_id {
        Some(t) => format!(" (panodaki görev #{t}; durumunu task_update ile sen güncelle)"),
        None => String::new(),
    };
    let mut s = format!("[Dex-Devretme] {from} #{} · sana iş verdi{task}:\n---\n{}\n---\n", d.id, job_text(d.patron_words.as_deref(), &d.prompt));
    if d.service_team_id.is_some() {
        let research = result_dir(conn, d)?.ok_or_else(|| crate::projects::ProjectError::Unavailable.to_string())?;
        let project = research.parent().and_then(Path::parent).unwrap_or(&research);
        s.push_str(&format!(
            "Bu bir alt takım hizmet isteği. İsteyen takımın proje klasörü: {}\n\
             - Proje klasöründe yalnız aşağıdaki sonuç dosyasını yaz; kalanını yalnız oku.\n\
             - İsteyen takımın hafızasına ({}) YAZMA; kendi hafızan kendi takımında.\n",
            project.display(),
            project.join(".dex").join("memory").display()
        ));
    }
    if let Some(w) = crate::worktree::get(conn, d.id)?.filter(|w| w.removed_at.is_none()) {
        s.push_str(&crate::worktree::prompt_line(&w));
    }
    match result_file(conn, d)? {
        Some(file) => s.push_str(&format!(
            "Teslim: raporun TAMAMINI dex MCP'sinin `report` aracında content ile ver: delegationId={}, content=<tam rapor>, summary=\"bitti: <tek cümle>\". \
             Dosyayı Dex yazar (arşiv: {}); rapor için dosya yazman gerekmez. content ile resultPath birlikte verilmez. \
             Lider summary'yi ve raporu görür; sohbet mesajın ona gitmez. Rapor vermeden durursan iş \"raporsuz bitti\" sayılır.",
            d.id,
            file.display()
        )),
        None => s.push_str(&format!(
            "Bitince dex MCP'sinin `report` aracıyla teslim et: delegationId={}, summary=<teslimin tamamı; sohbet mesajın lidere gitmez>. \
             Rapor vermeden durursan iş \"raporsuz bitti\" sayılır.",
            d.id
        )),
    }
    let notes = crate::worker_notes::for_job(conn, d.id)?;
    if !notes.is_empty() { s.push_str(&format!("\n\n{}", crate::worker_notes::text(&notes))); }
    Ok(s)
}

// ---- Hook side (called from hooks::serve; returns a JSON body to answer with) ----

pub(crate) fn emit_changed(ev: &Events, d: Option<&Delegation>) {
    if let Some(d) = d { crate::diagnostics::event("delegation", serde_json::json!({"id": d.id, "agent": d.to_agent, "status": d.status})); }
    let _ = ev.emit("delegations-changed", &d);
}

// ---- Görev #19: whose turn it is, the one-time Stop refusal, the job trigger ----

/// Per worker: is its current turn Dex's (a `[Dex]` prompt or a Stop-block delivery) or the
/// patron's; and the jobs that already had their one unreported-Stop refusal. In memory: a
/// restart closes every running job anyway (`reconcile_on_start`).
#[derive(Default)]
pub struct Turns(std::sync::Mutex<(std::collections::HashMap<String, bool>, std::collections::HashSet<i64>)>);

impl Turns {
    fn set(&self, agent: &str, dex: bool) {
        self.0.lock().unwrap().0.insert(agent.into(), dex);
    }

    /// Observe ownership only: unlike `on_prompt`, it never consumes pending results/questions.
    pub(crate) fn prompt(&self, agent: &str, prompt: &str) {
        if prompt.trim_start().starts_with("<task-notification>") {
            return;
        }
        self.set(agent, crate::dexmsg::is_dex_turn(prompt));
    }

    fn is_dex(&self, agent: &str) -> bool {
        self.dex_turn(agent).unwrap_or(false)
    }

    /// Görev #20: `Some(false)` — the patron is talking in this session; `None` — no turn yet.
    pub(crate) fn dex_turn(&self, agent: &str) -> Option<bool> {
        self.0.lock().unwrap().0.get(agent).copied()
    }

    /// `true` once per job.
    fn first_refusal(&self, id: i64) -> bool {
        self.0.lock().unwrap().1.insert(id)
    }
}

/// The only text Dex types into a claude pane for a job; the job itself comes as that prompt's
/// UserPromptSubmit context (system channel: no "did the user write this?" doubt, no long paste).
pub(crate) fn trigger_line(id: i64, worker: &str) -> String {
    format!(
        "[Dex-Devretme] {worker} #{id} · başladı — ayrıntı bu mesajın ekinde (görmüyorsan delegation_status ile oku)."
    )
}

fn trigger_id(prompt: &str) -> Option<i64> {
    let p = prompt.trim_start();
    if let Some(rest) = p.strip_prefix("[Dex-Devretme]") {
        // Yalnız pano tetikleyicisi: `[Dex-Devretme] Forge-1 #1745 · başladı …`.
        // İş gövdesi (`· sana iş verdi:`) ve sonuç satırları (`· bitti/…`) eşleşmez.
        let hash = rest.find(" #")?;
        let digits: String = rest[hash + 2..].chars().take_while(char::is_ascii_digit).collect();
        let tail = &rest[hash + 2 + digits.len()..];
        if !tail.starts_with(" · başladı") {
            return None;
        }
        digits.parse().ok()
    } else {
        // Eski biçim (`[Dex] yeni iş #N — …`): yükseltme sırasında uçuşan tetikler için.
        let rest = p.strip_prefix("[Dex] yeni iş #")?;
        rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
    }
}

/// Without turn state: every turn counts as the patron's, so no Stop is refused.
pub fn on_hook(db: &Db, ev: &Events, agent: &str, event: &str, body: &Value) -> Option<String> {
    on_hook_in(db, ev, &Turns::default(), agent, event, body)
}

pub fn on_hook_in(db: &Db, ev: &Events, turns: &Turns, agent: &str, event: &str, body: &Value) -> Option<String> {
    on_hook_pending(db, ev, turns, agent, event, body, None)
}

pub(crate) fn on_hook_pending(db: &Db, ev: &Events, turns: &Turns, agent: &str, event: &str, body: &Value, pending: Option<&crate::hook_delivery::Pending>) -> Option<String> {
    if !matches!(event, "Stop" | "StopFailure" | "UserPromptSubmit" | "PreToolUse") {
        return None;
    }
    let conn = db.0.lock().ok()?;
    let transcript = body.get("transcript_path").and_then(Value::as_str);
    let r = match event {
        "Stop" => on_stop_pending(&conn, turns, agent, false, transcript, pending),
        "StopFailure" => on_stop(&conn, turns, agent, true, transcript)
            .and_then(|(out, changed)| Ok((out, failure_reason(&conn, changed, body)?))),
        "UserPromptSubmit" => on_prompt(&conn, turns, agent, body),
        _ => on_pre_tool(&conn, agent, body),
    };
    drop(conn);
    match r {
        Ok((out, changed)) => {
            for d in &changed {
                emit_changed(ev, Some(d));
            }
            out.map(|v| v.to_string())
        }
        Err(err) => {
            eprintln!("dex delegate hook {event} {agent}: {err}");
            None
        }
    }
}

type HookOut = (Option<Value>, Vec<Delegation>);

/// Running rows the worker never reported → done_no_report (failed on StopFailure); then the
/// oldest queued one is delivered by blocking the stop.
/// Görev #19: in a Dex turn the first unreported Stop of a job is refused once — the worker
/// reports or asks its leader instead of asking the patron. A patron turn is never refused.
fn on_stop(conn: &Connection, turns: &Turns, agent: &str, failure: bool, transcript: Option<&str>) -> Result<HookOut, String> {
    on_stop_pending(conn, turns, agent, failure, transcript, None)
}

fn on_stop_pending(conn: &Connection, turns: &Turns, agent: &str, failure: bool, transcript: Option<&str>, pending: Option<&crate::hook_delivery::Pending>) -> Result<HookOut, String> {
    let mut changed = Vec::new();
    let running: Vec<i64> = conn
        .prepare("SELECT id FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id")
        .map_err(e)?
        .query_map([agent], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    // Görev #40: a background task is still open → the worker is not done; claude wakes it when the
    // task ends. The job stays running (no refusal spent, no done_no_report), and the open row also
    // keeps the #20 reset away (`has_other_work` queues any newer job).
    if !failure && awaiting_background(conn, &running, transcript) {
        return Ok((None, changed));
    }
    if !failure && turns.is_dex(agent) {
        if let Some(id) = running.iter().copied().find(|id| turns.first_refusal(*id)) {
            let reason = format!(
                "Açık işin var (#{id}). Ya report ile teslim et ya da ask_leader ile sor; patrona soru sorma."
            );
            return Ok((Some(json!({ "decision": "block", "reason": reason })), changed));
        }
    }
    // Nothing is lost: the leader reads what the worker said last.
    let last = transcript
        .filter(|_| !failure && !running.is_empty())
        .and_then(|p| crate::chat::last_assistant_text(Path::new(p), 1_500));
    for id in running {
        set_status(conn, id, if failure { "failed" } else { "done_no_report" })?;
        if let Some(m) = &last {
            conn.execute(
                "UPDATE delegations SET summary = IFNULL(summary, ?2) WHERE id = ?1",
                params![id, format!("worker'ın son mesajı: {m}")],
            )
            .map_err(e)?;
        }
        changed.push(get(conn, id)?);
    }
    if failure {
        return Ok((None, changed));
    }
    // Görev #20: a queued job is not continued in this session — each job gets a fresh one. The
    // turn ends; the post-Stop delivery (lib.rs) starts the next job after the quiet window.
    // Leader side (DEX-19): the turn that carried earlier results is over → read; results that
    // landed while it worked are handed over by continuing the same turn.
    changed.extend(mark_read(conn, agent)?);
    let done = pending_results(conn, agent, pending)?;
    if done.is_empty() {
        return Ok((None, changed));
    }
    let reason = results_text(conn, "sen çalışırken geldi; değerlendir, gerekirse sıradaki adımı at", &done);
    changed.extend(done);
    Ok((Some(json!({ "decision": "block", "reason": reason })), changed))
}

/// #316: StopFailure = the provider ended the turn with an error (e.g. Muse `503 overloaded` after
/// the CLI's own retries). The failed job carries that error, so the leader is told why instead of
/// a bare `failed`.
fn failure_reason(conn: &Connection, changed: Vec<Delegation>, body: &Value) -> Result<Vec<Delegation>, String> {
    let error = body.get("error").and_then(Value::as_str).unwrap_or("bilinmeyen");
    let message = ["last_assistant_message", "error_details"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()))
        .unwrap_or("");
    let hint = match crate::chain::classify(error, message) {
        crate::chain::Kind::Transient => "geçici; aynı worker'a yeniden ver ya da başka worker seç",
        crate::chain::Kind::Permanent => "kota/kredi bitti; başka sağlayıcıdaki worker'a ver",
        crate::chain::Kind::Auth => "jeton reddedildi; patron sağlayıcı jetonunu yenilemeli",
        crate::chain::Kind::Unknown => "worker'ın pane'ine bak",
    };
    let short: String = message.chars().take(400).collect();
    let why = format!("sağlayıcı hatası ({error}), oturum işi bitiremeden durdu: {short} — {hint}");
    changed
        .into_iter()
        .map(|d| {
            if d.status == "failed" {
                conn.execute("UPDATE delegations SET summary = IFNULL(summary, ?2) WHERE id = ?1", params![d.id, why]).map_err(e)?;
            }
            get(conn, d.id)
        })
        .collect()
}

/// Görev #40: longest a running job is held for an open background task, counted from its start.
/// A task that never ends and never wakes the worker would otherwise keep the job running forever.
const BACKGROUND_WAIT_MAX_MS: i64 = 2 * 60 * 60 * 1000;

fn awaiting_background(conn: &Connection, running: &[i64], transcript: Option<&str>) -> bool {
    let Some(path) = transcript.filter(|_| !running.is_empty()) else { return false };
    let fresh = running.iter().any(|id| get(conn, *id).is_ok_and(|d| now() - d.updated_at < BACKGROUND_WAIT_MAX_MS));
    fresh && background_open(Path::new(path))
}

/// Claude Code writes a started background task as a tool result carrying
/// `toolUseResult.backgroundTaskId`, and its end (completed / failed / killed) as a
/// `<task-notification><task-id>…` user row. Open = started and not yet notified. Hook bodies carry
/// no such field, so the transcript is the only source.
fn background_open(path: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut reader = BufReader::new(file);
    let (mut started, mut ended) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        if line.contains("<task-notification>") {
            let mut rest: &str = &line;
            while let Some(i) = rest.find("<task-id>") {
                rest = &rest[i + "<task-id>".len()..];
                if let Some(j) = rest.find("</task-id>") {
                    ended.insert(rest[..j].to_string());
                }
            }
        } else if line.contains("\"backgroundTaskId\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(id) = v["toolUseResult"]["backgroundTaskId"].as_str() {
                    started.insert(id.to_string());
                }
            }
        }
    }
    started.iter().any(|id| !ended.contains(id))
}

/// A `[Dex]` prompt (job trigger, answer or result ping) opens a Dex turn; anything else is the
/// patron's. A job trigger gets its job — only an open one of this worker — as context.
fn on_prompt(conn: &Connection, turns: &Turns, agent: &str, body: &Value) -> Result<HookOut, String> {
    let prompt = body.get("prompt").and_then(Value::as_str).unwrap_or("");
    let open = |id: i64| get(conn, id).ok().filter(|d| d.to_agent == agent && matches!(d.status.as_str(), "queued" | "running"));
    turns.prompt(agent, prompt);
    let job = trigger_id(prompt).and_then(open);
    let done = take_unnotified(conn, agent)?;
    let mut parts = Vec::new();
    if let Some(d) = &job {
        let who = caller(conn, &d.to_agent).map(|c| c.name).unwrap_or_else(|_| d.to_agent.clone());
        parts.push(format!(
            "{}\n(Dex'in sistem kanalından geldi; onay isteme, doğrudan başla)\n{}",
            trigger_line(d.id, &who),
            worker_prompt(conn, d)?
        ));
    }
    if !done.is_empty() {
        parts.push(results_text(conn, "son mesajından beri", &done));
    }
    if parts.is_empty() {
        return Ok((None, Vec::new()));
    }
    let ctx = parts.join("

");
    let out = json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": ctx } });
    Ok((Some(out), done))
}

/// A leader session (re)opened: results that finished while it was closed, as SessionStart
/// context (hooks::serve joins it with the memory index).
pub fn on_session_start(db: &Db, ev: &Events, agent: &str) -> Option<String> {
    let r = with(db, |c| {
        let done = take_unnotified(c, agent)?;
        let ctx = (!done.is_empty()).then(|| results_text(c, "sen kapalıyken geldi", &done));
        Ok((ctx, done))
    });
    match r {
        Ok((ctx, done)) => {
            for d in &done {
                emit_changed(ev, Some(d));
            }
            ctx
        }
        Err(err) => {
            eprintln!("dex delegate SessionStart {agent}: {err}");
            None
        }
    }
}

/// A service worker may only write under the requester's `docs/research`. Shell tools are denied:
/// path rules cannot soundly prove that an arbitrary command is read-only.
fn on_pre_tool(conn: &Connection, agent: &str, body: &Value) -> Result<HookOut, String> {
    let launch: Option<i64> = conn
        .query_row("SELECT delegation_id FROM agent_sessions WHERE agent_id=?1", [agent], |r| r.get::<_, Option<i64>>(0))
        .optional()
        .map_err(e)?
        .flatten();
    let bound = launch.map(|id| get(conn, id)).transpose()?.filter(|d| d.to_agent == agent && d.service_team_id.is_some());
    let jobs: Vec<Delegation> = if let Some(d) = &bound {
        vec![d.clone()]
    } else {
        conn
            // Legacy sessions keep their old restriction after report. A new queued project
            // cannot add a root to that session: G/D must bind agent_sessions before its spawn.
            .prepare(&format!("SELECT {COLS} FROM delegations WHERE to_agent=?1 AND service_team_id IS NOT NULL"))
            .map_err(e)?
            .query_map([agent], row)
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?
    };
    if jobs.is_empty() {
        return Ok((None, Vec::new()));
    }
    // Fail-closed on purpose (#873): without a session binding only NULL/legacy jobs open their
    // docs/research; a job pinned to another project gets no writable root, so its worker cannot
    // even write its report. G must bind agent_sessions.delegation_id at spawn before projects open.
    let research: Vec<PathBuf> = jobs
        .iter()
        .filter(|d| bound.is_some() || d.project_id.is_none() || d.project_id.as_deref() == Some(format!("legacy-{}", d.team_id).as_str()))
        .map(|d| result_dir(conn, d))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let report_file = bound.as_ref().map(|d| result_file(conn, d)).transpose()?.flatten();
    let tool = body.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let deny = |reason: &str| {
        let out = json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }});
        Ok((Some(out), Vec::new()))
    };
    if matches!(tool, "Bash" | "PowerShell" | "Shell") {
        return deny("Alt takım kabuk çalıştıramaz; isteyen takımın projesi salt okunurdur (Dex). Dosya çıktısını docs/research altına yaz.");
    }
    if matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") {
        let input = body.get("tool_input");
        let path = ["file_path", "notebook_path", "path"]
            .iter()
            .find_map(|k| input.and_then(|i| i.get(*k)).and_then(Value::as_str));
        if !path.is_some_and(|p| if let Some(file) = &report_file {
            path_within(Path::new(p),file) && path_within(file,Path::new(p))
        } else { bound.is_none() && research.iter().any(|root| path_within(Path::new(p), root)) }) {
            return deny("Alt takım yalnız isteyen takımın docs/research klasörüne yazabilir (Dex).");
        }
    }
    Ok((None, Vec::new()))
}

// ---- Starting / delivering ----

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartRequest {
    pub delegation_id: i64,
    pub agent_id: String,
    pub prompt: String,
    pub add_dirs: Vec<String>,
}

/// Service jobs get the requester's project as an extra (writable) dir; results go to its docs/research.
pub(crate) fn start_request(conn: &Connection, d: &Delegation) -> Result<StartRequest, String> {
    if !crate::job_model::can_start(conn, d.id)? { return Err("model_patron_approval_required".into()); }
    let add_dirs = match &d.service_team_id {
        Some(_) => {
            let research = result_dir(conn, d)?.ok_or_else(|| crate::projects::ProjectError::Unavailable.to_string())?;
            std::fs::create_dir_all(&research).map_err(|err| format!("{}: {err}", research.display()))?;
            vec![research.parent().and_then(Path::parent).unwrap_or(&research).to_string_lossy().into_owned()]
        }
        None => {
            // #298: the report folder exists before the job runs — a read-only reviewer cannot make it.
            if let Some(dir) = result_dir(conn, d)? {
                std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
            }
            Vec::new()
        }
    };
    Ok(StartRequest { delegation_id: d.id, agent_id: d.to_agent.clone(), prompt: worker_prompt(conn, d)?, add_dirs })
}

/// Work that goes before `d`: a running job, or an older queued one (FIFO). A newer queued row
/// does not count — with it, the oldest job would wait on the one behind it (Görev #12).
pub(crate) fn has_other_work(conn: &Connection, d: &Delegation) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM delegations WHERE to_agent = ?1 AND id <> ?2 AND status IN ('queued','running')
           AND (model IS NULL OR model_approval='approved')
           AND (status = 'running' OR id < ?2))",
        params![d.to_agent, d.id],
        |row| row.get(0),
    )
    .map_err(e)
}

pub(crate) fn started(db: &Db, ev: &Events, id: i64) -> Result<Delegation, String> {
    let d = with(db, |conn| {
        let d = get(conn, id)?;
        if d.to_agent.is_empty() { return Err("iş henüz worker'a atanmadı".into()); }
        if d.status == "queued" {
            crate::diagnostics::event("delegation", serde_json::json!({"id": id, "agent": d.to_agent, "status": "delivered"}));
            set_status(conn, id, "running")?;
        }
        get(conn, id)
    })?;
    emit_changed(ev, Some(&d));
    Ok(d)
}

pub(crate) fn start_failed(db: &Db, ev: &Events, id: i64, error: &str) -> Result<Delegation, String> {
    let d = with(db, |conn| {
        conn.execute(
            "UPDATE delegations SET status = 'failed', summary = ?2, updated_at = ?3 WHERE id = ?1 AND status = 'queued'",
            params![id, format!("başlatılamadı: {error}"), now()],
        )
        .map_err(e)?;
        get(conn, id)
    })?;
    emit_changed(ev, Some(&d));
    Ok(d)
}

/// New row → deliver now unless the worker already has work (then its next Stop delivers).
/// Waits briefly for the frontend's ack so the caller gets a real status.
pub fn dispatch(db: &Db, ev: &Events, d: Delegation, worker_running: bool) -> Result<Delegation, String> {
    if d.to_agent.is_empty() || !with(db, |c| crate::job_model::can_start(c, d.id))? {
        let _ = ev.emit("proposals-changed", &());
        emit_changed(ev, Some(&d));
        return Ok(d);
    }
    let req = {
        let conn = db.0.lock().map_err(|_| "db kilidi zehirli")?;
        let busy: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM delegations WHERE to_agent = ?1 AND id <> ?2 AND status IN ('queued','running') AND (model IS NULL OR model_approval='approved')",
                params![d.to_agent, d.id],
                |r| r.get(0),
            )
            .map_err(e)?;
        if busy > 0 || worker_running { None } else { Some(start_request(&conn, &d)?) }
    };
    emit_changed(ev, Some(&d));
    let Some(req) = req else { return Ok(d) };
    ev.emit("dex-delegate-start", &req)?;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let cur = with(db, |c| get(c, d.id))?;
        if cur.status != "queued" || acked(d.id) {
            return Ok(cur);
        }
    }
    with(db, |c| get(c, d.id))
}

static ACKED: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

fn acked(id: i64) -> bool {
    ACKED.lock().map(|v| v.contains(&id)).unwrap_or(false)
}

// ---- Frontend side (host commands) ----

/// The frontend answers `dex-delegate-start`: started (→ running), busy (stays queued; the next
/// Stop delivers it) or failed.
pub fn ack(db: &Db, ev: &Events, id: i64, error: Option<String>, queued: bool) -> Result<(), String> {
    let d = with(db, |c| {
        let d = get(c, id)?;
        if d.to_agent.is_empty() { return Err("iş henüz worker'a atanmadı".into()); }
        if d.status != "queued" {
            return Ok(d);
        }
        if let Some(err) = &error {
            c.execute(
                "UPDATE delegations SET status = 'failed', summary = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, format!("başlatılamadı: {err}"), now()],
            )
            .map_err(e)?;
        } else if !queued {
            set_status(c, id, "running")?;
        }
        get(c, id)
    })?;
    if let Ok(mut v) = ACKED.lock() {
        v.push(id);
        if v.len() > 256 {
            v.drain(..128);
        }
    }
    emit_changed(ev, Some(&d));
    Ok(())
}

/// The worker's session ended without a Stop for its job (killed, crashed, closed): running rows
/// become done_no_report, and a queued one is started fresh (nobody else would ever deliver it).
fn on_exit(conn: &Connection, agent: &str) -> Result<(Vec<Delegation>, Option<Delegation>), String> {
    let mut changed = Vec::new();
    // DEX-24: a job whose question has no delivered answer yet is not over: the answer resumes
    // the session (and nothing else is started meanwhile).
    let asking = "id IN (SELECT delegation_id FROM questions WHERE answer_delivered_at IS NULL)";
    let waiting: bool = conn
        .query_row(&format!("SELECT EXISTS (SELECT 1 FROM delegations WHERE to_agent = ?1 AND status = 'running' AND {asking})"), [agent], |r| r.get(0))
        .map_err(e)?;
    let running: Vec<i64> = conn
        .prepare(&format!("SELECT id FROM delegations WHERE to_agent = ?1 AND status = 'running' AND NOT {asking}"))
        .map_err(e)?
        .query_map([agent], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    for id in running {
        conn.execute(
            "UPDATE delegations SET status = 'done_no_report', summary = IFNULL(summary, 'worker oturumu rapor vermeden kapandı'), updated_at = ?2 WHERE id = ?1",
            params![id, now()],
        )
        .map_err(e)?;
        changed.push(get(conn, id)?);
    }
    if waiting {
        return Ok((changed, None));
    }
    let next = next_for_worker(conn, agent)?;
    Ok((changed, next))
}

/// On app start no worker session survives: open rows are closed so the leader hears about them.
/// #161 `keep_queued`: a clean restart (restart.rs) — a queued job was never delivered and the new
/// core delivers it; only `running` rows close.
pub fn reconcile_on_start(conn: &Connection, keep_queued: bool) -> Result<usize, String> {
    conn.execute(
        "UPDATE delegations SET
             summary = IFNULL(summary, CASE status WHEN 'running' THEN 'Dex kapandı; worker rapor vermeden durdu'
                                                   ELSE 'Dex kapandı; iş worker''a hiç teslim edilmedi' END),
             status = CASE status WHEN 'running' THEN 'done_no_report' ELSE 'failed' END,
             notified = 0, delivered_at = NULL, read_at = NULL, seen_at = NULL, updated_at = ?1
         WHERE status IN ('queued','running') AND model_approval IS NOT 'pending' AND (?2 = 0 OR status = 'running')
           AND NOT (status='queued' AND to_agent='' AND id IN (SELECT delegation_id FROM delegation_role_queue))",
        params![now(), keep_queued],
    )
    .map_err(e)
}

/// The patron opened the worker's pane (DEX-19): its finished results no longer need the dock mark.
/// Delivery to the leader is unaffected (that follows `notified` / `delivered_at`).
pub fn seen(db: &Db, ev: &Events, agent_id: &str) -> Result<(), String> {
    let changed = with(db, |c| {
        let ids: Vec<i64> = c
            .prepare("SELECT id FROM delegations WHERE to_agent = ?1 AND seen_at IS NULL AND status NOT IN ('queued','running')")
            .map_err(e)?
            .query_map([agent_id], |r| r.get(0))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let ts = now();
        ids.into_iter()
            .map(|id| {
                c.execute("UPDATE delegations SET seen_at = ?2 WHERE id = ?1", params![id, ts]).map_err(e)?;
                get(c, id)
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    for d in &changed {
        emit_changed(ev, Some(d));
    }
    Ok(())
}

pub fn session_exited(db: &Db, ev: &Events, agent_id: &str) -> Result<Option<Delegation>, String> {
    let (changed, next) = with(db, |c| on_exit(c, agent_id))?;
    for d in &changed {
        emit_changed(ev, Some(d));
    }
    Ok(next)
}

/// DEX-28: the stall watchdog (frontend) heard no hook from `agent` for `minutes` while it was
/// working. On a delegated job its leader gets a one-shot note (next Stop / prompt, `plan::note`);
/// the leader's id comes back so the frontend can ping it when idle. Nothing is stopped or changed.
pub fn stalled(conn: &Connection, agent: &str, minutes: u32) -> Result<Option<String>, String> {
    tell_leader(conn, agent, |who, id| {
        format!(
            "{who} devretme #{id} üzerinde {minutes} dk'dır hiçbir olay üretmedi; takılmış olabilir. \
             Dex hiçbir şeyi durdurmadı: patron pane'e bakabilir, sen gerekirse işi başka worker'a verebilirsin."
        )
    })
}

/// FIX-3: the worker's turn got no first reply in time (`first_reply`). Same one-shot leader note
/// as [`stalled`]; the turn is not touched (the patron may retry or cancel it from the pane).
pub fn slow_reply(conn: &Connection, agent: &str, text: &str) -> Result<Option<String>, String> {
    tell_leader(conn, agent, |who, id| {
        format!(
            "{who} devretme #{id} üzerinde ilk yanıtı alamadı: {text} Dex turu kesmedi; patron pane'den \
             yeniden deneyebilir ya da iptal edebilir, sen gerekirse işi başka worker'a verebilirsin."
        )
    })
}

fn tell_leader(conn: &Connection, agent: &str, text: impl FnOnce(&str, i64) -> String) -> Result<Option<String>, String> {
    let d = conn
        .query_row(
            &format!("SELECT {COLS} FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1"),
            [agent],
            row,
        )
        .optional()
        .map_err(e)?;
    let Some(d) = d else { return Ok(None) };
    let who = caller(conn, agent).map(|c| c.name).unwrap_or_else(|_| agent.into());
    crate::plan::note(conn, &d.from_agent, &crate::dexmsg::line(crate::dexmsg::SISTEM, text(&who, d.id)))?;
    Ok(Some(d.from_agent))
}

impl crate::Core {
    pub fn delegation_stalled(&self, agent_id: &str, minutes: u32) -> Result<Option<String>, String> {
        with(&self.db, |c| stalled(c, agent_id, minutes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    fn leader(c: &Connection) -> Caller {
        caller(c, "timur").unwrap()
    }

    #[test]
    fn role_slug_takes_precedence_over_another_workers_name() {
        let c = mem();
        c.execute("UPDATE agents SET name='backend' WHERE id='kaan'", [])
            .unwrap();
        assert_eq!(
            role_target(&c, "default", "backend").unwrap(),
            ("backend".into(), "ayla".into())
        );
    }

    #[test]
    fn unassigned_role_job_cannot_be_started_or_acknowledged() {
        let c = mem();
        let lead = leader(&c);
        let tx = c.unchecked_transaction().unwrap();
        let d = insert(
            &tx,
            NewDelegation {
                from: &lead,
                to_agent: "ayla".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "pool".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        queue_role(&tx, d.id, "backend", None).unwrap();
        tx.commit().unwrap();
        let d = get(&c, d.id).unwrap();
        assert!(
            start_request(&c, &d).is_err(),
            "unassigned jobs cannot produce a start request"
        );
        let db = Db::new(c);
        let ev = Events::new(crate::events::NullSink);
        assert!(started(&db, &ev, d.id).is_err());
        assert!(ack(&db, &ev, d.id, None, false).is_err());
        assert_eq!(with(&db, |c| get(c, d.id)).unwrap().status, "queued");
    }

    #[test]
    fn role_pool_disk_reopen_keeps_fifo_and_bound_queue_priority() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "role-pool-db-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dex.db");
        let c = crate::db::open(&path).unwrap();
        let lead = leader(&c);
        let job = |c: &Connection, prompt: &str| {
            insert(
                c,
                NewDelegation {
                    from: &lead,
                    to_agent: "ayla".into(),
                    team_id: "default".into(),
                    service_team_id: None,
                    prompt: prompt.into(),
                    task_id: None,
                    patron_words: None,
                    effort: Some(crate::effort::Effort::High),
                },
            )
            .unwrap()
        };
        let busy = job(&c, "busy");
        set_status(&c, busy.id, "running").unwrap();
        let tx = c.unchecked_transaction().unwrap();
        let one = job(&tx, "one");
        queue_role(&tx, one.id, "backend", None).unwrap();
        let two = job(&tx, "two");
        queue_role(&tx, two.id, "backend", None).unwrap();
        tx.commit().unwrap();
        let pinned = job(&c, "pinned");
        crate::review::set_fix_of(&c, pinned.id, busy.id).unwrap();
        drop(c);
        for _ in 0..2 {
            let c = crate::db::open(&path).unwrap();
            assert!(get(&c, one.id).unwrap().to_agent.is_empty());
            assert_eq!(get(&c, two.id).unwrap().queue_position, Some(2));
            assert_eq!(get(&c, pinned.id).unwrap().to_agent, "ayla");
            assert_eq!(crate::review::fix_of(&c, pinned.id).unwrap(), Some(busy.id));
            assert_eq!(
                c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                4
            );
        }
        let c = crate::db::open(&path).unwrap();
        set_status(&c, busy.id, "done").unwrap();
        assert_eq!(next_for_worker(&c, "ayla").unwrap().unwrap().id, pinned.id);
        assert!(get(&c, one.id).unwrap().to_agent.is_empty());
        set_status(&c, pinned.id, "done").unwrap();
        assert_eq!(on_exit(&c, "ayla").unwrap().1.unwrap().id, one.id);
        assert_eq!(
            get(&c, one.id).unwrap().effort,
            Some(crate::effort::Effort::High)
        );
        assert_eq!(get(&c, two.id).unwrap().queue_position, Some(1));
        set_status(&c, one.id, "done").unwrap();
        assert_eq!(next_for_worker(&c, "ayla").unwrap().unwrap().id, two.id);
        assert_eq!(active_count(&c, "ayla").unwrap(), 1);
    }

    #[test]
    fn single_project_job_is_pinned_and_multi_project_insert_requires_selection() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let l = leader(&c);
        let root = f.dir("one");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let one =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "One".into(), worktrees: None }, &ready).unwrap();
        let new = || NewDelegation {
            from: &l,
            to_agent: "kaan".into(),
            team_id: "default".into(),
            service_team_id: None,
            prompt: "x".into(),
            task_id: None,
            patron_words: None,
            effort: None,
        };
        let job = insert(&c, new()).unwrap();
        let pinned: (Option<String>, Option<String>, Option<String>) = c
            .query_row("SELECT project_id,project_root,merge_branch FROM delegations WHERE id=?1", [job.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(pinned, (Some(one.id), one.root_path, one.default_branch));
        let root = f.dir("two");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        crate::projects::create(&c, "default", crate::projects::NewProject { name: "Two".into(), worktrees: None }, &ready).unwrap();
        assert!(insert(&c, new()).unwrap_err().starts_with("project_required"));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn result_directory_keeps_the_job_root_after_team_default_changes() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let l = leader(&c);
        let root = f.dir("one");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let one =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "One".into(), worktrees: None }, &ready).unwrap();
        let d = insert(
            &c,
            NewDelegation {
                from: &l,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "x".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        let two_root = f.dir("two");
        let ready = crate::projects::prepare_root(two_root.to_str().unwrap(), None).unwrap();
        let two =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "Two".into(), worktrees: None }, &ready).unwrap();
        crate::projects::set_default(&c, "default", Some(&two.id)).unwrap();
        assert_eq!(result_dir(&c, &d).unwrap(), Some(PathBuf::from(one.root_path.unwrap()).join(".dex/results")));
    }

    #[test]
    fn service_guard_keeps_spawn_report_and_does_not_expand_to_a_queued_project() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        c.execute_batch(
            "INSERT INTO teams(id,name,kind,created_at) VALUES('svc','Service','service',1);
            INSERT INTO agents(id,team_id,name,kind,model,shirt,hair,skin) VALUES('researcher','svc','R','worker','sonnet','a','b','c');",
        )
        .unwrap();
        let l = leader(&c);
        let add = |name: &str| {
            let root = f.dir(name);
            let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            let p =
                crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready).unwrap();
            let context = resolve_context(&c, &f.0, &l, Some(&p.id), None).unwrap();
            insert_bound(
                &c,
                NewDelegation {
                    from: &l,
                    to_agent: "researcher".into(),
                    team_id: "default".into(),
                    service_team_id: Some("svc".into()),
                    prompt: "research".into(),
                    task_id: None,
                    patron_words: None,
                    effort: None,
                },
                &context,
            )
            .unwrap()
        };
        let one = add("one");
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES('researcher','s','claude',1,?1,?2)",params![one.project_id,one.id]).unwrap();
        let two = add("two");
        crate::projects::set_default(&c, "default", two.project_id.as_deref()).unwrap();
        let write = |file: PathBuf| on_pre_tool(&c, "researcher", &json!({"tool_name":"Write","tool_input":{"file_path":file}})).unwrap().0;
        let own = result_file(&c, &one).unwrap().unwrap();
        assert!(write(own.clone()).is_none(), "the launch report stays writable after default changes");
        assert_eq!(write(result_file(&c, &two).unwrap().unwrap()).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(write(own.parent().unwrap().join("other.md")).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        set_status(&c, one.id, "done").unwrap();
        assert!(write(own).is_none(), "report does not expand the still-live session");
    }

    #[test]
    fn unbound_service_session_on_a_pinned_project_cannot_write_even_its_report() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        c.execute_batch(
            "INSERT INTO teams(id,name,kind,created_at) VALUES('svc','Service','service',1);
            INSERT INTO agents(id,team_id,name,kind,model,shirt,hair,skin) VALUES('researcher','svc','R','worker','sonnet','a','b','c');",
        )
        .unwrap();
        let l = leader(&c);
        let root = f.dir("one");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let p =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "One".into(), worktrees: None }, &ready).unwrap();
        let context = resolve_context(&c, &f.0, &l, Some(&p.id), None).unwrap();
        let d = insert_bound(
            &c,
            NewDelegation {
                from: &l,
                to_agent: "researcher".into(),
                team_id: "default".into(),
                service_team_id: Some("svc".into()),
                prompt: "research".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
            &context,
        )
        .unwrap();
        assert_ne!(d.project_id, Some("legacy-default".into()));
        // No agent_sessions.delegation_id: fail-closed until G binds the session at spawn.
        let own = result_file(&c, &d).unwrap().unwrap();
        let out = on_pre_tool(&c, "researcher", &json!({"tool_name": "Write", "tool_input": {"file_path": own}})).unwrap().0;
        assert_eq!(out.unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    #[test]
    fn pick_by_role_name_and_least_busy() {
        let c = mem();
        assert_eq!(pick_worker(&c, "default", Some("Kaan")).unwrap(), "kaan");
        assert_eq!(pick_worker(&c, "default", Some("backend")).unwrap(), "ayla");
        assert!(pick_worker(&c, "default", Some("timur")).is_err(), "leaders are not workers");
        assert!(pick_worker(&c, "default", Some("yok")).unwrap_err().contains("kaan"));
        // A role outside the catalog gets the valid list; a catalog role the team lacks does not (it needs propose_member).
        let unknown = pick_worker(&c, "default", Some("android")).unwrap_err();
        assert!(unknown.contains("Geçerli roller:") && unknown.contains("backend — ") && unknown.contains("escalate"), "{unknown}");
        let missing = pick_worker(&c, "default", Some("seo")).unwrap_err();
        assert!(!missing.contains("Geçerli roller:"), "{missing}");
        let l = leader(&c);
        insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        assert_ne!(pick_worker(&c, "default", None).unwrap(), "kaan");
    }

    #[test]
    fn other_work_is_fifo_a_newer_queued_job_does_not_hold_back_the_oldest() {
        let c = mem();
        let l = leader(&c);
        let job = |p: &str| insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let (a, b, cc) = (job("bir"), job("iki"), job("üç"));
        assert!(!has_other_work(&c, &a).unwrap(), "oldest of three queued: nothing ahead");
        assert!(has_other_work(&c, &b).unwrap(), "b waits for a");
        assert!(has_other_work(&c, &cc).unwrap(), "c waits for a and b");
        set_status(&c, b.id, "running").unwrap();
        assert!(has_other_work(&c, &a).unwrap(), "a running job goes first whatever its id");
        set_status(&c, b.id, "done").unwrap();
        set_status(&c, a.id, "done").unwrap();
        assert!(!has_other_work(&c, &cc).unwrap(), "finished rows do not count");
    }

    /// #671: effort is stored on the job, read back for a respawn while it runs, and shown in the
    /// leader's result line — with a note when it went into the patron's own session.
    #[test]
    fn effort_rides_the_job_and_the_result_line() {
        use crate::effort::Effort;
        let c = mem();
        let l = leader(&c);
        let job = |e: Option<Effort>, to: &str| insert(&c, NewDelegation { from: &l, to_agent: to.into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: e }).unwrap();
        let plain = job(None, "ayla");
        let hi = job(Some(Effort::Xhigh), "kaan");
        assert_eq!((plain.effort, hi.effort), (None, Some(Effort::Xhigh)));
        assert!(!hi.effort_skipped);
        assert_eq!(running_effort(&c, "kaan").unwrap(), None, "queued is not running");
        set_status(&c, hi.id, "running").unwrap();
        set_status(&c, plain.id, "running").unwrap();
        assert_eq!(running_effort(&c, "kaan").unwrap(), Some(Effort::Xhigh));
        assert_eq!(running_effort(&c, "ayla").unwrap(), None, "a job without effort keeps none");
        effort_skipped(&c, plain.id).unwrap();
        assert!(!get(&c, plain.id).unwrap().effort_skipped, "no effort, nothing to skip");
        let text = |d: &Delegation| results_text(&c, "t", std::slice::from_ref(d));
        assert!(text(&get(&c, hi.id).unwrap()).contains("· effort xhigh\n"), "{}", text(&get(&c, hi.id).unwrap()));
        assert!(!text(&get(&c, plain.id).unwrap()).contains("effort"));
        effort_skipped(&c, hi.id).unwrap();
        let t = text(&get(&c, hi.id).unwrap());
        assert!(t.contains("· effort xhigh — effort uygulanmadı (patron oturumu)"), "{t}");
        assert_eq!(serde_json::to_value(get(&c, hi.id).unwrap()).unwrap()["effort"], "xhigh", "delegation_status shows it");
    }

    /// Görev #283: patron's words first (only when given), then the leader's text, then the rule;
    /// stored on the row so a queued start builds the same text.
    #[test]
    fn worker_prompt_puts_patron_words_above_leader_text() {
        let c = mem();
        let l = leader(&c);
        let new = |w: Option<&str>| NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "API'yi yaz".into(), task_id: None, patron_words: w.map(String::from), effort: None };
        let with = insert(&c, new(Some("  düzen değişmez  "))).unwrap();
        assert_eq!(get(&c, with.id).unwrap().patron_words.as_deref(), Some("düzen değişmez"));
        let t = worker_prompt(&c, &with).unwrap();
        let (p, l2, r) = (t.find("## Patronun sözü\ndüzen değişmez\n").unwrap(), t.find("## Liderin açıklaması\nAPI'yi yaz\n").unwrap(), t.find(PATRON_RULE).unwrap());
        assert!(p < l2 && l2 < r, "{t}");

        for blank in [None, Some("   ")] {
            let d = insert(&c, new(blank)).unwrap();
            assert_eq!(d.patron_words, None);
            let t = worker_prompt(&c, &d).unwrap();
            assert!(!t.contains("Patronun sözü\n") && t.contains("## Liderin açıklaması\nAPI'yi yaz\n") && t.contains(PATRON_RULE), "{t}");
        }
    }

    fn job(c: &Connection, l: &Caller, prompt: &str) -> Delegation {
        let d = insert(c, NewDelegation { from: l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: prompt.into(), task_id: None, patron_words: None, effort: None }).unwrap();
        set_status(c, d.id, "running").unwrap();
        get(c, d.id).unwrap()
    }

    fn prompt(c: &Connection, t: &Turns, text: &str) -> Option<Value> {
        on_prompt(c, t, "kaan", &json!({ "prompt": text })).unwrap().0
    }

    /// Görev #19 A: a Dex turn's first unreported Stop is refused once; `report` then ends it cleanly.
    #[test]
    fn dex_turn_stop_is_refused_once_and_report_closes_the_job() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "şemayı güncelle");
        prompt(&c, &t, &trigger_line(d.id, "Kaan"));
        let out = on_stop(&c, &t, "kaan", false, None).unwrap().0.expect("refused");
        assert_eq!(out["decision"], "block");
        let reason = out["reason"].as_str().unwrap();
        assert!(reason.contains(&format!("#{}", d.id)) && reason.contains("report") && reason.contains("ask_leader"), "{reason}");
        assert_eq!(get(&c, d.id).unwrap().status, "running");
        report(&c, &caller(&c, "kaan").unwrap(), d.id, "bitti: tamam", None).unwrap();
        assert!(on_stop(&c, &t, "kaan", false, None).unwrap().0.is_none());
        assert_eq!(get(&c, d.id).unwrap().status, "done");
    }

    /// Görev #19 A: the second unreported Stop closes the job, and the leader reads the worker's last words.
    #[test]
    fn second_unreported_stop_closes_the_job_with_the_workers_last_message() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "şemayı güncelle");
        let dir = std::env::temp_dir().join(format!("dex-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tr = dir.join("t.jsonl");
        std::fs::write(&tr, concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"eski"}]}}"#, "
",
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"düşün"},{"type":"text","text":"Başlayayım mı?"}]}}"#, "
",
        )).unwrap();
        let tr = tr.to_string_lossy().into_owned();
        prompt(&c, &t, &trigger_line(d.id, "Kaan"));
        assert!(on_stop(&c, &t, "kaan", false, Some(&tr)).unwrap().0.is_some(), "first: refused");
        let (out, changed) = on_stop(&c, &t, "kaan", false, Some(&tr)).unwrap();
        assert!(out.is_none(), "second: not refused again");
        assert_eq!(changed.len(), 1);
        let row = get(&c, d.id).unwrap();
        assert_eq!(row.status, "done_no_report");
        assert_eq!(row.summary.as_deref(), Some("worker'ın son mesajı: Başlayayım mı?"));
        let done = take_unnotified(&c, "timur").unwrap();
        assert!(results_text(&c, "x", &done).contains("worker'ın son mesajı: Başlayayım mı?"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Görev #40: an open background task keeps the job running (no refusal spent, no reset for a
    /// queued job); once it ends the gate is back, and a stale hold expires.
    #[test]
    fn open_background_task_keeps_the_job_running_until_it_ends() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "derle");
        let queued = job(&c, &l, "sıradaki");
        set_status(&c, queued.id, "queued").unwrap();
        let dir = std::env::temp_dir().join(format!("dex-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tr = dir.join("t.jsonl");
        let started = concat!(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"Command running in background with ID: b1."}]},"toolUseResult":{"backgroundTaskId":"b1"}}"#, "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"Command running in background with ID: b2."}]},"toolUseResult":{"backgroundTaskId":"b2"}}"#, "\n",
        );
        let note = |id: &str| format!("{{\"type\":\"user\",\"message\":{{\"content\":\"<task-notification>\\n<task-id>{id}</task-id>\\n<status>completed</status>\\n</task-notification>\"}}}}\n");
        std::fs::write(&tr, started).unwrap();
        let path = tr.to_string_lossy().into_owned();
        prompt(&c, &t, &trigger_line(d.id, "Kaan"));
        for _ in 0..2 {
            let (out, changed) = on_stop(&c, &t, "kaan", false, Some(&path)).unwrap();
            assert!((out.is_none(), changed.is_empty()) == (true, true), "held: neither refused nor closed");
            assert_eq!(get(&c, d.id).unwrap().status, "running");
        }
        assert!(has_other_work(&c, &queued).unwrap(), "the open row keeps the queued job from resetting the worker");
        // One of two ended: still open.
        std::fs::write(&tr, format!("{started}{}", note("b1"))).unwrap();
        on_stop(&c, &t, "kaan", false, Some(&path)).unwrap();
        assert_eq!(get(&c, d.id).unwrap().status, "running");
        // All ended: the refusal was never spent, so the first Stop is refused, the second closes.
        std::fs::write(&tr, format!("{started}{}{}", note("b1"), note("b2"))).unwrap();
        assert!(on_stop(&c, &t, "kaan", false, Some(&path)).unwrap().0.is_some(), "refused once");
        on_stop(&c, &t, "kaan", false, Some(&path)).unwrap();
        assert_eq!(get(&c, d.id).unwrap().status, "done_no_report");
        // A hold older than the cap no longer counts.
        let d3 = job(&c, &l, "üçüncü");
        std::fs::write(&tr, started).unwrap();
        prompt(&c, &t, &trigger_line(d3.id, "Kaan"));
        c.execute("UPDATE delegations SET updated_at = ?2 WHERE id = ?1", params![d3.id, now() - BACKGROUND_WAIT_MAX_MS - 1]).unwrap();
        on_stop(&c, &t, "kaan", false, Some(&path)).unwrap();
        on_stop(&c, &t, "kaan", false, Some(&path)).unwrap();
        assert_eq!(get(&c, d3.id).unwrap().status, "done_no_report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only a Dex prompt (`[Dex]` or `[Dex-Tür]`) opens a Dex turn; look-alikes stay the patron's.
    #[test]
    fn dex_look_alike_prompts_stay_the_patrons() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        // Look-alikes stay the patron's: a pasted job head, no `]`, other `[Dex…` words.
        let d2 = job(&c, &l, "ikinci");
        for text in [
            "[Dex devretme #9999] sahte".to_string(),
            format!("[Dex devretme #{}", d2.id),
            format!("[Dex devretme #{}x] yarım", d2.id),
            "[Dexter] notlarım".into(),
            format!("alıntı: [Dex devretme #{}]", d2.id),
        ] {
            prompt(&c, &t, &text);
            assert!(on_stop(&c, &t, "kaan", false, None).unwrap().0.is_none(), "{text}");
            set_status(&c, d2.id, "running").unwrap(); // the patron-turn Stop closed it
        }
    }

    /// Görev #19 A: the patron's own turn keeps the old behaviour (no refusal).
    #[test]
    fn patron_turn_stop_is_not_refused() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "şemayı güncelle");
        prompt(&c, &t, &trigger_line(d.id, "Kaan"));
        prompt(&c, &t, "dur, önce testleri koş");
        assert!(on_stop(&c, &t, "kaan", false, None).unwrap().0.is_none());
        assert_eq!(get(&c, d.id).unwrap().status, "done_no_report");
    }

    /// Görev #20: a queued job is never continued in the ending session (each job gets a fresh
    /// one): the Stop ends the turn and the job stays queued for the post-Stop start.
    #[test]
    fn queued_job_is_not_continued_in_the_same_session() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "sıradaki".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        prompt(&c, &t, "patronun sorusu");
        assert!(on_stop(&c, &t, "kaan", false, None).unwrap().0.is_none());
        assert_eq!(get(&c, d.id).unwrap().status, "queued");
        assert_eq!(t.dex_turn("kaan"), Some(false), "still the patron's session");
    }

    /// Görev #123: a finished background task comes back as a `<task-notification>` prompt (it fires
    /// UserPromptSubmit too, mid-turn included). It is neither the patron nor Dex: it must not
    /// decide whose session this is, or the next job is pasted into the old one instead of a fresh one.
    #[test]
    fn background_task_notification_keeps_the_turn_owner() {
        let (c, t) = (mem(), Turns::default());
        let note = "<task-notification>\n<task-id>b1x9ku68q</task-id>\n<status>completed</status>\n</task-notification>";
        prompt(&c, &t, &trigger_line(1, "Kaan"));
        prompt(&c, &t, note);
        assert_eq!(t.dex_turn("kaan"), Some(true), "still Dex's session");
        prompt(&c, &t, "patronun sorusu");
        prompt(&c, &t, note);
        assert_eq!(t.dex_turn("kaan"), Some(false), "still the patron's session");
        let fresh = Turns::default();
        prompt(&c, &fresh, note);
        assert_eq!(fresh.dex_turn("kaan"), None, "no turn yet: stays unknown");
    }

    /// Görev #19 B: the trigger carries only the id; the context is this worker's open job and nothing else.
    #[test]
    fn trigger_gets_only_this_workers_open_job_as_context() {
        let (c, t) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "UZUN-GÖREV-METNİ");
        assert_eq!(
            trigger_line(d.id, "Kaan"),
            format!("[Dex-Devretme] Kaan #{} · başladı — ayrıntı bu mesajın ekinde (görmüyorsan delegation_status ile oku).", d.id)
        );
        assert_eq!(trigger_id(&trigger_line(d.id, "Kaan")), Some(d.id));
        assert_eq!(trigger_id(&format!("[Dex] yeni iş #{} — eski biçim", d.id)), Some(d.id), "old triggers still parse");
        let body = worker_prompt(&c, &d).unwrap();
        assert!(body.starts_with(&format!("[Dex-Devretme] Timur #{} · sana iş verdi:", d.id)), "{body}");
        assert_eq!(trigger_id(&body), None, "the job body is not a trigger");
        assert!(!trigger_line(d.id, "Kaan").contains("UZUN-GÖREV-METNİ"), "no job text in the pane");
        let ctx = prompt(&c, &t, &trigger_line(d.id, "Kaan")).unwrap();
        let ctx = ctx["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.contains(&format!("[Dex-Devretme] Kaan #{}", d.id)) && ctx.contains("UZUN-GÖREV-METNİ") && ctx.contains(&format!("delegationId={}", d.id)), "{ctx}");
        // Another worker's job, a finished job, a quoted trigger, an unknown id: no job context.
        let other = insert(&c, NewDelegation { from: &l, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "AYLA-İŞİ".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        assert!(prompt(&c, &t, &trigger_line(other.id, "Ayla")).is_none());
        assert!(prompt(&c, &t, &format!("bak: {}", trigger_line(d.id, "Kaan"))).is_none());
        assert!(prompt(&c, &t, &trigger_line(9_999, "Kaan")).is_none());
        set_status(&c, d.id, "done").unwrap();
        assert!(prompt(&c, &t, &trigger_line(d.id, "Kaan")).is_none());
    }

    /// #1751: result lines carry the typed tag, who, job number, status and one-line summary.
    #[test]
    fn results_text_uses_typed_tag_lines() {
        let (c, _) = (mem(), Turns::default());
        let l = leader(&c);
        let d = job(&c, &l, "şemayı güncelle");
        report(&c, &caller(&c, "kaan").unwrap(), d.id, "bitti: tamam", None).unwrap();
        let done = take_unnotified(&c, "timur").unwrap();
        assert_eq!(done.len(), 1);
        let out = results_text(&c, "sen kapalıyken geldi", &done);
        assert!(out.starts_with("[Dex-Devretme] sen kapalıyken geldi:\n"), "{out}");
        assert!(out.contains(&format!("- [Dex-Devretme] Kaan #{} · bitti — bitti: tamam", d.id)), "{out}");
        assert!(!out.contains("özet (devamı)"), "tek satır özet yalnız başlıkta: {out}");
        // Çok satırlı özet: ilki başlıkta, devamı ayrıntıda, tekrar yok.
        let d2 = job(&c, &l, "uzun iş");
        report(&c, &caller(&c, "kaan").unwrap(), d2.id, "ilk satır\nikinci satır", None).unwrap();
        let done = take_unnotified(&c, "timur").unwrap();
        let out = results_text(&c, "x", &done);
        assert!(out.contains(&format!("- [Dex-Devretme] Kaan #{} · bitti — ilk satır\n", d2.id)), "{out}");
        assert!(out.contains("  özet (devamı): ikinci satır"), "{out}");
        assert_eq!(out.matches("ikinci satır").count(), 1, "{out}");
    }

    #[test]
    fn stop_marks_unreported_then_delivers_queue() {
        let c = mem();
        let l = leader(&c);
        let a = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "bir".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let b = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iki".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        set_status(&c, a.id, "running").unwrap();

        let (out, changed) = on_stop(&c, &Turns::default(), "kaan", false, None).unwrap();
        assert!(out.is_none(), "Görev #20: b waits for its own fresh session");
        assert_eq!(get(&c, a.id).unwrap().status, "done_no_report");
        assert_eq!(get(&c, b.id).unwrap().status, "queued");
        assert_eq!(changed.len(), 1);
        set_status(&c, b.id, "running").unwrap(); // started in its fresh session

        // Late report still lands; worker-only; leader hears about both exactly once.
        let kaan = caller(&c, "kaan").unwrap();
        assert!(report(&c, &l, b.id, "ok", None).is_err());
        report(&c, &kaan, b.id, "tamam", None).unwrap();
        let (ctx, _) = on_prompt(&c, &Turns::default(), "timur", &json!({})).unwrap();
        let ctx = ctx.unwrap().to_string();
        assert!(ctx.contains("RAPORSUZ") && ctx.contains("tamam"), "{ctx}");
        assert!(on_prompt(&c, &Turns::default(), "timur", &json!({})).unwrap().0.is_none());
        assert!(on_stop(&c, &Turns::default(), "kaan", false, None).unwrap().0.is_none());
    }

    /// DEX-28: only a worker on a running job has a leader to tell; the note is one-shot, nothing changes.
    #[test]
    fn stalled_worker_notes_its_leader_only_while_running() {
        let c = mem();
        let l = leader(&c);
        assert_eq!(stalled(&c, "kaan", 10).unwrap(), None, "no job → nobody to tell");
        let a = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        assert_eq!(stalled(&c, "kaan", 10).unwrap(), None, "queued is not running");
        set_status(&c, a.id, "running").unwrap();
        assert_eq!(stalled(&c, "kaan", 10).unwrap().as_deref(), Some("timur"));
        assert_eq!(get(&c, a.id).unwrap().status, "running", "the watchdog never touches the job");
        let n = crate::plan::take_notes(&c, "timur").unwrap().unwrap();
        assert!(n.contains(&format!("#{}", a.id)) && n.contains("10 dk"), "{n}");
        assert!(crate::plan::take_notes(&c, "timur").unwrap().is_none());
    }

    /// FIX-3: a worker's late first reply reaches its leader the same way.
    #[test]
    fn slow_first_reply_notes_the_leader() {
        let c = mem();
        let l = leader(&c);
        let text = "Yerel LLM yanıt vermiyor (90 sn). Sunucu model yüklüyor: qwen3-coder-next.";
        assert_eq!(slow_reply(&c, "kaan", text).unwrap(), None, "no job → nobody to tell");
        let a = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        set_status(&c, a.id, "running").unwrap();
        assert_eq!(slow_reply(&c, "kaan", text).unwrap().as_deref(), Some("timur"));
        assert_eq!(get(&c, a.id).unwrap().status, "running");
        let n = crate::plan::take_notes(&c, "timur").unwrap().unwrap();
        assert!(n.contains(&format!("#{}", a.id)) && n.contains("model yüklüyor: qwen3-coder-next"), "{n}");
    }

    /// #316: a provider error (Muse 503 overloaded) ends the turn with StopFailure — the job used to
    /// fail with no summary, so the leader saw a silent `failed`. The error now reaches the leader.
    #[test]
    fn stop_failure_tells_the_leader_why() {
        let c = mem();
        let l = leader(&c);
        let a = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        set_status(&c, a.id, "running").unwrap();
        let db = Db(std::sync::Arc::new(std::sync::Mutex::new(c)));
        let ev = Events::new(crate::events::NullSink);
        let body = json!({
            "hook_event_name": "StopFailure", "error": "server_error",
            "last_assistant_message": "API Error: 503 The service is temporarily overloaded. Please retry shortly.",
        });
        assert!(on_hook(&db, &ev, "kaan", "StopFailure", &body).is_none());
        let d = with(&db, |c| get(c, a.id)).unwrap();
        let s = d.summary.unwrap_or_default();
        assert_eq!(d.status, "failed");
        assert!(s.contains("server_error") && s.contains("503") && s.contains("geçici"), "{s}");
        let ctx = with(&db, |c| Ok(results_text(c, "x", &[get(c, a.id)?]))).unwrap();
        assert!(ctx.contains("· başarısız — sağlayıcı hatası") && s.contains("server_error"), "{ctx}");
    }

    #[test]
    fn session_exit_closes_running_and_hands_back_queue() {
        let c = mem();
        let l = leader(&c);
        let new = |p: &str| NewDelegation { from: &l, to_agent: "bora".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None };
        let a = insert(&c, new("a")).unwrap();
        let b = insert(&c, new("b")).unwrap();
        set_status(&c, a.id, "running").unwrap();
        let (changed, next) = on_exit(&c, "bora").unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!((changed[0].status.as_str(), changed[0].summary.is_some()), ("done_no_report", true));
        assert_eq!(next.map(|d| d.id), Some(b.id));
        assert!(start_request(&c, &get(&c, b.id).unwrap()).unwrap().prompt.contains("#2"));
    }

    #[test]
    fn restart_closes_open_rows_once() {
        let c = mem();
        let l = leader(&c);
        let new = |p: &str| NewDelegation { from: &l, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None };
        let a = insert(&c, new("a")).unwrap();
        let b = insert(&c, new("b")).unwrap();
        set_status(&c, a.id, "running").unwrap();
        assert_eq!(reconcile_on_start(&c, false).unwrap(), 2);
        assert_eq!(get(&c, a.id).unwrap().status, "done_no_report");
        assert_eq!(get(&c, b.id).unwrap().status, "failed");
        assert_eq!(reconcile_on_start(&c, false).unwrap(), 0);
        assert_eq!(take_unnotified(&c, "timur").unwrap().len(), 2);
    }

    /// DEX-19: a result reaches the leader exactly once, by whichever path comes first; the
    /// leader's next Stop marks it read; a late report is a new result.
    #[test]
    fn leader_gets_each_result_once_then_reads_it() {
        let c = mem();
        let l = leader(&c);
        let kaan = caller(&c, "kaan").unwrap();
        let new = |p: &str| NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None };
        let a = insert(&c, new("a")).unwrap();
        set_status(&c, a.id, "running").unwrap();
        report(&c, &kaan, a.id, "bitti-a", None).unwrap();
        let d = get(&c, a.id).unwrap();
        assert_eq!((d.delivered_at, d.read_at, d.seen_at), (None, None, None));

        // Leader was working: its Stop continues the turn with the result.
        let (out, changed) = on_stop(&c, &Turns::default(), "timur", false, None).unwrap();
        let out = out.unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("bitti-a"), "{out}");
        assert_eq!(changed.len(), 1);
        let d = get(&c, a.id).unwrap();
        assert!(d.delivered_at.is_some() && d.read_at.is_none());
        // Not again: not by prompt context, not by SessionStart, not by the next Stop.
        assert!(on_prompt(&c, &Turns::default(), "timur", &json!({})).unwrap().0.is_none());
        assert!(take_unnotified(&c, "timur").unwrap().is_empty());
        let (out, changed) = on_stop(&c, &Turns::default(), "timur", false, None).unwrap();
        assert!(out.is_none());
        assert_eq!(changed.len(), 1, "second Stop = the turn with the result ended → read");
        assert!(get(&c, a.id).unwrap().read_at.is_some());
        assert!(on_stop(&c, &Turns::default(), "timur", false, None).unwrap().1.is_empty(), "read once");

        // Idle leader: pasted ping → UserPromptSubmit delivers; StopFailure does not mark read.
        let b = insert(&c, new("b")).unwrap();
        set_status(&c, b.id, "running").unwrap();
        report(&c, &kaan, b.id, "bitti-b", None).unwrap();
        let (ctx, changed) = on_prompt(&c, &Turns::default(), "timur", &json!({})).unwrap();
        assert!(ctx.unwrap().to_string().contains("bitti-b"));
        assert_eq!(changed.len(), 1);
        assert!(on_stop(&c, &Turns::default(), "timur", true, None).unwrap().1.is_empty());
        assert!(get(&c, b.id).unwrap().read_at.is_none());

        // Patron opening the pane is separate from delivery.
        let db = Db(std::sync::Arc::new(std::sync::Mutex::new(c)));
        let ev = Events::new(crate::events::NullSink);
        seen(&db, &ev, "kaan").unwrap();
        let (sa, sb) = with(&db, |c| Ok((get(c, a.id)?, get(c, b.id)?))).unwrap();
        assert!(sa.seen_at.is_some() && sb.seen_at.is_some() && sb.read_at.is_none());

        // Closed leader: a finished row waits for SessionStart, handed out once.
        let x = with(&db, |c| {
            let x = insert(c, NewDelegation { from: &caller(c, "timur")?, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None })?;
            set_status(c, x.id, "failed")?;
            Ok(x)
        })
        .unwrap();
        let ctx = on_session_start(&db, &ev, "timur").unwrap();
        assert!(ctx.contains(&format!("#{}", x.id)) && ctx.contains("kapalıyken"), "{ctx}");
        assert!(on_session_start(&db, &ev, "timur").is_none());
    }

    /// Rows marked notified outside the delivery path (plan cancel, pre-migration-7) read as delivered and read.
    #[test]
    fn legacy_notified_rows_count_as_read() {
        let c = mem();
        let l = leader(&c);
        let a = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "a".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        c.execute("UPDATE delegations SET status = 'failed', notified = 1 WHERE id = ?1", [a.id]).unwrap();
        let d = get(&c, a.id).unwrap();
        assert_eq!((d.delivered_at, d.read_at), (Some(d.updated_at), Some(d.updated_at)));
        assert!(on_stop(&c, &Turns::default(), "timur", false, None).unwrap().1.is_empty());
    }

    /// Hata A: a main-team job is delivered as a file under `<cwd>/.dex/results`; the leader gets its path.
    #[test]
    fn report_requires_an_existing_result_file_and_leader_gets_its_path() {
        let c = mem();
        let fixture = crate::projects::tests::Fixture::new();
        let proj = fixture.dir("reports");
        crate::teams::update_team(
            &c,
            &fixture.0,
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(proj.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let (l, kaan) = (leader(&c), caller(&c, "kaan").unwrap());
        let new = |p: &str| NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None };
        let d = insert(&c, new("a")).unwrap();
        set_status(&c, d.id, "running").unwrap();
        let dir = proj.join(".dex").join("results");
        let file = dir.join(format!("{}.md", d.id));
        let p = worker_prompt(&c, &d).unwrap();
        assert!(p.contains(&file.display().to_string()) && p.contains("resultPath"), "{p}");

        let err = report(&c, &kaan, d.id, "bitti: x", None).unwrap_err();
        assert!(err.contains("resultPath gerekli") && err.contains(&file.display().to_string()), "{err}");
        assert!(report(&c, &kaan, d.id, "bitti: x", Some("  ")).unwrap_err().contains("resultPath gerekli"));
        let outside = proj.join("rapor.md");
        std::fs::write(&outside, "x").unwrap();
        assert!(report(&c, &kaan, d.id, "bitti: x", Some(&outside.to_string_lossy())).unwrap_err().contains("altına yazılmalı"));
        assert!(report(&c, &kaan, d.id, "bitti: x", Some(&dir.join("../../rapor.md").to_string_lossy())).is_err());
        assert!(report(&c, &kaan, d.id, "bitti: x", Some(&file.to_string_lossy())).unwrap_err().contains("dosyası yok"));
        assert_eq!(get(&c, d.id).unwrap().status, "running", "a refused report changes nothing");

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "# tam rapor").unwrap();
        report(&c, &kaan, d.id, "bitti: x", Some(&file.to_string_lossy())).unwrap();
        let (ctx, _) = on_prompt(&c, &Turns::default(), "timur", &json!({})).unwrap();
        let ctx = ctx.unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();
        assert!(ctx.contains("· bitti — bitti: x") && ctx.contains(&format!("rapor: {} — oku", file.display())), "{ctx}");

        // Session closed without `report` but the file was saved: the leader still gets the path.
        let b = insert(&c, new("b")).unwrap();
        set_status(&c, b.id, "running").unwrap();
        let b_file = dir.join(format!("{}.md", b.id));
        std::fs::write(&b_file, "yarım rapor").unwrap();
        on_exit(&c, "kaan").unwrap();
        let (out, _) = on_stop(&c, &Turns::default(), "timur", false, None).unwrap();
        let reason = out.unwrap()["reason"].as_str().unwrap().to_string();
        assert!(reason.contains("RAPORSUZ") && reason.contains(&format!("report'suz kaydedilmiş): {} — oku", b_file.display())), "{reason}");
    }

    #[test]
    fn content_writer_rejects_parent_components_before_creating_files() {
        let c = mem();
        let lead = leader(&c);
        let mut d = insert(&c, NewDelegation { from: &lead, to_agent: "kaan".into(), team_id: "default".into(),
            service_team_id: None, prompt: "report".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let fixture = crate::projects::tests::Fixture::new();
        let root = fixture.dir("project");
        d.project_root = Some(root.join("..").join("project").to_string_lossy().into_owned());
        assert!(write_report_content(&d, "body").unwrap_err().contains("güvenli proje kökü"));
        assert!(!root.join(".dex").exists());
    }

    #[cfg(windows)]
    #[test]
    fn content_writer_normalizes_windows_snapshot_prefix_and_case() {
        let c = mem();
        let lead = leader(&c);
        let mut d = insert(&c, NewDelegation { from: &lead, to_agent: "kaan".into(), team_id: "default".into(),
            service_team_id: None, prompt: "report".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let fixture = crate::projects::tests::Fixture::new();
        let root = fixture.dir("snapshot-case");
        d.project_root = Some(format!(r"\\?\{}", root.to_string_lossy().to_uppercase()));
        write_report_content(&d, "body").unwrap();
        assert_eq!(std::fs::read_to_string(root.join(".dex/results").join(format!("{}.md", d.id))).unwrap(), "body");
    }

    #[cfg(not(windows))]
    #[test]
    fn content_writer_unsupported_platform_keeps_job_open_without_io() {
        let c = mem();
        let fixture = crate::projects::tests::Fixture::new();
        let root = fixture.dir("unsupported-report");
        crate::teams::update_team(&c, &fixture.0, "default", crate::teams::TeamPatch {
            cwd: Some(Some(root.to_string_lossy().into_owned())), ..Default::default()
        }).unwrap();
        let lead = leader(&c);
        let d = insert(&c, NewDelegation { from: &lead, to_agent: "kaan".into(), team_id: "default".into(),
            service_team_id: None, prompt: "report".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let error = report_with_content(&c, &caller(&c, "kaan").unwrap(), d.id, "bitti", None, Some("body")).unwrap_err();
        assert!(error.contains("bu platformda desteklenmiyor"), "{error}");
        assert_eq!(get(&c, d.id).unwrap().status, "queued");
        assert!(get(&c, d.id).unwrap().result_path.is_none());
        assert!(!root.join(".dex").exists());
    }

    /// #298: a new team has no `.dex/results` — the start makes it, so a read-only reviewer's
    /// first report passes the permit (it cannot mkdir itself).
    #[test]
    fn start_makes_the_result_dir_for_a_reviewers_first_report() {
        let c = mem();
        let fixture = crate::projects::tests::Fixture::new();
        let proj = fixture.dir("review-report");
        crate::teams::update_team(
            &c,
            &fixture.0,
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(proj.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let l = leader(&c);
        let d = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "a".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let reviewer = crate::permit::Ctx { team_root: Some(proj.clone()), write: vec![proj.clone()], read: vec![proj.clone()], cwd: Some(proj.clone()), read_only: true, job: Some(d.id), ..Default::default() };
        let write = || crate::permit::decide("Write", &json!({ "file_path": format!(".dex/results/{}.md", d.id), "content": "x" }), &reviewer);
        assert!(matches!(write(), crate::permit::Verdict::Deny(_)), "no folder yet");
        start_request(&c, &d).unwrap();
        assert!(proj.join(".dex").join("results").is_dir());
        assert_eq!(write(), crate::permit::Verdict::Allow);
        start_request(&c, &d).unwrap();
    }

    /// A team without a project folder has nowhere for the file: summary-only reports still land.
    #[test]
    fn team_without_project_folder_reports_summary_only() {
        let c = mem();
        let (l, kaan) = (leader(&c), caller(&c, "kaan").unwrap());
        let d = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        assert!(worker_prompt(&c, &d).unwrap().contains("summary=<teslimin tamamı"));
        report(&c, &kaan, d.id, "tam metin", None).unwrap();
    }

    #[test]
    fn service_worker_can_only_write_research_and_cannot_use_shell() {
        let c = mem();
        let fixture = crate::projects::tests::Fixture::new();
        let proj = fixture.dir("requester");
        let cwd = proj.to_string_lossy().into_owned();
        crate::teams::update_team(&c, &fixture.0, "default", crate::teams::TeamPatch { cwd: Some(Some(cwd)), ..Default::default() })
            .unwrap();
        c.execute_batch(
            "INSERT INTO teams (id, name, kind, cwd, created_at) VALUES ('ara','Araştırma','service',NULL,0);
             INSERT INTO agents (id, team_id, name, kind, model, shirt, hair, skin) VALUES ('ece','ara','Ece','worker','sonnet','a','b','c');",
        )
        .unwrap();
        let l = leader(&c);
        let d = insert(&c, NewDelegation { from: &l, to_agent: "ece".into(), team_id: "default".into(), service_team_id: Some("ara".into()), prompt: "Rakip analizi yap".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let p = worker_prompt(&c, &d).unwrap();
        assert!(p.contains(&format!("{}-rakip-analizi-yap.md", d.id)), "{p}");

        let mem_file = proj.join(".dex").join("memory").join("shared").join("x.md");
        let body = json!({ "tool_name": "Write", "tool_input": { "file_path": mem_file.to_string_lossy() } });
        let (out, _) = on_pre_tool(&c, "ece", &body).unwrap();
        assert_eq!(out.unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        let ok = json!({ "tool_name": "Write", "tool_input": { "file_path": proj.join("docs/research/a.md").to_string_lossy() } });
        assert!(on_pre_tool(&c, "ece", &ok).unwrap().0.is_none());
        let source = json!({ "tool_name": "Edit", "tool_input": { "file_path": proj.join("src/a.rs").to_string_lossy() } });
        assert_eq!(on_pre_tool(&c, "ece", &source).unwrap().0.unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        let shell = json!({ "tool_name": "Bash", "tool_input": { "command": "echo pwned > src/a.rs" } });
        assert_eq!(on_pre_tool(&c, "ece", &shell).unwrap().0.unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");

        let ece = caller(&c, "ece").unwrap();
        // Service jobs keep their own root: `.dex/results` of the requester is not theirs.
        let dex_results = proj.join(".dex").join("results");
        std::fs::create_dir_all(&dex_results).unwrap();
        std::fs::write(dex_results.join(format!("{}.md", d.id)), "x").unwrap();
        let err = report(&c, &ece, d.id, "s", Some(&dex_results.join(format!("{}.md", d.id)).to_string_lossy())).unwrap_err();
        assert!(err.contains("alt takım sonucu") && err.contains("research"), "{err}");
        assert!(report(&c, &ece, d.id, "s", Some(&proj.join("src/a.md").to_string_lossy())).is_err());
        assert!(report(&c, &ece, d.id, "s", Some(&proj.join("docs/research/../x.md").to_string_lossy())).is_err());
        let research = proj.join("docs").join("research");
        std::fs::create_dir_all(&research).unwrap();
        std::fs::write(research.join("a.md"), "sonuç").unwrap();
        report(&c, &ece, d.id, "s", Some(&research.join("a.md").to_string_lossy())).unwrap();
        assert!(on_pre_tool(&c, "ece", &shell).unwrap().0.is_some(), "report must not unlock the same PTY");
        let _ = std::fs::remove_dir_all(&proj);
    }
}
