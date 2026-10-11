//! Worker → leader questions (DEX-24). A worker on a delegation asks with `ask_leader` instead of
//! the patron; the question reaches its leader the way a finished result does (DEX-19: Stop block
//! while working, pasted ping + UserPromptSubmit context while idle, SessionStart context while
//! closed). The leader `answer`s or `escalate`s to the patron (UI card); the answer reaches the
//! worker in the blocking MCP call. Hook delivery remains a fallback for ended turns.
//! Every change emits `questions-changed`; the database owns delivery deduplication.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};

use crate::db::{with, Db};
use crate::delegate::Caller;
use crate::events::Events;
use crate::Core;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

// Claude's HTTP transport defaults to a 60-second request timeout.
// Return before that deadline; wait_answer continues the same question and turn.
const WAIT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Default)]
pub(crate) struct Answers {
    gate: Mutex<()>,
    changed: Condvar,
}

impl Answers {
    pub(crate) fn notify(&self) {
        let _guard = self.gate.lock().unwrap();
        self.changed.notify_all();
    }

    fn wait(&self, db: &Db, worker: &Caller, id: i64, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.gate.lock().map_err(|_| "cevap kilidi zehirli")?;
        loop {
            // Always lock notification before DB; publishers notify only AFTER releasing DB.
            let q = with(db, |conn| {
                let q = get(conn, id)?;
                if worker.is_leader() || q.worker != worker.id {
                    return Err("yalnız kendi sorunun cevabını bekleyebilirsin".into());
                }
                if q.status == "answered" && q.answer_delivered_at.is_none() {
                    conn.execute("UPDATE questions SET answer_delivered_at = ?2 WHERE id = ?1", params![id, now()]).map_err(e)?;
                }
                Ok(q)
            })?;
            if q.status == "answered" {
                return Ok(if q.answer_delivered_at.is_some() {
                    json!({"questionId": id, "status": "delivered", "note": "Cevap bu oturuma zaten teslim edildi; cevaba göre devam et."})
                } else {
                    json!({"questionId": id, "delegationId": q.delegation_id, "status": "answered", "answer": q.answer,
                        "answeredBy": q.answered_by, "note": "Cevaba göre aynı turda devam et; iş bitince report ver."})
                });
            }
            if q.delegation_status != "running" {
                return Err(format!("soru #{id} için devretme artık çalışmıyor"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(json!({"questionId": id, "status": "pending", "note": format!("Henüz cevap yok (#{id}); wait_answer({{questionId: {id}}}) ile aynı turda beklemeye devam et. Turu bitirme.")}));
            }
            guard = self.changed.wait_timeout(guard, remaining).map_err(|_| "cevap kilidi zehirli")?.0;
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: i64,
    pub delegation_id: i64,
    /// Status of that delegation; the dock marks only questions of work still going on.
    pub delegation_status: String,
    pub worker: String,
    pub worker_name: String,
    pub leader: String,
    pub leader_name: String,
    pub team_id: String,
    pub team_name: String,
    pub question: String,
    pub options: Vec<String>,
    pub context: Option<String>,
    /// open → the leader has it; escalated → the patron has it; answered.
    pub status: String,
    /// The leader's note when escalating.
    pub note: Option<String>,
    pub answer: Option<String>,
    /// leader | patron
    pub answered_by: Option<String>,
    pub leader_delivered_at: Option<i64>,
    pub answer_delivered_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "q.id, q.delegation_id, IFNULL(d.status, 'failed'), q.worker, IFNULL(w.name, q.worker), q.leader,
    IFNULL(l.name, q.leader), q.team_id, IFNULL(t.name, q.team_id), q.question, q.options, q.context, q.status, q.note,
    q.answer, q.answered_by, q.leader_delivered_at, q.answer_delivered_at, q.created_at, q.updated_at";
const FROM: &str = "questions q LEFT JOIN delegations d ON d.id = q.delegation_id LEFT JOIN agents w ON w.id = q.worker
    LEFT JOIN agents l ON l.id = q.leader LEFT JOIN teams t ON t.id = q.team_id";

fn row(r: &Row) -> rusqlite::Result<Question> {
    let options: String = r.get(10)?;
    Ok(Question {
        id: r.get(0)?,
        delegation_id: r.get(1)?,
        delegation_status: r.get(2)?,
        worker: r.get(3)?,
        worker_name: r.get(4)?,
        leader: r.get(5)?,
        leader_name: r.get(6)?,
        team_id: r.get(7)?,
        team_name: r.get(8)?,
        question: r.get(9)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        context: r.get(11)?,
        status: r.get(12)?,
        note: r.get(13)?,
        answer: r.get(14)?,
        answered_by: r.get(15)?,
        leader_delivered_at: r.get(16)?,
        answer_delivered_at: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
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

pub fn get(conn: &Connection, id: i64) -> Result<Question, String> {
    conn.query_row(&format!("SELECT {COLS} FROM {FROM} WHERE q.id = ?1"), [id], row)
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("soru yok: {id}"))
}

fn select(conn: &Connection, filter: &str, p: impl rusqlite::Params) -> Result<Vec<Question>, String> {
    let mut st = conn.prepare(&format!("SELECT {COLS} FROM {FROM} WHERE {filter} ORDER BY q.id")).map_err(e)?;
    let rows = st.query_map(p, row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

/// For the UI: unanswered ones, plus the last day's answered ones (hover lines).
pub fn list_recent(conn: &Connection) -> Result<Vec<Question>, String> {
    select(conn, "q.status <> 'answered' OR q.updated_at > ?1", [now() - 24 * 3600 * 1000])
}

// ---- asking / answering ----

/// Worker side: the question joins the delegation the worker is running (or `delegationId`).
pub fn ask(conn: &Connection, c: &Caller, args: &Value) -> Result<Question, String> {
    if c.is_leader() {
        return Err("ask_leader worker'lar içindir; lider olarak belirsizliği patrona kendin sor".into());
    }
    let question = args.get("question").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()).ok_or("question gerekli")?;
    let options: Vec<String> = match args.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| "options bir metin dizisi olmalı")?,
    };
    let context = args.get("context").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let deleg: Option<(i64, String, String)> = match args.get("delegationId").and_then(Value::as_i64) {
        Some(id) => conn
            .query_row(
                "SELECT id, from_agent, team_id FROM delegations WHERE id = ?1 AND to_agent = ?2 AND status = 'running'",
                params![id, c.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(e)?,
        None => conn
            .query_row(
                "SELECT id, from_agent, team_id FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
                [&c.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(e)?,
    };
    let (delegation, leader, team) =
        deleg.ok_or("şu an sana devredilmiş, süren bir iş yok; ask_leader yalnız devretme sırasında kullanılır")?;
    let ts = now();
    conn.execute(
        "INSERT INTO questions (delegation_id, worker, leader, team_id, question, options, context, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![delegation, c.id, leader, team, question, json!(options).to_string(), context, ts],
    )
    .map_err(e)?;
    crate::diagnostics::event("question", json!({"id": conn.last_insert_rowid(), "agent": c.id, "status": "open", "bytes": question.len()}));
    get(conn, conn.last_insert_rowid())
}

fn own(conn: &Connection, c: &Caller, id: i64) -> Result<Question, String> {
    let q = get(conn, id)?;
    if !c.is_leader() || q.leader != c.id {
        return Err(format!("soru #{id} sana sorulmadı (lider: {})", q.leader_name));
    }
    if q.status == "answered" {
        return Err(format!("soru #{id} zaten cevaplandı ({})", q.answered_by.as_deref().unwrap_or("?")));
    }
    Ok(q)
}

fn set_answer(conn: &Connection, id: i64, text: &str, by: &str) -> Result<Question, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("cevap boş olamaz".into());
    }
    conn.execute(
        "UPDATE questions SET status = 'answered', answer = ?2, answered_by = ?3, answer_delivered_at = NULL, updated_at = ?4 WHERE id = ?1",
        params![id, text, by, now()],
    )
    .map_err(e)?;
    crate::diagnostics::event("question", json!({"id": id, "status": "answered", "bytes": text.len()}));
    get(conn, id)
}

/// Leader side. An escalated question can still be answered by the leader (first answer wins).
pub fn answer(conn: &Connection, c: &Caller, id: i64, text: &str) -> Result<Question, String> {
    own(conn, c, id)?;
    set_answer(conn, id, text, "leader")
}

pub fn escalate(conn: &Connection, c: &Caller, id: i64, note: Option<&str>) -> Result<Question, String> {
    let q = own(conn, c, id)?;
    if q.status == "escalated" {
        return Err(format!("soru #{id} zaten patronda"));
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    conn.execute("UPDATE questions SET status = 'escalated', note = ?2, updated_at = ?3 WHERE id = ?1", params![id, note, now()])
        .map_err(e)?;
    crate::diagnostics::event("question", json!({"id": id, "status": "escalated"}));
    get(conn, id)
}

/// The patron's answer (UI card). The leader hears it as a note (woken if idle: `Core::question_answer`).
pub fn patron_answer(conn: &Connection, id: i64, text: &str) -> Result<Question, String> {
    let q = get(conn, id)?;
    if q.status == "answered" {
        return Err(format!("soru #{id} zaten cevaplandı"));
    }
    let q = set_answer(conn, id, text, "patron")?;
    crate::plan::note(
        conn,
        &q.leader,
        &format!("Patron #{} sorusunu cevapladı (soran: {}; cevap worker'a iletildi): {}", q.id, q.worker_name, q.answer.as_deref().unwrap_or("")),
    )?;
    Ok(q)
}

// ---- delivery (callers hold the db lock: each row is handed out once) ----

fn take(conn: &Connection, filter: &str, stamp: &str, agent: &str) -> Result<Vec<Question>, String> {
    let ids: Vec<i64> = conn
        .prepare(&format!("SELECT id FROM questions WHERE {filter} ORDER BY id"))
        .map_err(e)?
        .query_map([agent], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    let ts = now();
    ids.into_iter()
        .map(|id| {
            conn.execute(&format!("UPDATE questions SET {stamp} = ?2 WHERE id = ?1"), params![id, ts]).map_err(e)?;
            get(conn, id)
        })
        .collect()
}

/// Open questions that have not reached the leader yet.
pub(crate) fn pending_for_leader(conn: &Connection, leader: &str) -> Result<Vec<Question>, String> {
    select(conn, "q.leader=?1 AND q.status='open' AND q.leader_delivered_at IS NULL", [leader])
}

/// Open questions that have not reached the leader yet, marked delivered by hook context.
pub fn take_for_leader(conn: &Connection, leader: &str) -> Result<Vec<Question>, String> {
    take(conn, "leader = ?1 AND status = 'open' AND leader_delivered_at IS NULL", "leader_delivered_at", leader)
}

/// Answers that have not reached the worker yet.
pub fn take_answers(conn: &Connection, worker: &str) -> Result<Vec<Question>, String> {
    take(conn, "worker = ?1 AND status = 'answered' AND answer_delivered_at IS NULL", "answer_delivered_at", worker)
}

/// The worker asked about its running job and the answer has not reached it yet.
fn waiting(conn: &Connection, worker: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM questions q JOIN delegations d ON d.id = q.delegation_id
             WHERE q.worker = ?1 AND d.status = 'running' AND q.answer_delivered_at IS NULL)",
        [worker],
        |r| r.get(0),
    )
    .map_err(e)
}

/// Scope to both the owner and the job; the worker/status index bounds this lookup.
pub(crate) fn unanswered(conn: &Connection, worker: &str, delegation: i64) -> Result<Option<i64>, String> {
    conn.query_row(
        "SELECT id FROM questions WHERE worker = ?1 AND delegation_id = ?2 AND status IN ('open','escalated') ORDER BY id LIMIT 1",
        params![worker, delegation],
        |r| r.get(0),
    )
    .optional()
    .map_err(e)
}

/// A worker on a delegation (queued or running): `AskUserQuestion` goes to its leader instead.
fn delegated(conn: &Connection, agent: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM delegations d JOIN agents a ON a.id = d.to_agent
             WHERE d.to_agent = ?1 AND a.kind = 'worker' AND d.status IN ('queued','running'))",
        [agent],
        |r| r.get(0),
    )
    .map_err(e)
}

pub(crate) fn leader_text(qs: &[Question], header: &str) -> String {
    let mut s = format!("# Dex: worker soruları ({header})\nHer biri için `answer` (questionId, text) ile cevapla; cevaplayamıyorsan `escalate` ile patrona çıkar.\n");
    for q in qs {
        s.push_str(&format!("- soru #{} — {} (devretme #{}): {}", q.id, q.worker_name, q.delegation_id, q.question.replace('\n', "\n  ")));
        if !q.options.is_empty() {
            s.push_str(&format!("\n  seçenekler: {}", q.options.join(" | ")));
        }
        if let Some(ctx) = &q.context {
            s.push_str(&format!("\n  bağlam: {}", ctx.replace('\n', "\n  ")));
        }
        s.push('\n');
    }
    s
}

fn answer_text(qs: &[Question]) -> String {
    let mut s = String::from("# Dex: sorularının cevabı geldi\n");
    for q in qs {
        let by = if q.answered_by.as_deref() == Some("patron") { "patron" } else { q.leader_name.as_str() };
        s.push_str(&format!(
            "- soru #{} (devretme #{}): {}\n  cevap ({by}): {}\n",
            q.id,
            q.delegation_id,
            q.question.replace('\n', " "),
            q.answer.as_deref().unwrap_or("").replace('\n', "\n  ")
        ));
    }
    s.push_str("Bu cevapla devretmedeki işe devam et; bitince `report` ile teslim et.\n");
    s
}

/// Adds `text` to a hook answer: Stop → block reason (blocks if it didn't), UserPromptSubmit →
/// additionalContext. Same shape as `plan::with_notes`.
fn merge(event: &str, out: Option<String>, text: &str) -> Option<String> {
    let mut v: Value = match &out {
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => return out,
        },
        None => json!({}),
    };
    if event == "Stop" {
        let reason = v.get("reason").and_then(Value::as_str).map(|r| format!("{r}\n\n{text}")).unwrap_or_else(|| text.to_string());
        v["decision"] = json!("block");
        v["reason"] = json!(reason);
    } else {
        let ctx = v
            .pointer("/hookSpecificOutput/additionalContext")
            .and_then(Value::as_str)
            .map(|c| format!("{c}\n{text}"))
            .unwrap_or_else(|| text.to_string());
        v["hookSpecificOutput"] = json!({ "hookEventName": "UserPromptSubmit", "additionalContext": ctx });
    }
    Some(v.to_string())
}

const DENY_ASK: &str = "Dex: devretmedeki bir worker patrona doğrudan soru sormaz. Belirsizliği dex MCP'sinin \
    `ask_leader` aracıyla liderine sor (question, varsa options/context); araç cevabı bekler. \
    Zaman aşımında `wait_answer` ile aynı turda bekle; turu bitirme.";

/// Hook side, around the delegation hooks (`inner`): a worker waiting for an answer keeps its job
/// (its Stop does not close it); answers and questions ride along; `AskUserQuestion` is denied to
/// delegated workers. Permission prompts (PermissionRequest) are untouched.
pub fn on_hook(db: &Db, ev: &Events, agent: &str, event: &str, body: &Value, inner: impl FnOnce() -> Option<String>) -> Option<String> {
    on_hook_pending(db, ev, agent, event, body, None, inner)
}

pub(crate) fn on_hook_pending(db: &Db, ev: &Events, agent: &str, event: &str, body: &Value, pending: Option<&crate::hook_delivery::Pending>, inner: impl FnOnce() -> Option<String>) -> Option<String> {
    let out = question_hook_pending(db, ev, agent, event, body, pending, inner);
    // PreToolUse belongs to the permission decision; notes ride the next context hook.
    if !matches!(event, "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure") { return out; }
    match with(db, |c| {
        let notes = crate::worker_notes::prepare(c, agent, body, &out)?;
        if notes.is_empty() { return Ok(out.clone()); }
        let merged = crate::worker_notes::merge(event, out.clone(), &crate::worker_notes::text(&notes))?;
        Ok(merged)
    }) {
        Ok(out) => out,
        Err(err) => { eprintln!("dex worker note hook: {err}"); out }
    }
}

fn question_hook_pending(db: &Db, ev: &Events, agent: &str, event: &str, body: &Value, pending: Option<&crate::hook_delivery::Pending>, inner: impl FnOnce() -> Option<String>) -> Option<String> {
    let r = with(db, |c| -> Result<Option<Option<String>>, String> {
        match event {
            "PreToolUse" => {
                let ask = body.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion");
                if ask && delegated(c, agent)? {
                    return Ok(Some(Some(
                        json!({ "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": DENY_ASK
                        }})
                        .to_string(),
                    )));
                }
                Ok(None)
            }
            "Stop" => {
                let notes = crate::worker_notes::pending(c, agent)?;
                let answers = if let Some(pending) = pending {
                    let answers = select(c, "q.worker=?1 AND q.status='answered' AND q.answer_delivered_at IS NULL", [agent])?;
                    pending.0.lock().unwrap().answers.extend(answers.iter().map(|q| q.id));
                    answers
                } else { take_answers(c, agent)? };
                if !answers.is_empty() || !notes.is_empty() {
                    let _ = ev.emit("questions-changed", &());
                    let mut out = if answers.is_empty() { None } else { merge("Stop", None, &answer_text(&answers)) };
                    if !notes.is_empty() {
                        out = crate::worker_notes::merge("Stop",out,&crate::worker_notes::text(&notes))?;
                        let ids = notes.iter().map(|n| n.id).collect::<Vec<_>>();
                        if let Some(pending) = pending { pending.0.lock().unwrap().worker_notes.extend(ids); }
                    }
                    return Ok(Some(out));
                }
                // Asked and ended the turn: the job is not over, it waits for the answer.
                Ok(waiting(c, agent)?.then_some(None))
            }
            _ => Ok(None),
        }
    });
    match r {
        Ok(Some(out)) => return out,
        Ok(None) => {}
        Err(err) => eprintln!("dex questions hook {event} {agent}: {err}"),
    }
    let mut out = inner();
    if !matches!(event, "Stop" | "UserPromptSubmit") {
        return out;
    }
    let taken = with(db, |c| {
        let answers = if event == "UserPromptSubmit" { take_answers(c, agent)? } else { Vec::new() };
        let asked = if let Some(pending) = pending {
            let asked = pending_for_leader(c, agent)?;
            pending.0.lock().unwrap().questions.extend(asked.iter().map(|q| q.id));
            asked
        } else { take_for_leader(c, agent)? };
        Ok((answers, asked))
    });
    let (answers, asked) = match taken {
        Ok(t) => t,
        Err(err) => {
            eprintln!("dex questions hook {event} {agent}: {err}");
            return out;
        }
    };
    if !answers.is_empty() {
        out = merge(event, out, &answer_text(&answers));
    }
    if !asked.is_empty() {
        let header = if event == "Stop" { "sen çalışırken geldi" } else { "son mesajından beri" };
        out = merge(event, out, &leader_text(&asked, header));
    }
    if !answers.is_empty() || !asked.is_empty() {
        let _ = ev.emit("questions-changed", &());
    }
    out
}

/// A leader session (re)opened: questions that came while it was closed, as SessionStart context.
pub fn on_session_start(db: &Db, ev: &Events, agent: &str) -> Option<String> {
    match with(db, |c| take_for_leader(c, agent)) {
        Ok(qs) if !qs.is_empty() => {
            let _ = ev.emit("questions-changed", &());
            Some(leader_text(&qs, "sen kapalıyken geldi"))
        }
        Ok(_) => None,
        Err(err) => {
            eprintln!("dex questions SessionStart {agent}: {err}");
            None
        }
    }
}

// ---- MCP ----

/// Görev #23: `answer` may carry `permit: P` (+ `scope`). Checked before anything is written: the
/// caller must own the question (leader), and #P must be a pending request of that question's
/// worker — a leader cannot open another worker's request, and a denied one never gets a number.
fn answer_with_permit(db: &Db, permits: &crate::permit::Permits, c: &Caller, id: i64, text: &str, args: &Value) -> Result<Value, String> {
    use crate::permit::Scope;
    let permit = args.get("permit").and_then(Value::as_i64);
    let scope = match args.get("scope").and_then(Value::as_str) {
        None | Some("once") => Scope::Once,
        Some("session") => Scope::Session,
        Some(other) => return Err(format!("scope: once ya da session olmalı, `{other}` değil")),
    };
    let (q, job) = with(db, |conn| {
        let q = own(conn, c, id)?;
        if let Some(p) = permit {
            let ask = permits.get(p).ok_or_else(|| format!("izin #{p} bekleyen bir istek değil (süresi geçmiş ya da hiç açılmamış)"))?;
            if ask.agent != q.worker {
                return Err(format!("izin #{p} bu sorunun worker'ına ({}) ait değil", q.worker_name));
            }
        }
        let job: Option<i64> = conn
            .query_row("SELECT id FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id LIMIT 1", [&q.worker], |r| r.get(0))
            .optional()
            .map_err(e)?;
        Ok((answer(conn, c, id, text)?, job))
    })?;
    let mut note = format!("Cevap worker'a ({}) teslim edilecek.", q.worker_name);
    if let Some(ask) = permit.and_then(|p| permits.grant(p, scope, job)) {
        let how = if scope == Scope::Once { "bir kez" } else { "bu iş ve oturum süresince" };
        eprintln!("dex permit {}: izin #{} verildi ({how}): {}", q.worker, ask.id, crate::permit::redact(&ask.what));
        note.push_str(&format!(" İzin #{} verildi ({how}): `{}`.", ask.id, ask.what));
    }
    Ok(json!({ "ok": true, "questionId": q.id, "note": note }))
}

pub fn tools() -> Vec<Value> {
    vec![
        json!({ "name": "ask_leader", "description": "YALNIZ DEVRETMEDEKİ WORKER. Belirsizliği liderine sor; araç cevabı en fazla 45 saniye bloklayarak bekler. Cevap gelince aynı turda işe devam et. pending dönerse questionId ile wait_answer çağır; turu bitirme. report'u cevap geldikten ve iş bittikten sonra ver; cevaplanmamış sorun varsa reddedilir.",
          "inputSchema": { "type": "object", "required": ["question"], "properties": {
              "question": { "type": "string" },
              "options": { "type": "array", "items": { "type": "string" }, "description": "Varsa seçenekler" },
              "context": { "type": "string", "description": "Liderin karar vermesi için kısa bağlam" },
              "delegationId": { "type": "integer", "description": "Verilmezse süren son devretmen" } } } }),
        json!({ "name": "wait_answer", "description": "YALNIZ WORKER. Kendi sorunun cevabını aynı turda en fazla 45 saniye bekle. pending dönerse aynı questionId ile tekrar çağır; turu bitirme. Cevaba göre işe devam et.",
          "inputSchema": { "type": "object", "required": ["questionId"], "additionalProperties": false,
              "properties": { "questionId": { "type": "integer" } } } }),
        json!({ "name": "answer", "description": "YALNIZ LİDER. Worker'ının sorusunu cevaplar; cevap worker'a teslim edilir. Soru bir Dex izin isteğiyse (izin #P) `permit` ile onaylayabilirsin: yalnız o worker'ın o birebir isteği geçer; ret listesindekiler (git push vb.) açılamaz.",
          "inputSchema": { "type": "object", "required": ["questionId", "text"], "properties": {
              "questionId": { "type": "integer" }, "text": { "type": "string" },
              "permit": { "type": "integer", "description": "Onaylanan izin isteği #P" },
              "scope": { "type": "string", "enum": ["once", "session"], "description": "once (varsayılan): bir kez; session: bu iş ve oturum süresince aynı birebir istek" } } } }),
        json!({ "name": "escalate", "description": "YALNIZ LİDER. Cevaplayamadığın worker sorusunu patrona çıkarır (Dex arayüzünde kart). Patronun cevabı worker'a gider, sana da bildirilir.",
          "inputSchema": { "type": "object", "required": ["questionId"], "properties": {
              "questionId": { "type": "integer" }, "note": { "type": "string", "description": "Patrona kısa not" } } } }),
    ]
}

/// None → not one of these tools.
pub(crate) fn mcp_call(db: &Db, ev: &Events, permits: &crate::permit::Permits, answers: &Answers, c: &Caller, name: &str, args: &Value) -> Option<Result<Value, String>> {
    let id = || args.get("questionId").and_then(Value::as_i64).ok_or_else(|| "questionId gerekli".to_string());
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let r = match name {
        "ask_leader" => with(db, |conn| ask(conn, c, args)).and_then(|q| {
            let _ = ev.emit("questions-changed", &());
            answers.wait(db, c, q.id, WAIT_TIMEOUT)
        }),
        "wait_answer" => id().and_then(|id| answers.wait(db, c, id, WAIT_TIMEOUT)),
        "answer" => id().and_then(|id| answer_with_permit(db, permits, c, id, text("text").unwrap_or(""), args)),
        "escalate" => id().and_then(|id| with(db, |conn| escalate(conn, c, id, text("note"))))
            .map(|q| json!({ "ok": true, "questionId": q.id, "note": "Patrona çıkarıldı. Patronun cevabı worker'a gider; sana da bildirilir (boştaysan yeni mesajla, çalışıyorsan tur sonunda)." })),
        _ => return None,
    };
    if r.is_ok() {
        answers.notify();
        let _ = ev.emit("questions-changed", &());
    }
    Some(r)
}

impl Core {
    pub fn question_list(&self) -> Result<Vec<Question>, String> {
        with(&self.db, list_recent)
    }

    /// The patron answers an escalated question from its card; an idle leader is woken with it.
    pub fn question_answer(&self, id: i64, text: &str) -> Result<Question, String> {
        let q = with(&self.db, |c| patron_answer(c, id, text))?;
        self.answers.notify();
        let _ = self.events.emit("questions-changed", &());
        self.leader_notes_wake(&q.leader);
        Ok(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegate::{caller, insert, set_status, NewDelegation};
    use std::path::Path;

    fn setup() -> (Db, Events, i64) {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let l = caller(&c, "timur").unwrap();
        let d = insert(&c, NewDelegation { from: &l, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        set_status(&c, d.id, "running").unwrap();
        (Db(std::sync::Arc::new(std::sync::Mutex::new(c))), Events::new(crate::events::NullSink), d.id)
    }

    fn hook(db: &Db, ev: &Events, agent: &str, event: &str, body: Value) -> Option<Value> {
        let inner = || crate::delegate::on_hook(db, ev, agent, event, &body);
        on_hook(db, ev, agent, event, &body, inner).map(|s| serde_json::from_str(&s).unwrap())
    }

    fn status(db: &Db, id: i64) -> String {
        with(db, |c| crate::delegate::get(c, id)).unwrap().status
    }

    #[test]
    fn ask_blocks_until_answer_and_does_not_repeat_at_stop() {
        let (db, ev, _) = setup();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread_db = db.clone();
        let thread_ev = ev.clone();
        let answers = std::sync::Arc::new(Answers::default());
        let thread_answers = answers.clone();
        let waiter = std::thread::spawn(move || {
            let worker = with(&thread_db, |c| caller(c, "kaan")).unwrap();
            tx.send(mcp_call(&thread_db, &thread_ev, &crate::permit::Permits::default(), &thread_answers, &worker,
                "ask_leader", &json!({"question": "Karar?"})).unwrap()).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_millis(150)).is_err(), "ask_leader must wait for its answer");
        let (leader, id) = with(&db, |c| Ok((caller(c, "timur")?, list_recent(c)?.pop().unwrap().id))).unwrap();
        mcp_call(&db, &ev, &crate::permit::Permits::default(), &answers, &leader, "answer",
            &json!({"questionId": id, "text": "devam"})).unwrap().unwrap();
        let reply = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(reply["answer"], "devam");
        waiter.join().unwrap();
        assert!(with(&db, |c| take_answers(c, "kaan")).unwrap().is_empty());
        assert!(!hook(&db, &ev, "kaan", "Stop", json!({})).unwrap_or_default().to_string().contains("devam"));
    }

    #[test]
    fn timeout_can_continue_with_wait_answer_without_creating_another_question() {
        let (db, ev, _) = setup();
        let answers = Answers::default();
        let worker = with(&db, |c| caller(c, "kaan")).unwrap();
        let q = with(&db, |c| ask(c, &worker, &json!({"question": "Karar?"}))).unwrap();
        let pending = answers.wait(&db, &worker, q.id, Duration::from_millis(5)).unwrap();
        assert_eq!(pending["status"], "pending");
        assert_eq!(pending["questionId"], q.id);
        assert!(pending["note"].as_str().unwrap().contains("wait_answer"));
        with(&db, |c| answer(c, &caller(c, "timur")?, q.id, "devam")).unwrap();
        let reply = mcp_call(&db, &ev, &crate::permit::Permits::default(), &answers, &worker,
            "wait_answer", &json!({"questionId": q.id})).unwrap().unwrap();
        assert_eq!(reply["answer"], "devam");
        assert_eq!(with(&db, list_recent).unwrap().len(), 1);
        assert!(with(&db, |c| take_answers(c, "kaan")).unwrap().is_empty());
        assert_eq!(answers.wait(&db, &worker, q.id, Duration::ZERO).unwrap()["status"], "delivered");
    }

    #[test]
    fn wait_checks_ownership_and_preserves_hook_fallback() {
        let (db, ev, _) = setup();
        let answers = Answers::default();
        let worker = with(&db, |c| caller(c, "kaan")).unwrap();
        let q = with(&db, |c| ask(c, &worker, &json!({"question": "Karar?"}))).unwrap();
        for agent in ["ayla", "timur"] {
            let other = with(&db, |c| caller(c, agent)).unwrap();
            assert!(answers.wait(&db, &other, q.id, Duration::ZERO).is_err());
        }
        assert!(answers.wait(&db, &worker, -1, Duration::ZERO).is_err());
        answers.wait(&db, &worker, q.id, Duration::ZERO).unwrap();
        with(&db, |c| patron_answer(c, q.id, "devam")).unwrap();
        assert!(hook(&db, &ev, "kaan", "Stop", json!({})).unwrap().to_string().contains("devam"));
        let reply = answers.wait(&db, &worker, q.id, Duration::ZERO).unwrap();
        assert_eq!(reply["status"], "delivered");
        assert!(reply.get("answer").is_none());
    }

    #[test]
    fn waiting_worker_keeps_its_session_and_queued_job_until_answer() {
        struct Sink;
        impl crate::PtySink for Sink {
            fn data(&mut self, _: &[u8]) -> bool { true }
            fn exit(&mut self) {}
        }
        let root = std::env::temp_dir().join(format!("dex-question-session-{}", std::process::id()));
        let core = Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(crate::events::NullSink),
        ).unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        core.pty_spawn(crate::pty::Spawn {
            id: "kaan".into(), cmd: crate::shell::SHELL_CMD.into(),
            cwd: Some(root.join("cwd").to_string_lossy().into_owned()), cols: 80, rows: 24,
            ..Default::default()
        }, Sink).unwrap();
        let (d, next, q) = with(&core.db, |c| {
            let leader = caller(c, "timur")?;
            let new_job = || insert(c, NewDelegation { from: &leader, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None });
            let d = new_job()?;
            set_status(c, d.id, "running")?;
            let q = ask(c, &caller(c, "kaan")?, &json!({ "question": "Onay?" }))?;
            Ok((d, new_job()?, q))
        }).unwrap();
        core.delegation_on_hook("kaan", "UserPromptSubmit", &json!({ "prompt": crate::delegate::trigger_line(d.id) }), false);
        // No status hook was recorded: silence is already beyond the ten-minute threshold.
        assert!(core.agent_states.silent_for("kaan") >= crate::agents::IDLE_CLOSE_MS);
        for escalated in [false, true] {
            if escalated {
                with(&core.db, |c| escalate(c, &caller(c, "timur")?, q.id, None)).unwrap();
            }
            for _ in 0..2 {
                assert!(core.delegation_on_hook("kaan", "Stop", &json!({}), false).is_none());
                assert_eq!(status(&core.db, d.id), "running");
            }
            assert!(!core.idle_close_now("kaan"));
            assert_eq!(core.delegation_dispatch(next.clone()).unwrap().status, "queued");
            assert!(core.ptys.is_running("kaan"));
            assert!(!core.resetting.lock().unwrap().contains_key("kaan"));
        }
        with(&core.db, |c| answer(c, &caller(c, "timur")?, q.id, "devam")).unwrap();
        let out = core.delegation_on_hook("kaan", "UserPromptSubmit", &json!({ "prompt": "[Dex] sorunun cevabı geldi" }), false).unwrap();
        assert!(out.contains(&format!("devretme #{}", d.id)) && out.contains("devam"));
        assert_eq!(status(&core.db, d.id), "running");
        assert_eq!(core.delegation_dispatch(next).unwrap().status, "queued");
        assert!(core.ptys.is_running("kaan"));
        assert!(!core.resetting.lock().unwrap().contains_key("kaan"));
        assert!(with(&core.db, |c| take_answers(c, "kaan")).unwrap().is_empty());
        // Keep teardown from starting the queued job in a real agent process.
        core.chain_guard.lock().unwrap().insert("kaan".into());
        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn report_waits_for_all_questions_of_its_delegation() {
        let (db, ev, d) = setup();
        let (worker, leader) = with(&db, |c| Ok((caller(c, "kaan")?, caller(c, "timur")?))).unwrap();
        let first = with(&db, |c| ask(c, &worker, &json!({ "question": "Karar?" }))).unwrap();
        let second = with(&db, |c| ask(c, &worker, &json!({ "question": "Onay?" }))).unwrap();
        with(&db, |c| escalate(c, &leader, second.id, None)).unwrap();
        for id in [first.id, second.id] {
            let err = with(&db, |c| crate::delegate::report(c, &worker, d, "bekliyor", None)).unwrap_err();
            assert!(err.contains(&format!("#{id}")), "{err}");
            assert_eq!(status(&db, d), "running");
            assert!(hook(&db, &ev, "kaan", "Stop", json!({})).is_none());
            assert_eq!(status(&db, d), "running");
            with(&db, |c| answer(c, &leader, id, "devam et")).unwrap();
            let out = hook(&db, &ev, "kaan", "Stop", json!({})).unwrap();
            assert_eq!(out["decision"], "block");
            assert_eq!(status(&db, d), "running");
        }
        with(&db, |c| crate::delegate::report(c, &worker, d, "bitti: tamam", None)).unwrap();
        assert_eq!(status(&db, d), "done");
    }

    #[test]
    fn ask_answer_round_trip_once_each_way() {
        let (db, ev, d) = setup();
        let (kaan, timur) = with(&db, |c| Ok((caller(c, "kaan")?, caller(c, "timur")?))).unwrap();
        assert!(with(&db, |c| ask(c, &timur, &json!({ "question": "?" }))).is_err(), "leaders don't ask_leader");
        assert!(with(&db, |c| ask(c, &caller(c, "ayla")?, &json!({ "question": "?" }))).unwrap_err().contains("süren bir iş yok"));
        let q = with(&db, |c| ask(c, &kaan, &json!({ "question": "Mavi mi kırmızı mı?", "options": ["mavi", "kırmızı"] }))).unwrap();
        assert_eq!((q.delegation_id, q.leader.as_str(), q.status.as_str()), (d, "timur", "open"));

        // Worker ends its turn: the job stays running, nothing is delivered to it.
        assert!(hook(&db, &ev, "kaan", "Stop", json!({})).is_none());
        assert_eq!(status(&db, d), "running");

        // Leader working → its Stop blocks with the question, once.
        let out = hook(&db, &ev, "timur", "Stop", json!({})).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("Mavi mi kırmızı mı?") && out["reason"].as_str().unwrap().contains("mavi | kırmızı"));
        assert!(hook(&db, &ev, "timur", "UserPromptSubmit", json!({})).is_none(), "not twice");
        assert!(on_session_start(&db, &ev, "timur").is_none());

        // Only the asked leader answers; answers once.
        let other = with(&db, |c| {
            c.execute("INSERT INTO agents (id, team_id, name, kind, model, shirt, hair, skin) VALUES ('zed','default','Zed','leader','sonnet','a','b','c')", []).map_err(e)?;
            caller(c, "zed")
        })
        .unwrap();
        assert!(with(&db, |c| answer(c, &other, q.id, "mavi")).is_err());
        assert!(with(&db, |c| answer(c, &kaan, q.id, "mavi")).is_err());
        with(&db, |c| answer(c, &timur, q.id, "mavi")).unwrap();
        assert!(with(&db, |c| answer(c, &timur, q.id, "kırmızı")).is_err());

        // Idle worker: the pasted ping's UserPromptSubmit carries the answer, once.
        let out = hook(&db, &ev, "kaan", "UserPromptSubmit", json!({})).unwrap();
        let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.contains("cevap (Timur): mavi"), "{ctx}");
        assert!(hook(&db, &ev, "kaan", "UserPromptSubmit", json!({})).is_none());
        // Answer taken → the next Stop is a normal one again (unreported job closes).
        assert!(hook(&db, &ev, "kaan", "Stop", json!({})).is_none());
        assert_eq!(status(&db, d), "done_no_report");
    }

    #[test]
    fn working_worker_gets_answer_at_stop_and_escalation_goes_via_patron() {
        let (db, ev, d) = setup();
        let (kaan, timur) = with(&db, |c| Ok((caller(c, "kaan")?, caller(c, "timur")?))).unwrap();
        let q = with(&db, |c| ask(c, &kaan, &json!({ "question": "Bütçe?" }))).unwrap();
        // Idle leader: the ping's prompt carries it.
        let out = hook(&db, &ev, "timur", "UserPromptSubmit", json!({})).unwrap();
        assert!(out["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("Bütçe?"));
        with(&db, |c| escalate(c, &timur, q.id, Some("karar patronun"))).unwrap();
        assert!(with(&db, |c| escalate(c, &timur, q.id, None)).is_err());
        let q2 = with(&db, |c| patron_answer(c, q.id, "1000 TL")).unwrap();
        assert_eq!(q2.answered_by.as_deref(), Some("patron"));
        assert!(with(&db, |c| patron_answer(c, q.id, "x")).is_err());

        // Worker still working → its Stop blocks with the answer; the job stays running.
        let out = hook(&db, &ev, "kaan", "Stop", json!({})).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("cevap (patron): 1000 TL"));
        assert_eq!(status(&db, d), "running");
        // The leader hears the patron's answer in its next context (a note).
        let out = with(&db, |c| crate::plan::take_notes(c, "timur")).unwrap().unwrap();
        assert!(out.contains("1000 TL"), "{out}");
    }

    /// A worker closed while waiting keeps its job (the answer resumes it); once the answer is
    /// delivered, closing without a report ends the job as usual.
    #[test]
    fn closed_while_waiting_keeps_the_job() {
        let (db, ev, d) = setup();
        let (kaan, timur) = with(&db, |c| Ok((caller(c, "kaan")?, caller(c, "timur")?))).unwrap();
        let q = with(&db, |c| ask(c, &kaan, &json!({ "question": "?" }))).unwrap();
        crate::delegate::session_exited(&db, &ev, "kaan").unwrap();
        assert_eq!(status(&db, d), "running");
        with(&db, |c| answer(c, &timur, q.id, "evet")).unwrap();
        assert!(hook(&db, &ev, "kaan", "UserPromptSubmit", json!({})).is_some());
        crate::delegate::session_exited(&db, &ev, "kaan").unwrap();
        assert_eq!(status(&db, d), "done_no_report");
    }

    #[test]
    fn ask_user_question_denied_only_to_delegated_workers() {
        let (db, ev, _) = setup();
        let ask = json!({ "tool_name": "AskUserQuestion", "tool_input": {} });
        let out = hook(&db, &ev, "kaan", "PreToolUse", ask.clone()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("ask_leader"));
        assert!(hook(&db, &ev, "ayla", "PreToolUse", ask.clone()).is_none(), "worker without a delegation");
        assert!(hook(&db, &ev, "timur", "PreToolUse", ask).is_none(), "leader");
        assert!(hook(&db, &ev, "kaan", "PreToolUse", json!({ "tool_name": "Bash", "tool_input": {} })).is_none());
        assert!(hook(&db, &ev, "kaan", "PermissionRequest", json!({ "tool_name": "Bash" })).is_none(), "permissions untouched");
    }
}

/// Görev #122: the patron's answer wakes an idle leader once; a busy one hears it at its Stop.
#[cfg(test)]
mod wake_tests {
    use crate::delegate::{caller, insert, set_status, NewDelegation};
    use crate::*;
    use serde_json::json;

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    /// The default team's leader `timur` with a shell PTY standing in for claude; `kaan` runs a job.
    fn core(tag: &str) -> (Arc<Core>, std::path::PathBuf, i64) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("dex-qwake-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
        core.pty_spawn(pty::Spawn { id: "timur".into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, Sink).unwrap();
        core.agent_states.hook("timur", Some(agents::Status::Idle), None);
        let d = with(&core.db, |c| {
            let d = insert(c, NewDelegation { from: &caller(c, "timur")?, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None })?;
            set_status(c, d.id, "running")?;
            Ok(d.id)
        })
        .unwrap();
        (core, root, d)
    }

    fn done(core: Arc<Core>, root: std::path::PathBuf) {
        core.ptys.kill("timur");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    /// kaan asks, timur escalates: the question is the patron's.
    fn escalated(core: &Core, text: &str) -> i64 {
        with(&core.db, |c| {
            let q = super::ask(c, &caller(c, "kaan")?, &json!({ "question": text }))?;
            super::take_for_leader(c, "timur")?; // the leader saw it
            super::escalate(c, &caller(c, "timur")?, q.id, None)?;
            Ok(q.id)
        })
        .unwrap()
    }

    fn pending_notes(core: &Core) -> bool {
        with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM leader_notes WHERE agent_id = 'timur' AND notified = 0", [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string()))
            .unwrap()
            > 0
    }
    fn accept_pending(core: &Core) {
        let prompt = core.leader_pastes.lock().unwrap().get("timur").unwrap().text.clone();
        ureq::post(&format!("http://127.0.0.1:{}/hook", core.hooks.port))
            .set("Authorization", &format!("Bearer {}", core.hooks.token))
            .set("X-Dex-Agent", "timur")
            .set("X-Dex-Key", &core.mcp.key_for("timur"))
            .send_json(json!({"hook_event_name":"UserPromptSubmit", "prompt":prompt})).unwrap();
    }

    #[test]
    fn idle_leader_is_woken_by_the_patrons_answer_and_its_context_does_not_repeat_it() {
        let (core, root, _) = core("idle");
        let q = escalated(&core, "Bütçe?");
        core.question_answer(q, "1000 TL").unwrap();
        assert!(pending_notes(&core), "paste is pending until UPS acceptance");
        accept_pending(&core);
        assert!(!pending_notes(&core), "pasted as the wake prompt, marked sent");
        assert_eq!(core.agent_states.get("timur").status, agents::Status::Working, "a turn started");
        // Another prompt does not bring the accepted note again.
        let out = core.delegation_on_hook("timur", "UserPromptSubmit", &json!({ "prompt": "[Dex] Patron …" }), false);
        assert!(!out.unwrap_or_default().contains("1000 TL"));
        done(core, root);
    }

    #[test]
    fn escalated_question_wakes_blocking_worker_from_patron_card() {
        let (core, root, _) = core("blocking-patron");
        let q = escalated(&core, "Karar?");
        let worker_core = core.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let reply: Value = ureq::post(&format!("http://127.0.0.1:{}/mcp", worker_core.hooks.port))
                .timeout(std::time::Duration::from_secs(5))
                .set("Authorization", &format!("Bearer {}", worker_core.hooks.token))
                .set("X-Dex-Agent", "kaan")
                .set("X-Dex-Key", &worker_core.mcp.key_for("kaan"))
                .send_json(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "wait_answer", "arguments": {"questionId": q}}}))
                .unwrap().into_json().unwrap();
            tx.send(reply).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err());
        core.question_answer(q, "devam").unwrap();
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()["result"]["structuredContent"]["answer"], "devam");
        waiter.join().unwrap();
        assert!(with(&core.db, |c| super::take_answers(c, "kaan")).unwrap().is_empty());
        done(core, root);
    }

    #[test]
    fn busy_leader_hears_it_at_its_stop_once_and_is_not_woken_again() {
        let (core, root, _) = core("busy");
        core.agent_states.hook("timur", Some(agents::Status::Working), Some("leader-session"));
        let q = escalated(&core, "Bütçe?");
        core.question_answer(q, "1000 TL").unwrap();
        assert!(pending_notes(&core), "working: nothing pasted");
        let out = core.delegation_on_hook("timur", "Stop", &json!({}), false).unwrap();
        assert!(out.contains("block") && out.contains("1000 TL"), "{out}");
        assert!(pending_notes(&core), "Stop output is pending until CLI continuation");
        crate::claude_turn::tests::accepted_stop(&core, "timur");
        core.claude_hook_received("timur", "PreToolUse", Some("leader-session"), agents::next_seq());
        assert!(!pending_notes(&core), "CLI continuation accepts the note");
        // Idle afterwards: a late wake finds nothing to paste.
        core.agent_states.hook("timur", Some(agents::Status::Idle), None);
        core.leader_notes_wake("timur");
        assert_eq!(core.agent_states.get("timur").status, agents::Status::Idle, "no second notice");
        done(core, root);
    }

    #[test]
    fn patrons_draft_holds_the_wake_until_it_ends() {
        let (core, root, _) = core("draft");
        core.agent_input("timur", true);
        let q = escalated(&core, "Bütçe?");
        core.question_answer(q, "1000 TL").unwrap();
        assert!(pending_notes(&core), "the patron is typing");
        core.agent_input("timur", false);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !core.leader_pastes.lock().unwrap().contains_key("timur") && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(pending_notes(&core), "typing-window expiry only pastes; UPS acknowledges");
        accept_pending(&core);
        assert!(!pending_notes(&core), "goes once the window is over");
        done(core, root);
    }

    #[test]
    fn member_proposal_decision_wakes_an_idle_leader() {
        let (core, root, _) = core("proposal");
        let p = with(&core.db, |c| crate::proposals::propose(c, &caller(c, "timur")?, &json!({ "role": "qa", "reason": "x" }))).unwrap();
        core.proposal_decide(p.id, false, None).unwrap();
        assert!(pending_notes(&core));
        accept_pending(&core);
        assert!(!pending_notes(&core));
        assert_eq!(core.agent_states.get("timur").status, agents::Status::Working);
        done(core, root);
    }
}
