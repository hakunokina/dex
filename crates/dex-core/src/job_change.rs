//! #2048: the leader cancels a job that has not started, or moves it to another worker / the role
//! pool (`delegation_cancel` / `delegation_reassign`). Nothing changes on the call: it is a card in
//! the patron's approval queue (`member_proposals`, linked here like a job model). The decision
//! re-checks that the job still has not started (queued, no worktree, no start in flight); the
//! leader hears the outcome as a note either way.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;

use crate::db::{tasks, with, Db};
use crate::delegate::{self, Caller, Delegation};
use crate::events::Events;
use crate::proposals::Proposal;
use crate::Core;

// Additive, IF NOT EXISTS; the versioned runner applies it once. Before commit: ROLLBACK. After
// commit: keep the table; to switch the feature off, reject the pending cards:
// UPDATE member_proposals SET status='rejected' WHERE status='pending' AND id IN (SELECT proposal_id FROM job_changes).
// Job ids are plain columns (like delegations' agent ids): history outlives its rows.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS job_changes (
    proposal_id   INTEGER PRIMARY KEY REFERENCES member_proposals(id) ON DELETE CASCADE,
    action        TEXT    NOT NULL CHECK (action IN ('cancel','reassign')),
    delegation_id INTEGER,
    plan_item_id  INTEGER,
    to_agent      TEXT,
    to_role       TEXT,
    reason        TEXT    NOT NULL,
    CHECK (delegation_id IS NOT NULL OR plan_item_id IS NOT NULL),
    CHECK ((action = 'cancel' AND to_agent IS NULL AND to_role IS NULL) OR (action = 'reassign' AND (to_agent IS NULL) <> (to_role IS NULL)))
);
CREATE INDEX IF NOT EXISTS job_changes_delegation ON job_changes(delegation_id);
CREATE INDEX IF NOT EXISTS job_changes_item ON job_changes(plan_item_id);";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

/// Jobs a dispatch is starting right now (Start / Paste path). A decision never changes one of
/// them; a start never begins on a row a decision changed (both sides take this lock first).
#[derive(Default)]
pub(crate) struct Starting(Mutex<HashSet<i64>>);

pub(crate) struct StartGuard<'a> {
    set: &'a Starting,
    id: i64,
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.set.0.lock() {
            s.remove(&self.id);
        }
    }
}

impl Starting {
    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashSet<i64>>, String> {
        self.0.lock().map_err(|_| "başlatma kümesi zehirli".to_string())
    }

    /// None: the row is no longer this queued job of this worker, or another dispatch starts it.
    pub(crate) fn begin<'a>(&'a self, db: &Db, d: &Delegation) -> Result<Option<StartGuard<'a>>, String> {
        let mut set = self.0.lock().map_err(|_| "başlatma kümesi zehirli")?;
        if set.contains(&d.id) {
            return Ok(None);
        }
        let cur = with(db, |c| delegate::get(c, d.id))?;
        if cur.status != "queued" || cur.to_agent != d.to_agent {
            return Ok(None);
        }
        set.insert(d.id);
        Ok(Some(StartGuard { set: self, id: d.id }))
    }
}

#[derive(Debug, Clone)]
struct Item {
    id: i64,
    plan: i64,
    key: String,
    title: String,
    status: String,
    task_id: Option<i64>,
    delegation_id: Option<i64>,
    team_id: String,
    plan_status: String,
    role: Option<String>,
    agent: Option<String>,
}

const ITEM: &str = "SELECT i.id, i.plan_id, i.key, i.title, i.status, i.task_id, i.delegation_id, p.team_id, p.status,
    (SELECT role_slug FROM plan_targets WHERE item_id=i.id), (SELECT agent_id FROM plan_targets WHERE item_id=i.id)
    FROM plan_items i JOIN plans p ON p.id=i.plan_id";

fn item(conn: &Connection, filter: &str, args: impl rusqlite::Params) -> Result<Option<Item>, String> {
    conn.query_row(&format!("{ITEM} {filter}"), args, |r| {
        Ok(Item {
            id: r.get(0)?,
            plan: r.get(1)?,
            key: r.get(2)?,
            title: r.get(3)?,
            status: r.get(4)?,
            task_id: r.get(5)?,
            delegation_id: r.get(6)?,
            team_id: r.get(7)?,
            plan_status: r.get(8)?,
            role: r.get(9)?,
            agent: r.get(10)?,
        })
    })
    .optional()
    .map_err(e)
}

/// A job that has not started: a queued delegation (maybe a plan item's) or a waiting plan item.
struct Job {
    d: Option<Delegation>,
    item: Option<Item>,
}

impl Job {
    fn label(&self) -> String {
        match (&self.d, &self.item) {
            (Some(d), Some(it)) => format!("İş #{} (plan #{} · görev {})", d.id, it.plan, it.key),
            (Some(d), None) => format!("İş #{}", d.id),
            (None, Some(it)) => format!("Plan #{} · görev {}", it.plan, it.key),
            (None, None) => String::new(),
        }
    }

    fn task(&self) -> Option<i64> {
        self.d.as_ref().and_then(|d| d.task_id).or(self.item.as_ref().and_then(|i| i.task_id))
    }

    /// The team whose workers can take it (a service job stays in its service team).
    fn pool(&self) -> &str {
        match (&self.d, &self.item) {
            (Some(d), _) => d.service_team_id.as_deref().unwrap_or(&d.team_id),
            (None, Some(it)) => &it.team_id,
            (None, None) => "",
        }
    }
}

fn resolve(conn: &Connection, team: &str, delegation: Option<i64>, plan_item: Option<i64>) -> Result<Job, String> {
    let job = match (delegation, plan_item) {
        (Some(id), _) => Job { d: Some(delegate::get(conn, id)?), item: item(conn, "WHERE i.delegation_id=?1", [id])? },
        (None, Some(id)) => {
            let it = item(conn, "WHERE i.id=?1", [id])?.ok_or_else(|| format!("plan görevi yok: {id}"))?;
            // A waiting item started since the request: its job is what counts now.
            let d = match it.delegation_id {
                Some(j) if it.status == "running" => Some(delegate::get(conn, j)?),
                _ => None,
            };
            Job { d, item: Some(it) }
        }
        (None, None) => return Err("delegationId ya da planId+key gerekli".into()),
    };
    let own = job.d.as_ref().map(|d| d.team_id.as_str()).or(job.item.as_ref().map(|i| i.team_id.as_str()));
    if own != Some(team) {
        return Err(format!("{} senin takımının işi değil", job.label()));
    }
    Ok(job)
}

fn unstarted(conn: &Connection, job: &Job, starting: &HashSet<i64>) -> Result<(), String> {
    let label = job.label();
    if let Some(d) = &job.d {
        if d.status != "queued" {
            return Err(format!(
                "{label} başlamış ya da kapanmış (durum: {}); yalnız başlamamış (queued) iş iptal edilir ya da aktarılır",
                d.status
            ));
        }
        if starting.contains(&d.id) {
            return Err(format!("{label} şu an başlatılıyor; yalnız başlamamış iş iptal edilir ya da aktarılır"));
        }
        let tree: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM delegation_worktrees WHERE delegation_id=?1 AND removed_at IS NULL)",
                [d.id],
                |r| r.get(0),
            )
            .map_err(e)?;
        if tree {
            return Err(format!("{label} daha önce başlatıldı (çalışma dalı açık); iptal/aktarma yapılmaz"));
        }
    } else if let Some(it) = &job.item {
        if it.plan_status != "active" {
            return Err(format!("plan #{} etkin değil ({})", it.plan, it.plan_status));
        }
        if !matches!(it.status.as_str(), "waiting" | "blocked") {
            return Err(format!("{label} durumu {}: yalnız bekleyen (waiting/blocked) plan görevi", it.status));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
enum To {
    Agent(String),
    Role(String),
}

fn agent_name(conn: &Connection, id: &str) -> String {
    delegate::caller(conn, id).map(|c| c.name).unwrap_or_else(|_| id.to_string())
}

fn describe(conn: &Connection, to: &To) -> String {
    match to {
        To::Agent(a) => agent_name(conn, a),
        To::Role(r) => format!("rol kuyruğu: {r}"),
    }
}

/// Where the job is now (Agent, or Role while it waits in the pool / a role item).
fn current(job: &Job) -> Option<To> {
    match (&job.d, &job.item) {
        (Some(d), _) if d.to_agent.is_empty() => d.target_role.clone().map(To::Role),
        (Some(d), _) => Some(To::Agent(d.to_agent.clone())),
        (None, Some(it)) => it.agent.clone().map(To::Agent).or_else(|| it.role.clone().map(To::Role)),
        (None, None) => None,
    }
}

fn parse_target(conn: &Connection, job: &Job, args: &Value) -> Result<To, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    match (s("agentId"), s("role")) {
        (Some(a), None) => Ok(To::Agent(delegate::pick_worker(conn, job.pool(), Some(a))?)),
        (None, Some(r)) => {
            if job.d.as_ref().is_some_and(|d| d.service_team_id.is_some()) {
                return Err("alt takım işi rol kuyruğuna aktarılamaz; agentId ver".into());
            }
            Ok(To::Role(delegate::role_target(conn, job.pool(), r)?.0))
        }
        _ => Err("agentId ya da role'den tam biri gerekli".into()),
    }
}

/// Valid now (request and decision): a worker of the pool, not where the job already is, not the
/// author of the job a review reads. Returns the agent the role pool must skip.
fn check_target(conn: &Connection, job: &Job, to: &To) -> Result<Option<String>, String> {
    let label = job.label();
    if current(job).as_ref() == Some(to) {
        return Err(format!("{label} zaten {}", describe(conn, to)));
    }
    let mut exclude = None;
    if let Some(d) = &job.d {
        if d.model_approval.as_deref() == Some("pending") {
            return Err(format!("{label} patron model onayı bekliyor; aktarma o karardan sonra"));
        }
        let gpu: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM gpu_grants WHERE delegation_id=?1)", [d.id], |r| r.get(0))
            .map_err(e)?;
        if gpu {
            return Err(format!("{label} GPU iznine bağlı (izin worker'a özel); aktarılamaz"));
        }
        exclude = conn
            .query_row("SELECT exclude_agent FROM delegation_role_queue WHERE delegation_id=?1", [d.id], |r| r.get(0))
            .optional()
            .map_err(e)?
            .flatten();
        if let Some(r) = crate::review::get(conn, d.id)? {
            exclude = delegate::get(conn, r.target_id).ok().map(|t| t.to_agent).filter(|a| !a.is_empty()).or(exclude);
        }
    }
    let ok: bool = match to {
        To::Agent(a) => {
            if exclude.as_deref() == Some(a.as_str()) {
                return Err(format!("{} bu işi alamaz (incelenen işin yazarı)", agent_name(conn, a)));
            }
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM agents WHERE id=?1 AND team_id=?2 AND kind='worker' AND IFNULL(role_slug,'')<>'assistant')",
                params![a, job.pool()],
                |r| r.get(0),
            )
            .map_err(e)?
        }
        To::Role(r) => conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agents WHERE team_id=?1 AND kind='worker' AND role_slug=?2 AND (?3 IS NULL OR id<>?3))",
                params![job.pool(), r, exclude],
                |r| r.get(0),
            )
            .map_err(e)?,
    };
    if !ok {
        return Err(format!("hedef artık geçerli değil: {}", describe(conn, to)));
    }
    Ok(exclude)
}

/// `delegationId`, or `planId`+`key` for a plan task (also one not handed out yet).
fn locate(conn: &Connection, lead: &Caller, args: &Value) -> Result<(Option<i64>, Option<i64>), String> {
    let id = args.get("delegationId").and_then(Value::as_i64);
    let plan = args.get("planId").and_then(Value::as_i64);
    let key = args.get("key").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty());
    match (id, plan, key) {
        (Some(id), None, None) => Ok((Some(id), None)),
        (None, Some(plan), Some(key)) => {
            let it = item(conn, "WHERE i.plan_id=?1 AND i.key=?2", params![plan, key])?
                .ok_or_else(|| format!("plan #{plan} içinde görev yok: {key}"))?;
            if it.team_id != lead.team_id {
                return Err(format!("plan #{plan} senin takımında değil"));
            }
            Ok((None, Some(it.id)))
        }
        _ => Err("ya delegationId ya da planId+key ver".into()),
    }
}

/// The leader's call: validated now, applied only after the patron approves.
pub fn request(conn: &Connection, lead: &Caller, action: &str, args: &Value) -> Result<Proposal, String> {
    if !lead.is_leader() {
        return Err(format!("yetki yok: yalnız takım liderine açık ({} bir worker)", lead.name));
    }
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or("reason gerekli (patron bunu görecek)")?;
    let (delegation, plan_item) = locate(conn, lead, args)?;
    let tx = conn.unchecked_transaction().map_err(e)?;
    let job = resolve(&tx, &lead.team_id, delegation, plan_item)?;
    unstarted(&tx, &job, &HashSet::new())?;
    let (d_id, i_id) = (job.d.as_ref().map(|d| d.id), job.item.as_ref().map(|i| i.id));
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM job_changes j JOIN member_proposals p ON p.id=j.proposal_id
             WHERE p.status='pending' AND (j.delegation_id=?1 OR j.plan_item_id=?2))",
            params![d_id, i_id],
            |r| r.get(0),
        )
        .map_err(e)?;
    if pending {
        return Err(format!("{} için patron kararı bekleyen bir istek zaten var", job.label()));
    }
    let to = match action {
        "cancel" => None,
        "reassign" => {
            let to = parse_target(&tx, &job, args)?;
            check_target(&tx, &job, &to)?;
            Some(to)
        }
        _ => return Err(format!("bilinmeyen işlem: {action}")),
    };
    let title = job
        .task()
        .and_then(|t| tasks::get(&tx, t).ok())
        .map(|t| t.title)
        .or(job.item.as_ref().map(|i| i.title.clone()))
        .or(job.d.as_ref().map(|d| delegate::first_line(&d.prompt)))
        .unwrap_or_default();
    let now_at = current(&job).map(|c| describe(&tx, &c)).unwrap_or_else(|| "-".into());
    let what = match &to {
        None => format!(
            "İşlem: iptal — iş başlamadan kapanır{}",
            if job.item.is_some() { "; plana bağlı görevler bekler" } else { "" }
        ),
        Some(to) => format!("İşlem: aktarma → {}", describe(&tx, to)),
    };
    let text = format!("{}: {title}\nŞu an: {now_at}\n{what}\nGerekçe: {reason}", job.label());
    let (role_slug, name) = match &to {
        Some(To::Agent(a)) => (
            crate::teams::get_agent(&tx, a).ok().and_then(|a| a.role_slug).unwrap_or_default(),
            Some(agent_name(&tx, a)),
        ),
        Some(To::Role(r)) => (r.clone(), None),
        None => (String::new(), None),
    };
    tx.execute(
        "INSERT INTO member_proposals (team_id, leader, role_slug, name, model, provider, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, '', '', ?5, ?6)",
        params![lead.team_id, lead.id, role_slug, name, text, now()],
    )
    .map_err(e)?;
    let proposal = tx.last_insert_rowid();
    let (to_agent, to_role) = match &to {
        Some(To::Agent(a)) => (Some(a.as_str()), None),
        Some(To::Role(r)) => (None, Some(r.as_str())),
        None => (None, None),
    };
    tx.execute(
        "INSERT INTO job_changes (proposal_id, action, delegation_id, plan_item_id, to_agent, to_role, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![proposal, action, d_id, i_id, to_agent, to_role, reason],
    )
    .map_err(e)?;
    tx.commit().map_err(e)?;
    crate::proposals::get(conn, proposal)
}

struct Change {
    action: String,
    delegation: Option<i64>,
    plan_item: Option<i64>,
    to: Option<To>,
    reason: String,
}

fn change(conn: &Connection, proposal: i64) -> Result<Change, String> {
    conn.query_row(
        "SELECT action, delegation_id, plan_item_id, to_agent, to_role, reason FROM job_changes WHERE proposal_id=?1",
        [proposal],
        |r| {
            let (agent, role): (Option<String>, Option<String>) = (r.get(3)?, r.get(4)?);
            Ok(Change {
                action: r.get(0)?,
                delegation: r.get(1)?,
                plan_item: r.get(2)?,
                to: agent.map(To::Agent).or(role.map(To::Role)),
                reason: r.get(5)?,
            })
        },
    )
    .map_err(e)
}

/// The patron's answer; the caller owns the transaction. Approve applies only if the job still
/// has not started (`starting`: jobs a dispatch is starting now). The leader always gets a note.
pub(crate) fn decision(conn: &Connection, p: &Proposal, approve: bool, starting: &HashSet<i64>) -> Result<(), String> {
    let c = change(conn, p.id)?;
    conn.execute(
        "UPDATE member_proposals SET status=?2, decided_at=?3 WHERE id=?1",
        params![p.id, if approve { "approved" } else { "rejected" }, now()],
    )
    .map_err(e)?;
    let what = if c.action == "cancel" { "iptal" } else { "aktarma" };
    let label = resolve(conn, &p.team_id, c.delegation, c.plan_item).map(|j| j.label()).unwrap_or_else(|_| {
        c.delegation.map(|d| format!("İş #{d}")).unwrap_or_else(|| "Plan görevi".into())
    });
    let text = if !approve {
        format!("{label} {what} isteğin patron tarafından REDDEDİLDİ; iş olduğu gibi sürüyor.")
    } else {
        conn.execute_batch("SAVEPOINT job_change").map_err(e)?;
        match apply(conn, &p.team_id, &c, starting) {
            Ok(done) => {
                conn.execute_batch("RELEASE job_change").map_err(e)?;
                format!("{label} {what} isteğin ONAYLANDI ve uygulandı: {done}")
            }
            Err(why) => {
                conn.execute_batch("ROLLBACK TO job_change; RELEASE job_change").map_err(e)?;
                format!("{label} {what} isteğin patron tarafından onaylandı ama UYGULANMADI: {why}")
            }
        }
    };
    crate::plan::note(conn, &p.leader, &crate::dexmsg::line(crate::dexmsg::DEVRETME, text))
}

fn apply(conn: &Connection, team: &str, c: &Change, starting: &HashSet<i64>) -> Result<String, String> {
    let job = resolve(conn, team, c.delegation, c.plan_item)?;
    unstarted(conn, &job, starting)?;
    match &c.to {
        None => cancel(conn, &job, &c.reason),
        Some(to) => {
            let exclude = check_target(conn, &job, to)?;
            reassign(conn, &job, to, exclude.as_deref())
        }
    }
}

fn set_task(conn: &Connection, job: &Job, patch: tasks::TaskPatch) -> Result<(), String> {
    if let Some(t) = job.task().filter(|t| tasks::get(conn, *t).is_ok()) {
        tasks::update(conn, t, patch)?;
    }
    Ok(())
}

/// Like a failed task (plan_retry semantics): its plan item fails, dependents wait (blocked).
fn cancel(conn: &Connection, job: &Job, reason: &str) -> Result<String, String> {
    if let Some(d) = &job.d {
        // notified = 1: the leader hears it from the decision note, not again as a result.
        let n = conn
            .execute(
                "UPDATE delegations SET status='failed', summary=?2, notified=1, updated_at=?3 WHERE id=?1 AND status='queued'",
                params![d.id, format!("lider iptal etti (patron onayıyla): {reason}"), now()],
            )
            .map_err(e)?;
        if n != 1 {
            return Err("iş artık kuyrukta değil".into());
        }
        crate::review::finish_task(conn, d.id)?;
        delegate::sync_task(conn, d.id)?;
    }
    let Some(it) = &job.item else {
        return Ok("iş kapandı (failed), başlamadı; pano kartı yapılacaklara döndü.".into());
    };
    conn.execute("UPDATE plan_items SET status='failed' WHERE id=?1", [it.id]).map_err(e)?;
    set_task(conn, job, tasks::TaskPatch { status: Some("todo".into()), ..Default::default() })?;
    Ok(format!(
        "plan #{} görev {} başarısız sayıldı ve başlamadı; ona bağlı görevler bekler. plan_retry ile yeniden devredebilir ya da plan_cancel ile kalanları iptal edebilirsin.",
        it.plan, it.key
    ))
}

fn reassign(conn: &Connection, job: &Job, to: &To, exclude: Option<&str>) -> Result<String, String> {
    if let Some(d) = &job.d {
        conn.execute("DELETE FROM delegation_role_queue WHERE delegation_id=?1", [d.id]).map_err(e)?;
        match to {
            To::Agent(a) => {
                conn.execute(
                    "UPDATE delegations SET to_agent=?2, updated_at=?3 WHERE id=?1 AND status='queued'",
                    params![d.id, a, now()],
                )
                .map_err(e)?;
                set_task(conn, job, tasks::TaskPatch { agent_id: Some(Some(a.clone())), ..Default::default() })?;
            }
            To::Role(r) => {
                // queue_role checks the provisional recipient, then empties it.
                let rep: String = conn
                    .query_row(
                        "SELECT id FROM agents WHERE team_id=?1 AND kind='worker' AND role_slug=?2 AND (?3 IS NULL OR id<>?3) ORDER BY sort, id LIMIT 1",
                        params![d.team_id, r, exclude],
                        |r| r.get(0),
                    )
                    .map_err(e)?;
                conn.execute("UPDATE delegations SET to_agent=?2, updated_at=?3 WHERE id=?1", params![d.id, rep, now()])
                    .map_err(e)?;
                delegate::queue_role(conn, d.id, r, exclude)?;
            }
        }
    }
    if let Some(it) = &job.item {
        // A later plan_retry starts the item on the new target too.
        let (role, agent) = match to {
            To::Agent(a) => (None, Some(a.as_str())),
            To::Role(r) => (Some(r.as_str()), None),
        };
        conn.execute("DELETE FROM plan_targets WHERE item_id=?1", [it.id]).map_err(e)?;
        conn.execute(
            "INSERT INTO plan_targets (item_id, role_slug, agent_id) VALUES (?1, ?2, ?3)",
            params![it.id, role, agent],
        )
        .map_err(e)?;
        conn.execute("UPDATE plan_items SET target=?2 WHERE id=?1", params![it.id, role.or(agent)]).map_err(e)?;
        if job.d.is_none() {
            set_task(conn, job, tasks::TaskPatch { agent_id: Some(agent.map(str::to_owned)), ..Default::default() })?;
        }
    }
    Ok(format!("{} artık {}.", job.label(), describe(conn, to)))
}

/// The job a decision may hand to its new worker: still queued, assigned.
fn queued_job(conn: &Connection, proposal: i64) -> Result<Option<Delegation>, String> {
    let c = change(conn, proposal)?;
    let id = c.delegation.or(match c.plan_item {
        Some(i) => item(conn, "WHERE i.id=?1", [i])?.and_then(|it| it.delegation_id),
        None => None,
    });
    Ok(match id {
        Some(id) => Some(delegate::get(conn, id)?).filter(|d| d.status == "queued" && !d.to_agent.is_empty()),
        None => None,
    })
}

// ---- MCP (lead only; mcp.rs hands over names it doesn't know) ----

pub const TOOLS: [&str; 2] = ["delegation_cancel", "delegation_reassign"];

pub fn tools() -> Vec<Value> {
    let job = || {
        json!({
            "delegationId": { "type": "integer", "description": "Başlamamış (queued) devretme" },
            "planId": { "type": "integer", "description": "Henüz devredilmemiş plan görevi için, key ile" },
            "key": { "type": "string", "description": "planId ile: plan görevinin key'i" },
            "reason": { "type": "string", "minLength": 1, "description": "Patronun göreceği gerekçe" }
        })
    };
    let mut reassign = job();
    reassign["agentId"] = json!({ "type": "string", "description": "Yeni worker (id ya da ad)" });
    reassign["role"] = json!({ "type": "string", "description": "Ya da rol kuyruğu (katalog rolü); o roldeki ilk boş worker alır" });
    vec![
        json!({ "name": "delegation_cancel", "description": "YALNIZ LİDER. Başlamamış bir işi (kuyruktaki devretme ya da bekleyen plan görevi) iptal etmek için patron onayı ister; çağrı hemen uygulamaz. Patron onaylarsa ve iş hâlâ başlamadıysa kapanır; plan görevinin bağımlıları bekler (plan_retry/plan_cancel). Başlamış/bitmiş iş reddedilir. Sonuç sana bildirilir.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["reason"], "properties": job() } }),
        json!({ "name": "delegation_reassign", "description": "YALNIZ LİDER. Başlamamış bir işi başka worker'a (agentId) ya da rol kuyruğuna (role) aktarmak için patron onayı ister; çağrı hemen uygulamaz. Patron onaylarsa ve iş hâlâ başlamadıysa aktarılır (pano kartı, dal bağlamı ve fixOf işle taşınır). Başlamış/bitmiş iş reddedilir. Sonuç sana bildirilir.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["reason"], "properties": reassign } }),
    ]
}

/// None → not one of these tools.
pub fn mcp_call(db: &Db, ev: &Events, c: &Caller, name: &str, args: &Value) -> Option<Result<Value, String>> {
    let action = match name {
        "delegation_cancel" => "cancel",
        "delegation_reassign" => "reassign",
        _ => return None,
    };
    if !c.is_leader() {
        return Some(Err(format!("yetki yok: {name} yalnız takım liderine açık ({} bir worker)", c.name)));
    }
    Some(with(db, |conn| request(conn, c, action, args)).map(|p| {
        let _ = ev.emit("proposals-changed", &());
        json!({ "proposalId": p.id, "status": p.status,
                "note": "Patronun onayına gönderildi; iş o zamana kadar olduğu gibi sürer (bu arada başlarsa istek uygulanmaz). Sonuç sana bildirilir." })
    }))
}

impl Core {
    /// After the patron's decision on a job change: what moved is shown, a reassigned job goes to
    /// its new worker, plans move on (a cancelled item blocks its dependents, a role pool assigns).
    pub(crate) fn job_change_decided(&self, p: &Proposal) {
        self.changed("delegations-changed");
        self.changed("tasks-changed");
        self.changed("plans-changed");
        if p.status == "approved" {
            if let Ok(Some(d)) = with(&self.db, |c| queued_job(c, p.id)) {
                if let Err(err) = self.delegation_dispatch(d) {
                    eprintln!("dex: job change #{}: dispatch: {err}", p.id);
                }
            }
        }
        if let Err(err) = self.plan_resume() {
            eprintln!("dex: job change #{}: plan: {err}", p.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn db() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    fn lead(c: &Connection) -> Caller {
        delegate::caller(c, "timur").unwrap()
    }

    fn job(c: &Connection, to: &str) -> Delegation {
        let lead = lead(c);
        let task = delegate::auto_task(c, to, "default", Some("kart"), "iş", None, &delegate::ProjectSnapshot {
            project_id: None, project_root: None, merge_branch: None, repo_key: None,
        }).unwrap();
        delegate::insert(c, delegate::NewDelegation {
            from: &lead,
            to_agent: to.into(),
            team_id: "default".into(),
            service_team_id: None,
            prompt: "iş".into(),
            task_id: Some(task.id),
            patron_words: None,
            effort: None,
        })
        .unwrap()
    }

    fn decide(c: &Connection, p: &Proposal, approve: bool) -> Proposal {
        crate::proposals::decide(c, Path::new("."), p.id, approve, None).unwrap()
    }

    fn notes(c: &Connection) -> String {
        crate::plan::take_notes(c, "timur").unwrap().unwrap_or_default()
    }

    #[test]
    fn cancel_waits_for_the_patron_then_closes_the_queued_job() {
        let c = db();
        let d = job(&c, "kaan");
        let p = request(&c, &lead(&c), "cancel", &json!({"delegationId": d.id, "reason": "gereksiz"})).unwrap();
        assert_eq!((p.status.as_str(), p.change.as_deref(), p.delegation_id), ("pending", Some("cancel"), None));
        assert!(p.reason.contains(&format!("İş #{}", d.id)) && p.reason.contains("kart") && p.reason.contains("gereksiz"), "{}", p.reason);
        assert_eq!(delegate::get(&c, d.id).unwrap().status, "queued", "nothing changes before the patron");
        assert!(request(&c, &lead(&c), "cancel", &json!({"delegationId": d.id, "reason": "x"})).unwrap_err().contains("zaten var"));

        let done = decide(&c, &p, true);
        assert_eq!(done.status, "approved");
        let after = delegate::get(&c, d.id).unwrap();
        assert_eq!(after.status, "failed");
        assert!(after.summary.unwrap().contains("gereksiz"));
        assert_eq!(tasks::get(&c, after.task_id.unwrap()).unwrap().status, "todo");
        assert!(delegate::take_unnotified(&c, "timur").unwrap().is_empty(), "heard once, as the note");
        assert!(notes(&c).contains("ONAYLANDI ve uygulandı"));
        assert!(crate::proposals::decide(&c, Path::new("."), p.id, true, None).is_err(), "decided once");
    }

    #[test]
    fn rejected_or_started_job_is_left_as_it_is_and_the_leader_hears() {
        let c = db();
        let d = job(&c, "kaan");
        let p = request(&c, &lead(&c), "cancel", &json!({"delegationId": d.id, "reason": "r"})).unwrap();
        assert_eq!(decide(&c, &p, false).status, "rejected");
        assert_eq!(delegate::get(&c, d.id).unwrap().status, "queued");
        assert!(notes(&c).contains("REDDEDİLDİ"));

        // Race: the job started between the request and the approval.
        let p = request(&c, &lead(&c), "reassign", &json!({"delegationId": d.id, "agentId": "ayla", "reason": "r"})).unwrap();
        delegate::set_status(&c, d.id, "running").unwrap();
        decide(&c, &p, true);
        let after = delegate::get(&c, d.id).unwrap();
        assert_eq!((after.status.as_str(), after.to_agent.as_str()), ("running", "kaan"));
        let n = notes(&c);
        assert!(n.contains("UYGULANMADI") && n.contains("running"), "{n}");

        // A start in flight counts as started.
        let q = job(&c, "bora");
        let p = request(&c, &lead(&c), "cancel", &json!({"delegationId": q.id, "reason": "r"})).unwrap();
        crate::proposals::decide_in(&c, Path::new("."), p.id, true, None, &HashSet::from([q.id])).unwrap();
        assert_eq!(delegate::get(&c, q.id).unwrap().status, "queued");
        assert!(notes(&c).contains("başlatılıyor"));
    }

    #[test]
    fn started_or_closed_jobs_and_workers_are_refused() {
        let c = db();
        let d = job(&c, "kaan");
        delegate::set_status(&c, d.id, "running").unwrap();
        let err = request(&c, &lead(&c), "cancel", &json!({"delegationId": d.id, "reason": "r"})).unwrap_err();
        assert!(err.contains("başlamış") && err.contains("running"), "{err}");
        delegate::set_status(&c, d.id, "done").unwrap();
        assert!(request(&c, &lead(&c), "reassign", &json!({"delegationId": d.id, "agentId": "ayla", "reason": "r"})).unwrap_err().contains("done"));

        let q = job(&c, "kaan");
        let worker = delegate::caller(&c, "ayla").unwrap();
        assert!(request(&c, &worker, "cancel", &json!({"delegationId": q.id, "reason": "r"})).unwrap_err().contains("yetki yok"));
        assert!(request(&c, &lead(&c), "cancel", &json!({"delegationId": q.id})).unwrap_err().contains("reason"));
        assert!(request(&c, &lead(&c), "reassign", &json!({"delegationId": q.id, "agentId": "kaan", "reason": "r"})).unwrap_err().contains("zaten"));
        assert!(request(&c, &lead(&c), "reassign", &json!({"delegationId": q.id, "reason": "r"})).unwrap_err().contains("tam biri"));
        c.execute(
            "INSERT INTO delegation_worktrees (delegation_id, agent_id, root, path, branch, base, created_at) VALUES (?1,'kaan','r','p','b','x',1)",
            [q.id],
        )
        .unwrap();
        assert!(request(&c, &lead(&c), "cancel", &json!({"delegationId": q.id, "reason": "r"})).unwrap_err().contains("çalışma dalı"));
        assert!(crate::proposals::list_pending(&c).unwrap().is_empty());
    }

    #[test]
    fn reassign_moves_the_job_its_card_and_its_fix_context() {
        let c = db();
        let d = job(&c, "kaan");
        crate::review::set_fix_of(&c, d.id, 99).unwrap();
        let p = request(&c, &lead(&c), "reassign", &json!({"delegationId": d.id, "agentId": "Ayla", "reason": "kaan meşgul"})).unwrap();
        assert_eq!(p.name.as_deref(), Some("Ayla"));
        assert!(p.reason.contains("Şu an: Kaan") && p.reason.contains("aktarma → Ayla"), "{}", p.reason);
        decide(&c, &p, true);
        let after = delegate::get(&c, d.id).unwrap();
        assert_eq!((after.status.as_str(), after.to_agent.as_str()), ("queued", "ayla"));
        assert_eq!(tasks::get(&c, after.task_id.unwrap()).unwrap().agent_id.as_deref(), Some("ayla"));
        assert_eq!(crate::review::fix_of(&c, d.id).unwrap(), Some(99));
        assert_eq!(queued_job(&c, p.id).unwrap().unwrap().id, d.id);
        assert!(notes(&c).contains("artık Ayla"));

        // To the role pool: the job waits without a recipient, its card unassigned.
        let p = request(&c, &lead(&c), "reassign", &json!({"delegationId": d.id, "role": "qa", "reason": "r"})).unwrap();
        decide(&c, &p, true);
        let pooled = delegate::get(&c, d.id).unwrap();
        assert_eq!((pooled.to_agent.as_str(), pooled.target_role.as_deref()), ("", Some("qa")));
        assert_eq!(tasks::get(&c, pooled.task_id.unwrap()).unwrap().agent_id, None);
        let assigned = delegate::assign_role_jobs(&c).unwrap();
        assert_eq!(assigned.iter().map(|d| d.to_agent.as_str()).collect::<Vec<_>>(), vec!["bora"]);
    }

    #[test]
    fn model_pending_and_gpu_bound_jobs_are_not_moved() {
        let c = db();
        let d = job(&c, "kaan");
        c.execute("UPDATE delegations SET model='opus', model_approval='pending' WHERE id=?1", [d.id]).unwrap();
        assert!(request(&c, &lead(&c), "reassign", &json!({"delegationId": d.id, "agentId": "ayla", "reason": "r"})).unwrap_err().contains("model onayı"));
        let g = job(&c, "bora");
        c.execute_batch(&format!(
            "INSERT INTO gpu_requests (id,team_id,leader,worker,types,reason,budget,created_at) VALUES (1,'default','timur','bora','[]','r',1,1);
             INSERT INTO gpu_grants (request_id,team_id,worker,types,budget,delegation_id,created_at) VALUES (1,'default','bora','[]',1,{},1);",
            g.id
        ))
        .unwrap();
        assert!(request(&c, &lead(&c), "reassign", &json!({"delegationId": g.id, "agentId": "ayla", "reason": "r"})).unwrap_err().contains("GPU"));
    }

    fn plan(c: &Connection) -> i64 {
        let p: crate::plan::NewPlan = serde_json::from_value(json!({ "title": "t", "tasks": [
            {"key":"A","title":"a","prompt":"a","agentId":"kaan"},
            {"key":"B","title":"b","prompt":"b","agentId":"kaan","dependsOn":["A"]},
            {"key":"C","title":"c","prompt":"c","agentId":"ayla","dependsOn":["B"]}
        ]}))
        .unwrap();
        let id = crate::plan::create(c, &lead(c), p).unwrap();
        crate::plan::step(c).unwrap();
        id
    }

    fn status(c: &Connection, plan: i64) -> Vec<(String, String)> {
        crate::plan::get(c, plan).unwrap().items.into_iter().map(|i| (i.key, i.status)).collect()
    }

    #[test]
    fn plan_task_cancel_blocks_dependents_and_retry_restores_them() {
        let c = db();
        let id = plan(&c);
        let b = json!({"planId": id, "key": "B", "reason": "gerek kalmadı"});
        let p = request(&c, &lead(&c), "cancel", &b).unwrap();
        assert!(p.reason.contains("görev B") && p.reason.contains("bağlı görevler bekler"), "{}", p.reason);
        decide(&c, &p, true);
        crate::plan::step(&c).unwrap();
        let s = status(&c, id);
        assert_eq!(s[1], ("B".into(), "failed".into()));
        assert_eq!(s[2], ("C".into(), "blocked".into()));
        assert!(notes(&c).contains("plan_retry"));
        crate::plan::retry(&c, &lead(&c), id, "B").unwrap();
        assert_eq!(status(&c, id)[2], ("C".into(), "waiting".into()));

        // A's job is queued at its worker: cancelling it by delegationId fails the item the same way.
        let a = crate::plan::get(&c, id).unwrap().items[0].delegation_id.unwrap();
        let p = request(&c, &lead(&c), "cancel", &json!({"delegationId": a, "reason": "r"})).unwrap();
        decide(&c, &p, true);
        crate::plan::step(&c).unwrap();
        assert_eq!(status(&c, id).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), vec!["failed", "blocked", "blocked"]);
        assert_eq!(delegate::get(&c, a).unwrap().status, "failed");
    }

    #[test]
    fn plan_task_reassign_retargets_waiting_and_queued_items() {
        let c = db();
        let id = plan(&c);
        let p = request(&c, &lead(&c), "reassign", &json!({"planId": id, "key": "B", "agentId": "bora", "reason": "r"})).unwrap();
        decide(&c, &p, true);
        let a = crate::plan::get(&c, id).unwrap().items[0].delegation_id.unwrap();
        let p = request(&c, &lead(&c), "reassign", &json!({"delegationId": a, "agentId": "ayla", "reason": "r"})).unwrap();
        decide(&c, &p, true);
        assert_eq!(delegate::get(&c, a).unwrap().to_agent, "ayla");
        let pinned: Vec<Option<String>> = c
            .prepare("SELECT t.agent_id FROM plan_items i JOIN plan_targets t ON t.item_id=i.id WHERE i.plan_id=?1 ORDER BY i.id")
            .unwrap()
            .query_map([id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(pinned, vec![Some("ayla".into()), Some("bora".into()), Some("ayla".into())]);
        // B starts on its new worker once A is done.
        delegate::set_status(&c, a, "done").unwrap();
        crate::plan::step(&c).unwrap();
        let b = crate::plan::get(&c, id).unwrap().items[1].delegation_id.unwrap();
        assert_eq!(delegate::get(&c, b).unwrap().to_agent, "bora");
    }

    #[test]
    fn schema_applies_twice_and_card_lists_with_its_action() {
        let c = db();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        let d = job(&c, "kaan");
        request(&c, &lead(&c), "cancel", &json!({"delegationId": d.id, "reason": "r"})).unwrap();
        let rows = crate::proposals::list_pending(&c).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].change.as_deref(), Some("cancel"));
    }
}
