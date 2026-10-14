//! Leader plans (DEX-22): one call hands the team several tasks with dependencies. Every task is a
//! board task; tasks without prerequisites are delegated at once, the rest as soon as all their
//! prerequisites are `done`. Nothing polls: [`tap`] watches `delegations-changed` (report, Stop,
//! ack, session exit all emit it) and [`advance`] re-evaluates the active plans.
//!
//! A prerequisite that fails (or ends without a report) leaves its dependents waiting (`blocked`)
//! and the leader gets a note; a late `report` of that prerequisite unblocks them. Plans live in
//! the DB, so a restart continues them: work interrupted by the shutdown is delegated again once
//! the frontend is up ([`requeue_interrupted`] + `plan_resume`).
//!
//! Leader notes (`leader_notes`) are one-shot lines for the leader's next hook context; member
//! proposals use them too.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Mutex};

use crate::db::{tasks, with, Db};
use crate::delegate::{self, Caller, Delegation};
use crate::events::{EventSink, Events};
use crate::Core;

const MAX_TASKS: usize = 30;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

// ---- shapes ----

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlanTask {
    pub key: String,
    pub title: String,
    pub prompt: String,
    pub role: Option<String>,
    pub agent_id: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Görev #283: the patron's verbatim words behind this task.
    pub patron_words: Option<String>,
    /// #671: the leader's effort for this task's job; serde rejects anything but medium|high|xhigh.
    pub effort: Option<crate::effort::Effort>,
    pub model: Option<String>,
    pub model_reason: Option<String>,
    #[serde(skip)]
    pub(crate) model_snapshot: Option<crate::job_model::Snapshot>,
    #[serde(default, deserialize_with = "project_id")]
    pub project: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewPlan {
    #[serde(default)]
    pub title: String,
    pub tasks: Vec<PlanTask>,
    #[serde(default, deserialize_with = "project_id")]
    pub project: Option<String>,
}

fn project_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let id = String::deserialize(d)?;
    if id.is_empty() {
        return Err(serde::de::Error::custom("project_not_found"));
    }
    Ok(Some(id))
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub key: String,
    pub title: String,
    #[serde(skip)]
    pub id: i64,
    #[serde(skip)]
    pub prompt: String,
    pub target: String,
    pub depends_on: Vec<String>,
    pub status: String,
    pub task_id: Option<i64>,
    pub delegation_id: Option<i64>,
    pub to_agent: Option<String>,
    pub target_role: Option<String>,
    pub queue_position: Option<i64>,
    #[serde(skip)]
    pinned_agent: Option<String>,
    pub attempts: i64,
    #[serde(skip)]
    pub patron_words: Option<String>,
    pub effort: Option<crate::effort::Effort>,
    pub model: Option<String>,
    pub model_reason: Option<String>,
    #[serde(skip)]
    pub(crate) model_route: Option<crate::job_model::Route>,
    #[serde(flatten)]
    pub project: delegate::ProjectSnapshot,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: i64,
    pub team_id: String,
    pub leader: String,
    pub title: String,
    pub status: String,
    pub items: Vec<Item>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn item_row(r: &Row) -> rusqlite::Result<Item> {
    let deps: String = r.get(5)?;
    Ok(Item {
        id: r.get(0)?,
        key: r.get(1)?,
        title: r.get(2)?,
        prompt: r.get(3)?,
        target: r.get(4)?,
        depends_on: serde_json::from_str(&deps).unwrap_or_default(),
        status: r.get(6)?,
        task_id: r.get(7)?,
        delegation_id: r.get(8)?,
        to_agent: None,
        target_role: r.get(16)?,
        queue_position: None,
        pinned_agent: r.get(17)?,
        attempts: r.get(9)?,
        patron_words: r.get(10)?,
        effort: r.get(11)?,
        model: r.get(18)?,
        model_reason: r.get(19)?,
        model_route: match (r.get::<_, Option<String>>(20)?, r.get::<_, Option<String>>(21)?) {
            (Some(engine), Some(provider)) => Some(crate::job_model::Route { engine, provider }),
            _ => None,
        },
        project: delegate::ProjectSnapshot {
            project_id: r.get(12)?,
            project_root: r.get(13)?,
            merge_branch: r.get(14)?,
            repo_key: r.get(15)?,
        },
    })
}

fn items_of(conn: &Connection, plan: i64) -> Result<Vec<Item>, String> {
    let mut st = conn
        .prepare(
            "SELECT id, key, title, prompt, target, depends_on, status, task_id, delegation_id, attempts, patron_words, effort, project_id, project_root, merge_branch, repo_key,
             (SELECT role_slug FROM plan_targets WHERE item_id=plan_items.id),
             (SELECT agent_id FROM plan_targets WHERE item_id=plan_items.id), model, model_reason, model_engine, model_provider
             FROM plan_items WHERE plan_id = ?1 ORDER BY id",
        )
        .map_err(e)?;
    let rows = st.query_map([plan], item_row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

pub fn get(conn: &Connection, id: i64) -> Result<Plan, String> {
    let head = conn
        .query_row(
            "SELECT id, team_id, leader, title, status, created_at, updated_at FROM plans WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("plan yok: {id}"))?;
    let (id, team_id, leader, title, status, created_at, updated_at) = head;
    let mut items = items_of(conn, id)?;
    for item in &mut items {
        if let Some(job) = item.delegation_id {
            let d = delegate::get(conn, job)?;
            item.to_agent = (!d.to_agent.is_empty()).then_some(d.to_agent);
            item.target_role = d.target_role;
            item.queue_position = d.queue_position;
        }
    }
    Ok(Plan { id, team_id, leader, title, status, items, created_at, updated_at })
}

fn set_item(conn: &Connection, id: i64, status: &str) -> Result<(), String> {
    conn.execute("UPDATE plan_items SET status = ?2 WHERE id = ?1", params![id, status]).map_err(e)?;
    Ok(())
}

fn touch(conn: &Connection, plan: i64, status: Option<&str>) -> Result<(), String> {
    conn.execute(
        "UPDATE plans SET status = IFNULL(?2, status), updated_at = ?3 WHERE id = ?1",
        params![plan, status, now()],
    )
    .map_err(e)?;
    Ok(())
}

// ---- leader notes ----

pub fn note(conn: &Connection, agent: &str, text: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO leader_notes (agent_id, text, created_at) VALUES (?1, ?2, ?3)",
        params![agent, text, now()],
    )
    .map_err(e)?;
    Ok(())
}

/// Unsent notes of `agent`, marked sent.
fn take_note_rows(conn: &Connection, agent: &str) -> Result<Vec<(i64, String)>, String> {
    note_rows(conn, agent, None)
}

fn note_rows(conn: &Connection, agent: &str, pending: Option<&crate::hook_delivery::Pending>) -> Result<Vec<(i64, String)>, String> {
    let mut st = conn
        .prepare("SELECT id, text FROM leader_notes WHERE agent_id = ?1 AND notified = 0 ORDER BY id")
        .map_err(e)?;
    let rows: Vec<(i64, String)> =
        st.query_map([agent], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
    for (id, _) in &rows {
        if let Some(pending) = pending {
            pending.0.lock().unwrap().notes.push(*id);
        } else {
            conn.execute("UPDATE leader_notes SET notified = 1 WHERE id = ?1", [id]).map_err(e)?;
        }
    }
    Ok(rows)
}

/// Unsent notes of `agent` as one block (marked sent), or None.
pub fn take_notes(conn: &Connection, agent: &str) -> Result<Option<String>, String> {
    notes_pending(conn, agent, None)
}

/// Tek not metni: etiketliyse aynen, değilse `[Dex-Not]` alır.
/// Okuma anında uygulanır; DB'ye yazılmaz, tekrar okuma çift etiket üretmez.
pub(crate) fn tag_note_text(t: &str) -> String {
    let mut lines = t.lines();
    let first = lines.next().unwrap_or("");
    let mut out = if crate::dexmsg::tag_of(first).is_some() {
        first.to_string()
    } else {
        crate::dexmsg::line(crate::dexmsg::NOT, first)
    };
    for l in lines {
        out.push('\n');
        out.push_str(l);
    }
    out
}

fn notes_pending(conn: &Connection, agent: &str, pending: Option<&crate::hook_delivery::Pending>) -> Result<Option<String>, String> {
    let rows = if pending.is_some() { note_rows(conn, agent, pending)? } else { take_note_rows(conn, agent)? };
    if rows.is_empty() {
        return Ok(None);
    }
    let mut out = format!("[{}] bekleyen bildirimlerin (plan / üye önerisi / not):\n", crate::dexmsg::NOT);
    for (_, t) in rows {
        out.push_str(&format!("- {}\n", tag_note_text(&t).replace('\n', "\n  ")));
    }
    Ok(Some(out))
}

/// Adds the agent's pending notes to a hook answer: UserPromptSubmit → `additionalContext`;
/// Stop → the block reason (a Stop that would not block now blocks once to deliver them).
pub fn with_notes(db: &Db, agent: &str, event: &str, out: Option<String>) -> Option<String> {
    with_notes_pending(db, agent, event, out, None)
}

pub(crate) fn with_notes_pending(db: &Db, agent: &str, event: &str, out: Option<String>, pending: Option<&crate::hook_delivery::Pending>) -> Option<String> {
    if !matches!(event, "UserPromptSubmit" | "Stop") {
        return out;
    }
    let mut v: Value = match &out {
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => return out,
        },
        None => json!({}),
    };
    if event == "Stop" && out.is_some() && v.get("decision").and_then(Value::as_str) != Some("block") {
        return out;
    }
    let notes = match with(db, |c| notes_pending(c, agent, pending)) {
        Ok(Some(n)) => n,
        _ => return out,
    };
    if event == "Stop" {
        let reason = v.get("reason").and_then(Value::as_str).map(|r| format!("{r}\n\n{notes}")).unwrap_or(notes);
        v["decision"] = json!("block");
        v["reason"] = json!(reason);
    } else {
        let ctx = v
            .pointer("/hookSpecificOutput/additionalContext")
            .and_then(Value::as_str)
            .map(|c| format!("{c}\n{notes}"))
            .unwrap_or(notes);
        v["hookSpecificOutput"] = json!({ "hookEventName": "UserPromptSubmit", "additionalContext": ctx });
    }
    Some(v.to_string())
}

/// The patron's typing window ([`crate::agents::States::typing`]) plus a margin.
const WAKE_RETRY: std::time::Duration = std::time::Duration::from_millis(5_100);
const ACCEPT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) struct LeaderPaste {
    pub(crate) text: String,
    notes: Vec<i64>,
    questions: Vec<i64>,
    results: Vec<i64>,
    session: Option<String>,
    pty_generation: Option<u64>,
    pasted: bool,
    accepted: bool,
    cancelled: bool,
    generation: u64,
    retry_at: std::time::Instant,
}

impl LeaderPaste {
    pub(crate) fn pending(&self) -> bool {
        !self.accepted && !self.cancelled
    }
}

fn delivery_audit(agent: &str, reason: &str) {
    crate::diagnostics::event("leader_delivery", json!({"agent":agent,"reason":reason}));
}

impl crate::Core {
    /// Any patron input relinquishes the draft until the next hook delivers its pending rows.
    pub(crate) fn leader_paste_input(&self, id: &str) {
        delivery_audit(id, "patron_input");
        if let Some(flight) = self.leader_pastes.lock().unwrap().get_mut(id) {
            flight.cancelled = true;
        }
    }

    /// Matching UPS acknowledges the text already in the prompt. Results still ride the normal
    /// UPS context path; their existing delivery stamp is the only stamp.
    pub(crate) fn leader_paste_accepted(&self, id: &str, prompt: &str) -> bool {
        let mut flights = self.leader_pastes.lock().unwrap();
        let Some(flight) = flights.get_mut(id) else { return true };
        if flight.accepted || flight.cancelled { return true; }
        if prompt.trim() != flight.text.trim() {
            delivery_audit(id, "prompt_mismatch");
            flight.cancelled = true; // relinquish the draft; pending rows ride this hook's context
            return true;
        }
        let stamped = with(&self.db, |c| {
            let tx = c.unchecked_transaction().map_err(e)?;
            for n in &flight.notes {
                tx.execute("UPDATE leader_notes SET notified=1 WHERE id=?1 AND agent_id=?2", params![n, id]).map_err(e)?;
            }
            for q in &flight.questions {
                tx.execute("UPDATE questions SET leader_delivered_at=?3 WHERE id=?1 AND leader=?2 AND leader_delivered_at IS NULL",
                    params![q, id, now()]).map_err(e)?;
            }
            tx.commit().map_err(e)
        });
        if stamped.is_err() {
            delivery_audit(id, "db_error");
            log::warn!("leader paste acknowledgement failed");
            return false;
        }
        let changed = !flight.questions.is_empty();
        // Keep the reservation until dex-hook: the receiver applies Working after this callback.
        // An event emitted by stamping must not paste a later notification into this same turn.
        flight.accepted = true;
        delivery_audit(id, "paste_accept");
        drop(flights);
        if changed { let _ = self.events.emit("questions-changed", &()); }
        true
    }
}

impl crate::Core {
    /// Görev #122: a patron decision (escalated question answered, member proposal decided) wakes
    /// an idle leader at once: pending notes and worker questions go in as a `[Dex]` prompt,
    /// including their text. Matching UPS marks them sent so its context does not repeat them.
    /// Busy → its Stop delivers them (`with_notes`); suspended → resume; manually closed → its next prompt; patron typing →
    /// tried again after the typing window.
    pub(crate) fn leader_notes_wake(&self, id: &str) {
        if !self.ptys.is_running(id) {
            delivery_audit(id, "offline");
            self.leader_pastes.lock().unwrap().remove(id);
            if self.sleeping_leaders.lock().unwrap().contains(id) && self.leader_has_events(id) {
                // A prompt starts a turn; SessionStart keeps delivering queued questions/results.
                if self.agent_resume_sleeping(id).is_err() { log::warn!("sleeping leader resume failed"); }
            }
            return;
        }
        if !self.ptys.is_running(id) || self.chat_queues.lock().unwrap().get(id).is_some_and(|q| !q.is_empty()) {
            delivery_audit(id, "chat_queue");
            return; // the chat queue goes first; its turn's Stop brings the notes
        }
        let status = match self.agent_states.get(id).status {
            crate::agents::Status::Offline => crate::agents::Status::Idle,
            s => s,
        };
        if status != crate::agents::Status::Idle {
            delivery_audit(id, "busy");
            return;
        }
        // #447: a session still starting (no hook yet: a restarted core's leader) is not at its
        // input box either.
        if self.agent_states.pre_session(id) { delivery_audit(id, "starting"); return; } // SessionStart's dex-hook retries it
        if self.agent_states.typing(id) {
            delivery_audit(id, "draft");
            let (weak, agent) = (self.weak_self.clone(), id.to_string());
            std::thread::spawn(move || {
                std::thread::sleep(WAKE_RETRY);
                if let Some(core) = weak.upgrade() {
                    core.leader_notes_wake(&agent);
                }
            });
            return;
        }
        // #447: a drain holds the turn — the notes stay unsent (the drain's end, or the restarted
        // core, wakes the leader with them).
        let Some(turn) = self.admit(id) else { delivery_audit(id, "draining"); return };
        // Serialize concurrent event/timer wakes; rows stay pending until matching UPS.
        let mut flights = self.leader_pastes.lock().unwrap();
        // A hook or patron input can change readiness while another wake holds the paste lock.
        if self.agent_states.get(id).in_turn() || self.agent_states.pre_session(id) || self.agent_states.typing(id) { delivery_audit(id, "readiness"); return; }
        let pty_generation = self.ptys.generation(id);
        if flights.get(id).is_some_and(|p| p.pty_generation != pty_generation) { flights.remove(id); }
        let pending = with(&self.db, |c| {
            let rows: Vec<(i64, String)> = c.prepare("SELECT id,text FROM leader_notes WHERE agent_id=?1 AND notified=0 ORDER BY id")
                .map_err(e)?.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
            let questions = crate::questions::pending_for_leader(c, id)?;
            let results: Vec<(i64, String, String, Option<String>)> = c.prepare("SELECT d.id,IFNULL(a.name,d.to_agent),d.status,d.summary FROM delegations d LEFT JOIN agents a ON a.id=d.to_agent
                WHERE d.from_agent=?1 AND d.notified=0 AND d.status IN ('done','done_no_report','failed') ORDER BY d.id")
                .map_err(e)?.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
            Ok((rows, questions, results))
        });
        let (rows, questions, results) = match pending {
            Ok(pending) => pending,
            Err(_) => { delivery_audit(id, "db_error"); log::warn!("pending leader delivery lookup failed"); return; }
        };
        if let Some(flight) = flights.get(id) {
            if flight.accepted { delivery_audit(id, "reserved"); return; }
            let pending = flight.notes.iter().any(|n| rows.iter().any(|r| r.0 == *n))
                || flight.questions.iter().any(|n| questions.iter().any(|q| q.id == *n))
                || flight.results.iter().any(|n| results.iter().any(|r| r.0 == *n));
            if !pending {
                flights.remove(id); // SessionStart/Stop delivered it through context; never resend
                return;
            }
            if flight.cancelled { delivery_audit(id, "cancelled"); return; }
            if std::time::Instant::now() < flight.retry_at { delivery_audit(id, "accept_wait"); return; }
        } else if rows.is_empty() && questions.is_empty() && results.is_empty() {
            delivery_audit(id, "no_pending");
            return;
        }
        let flight = flights.entry(id.into()).or_insert_with(|| {
            let mut parts = rows.iter().map(|(_, t)| tag_note_text(t)).collect::<Vec<_>>();
            if !questions.is_empty() { parts.push(crate::questions::leader_text(&questions, "sen boşta iken geldi")); }
            for (n, name, status, summary) in &results {
                let durum = match status.as_str() {
                    "done" => "bitti",
                    "done_no_report" => "RAPORSUZ bitti",
                    _ => "başarısız",
                };
                let mut line = format!("[{}] {name} #{n} · {durum}", crate::dexmsg::DEVRETME);
                if let Some(first) = summary.as_deref().map(crate::delegate::first_line).filter(|s| !s.is_empty()) {
                    line.push_str(&format!(" — {first}"));
                }
                parts.push(line);
            }
            LeaderPaste {
                text: parts.join("\n"),
                notes: rows.iter().map(|r| r.0).collect(),
                questions: questions.iter().map(|q| q.id).collect(),
                results: results.iter().map(|r| r.0).collect(),
                session: self.agent_states.get(id).session_id,
                pty_generation,
                pasted: false, accepted: false, cancelled: false, generation: 0, retry_at: std::time::Instant::now(),
            }
        });
        if !flight.pasted { delivery_audit(id, "paste"); }
        let sent = if flight.pasted { Ok(()) } else { self.paste_text(id, &flight.text) };
        if sent.is_ok() { flight.pasted = true; } else {
            delivery_audit(id, "write_error");
            log::warn!("leader prompt paste failed; awaiting retry");
        }
        // One acknowledgement deadline per attempt, cancelled by UPS or a newer attempt.
        // The retry writes only Enter while this same draft remains unaccepted.
        flight.generation = crate::agents::next_seq();
        flight.retry_at = std::time::Instant::now() + ACCEPT_WAIT;
        let generation = flight.generation;
        drop(flights);
        // Do not hold the cross-team paste map across the TUI's redraw delay. Recheck at the
        // actual Enter: a concurrent UPS, patron input or process replacement cancels it.
        if sent.is_ok() && self.paste_enter_with(|| {
            // Keep ownership through the write so input/UPS cannot cancel between them.
            let flights = self.leader_pastes.lock().unwrap();
            let allowed = flights.get(id).is_some_and(|p| p.generation == generation && !p.accepted && !p.cancelled)
                && self.ptys.generation(id) == pty_generation
                && !self.agent_states.get(id).in_turn()
                && !self.agent_states.typing(id)
                && !self.agent_states.pre_session(id);
            if allowed {
                delivery_audit(id, "enter");
                self.claude_prompt_submit(id, false, flights.get(id).map(|p| (p.notes.clone(), p.questions.clone(), p.results.clone())), || self.ptys.write(id, b"\r"))
            } else { delivery_audit(id, "readiness"); Ok(()) }
        }).is_err() { delivery_audit(id, "write_error"); log::warn!("leader prompt Enter failed; awaiting retry"); }
        drop(turn);
        let (weak, agent) = (self.weak_self.clone(), id.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(ACCEPT_WAIT);
            if let Some(core) = weak.upgrade() {
                let current = core.leader_pastes.lock().unwrap().get(&agent).is_some_and(|p| p.generation == generation && !p.cancelled);
                if current { core.leader_notes_wake(&agent); }
            }
        });
    }
}

// ---- creating ----

/// Validates the whole plan (keys, targets, dependencies, cycles) before anything is written.
fn validate(conn: &Connection, team: &str, p: &NewPlan) -> Result<(), String> {
    if p.tasks.is_empty() {
        return Err("tasks boş olamaz".into());
    }
    if p.tasks.len() > MAX_TASKS {
        return Err(format!("bir planda en çok {MAX_TASKS} görev olur"));
    }
    let mut keys = HashSet::new();
    for t in &p.tasks {
        let k = t.key.trim();
        if k.is_empty() {
            return Err("her görevin bir key'i olmalı".into());
        }
        if !keys.insert(k) {
            return Err(format!("key iki kez kullanıldı: {k}"));
        }
        if t.title.trim().is_empty() || t.prompt.trim().is_empty() {
            return Err(format!("{k}: title ve prompt gerekli"));
        }
        let target = target_of(t).ok_or_else(|| format!("{k}: role ya da agentId gerekli"))?;
        if t.agent_id.is_some() {
            delegate::pick_worker(conn, team, Some(target))
        } else {
            delegate::role_target(conn, team, target).map(|(_, worker)| worker)
        }.map_err(|err| format!("{k}: {err}"))?;
        crate::job_model::args(&json!({"model":t.model,"modelReason":t.model_reason}))?;
    }
    for t in &p.tasks {
        for d in &t.depends_on {
            let d = d.trim();
            if d == t.key.trim() {
                return Err(format!("{d} kendine bağımlı olamaz"));
            }
            if !keys.contains(d) {
                return Err(format!("{}: dependsOn içinde bilinmeyen key: {d}", t.key.trim()));
            }
        }
    }
    // Kahn: whatever never reaches in-degree 0 sits on a cycle (or behind one).
    let mut indeg: HashMap<&str, usize> = p.tasks.iter().map(|t| (t.key.trim(), 0)).collect();
    for t in &p.tasks {
        *indeg.get_mut(t.key.trim()).unwrap() = dep_keys(t).len();
    }
    let mut ready: Vec<&str> = indeg.iter().filter(|(_, n)| **n == 0).map(|(k, _)| *k).collect();
    let mut seen = 0;
    while let Some(k) = ready.pop() {
        seen += 1;
        for t in p.tasks.iter().filter(|t| dep_keys(t).contains(k)) {
            let n = indeg.get_mut(t.key.trim()).unwrap();
            *n -= 1;
            if *n == 0 {
                ready.push(t.key.trim());
            }
        }
    }
    if seen < p.tasks.len() {
        let mut stuck: Vec<&str> = indeg.iter().filter(|(_, n)| **n > 0).map(|(k, _)| *k).collect();
        stuck.sort();
        return Err(format!("bağımlılıkta döngü var: {}", stuck.join(", ")));
    }
    Ok(())
}

fn target_of(t: &PlanTask) -> Option<&str> {
    t.agent_id.as_deref().or(t.role.as_deref()).map(str::trim).filter(|s| !s.is_empty())
}

fn dep_keys(t: &PlanTask) -> HashSet<&str> {
    t.depends_on.iter().map(|d| d.trim()).collect()
}

/// Writes the plan + one board task per item (all or nothing). Delegating is [`advance`]'s job.
pub fn create(conn: &Connection, lead: &Caller, p: NewPlan) -> Result<i64, String> {
    let contexts = resolve_projects(conn, std::path::Path::new(""), lead, &p)?;
    create_bound(conn, lead, p, &contexts)
}

pub(crate) fn resolve_projects(
    conn: &Connection,
    data_dir: &std::path::Path,
    lead: &Caller,
    p: &NewPlan,
) -> Result<Vec<crate::project_context::ProjectContext>, String> {
    validate(conn, &lead.team_id, p)?;
    p.tasks.iter().map(|t| delegate::resolve_context(conn, data_dir, lead, t.project.as_deref().or(p.project.as_deref()), None)).collect()
}

/// Contexts have been resolved before any write; MCP validates their roots outside the mutex.
pub(crate) fn create_bound(
    conn: &Connection,
    lead: &Caller,
    mut p: NewPlan,
    contexts: &[crate::project_context::ProjectContext],
) -> Result<i64, String> {
    crate::job_model::snapshot_plan(conn, &lead.team_id, &mut p)?;
    crate::job_model::validate_plan_catalog(&p)?;
    create_prepared(conn, lead, p, contexts, &vec![None; contexts.len()])
}

pub(crate) fn create_prepared(
    conn: &Connection,
    lead: &Caller,
    p: NewPlan,
    contexts: &[crate::project_context::ProjectContext],
    ready: &[Option<crate::projects::PreparedRoot>],
) -> Result<i64, String> {
    validate(conn, &lead.team_id, &p)?;
    if contexts.len() != p.tasks.len() || ready.len() != contexts.len() || contexts.iter().any(|c| c.team_id != lead.team_id) {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    // Check every original selection before verify fills legacy repo metadata. Otherwise
    // the first item changes the metadata seen by the second item of the same project.
    for c in contexts {
        delegate::revalidate_context(&tx, lead, c, None)?;
    }
    let contexts = contexts
        .iter()
        .zip(ready)
        .map(|(c, r)| delegate::verify_context_metadata(&tx, lead, c, r.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let ts = now();
    tx.execute(
        "INSERT INTO plans (team_id, leader, title, status, created_at, updated_at) VALUES (?1, ?2, ?3, 'active', ?4, ?4)",
        params![lead.team_id, lead.id, p.title.trim(), ts],
    )
    .map_err(e)?;
    let plan = tx.last_insert_rowid();
    for (t, context) in p.tasks.iter().zip(&contexts) {
        if let Some(model) = &t.model {
            let snapshot = t.model_snapshot.as_ref().ok_or("iş modelinin doğrulanan rotası eksik")?;
            if !snapshot.revalidate(&tx)? {
                snapshot.note_changed_route(&tx, &lead.id, &format!("Plan #{plan} · {}", t.key.trim()), model)?;
            }
        }
        let snapshot = delegate::ProjectSnapshot::from(context);
        let key = t.key.trim();
        let mut deps: Vec<&str> = dep_keys(t).into_iter().collect();
        deps.sort();
        let patron = delegate::patron_words(t.patron_words.as_deref());
        let mut desc = format!("{}{}\n\n— Plan #{plan} · {key}", delegate::patron_section(patron.as_deref()), t.prompt.trim());
        if !deps.is_empty() {
            desc.push_str(&format!(" · önkoşul: {}", deps.join(", ")));
        }
        let task = tasks::create(&tx, tasks::NewTask {
            title: t.title.trim().into(),
            description: desc,
            status: None,
            agent_id: None,
            team_id: Some(lead.team_id.clone()),
        })?;
        tx.execute("UPDATE tasks SET project_id=?2 WHERE id=?1", params![task.id, snapshot.project_id]).map_err(e)?;
        let (target, role) = if t.agent_id.is_some() {
            (delegate::pick_worker(&tx, &lead.team_id, target_of(t))?, None)
        } else {
            let (role, _) = delegate::role_target(&tx, &lead.team_id, target_of(t).unwrap_or(""))?;
            (role.clone(), Some(role))
        };
        tx.execute(
            "INSERT INTO plan_items (plan_id, key, title, prompt, target, depends_on, task_id, patron_words, effort, project_id, project_root, merge_branch, repo_key, model, model_reason, model_engine, model_provider) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![plan, key, t.title.trim(), t.prompt.trim(), target, json!(deps).to_string(), task.id, patron, t.effort,
                snapshot.project_id, snapshot.project_root, snapshot.merge_branch, snapshot.repo_key, t.model, t.model_reason,
                t.model_snapshot.as_ref().map(|s| &s.route.engine), t.model_snapshot.as_ref().map(|s| &s.route.provider)],
        )
        .map_err(e)?;
        tx.execute(
            "INSERT INTO plan_targets(item_id,role_slug,agent_id) VALUES(?1,?2,?3)",
            params![tx.last_insert_rowid(), role, if role.is_none() { Some(&target) } else { None }],
        ).map_err(e)?;
    }
    tx.commit().map_err(e)?;
    Ok(plan)
}

// ---- advancing ----

/// G gives only `report_file` read access; C merges only SameRepository.git_branch.
/// No project directory permission is implied by dependsOn, even in the same team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DependencyScope {
    SameRepository,
    ReportOnly,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyAccess {
    pub delegation_id: i64,
    pub scope: DependencyScope,
    pub report_file: Option<std::path::PathBuf>,
    pub git_branch: Option<String>,
}

pub const CROSS_PROJECT_REPORT_RULE: &str = "Farklı projedeki önkoşul yalnız sıralama ve rapor bağımlılığıdır; yalnız belirtilen rapor dosyasını salt oku. Diğer projenin kaynaklarına/dalına erişim veya merge izni vermez.";

/// Same project, then the same pinned repository. #901: an item finished before migration 29 has
/// no saved `repo_key`; it counts when both jobs' snapshot roots are the same folder (no guess).
fn same_repository(project: &Option<String>, repo: &Option<String>, root: Option<&str>, dep_project: &Option<String>, dep_repo: &Option<String>, dep_root: Option<&str>) -> bool {
    use crate::projects::path_key;
    project.is_some() && project == dep_project && match (repo, dep_repo) {
        (Some(a), Some(b)) => a == b,
        _ => root.zip(dep_root).is_some_and(|(a, b)| path_key(a) == path_key(b)),
    }
}

/// Saved item identities, never teams.cwd/default or a caller-supplied path. Two bounded
/// queries for a plan (MAX_TASKS); no per-dependency lookup or repository inspection.
pub fn dependency_access(conn: &Connection, delegation: i64) -> Result<Vec<DependencyAccess>, String> {
    type Head = (i64, String, Option<String>, Option<String>, Option<String>);
    let head: Option<Head> = conn
        .query_row(
            "SELECT i.plan_id,i.depends_on,i.project_id,i.repo_key,d.project_root FROM plan_items i JOIN delegations d ON d.id=i.delegation_id WHERE i.delegation_id=?1",
            [delegation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(e)?;
    let Some((plan, deps, project, repo, head_root)) = head else { return Ok(vec![]) };
    let deps: Vec<String> = serde_json::from_str(&deps).map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare(
            "SELECT i.key,d.id,i.project_id,i.repo_key,d.result_path,d.project_root,d.service_team_id,w.branch
        FROM plan_items i JOIN delegations d ON d.id=i.delegation_id
        LEFT JOIN delegation_worktrees w ON w.delegation_id=d.id AND w.removed_at IS NULL WHERE i.plan_id=?1",
        )
        .map_err(e)?;
    let rows = st
        .query_map([plan], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(e)?;
    let mut access = Vec::new();
    for row in rows {
        let (key, id, dep_project, dep_repo, report, root, service, branch) = row.map_err(e)?;
        if !deps.contains(&key) {
            continue;
        }
        let same = same_repository(&project, &repo, head_root.as_deref(), &dep_project, &dep_repo, root.as_deref());
        let report_file = report.map(std::path::PathBuf::from).filter(|path| {
            root.as_ref().is_some_and(|root| {
                let root = std::path::Path::new(root);
                let dir = if service.is_some() { root.join("docs/research") } else { root.join(".dex/results") };
                delegate::path_within(path, &dir)
            })
        });
        access.push(DependencyAccess {
            delegation_id: id,
            scope: if same { DependencyScope::SameRepository } else { DependencyScope::ReportOnly },
            report_file,
            git_branch: if same { branch } else { None },
        });
    }
    Ok(access)
}

/// What the worker gets: the leader's prompt + where it sits in the plan + the prerequisites' results.
fn item_prompt(conn: &Connection, plan: i64, it: &Item, deps: &[Item]) -> String {
    let mut s = format!("{}\n\n[Plan #{plan} · görev {}: {}]", it.prompt, it.key, it.title);
    if it.attempts > 0 {
        s.push_str("\nNot: önceki deneme yarım kaldı (Dex kapandı ya da iş başarısız/raporsuz bitti); çalışma dizinindeki yarım işi kontrol ederek devam et.");
    }
    if !deps.is_empty() {
        s.push_str("\nÖnkoşul sonuçları:");
        for d in deps {
            let got = d.delegation_id.and_then(|id| delegate::get(conn, id).ok());
            let who = got.as_ref().and_then(|g| delegate::caller(conn, &g.to_agent).ok()).map(|c| c.name).unwrap_or_default();
            s.push_str(&format!("\n- {} ({}) — {who}: {}", d.key, d.title, got.as_ref().and_then(|g| g.summary.clone()).unwrap_or_default()));
            if let Some(p) = got.as_ref().and_then(|g| g.result_path.as_deref()) {
                s.push_str(&format!("\n  sonuç dosyası: {p}"));
            }
            let dep_root = got.as_ref().and_then(|g| g.project_root.as_deref());
            let differs = d.project.project_id != it.project.project_id || d.project.repo_key != it.project.repo_key;
            if differs && !same_repository(&it.project.project_id, &it.project.repo_key, it.project.project_root.as_deref(), &d.project.project_id, &d.project.repo_key, dep_root) {
                s.push_str(&format!("\n  {CROSS_PROJECT_REPORT_RULE}"));
            }
        }
    }
    s
}

fn start_item(conn: &Connection, lead: &Caller, plan: i64, it: &Item, deps: &[Item]) -> Result<Delegation, String> {
    // A folderless legacy project (no root anywhere) keeps the summary-only path. A root that
    // migration 29 left NULL cannot be filled later (trigger), so a retry cannot recover:
    // fail before any row is written instead of starting rootless (no team-cwd fallback).
    if let (Some(id), None) = (&it.project.project_id, &it.project.project_root) {
        if crate::projects::get(conn, &lead.team_id, id).map_err(|e| e.to_string())?.root_path.is_some() {
            return Err(format!("{}: proje kökü yok — planı iptal edip yeniden oluştur", crate::projects::ProjectError::Unavailable));
        }
    }
    let (to,role) = if let Some(agent) = &it.pinned_agent {
        let worker = delegate::caller(conn, agent)?;
        if worker.kind != "worker" || worker.team_id != lead.team_id { return Err("plan worker'ı artık bu takımda değil".into()); }
        (agent.clone(), None)
    } else if let Some(role) = &it.target_role {
        (delegate::role_target(conn, &lead.team_id, role)?.1, Some(role.clone()))
    } else {
        // Pre-pool plans stored one untyped key: id/name means pinned, other aliases mean role.
        let to = delegate::pick_worker(conn, &lead.team_id, Some(&it.target))?;
        let named: bool = conn.query_row("SELECT lower(id)=lower(?2) OR lower(name)=lower(?2) FROM agents WHERE id=?1",params![to,it.target],|r|r.get(0)).map_err(e)?;
        let role = if named { None } else { Some(delegate::role_target(conn, &lead.team_id, &it.target)?.0) };
        (to, role)
    };
    let tx = conn.unchecked_transaction().map_err(e)?;
    let conn = &tx;
    let task_id = it.task_id.filter(|t| tasks::get(conn, *t).is_ok());
    if let Some(t) = task_id {
        tasks::update(conn, t, tasks::TaskPatch { status: Some("doing".into()), agent_id: Some(Some(to.clone())), ..Default::default() })?;
    }
    let d = delegate::insert_snapshot(conn, delegate::NewDelegation {
        from: lead,
        to_agent: to,
        team_id: lead.team_id.clone(),
        service_team_id: None,
        prompt: item_prompt(conn, plan, it, deps),
        task_id,
        patron_words: it.patron_words.clone(),
        effort: it.effort,
    }, &it.project)?;
    crate::job_model::request(conn, d.id, it.model.as_deref(), it.model_reason.as_deref(), role.as_deref(), it.model_route.as_ref())?;
    if let Some(role) = role {
        delegate::queue_role(conn, d.id, &role, None)?;
        delegate::assign_role_job(conn, d.id)?;
    }
    conn.execute(
        "UPDATE plan_items SET status = 'running', delegation_id = ?2, attempts = attempts + 1 WHERE id = ?1",
        params![it.id, d.id],
    )
    .map_err(e)?;
    let d = delegate::get(conn, d.id)?;
    tx.commit().map_err(e)?;
    Ok(d)
}

/// Brings every active plan up to date with its delegations and starts what became ready.
/// Pure DB work; returns the new delegations (to dispatch) and whether the board changed.
pub fn step(conn: &Connection) -> Result<(Vec<Delegation>, bool), String> {
    let plans: Vec<(i64, String)> = conn
        .prepare("SELECT id, leader FROM plans WHERE status = 'active' ORDER BY id")
        .map_err(e)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    let (mut started, mut board) = (Vec::new(), false);
    for (plan, leader) in plans {
        let Ok(lead) = delegate::caller(conn, &leader) else {
            touch(conn, plan, Some("cancelled"))?; // the leader is gone; nobody would hear about it
            continue;
        };
        let mut items = items_of(conn, plan)?;
        let mut failed = Vec::new();
        // 1. delegation → item (a late report can turn a failed item into done).
        for it in items.iter_mut().filter(|i| matches!(i.status.as_str(), "running" | "failed")) {
            let Some(d) = it.delegation_id.and_then(|id| delegate::get(conn, id).ok()) else { continue };
            let new = match d.status.as_str() {
                "done" => "done",
                "done_no_report" | "failed" => "failed",
                _ => "running",
            };
            if new == it.status {
                continue;
            }
            set_item(conn, it.id, new)?;
            it.status = new.into();
            if new == "done" {
                if let Some(t) = it.task_id.filter(|t| tasks::get(conn, *t).is_ok()) {
                    tasks::update(conn, t, tasks::TaskPatch { status: Some("done".into()), ..Default::default() })?;
                    board = true;
                }
            } else if new == "failed" {
                // Nobody is on it any more: back to the board's to-do column (assignee kept as a hint).
                if let Some(t) = it.task_id.filter(|t| tasks::get(conn, *t).is_ok()) {
                    tasks::update(conn, t, tasks::TaskPatch { status: Some("todo".into()), ..Default::default() })?;
                    board = true;
                }
                failed.push(format!("{} (devretme #{}, {})", it.key, d.id, if d.status == "failed" { "başarısız" } else { "raporsuz bitti" }));
            }
        }
        // 2. waiting/blocked items until nothing changes (blocking propagates down the chain).
        let mut blocked = Vec::new();
        loop {
            let mut changed = false;
            for i in 0..items.len() {
                if !matches!(items[i].status.as_str(), "waiting" | "blocked") {
                    continue;
                }
                let deps: Vec<Item> = items.iter().filter(|d| items[i].depends_on.contains(&d.key)).cloned().collect();
                let next = if deps.iter().all(|d| d.status == "done") {
                    match start_item(conn, &lead, plan, &items[i], &deps) {
                        Ok(d) => {
                            items[i].delegation_id = Some(d.id);
                            started.push(d);
                            board = true;
                            "running"
                        }
                        Err(err) => {
                            set_item(conn, items[i].id, "failed")?;
                            failed.push(format!("{} (başlatılamadı: {err})", items[i].key));
                            "failed"
                        }
                    }
                } else if deps.iter().any(|d| matches!(d.status.as_str(), "failed" | "blocked" | "cancelled")) {
                    "blocked"
                } else {
                    "waiting"
                };
                if next != items[i].status {
                    if next != "running" && next != "failed" {
                        set_item(conn, items[i].id, next)?;
                    }
                    if next == "blocked" {
                        blocked.push(items[i].key.clone());
                    }
                    items[i].status = next.into();
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if !failed.is_empty() {
            let mut t = format!("Plan #{plan}: başarısız: {}.", failed.join("; "));
            if !blocked.is_empty() {
                t.push_str(&format!(
                    " Önkoşulu bittiği için bekleyenler: {}. Önkoşulun worker'ı sonradan report verirse plan kendiliğinden sürer; \
                     yoksa plan_retry ile yeniden devret ya da plan_cancel ile kalanları iptal et.",
                    blocked.join(", ")
                ));
            }
            note(conn, &lead.id, &crate::dexmsg::line(crate::dexmsg::PLAN, t))?;
        }
        if items.iter().all(|i| i.status == "done") {
            touch(conn, plan, Some("done"))?;
            note(conn, &lead.id, &crate::dexmsg::line(crate::dexmsg::PLAN, format!("Plan #{plan} tamamlandı ({} görev).", items.len())))?;
        } else {
            touch(conn, plan, None)?;
        }
    }
    let assigned = delegate::assign_role_jobs(conn)?;
    for d in assigned {
        if !started.iter().any(|s| s.id == d.id) { started.push(d); }
        board = true;
    }
    for d in &mut started { *d = delegate::get(conn, d.id)?; }
    Ok((started, board))
}

// One advance at a time: the MCP call and the event watcher must never start the same item twice.
static GATE: Mutex<()> = Mutex::new(());

/// [`step`], then deliver what started (each dispatch waits for its pane's ack; they run side by side).
pub fn advance(db: &Db, ev: &Events) -> Result<(), String> {
    advance_with_ptys(db, ev, &crate::pty::Ptys::default())
}

pub fn advance_with_ptys(db: &Db, ev: &Events, ptys: &crate::pty::Ptys) -> Result<(), String> {
    let _g = GATE.lock().map_err(|_| "plan kilidi zehirli")?;
    let (started, board) = with(db, step)?;
    if board {
        let _ = ev.emit("tasks-changed", &());
    }
    if !started.is_empty() {
        let _ = ev.emit("plans-changed", &());
    }
    for d in started {
        let (db, ev, ptys) = (db.clone(), ev.clone(), ptys.clone());
        std::thread::spawn(move || {
            if let Some(core) = ptys.core() {
                if let Err(err) = core.delegation_dispatch(d) {
                    eprintln!("dex plan dispatch: {err}");
                }
                return;
            }
            let running = ptys.is_running(&d.to_agent);
            if let Err(err) = delegate::dispatch(&db, &ev, d, running) {
                eprintln!("dex plan dispatch: {err}");
            }
        });
    }
    Ok(())
}

/// Before `reconcile_on_start` closes last session's open delegations: plan items that were in
/// flight go back to `waiting`, so [`advance`] delegates them again (attempts > 0 → the prompt says so).
/// #161 `keep_queued`: a clean restart keeps queued jobs — their items keep them (no second job).
pub fn requeue_interrupted(conn: &Connection, keep_queued: bool) -> Result<usize, String> {
    conn.execute(
        "UPDATE plan_items SET status = 'waiting', delegation_id = NULL
         WHERE status = 'running'
           AND plan_id IN (SELECT id FROM plans WHERE status = 'active')
           AND delegation_id IN (SELECT id FROM delegations WHERE status = 'running' OR (?1 = 0 AND status = 'queued' AND to_agent<>'' AND model_approval IS NOT 'pending'))",
        [keep_queued],
    )
    .map_err(e)
}

// ---- event watch ----

/// Results, questions, plan changes and final idle transitions kick pending-leader delivery.
struct Tap {
    inner: Events,
    kick: mpsc::Sender<Option<String>>,
}

impl EventSink for Tap {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        let ready = event == "dex-hook" && matches!(payload.get("event").and_then(Value::as_str), Some("SessionStart" | "UserPromptSubmit"));
        if ready || matches!(event, "delegations-changed" | "questions-changed" | "plans-changed" | "agent-idle") {
            let agent = ready
                .then(|| payload.get("agentId").and_then(Value::as_str).map(String::from)).flatten();
            let _ = self.kick.send(agent);
        }
        self.inner.emit(event, &payload)
    }
}

pub fn tap(events: Events) -> (Events, mpsc::Receiver<Option<String>>) {
    let (kick, rx) = mpsc::channel();
    (Events::new(Tap { inner: events, kick }), rx)
}

/// Kicks arriving in a burst collapse into one advance.
pub fn watch(rx: mpsc::Receiver<Option<String>>, db: Db, ev: Events, ptys: crate::pty::Ptys) {
    std::thread::spawn(move || {
        while let Ok(kick) = rx.recv() {
            let mut ready_agents = kick.into_iter().collect::<HashSet<_>>();
            while let Ok(kick) = rx.try_recv() { ready_agents.extend(kick); }
            if let Some(core) = ptys.core() {
                // UPS releases an accepted or cancelled reservation. A new input box cannot contain the old
                // paste; context has delivered results/questions, so rebuild only pending rows.
                let mut flights = core.leader_pastes.lock().unwrap();
                for agent in ready_agents {
                    if flights.get(&agent).is_some_and(|p| p.accepted || p.cancelled || p.session != core.agent_states.get(&agent).session_id
                        || p.pty_generation != core.ptys.generation(&agent)) {
                        flights.remove(&agent);
                    }
                }
            }
            if let Err(err) = advance_with_ptys(&db, &ev, &ptys) {
                eprintln!("dex plan advance: {err}");
            }
            if let Some(core) = ptys.core() { core.wake_sleeping_leaders(); }
        }
    });
}

// ---- leader-facing ----

fn own_plan(conn: &Connection, c: &Caller, id: i64) -> Result<Plan, String> {
    let p = get(conn, id)?;
    if p.team_id != c.team_id {
        return Err(format!("plan #{id} senin takımında değil"));
    }
    Ok(p)
}

/// Active plans of the team + the latest finished ones.
pub fn list_for(conn: &Connection, team: &str) -> Result<Vec<Plan>, String> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM plans WHERE team_id = ?1 ORDER BY status <> 'active', id DESC LIMIT 5")
        .map_err(e)?
        .query_map([team], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<_, _>>()
        .map_err(e)?;
    ids.into_iter().map(|id| get(conn, id)).collect()
}

/// Waiting/blocked items → cancelled (their board tasks go); a job still queued at its worker is
/// withdrawn. A job already running can't be stopped: it finishes and reports as usual.
pub fn cancel(conn: &Connection, c: &Caller, id: i64) -> Result<Value, String> {
    let p = own_plan(conn, c, id)?;
    if p.status != "active" {
        return Err(format!("plan #{id} zaten {}", if p.status == "done" { "bitti" } else { "iptal" }));
    }
    let (mut cancelled, mut still_running, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    let mut withdrawn = Vec::new();
    for it in &p.items {
        let drop_task = |conn: &Connection| -> Result<(), String> {
            if let Some(t) = it.task_id {
                let _ = tasks::delete(conn, t);
            }
            Ok(())
        };
        match it.status.as_str() {
            "waiting" | "blocked" => {
                set_item(conn, it.id, "cancelled")?;
                drop_task(conn)?;
                cancelled.push(it.key.clone());
            }
            "running" => {
                let queued = it.delegation_id.and_then(|d| delegate::get(conn, d).ok()).filter(|d| d.status == "queued");
                match queued {
                    Some(d) => {
                        conn.execute(
                            "UPDATE delegations SET status = 'failed', summary = 'plan iptal edildi', notified = 1, updated_at = ?2 WHERE id = ?1",
                            params![d.id, now()],
                        )
                        .map_err(e)?;
                        set_item(conn, it.id, "cancelled")?;
                        drop_task(conn)?;
                        withdrawn.push(d.id);
                        cancelled.push(it.key.clone());
                    }
                    None => still_running.push(it.key.clone()),
                }
            }
            _ => kept.push(format!("{} ({})", it.key, it.status)),
        }
    }
    touch(conn, id, Some("cancelled"))?;
    Ok(json!({ "planId": id, "cancelled": cancelled, "stillRunning": still_running, "kept": kept, "withdrawnDelegations": withdrawn,
                "note": if still_running.is_empty() { "" } else { "çalışan işler durdurulamaz; bitince sonuçları normal devretme olarak gelir" } }))
}

/// DEX-28: a `failed` item (its job failed, ended without a report, or could not start) goes back to
/// `waiting` with no delegation; [`step`] delegates it again (its prerequisites are done) and its
/// blocked dependents wait for it again. A late report of the old job no longer moves the item.
pub fn retry(conn: &Connection, c: &Caller, id: i64, key: &str) -> Result<Value, String> {
    let p = own_plan(conn, c, id)?;
    if p.status != "active" {
        return Err(format!("plan #{id} {}; yeniden denenemez", if p.status == "done" { "bitti" } else { "iptal" }));
    }
    let key = key.trim();
    let it = p.items.iter().find(|i| i.key == key).ok_or_else(|| format!("plan #{id} içinde görev yok: {key}"))?;
    if it.status != "failed" {
        return Err(format!("{key} yeniden denenemez: durumu {} (yalnız failed)", it.status));
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    let conn = &tx;
    conn.execute("UPDATE plan_items SET status = 'waiting', delegation_id = NULL WHERE id = ?1", [it.id]).map_err(e)?;
    // Everything blocked behind it waits again (step re-blocks what still has another failed prerequisite).
    let mut back: HashSet<&str> = HashSet::from([key]);
    let mut requeued = Vec::new();
    loop {
        let next: Vec<&Item> = p
            .items
            .iter()
            .filter(|i| i.status == "blocked" && !back.contains(i.key.as_str()) && i.depends_on.iter().any(|d| back.contains(d.as_str())))
            .collect();
        if next.is_empty() {
            break;
        }
        for i in next {
            set_item(conn, i.id, "waiting")?;
            back.insert(&i.key);
            requeued.push(i.key.clone());
        }
    }
    touch(conn, id, None)?;
    tx.commit().map_err(e)?;
    Ok(json!({ "planId": id, "retried": key, "waitingAgain": requeued }))
}

// ---- MCP (lead only; mcp.rs hands over names it doesn't know) ----

pub fn tools() -> Vec<Value> {
    vec![
        json!({ "name": "plan", "description": "YALNIZ LİDER. Birden çok görevi tek çağrıda, bağımlılıklarıyla devreder. Her görev panoda görünür; dependsOn'u olmayanlar hemen paralel başlar, olanlar önkoşulları bitince Dex tarafından otomatik devredilir (yoklama gerekmez). Önkoşul başarısız olursa bağımlılar bekler, sana bildirilir. Tek, bağımsız iş için delegate yeter.",
          "inputSchema": { "type": "object", "required": ["tasks"], "properties": {
              "title": { "type": "string" },
              "project": crate::mcp::project_schema(),
              "tasks": { "type": "array", "items": { "type": "object", "required": ["key", "title", "prompt"], "properties": {
                  "key": { "type": "string", "description": "Plan içinde benzersiz kısa ad (ör. A, api)" },
                  "title": { "type": "string" }, "prompt": { "type": "string" },
                  "role": { "type": "string", "description": "Katalogdaki rol slug'ı/adı (liste lider kurallarında); o roldeki en boş worker'a gider" },
                  "agentId": { "type": "string", "description": "Belirli bir worker (id ya da ad)" },
                  "project": crate::mcp::project_schema(),
                  "dependsOn": { "type": "array", "items": { "type": "string" }, "description": "Önce bitmesi gereken görevlerin key'leri" },
                  "patronWords": { "type": "string", "description": "Görev patronun bir isteğinden doğuyorsa patronun ilgili sözü, birebir alıntı; worker'a liderin açıklamasından önce ve üstün gider" },
                  "effort": crate::effort::schema(),
                  "model": crate::job_model::fields()["model"],
                  "modelReason": crate::job_model::fields()["modelReason"] } } } } } }),
        json!({ "name": "plan_status", "description": "YALNIZ LİDER. Planın (id verilmezse takımın son planlarının) görev görev durumu.",
          "inputSchema": { "type": "object", "properties": { "planId": { "type": "integer" } } } }),
        json!({ "name": "plan_cancel", "description": "YALNIZ LİDER. Planın başlamamış görevlerini iptal eder; bitenler kalır, çalışanlar bitince raporlar.",
          "inputSchema": { "type": "object", "required": ["planId"], "properties": { "planId": { "type": "integer" } } } }),
        json!({ "name": "plan_retry", "description": "YALNIZ LİDER. Planın başarısız (failed / raporsuz biten) görevini yeniden devreder; onu bekleyen bağımlılar yeniden bekler ve o bitince normal akışla sürer.",
          "inputSchema": { "type": "object", "required": ["planId", "key"], "properties": {
              "planId": { "type": "integer" }, "key": { "type": "string", "description": "Yeniden denenecek görevin key'i" } } } }),
        json!({ "name": "propose_member", "description": "YALNIZ LİDER. Takıma yeni worker önerir; patron onaylarsa eklenir (sen ekleyemezsin). Sonuç sana bildirilir (boştaysan yeni mesajla, çalışıyorsan tur sonunda). role yalnız rol kataloğundan.",
          "inputSchema": { "type": "object", "required": ["role", "reason"], "properties": {
              "role": { "type": "string", "description": "Katalogdaki rol slug'ı ya da adı; liste lider kurallarında" },
              "name": { "type": "string" }, "model": { "type": "string" }, "provider": { "type": "string" },
              "reason": { "type": "string", "description": "Patronun göreceği gerekçe" } } } }),
    ]
}

/// None → not one of these tools.
pub fn mcp_call(db: &Db, ev: &Events, c: &Caller, name: &str, args: &Value) -> Option<Result<Value, String>> {
    mcp_call_with_ptys(db, ev, &crate::pty::Ptys::default(), c, name, args)
}

pub fn mcp_call_with_ptys(db: &Db, ev: &Events, ptys: &crate::pty::Ptys, c: &Caller, name: &str, args: &Value) -> Option<Result<Value, String>> {
    if !matches!(name, "plan" | "plan_status" | "plan_cancel" | "plan_retry" | "propose_member") {
        return None;
    }
    if !c.is_leader() {
        return Some(Err(format!("yetki yok: {name} yalnız takım liderine açık ({} bir worker)", c.name)));
    }
    Some(match name {
        "plan" => (|| {
            let p: NewPlan = serde_json::from_value(args.clone()).map_err(|err| format!("plan biçimi: {err}"))?;
            let id = if let Some(core) = ptys.core() { core.plan_create_validated(c, p)? } else { with(db, |conn| create(conn, c, p))? };
            let _ = ev.emit("tasks-changed", &());
            let _ = ev.emit("plans-changed", &());
            advance_with_ptys(db, ev, ptys)?;
            Ok(json!({ "plan": with(db, |conn| get(conn, id))?,
                       "note": "running = devredildi; waiting = önkoşul bekliyor (Dex otomatik başlatır). Sonuçlar ve plan bildirimleri sonraki mesajlarında bağlamına gelir." }))
        })(),
        "plan_status" => with(db, |conn| match args.get("planId").and_then(Value::as_i64) {
            Some(id) => own_plan(conn, c, id).map(|p| json!(p)),
            None => list_for(conn, &c.team_id).map(|p| json!({ "plans": p })),
        }),
        "plan_cancel" => (|| {
            let id = args.get("planId").and_then(Value::as_i64).ok_or("planId gerekli")?;
            let out = with(db, |conn| cancel(conn, c, id))?;
            let _ = ev.emit("tasks-changed", &());
            let _ = ev.emit("plans-changed", &());
            if out["withdrawnDelegations"].as_array().is_some_and(|a| !a.is_empty()) {
                let _ = ev.emit("proposals-changed", &());
                let _ = ev.emit("delegations-changed", &Option::<()>::None);
            }
            Ok(out)
        })(),
        "plan_retry" => (|| {
            let id = args.get("planId").and_then(Value::as_i64).ok_or("planId gerekli")?;
            let key = args.get("key").and_then(Value::as_str).ok_or("key gerekli")?;
            let out = with(db, |conn| retry(conn, c, id, key))?;
            let _ = ev.emit("plans-changed", &());
            advance_with_ptys(db, ev, ptys)?;
            Ok(json!({ "result": out, "plan": with(db, |conn| get(conn, id))? }))
        })(),
        _ => (|| {
            let p = with(db, |conn| crate::proposals::propose(conn, c, args))?;
            let _ = ev.emit("proposals-changed", &());
            Ok(json!({ "proposalId": p.id, "status": p.status,
                       "note": "Patronun onayına gönderildi. Onay/ret sonucu sana bildirilir (boştaysan yeni mesajla, çalışıyorsan tur sonunda); o zamana kadar üyeyi ekli sayma." }))
        })(),
    })
}

impl Core {
    /// Continue active plans. Backend-owned agent starts make this independent from clients.
    pub fn plan_resume(&self) -> Result<(), String> {
        advance_with_ptys(&self.db, &self.events, &self.ptys)
    }

    pub fn plan_list(&self) -> Result<Vec<Plan>, String> {
        let ids: Vec<i64> = with(&self.db, |c| {
            c.prepare("SELECT id FROM plans WHERE status = 'active' OR updated_at > ?1 ORDER BY id")
                .map_err(e)?
                .query_map([now() - 24 * 3600 * 1000], |r| r.get(0))
                .map_err(e)?
                .collect::<Result<_, _>>()
                .map_err(e)
        })?;
        with(&self.db, |c| ids.into_iter().map(|id| get(c, id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn mem() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    fn task(key: &str, role: &str, deps: &[&str]) -> Value {
        json!({ "key": key, "title": format!("iş {key}"), "prompt": format!("{key} yap"), "role": role, "dependsOn": deps })
    }

    fn new_plan(tasks: Vec<Value>) -> NewPlan {
        serde_json::from_value(json!({ "title": "t", "tasks": tasks })).unwrap()
    }

    #[test]
    fn role_pool_waits_for_the_first_free_worker() {
        let c = mem();
        c.execute("UPDATE agents SET role_slug='backend' WHERE id='kaan'", [])
            .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let busy = create(
            &c,
            &lead,
            new_plan(vec![
                json!({"key":"one","title":"one","prompt":"one","agentId":"ayla"}),
                json!({"key":"two","title":"two","prompt":"two","agentId":"kaan"}),
            ]),
        )
        .unwrap();
        let (jobs, _) = step(&c).unwrap();
        for d in &jobs {
            delegate::set_status(&c, d.id, "running").unwrap();
        }
        let plan = create(&c, &lead, new_plan(vec![task("pool", "backend", &[])])).unwrap();
        step(&c).unwrap();
        let queued = get(&c, plan).unwrap().items[0].delegation_id.unwrap();
        assert!(
            delegate::get(&c, queued).unwrap().to_agent.is_empty(),
            "busy workers must not own pool jobs"
        );
        let freed = jobs.iter().find(|d| d.to_agent == "kaan").unwrap();
        delegate::set_status(&c, freed.id, "done").unwrap();
        step(&c).unwrap();
        assert_eq!(delegate::get(&c, queued).unwrap().to_agent, "kaan");
        assert_eq!(get(&c, busy).unwrap().items.len(), 2);
    }

    #[test]
    fn role_pool_restart_keeps_the_original_plan_delegation() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let id = create(
            &c,
            &lead,
            new_plan(vec![
                task("first", "backend", &[]),
                task("pool", "backend", &[]),
            ]),
        )
        .unwrap();
        step(&c).unwrap();
        let before = get(&c, id).unwrap();
        let pooled = before.items[1].delegation_id.unwrap();
        assert!(delegate::get(&c, pooled).unwrap().to_agent.is_empty());
        assert_eq!(
            requeue_interrupted(&c, false).unwrap(),
            1,
            "unassigned jobs must not be duplicated"
        );
        assert_eq!(delegate::reconcile_on_start(&c, false).unwrap(), 1);
        step(&c).unwrap();
        assert_eq!(get(&c, id).unwrap().items[1].delegation_id, Some(pooled));
        assert_eq!(delegate::get(&c, pooled).unwrap().to_agent, "ayla");
    }

    #[test]
    fn role_pool_status_shows_fifo_positions_and_the_assigned_worker() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let id = create(
            &c,
            &lead,
            new_plan(vec![
                task("busy", "backend", &[]),
                task("one", "backend", &[]),
                task("two", "backend", &[]),
            ]),
        )
        .unwrap();
        step(&c).unwrap();
        let p = get(&c, id).unwrap();
        let status = json!(p);
        assert_eq!(status["items"][1]["targetRole"], "backend");
        assert_eq!(status["items"][1]["queuePosition"], 1);
        assert_eq!(status["items"][2]["queuePosition"], 2);
        let busy = p.items[0].delegation_id.unwrap();
        let one = p.items[1].delegation_id.unwrap();
        let two = p.items[2].delegation_id.unwrap();
        delegate::set_status(&c, busy, "done").unwrap();
        step(&c).unwrap();
        let status = json!(get(&c, id).unwrap());
        assert_eq!(status["items"][1]["toAgent"], "ayla");
        assert_eq!(status["items"][2]["queuePosition"], 1);
        assert_eq!(delegate::get(&c, one).unwrap().to_agent, "ayla");
        assert!(delegate::get(&c, two).unwrap().to_agent.is_empty());
        delegate::set_status(&c, one, "done").unwrap();
        step(&c).unwrap();
        assert_eq!(delegate::get(&c, two).unwrap().to_agent, "ayla");
    }

    #[test]
    fn pinned_plan_target_never_falls_back_to_another_workers_name() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let id = create(
            &c,
            &lead,
            new_plan(vec![
                task("A", "frontend", &[]),
                json!({"key":"B","title":"B","prompt":"B","agentId":"ayla","dependsOn":["A"]}),
            ]),
        )
        .unwrap();
        step(&c).unwrap();
        finish(&c, id, "A", "done");
        c.execute("UPDATE agents SET name='ayla' WHERE id='bora'", [])
            .unwrap();
        c.execute("DELETE FROM agents WHERE id='ayla'", []).unwrap();
        let (jobs, _) = step(&c).unwrap();
        assert!(
            jobs.is_empty(),
            "an explicit recipient cannot be replaced by a name match"
        );
        assert_eq!(get(&c, id).unwrap().items[1].status, "failed");
    }

    #[test]
    fn mixed_plan_pins_item_override_and_retry_uses_creation_snapshot() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let project = |name: &str| {
            let root = f.dir(name);
            let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready).unwrap()
        };
        let one = project("one");
        let two = project("two");
        let mut b = task("B", "backend", &["A"]);
        b["project"] = json!(two.id);
        let p: NewPlan = serde_json::from_value(json!({"project": one.id, "tasks": [task("A", "frontend", &[]), b]})).unwrap();
        let id = create(&c, &lead, p).unwrap();
        let pins: Vec<(String, Option<String>, Option<String>, Option<String>)> = c
            .prepare("SELECT key,project_id,project_root,merge_branch FROM plan_items WHERE plan_id=?1 ORDER BY id")
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            pins,
            vec![
                ("A".into(), Some(one.id.clone()), one.root_path.clone(), one.default_branch.clone()),
                ("B".into(), Some(two.id.clone()), two.root_path.clone(), two.default_branch.clone())
            ]
        );
        let (first, _) = step(&c).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(json!(first[0])["projectId"], one.id);
        let report = std::path::PathBuf::from(one.root_path.as_ref().unwrap()).join(".dex/results/a.md");
        std::fs::create_dir_all(report.parent().unwrap()).unwrap();
        std::fs::write(&report, "A report").unwrap();
        let worker = delegate::caller(&c, &first[0].to_agent).unwrap();
        delegate::report(&c, &worker, first[0].id, "A done", report.to_str()).unwrap();
        let (b, _) = step(&c).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(json!(b[0])["projectId"], two.id);
        delegate::set_status(&c, b[0].id, "failed").unwrap();
        step(&c).unwrap();
        // A DB edit after creation must not change the waiting/retried item's snapshot.
        c.execute("UPDATE projects SET default_branch='changed' WHERE id=?1", [&two.id]).unwrap();
        crate::projects::set_default(&c, "default", Some(&one.id)).unwrap();
        retry(&c, &lead, id, "B").unwrap();
        let (again, _) = step(&c).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(json!(again[0])["projectRoot"], json!(two.root_path));
        assert_eq!(json!(again[0])["mergeBranch"], json!(two.default_branch));
        let access = dependency_access(&c, again[0].id).unwrap();
        assert_eq!(access.len(), 1);
        assert_eq!(access[0].scope, DependencyScope::ReportOnly);
        assert_eq!(access[0].report_file, Some(report));
        assert!(access[0].git_branch.is_none());
    }

    /// #901: a prerequisite finished before migration 29 (no saved repo_key/root) whose branch has
    /// not landed yet still reaches its dependant in the same project and folder.
    #[test]
    fn prerequisite_done_before_the_upgrade_counts_by_project_and_snapshot_root() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let root = f.dir("one");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let one = crate::projects::create(&c, "default", crate::projects::NewProject { name: "one".into(), worktrees: None }, &ready).unwrap();
        let p: NewPlan = serde_json::from_value(json!({"project": one.id, "tasks": [task("A", "frontend", &[]), task("B", "backend", &["A"])]})).unwrap();
        let id = create(&c, &lead, p).unwrap();
        let (a, _) = step(&c).unwrap();
        c.execute(
            "INSERT INTO delegation_worktrees (delegation_id, agent_id, root, path, branch, base, created_at) VALUES (?1, ?2, ?3, 'wt', 'dex/a', 'x', 1)",
            params![a[0].id, a[0].to_agent, one.root_path],
        )
        .unwrap();
        let report = root.join(".dex/results/a.md");
        std::fs::create_dir_all(report.parent().unwrap()).unwrap();
        std::fs::write(&report, "A report").unwrap();
        let worker = delegate::caller(&c, &a[0].to_agent).unwrap();
        delegate::report(&c, &worker, a[0].id, "A done", report.to_str()).unwrap();
        // The terminal item as migration 29 left it: no snapshot root/repository saved.
        c.execute_batch("DROP TRIGGER plan_items_project_snapshot").unwrap();
        c.execute("UPDATE plan_items SET project_root = NULL, repo_key = NULL WHERE plan_id = ?1 AND key = 'A'", [id]).unwrap();
        let (b, _) = step(&c).unwrap();
        assert_eq!(b.len(), 1);
        assert!(!b[0].prompt.contains(CROSS_PROJECT_REPORT_RULE), "{}", b[0].prompt);
        let access = dependency_access(&c, b[0].id).unwrap();
        assert_eq!(access.len(), 1);
        assert_eq!(access[0].scope, DependencyScope::SameRepository);
        assert_eq!(access[0].git_branch.as_deref(), Some("dex/a"));
        // Another folder of the same project is no proof of the same repository.
        c.execute_batch("DROP TRIGGER delegations_project_snapshot").unwrap();
        c.execute("UPDATE delegations SET project_root = 'elsewhere' WHERE id = ?1", [a[0].id]).unwrap();
        assert_eq!(dependency_access(&c, b[0].id).unwrap()[0].scope, DependencyScope::ReportOnly);
    }

    #[test]
    fn legacy_plan_verifies_metadata_once_and_pins_every_item() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let root = f.dir("legacy");
        crate::teams::update_team(
            &c,
            &f.0,
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(root.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let p = new_plan(vec![task("A", "frontend", &[]), task("B", "backend", &[])]);
        let contexts = resolve_projects(&c, &f.0, &lead, &p).unwrap();
        let ready = contexts.iter().map(crate::project_context::validate).collect::<Result<Vec<_>, _>>().unwrap();
        let id = create_prepared(&c, &lead, p, &contexts, &ready).unwrap();
        for item in items_of(&c, id).unwrap() {
            assert_eq!(item.project.project_id.as_deref(), Some("legacy-default"));
            assert_eq!(item.project.merge_branch.as_deref(), Some("fixture-main"));
            assert!(item.project.repo_key.is_some());
        }
    }

    #[test]
    fn invalid_plan_project_types_never_become_implicit_selection() {
        for invalid in [json!(null), json!(42), json!(""), json!({"path":"x"})] {
            assert!(serde_json::from_value::<NewPlan>(json!({"project":invalid,"tasks":[task("A","backend",&[])]})).is_err());
            let mut item = task("A", "backend", &[]);
            item["project"] = invalid;
            assert!(serde_json::from_value::<NewPlan>(json!({"tasks":[item]})).is_err());
        }
    }

    #[test]
    fn late_plan_insert_failure_rolls_back_tasks_and_verified_metadata() {
        let f = crate::projects::tests::Fixture::new();
        let c = mem();
        let root = f.dir("legacy");
        crate::teams::update_team(
            &c,
            &f.0,
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(root.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let p = new_plan(vec![task("A", "frontend", &[]), task("B", "backend", &[])]);
        let contexts = resolve_projects(&c, &f.0, &lead, &p).unwrap();
        let ready = contexts.iter().map(crate::project_context::validate).collect::<Result<Vec<_>, _>>().unwrap();
        c.execute_batch(
            "CREATE TRIGGER reject_item BEFORE INSERT ON plan_items WHEN NEW.key='B' BEGIN SELECT RAISE(ABORT,'late-plan-failure'); END;",
        )
        .unwrap();
        assert!(create_prepared(&c, &lead, p, &contexts, &ready).unwrap_err().contains("late-plan-failure"));
        for table in ["tasks", "plans", "plan_items", "delegations"] {
            assert_eq!(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0, "{table}");
        }
        assert_eq!(crate::projects::get(&c, "default", "legacy-default").unwrap().default_branch, None);
    }

    #[test]
    fn plan_item_start_failure_leaves_no_job_or_partial_task_assignment() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let id = create(&c, &lead, new_plan(vec![task("A", "backend", &[])])).unwrap();
        let item = items_of(&c, id).unwrap().remove(0);
        c.execute_batch("CREATE TRIGGER reject_running BEFORE UPDATE OF status ON plan_items WHEN NEW.status='running' BEGIN SELECT RAISE(ABORT,'late-start-failure'); END;").unwrap();
        assert!(start_item(&c, &lead, id, &item, &[]).unwrap_err().contains("late-start-failure"));
        let task = tasks::get(&c, item.task_id.unwrap()).unwrap();
        assert_eq!((task.status, task.agent_id), ("todo".into(), None));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let after = items_of(&c, id).unwrap().remove(0);
        assert_eq!((after.status, after.delegation_id, after.attempts), ("waiting".into(), None, 0));
    }

    #[test]
    fn retry_failure_rolls_back_the_failed_item_and_its_dependents() {
        let c = mem();
        let id = diamond(&c);
        step(&c).unwrap();
        finish(&c, id, "A", "failed");
        step(&c).unwrap();
        let before = get(&c, id).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        c.execute_batch("CREATE TRIGGER reject_retry BEFORE UPDATE OF status ON plan_items WHEN NEW.key='C' AND NEW.status='waiting' BEGIN SELECT RAISE(ABORT,'retry-failure'); END;").unwrap();
        assert!(retry(&c, &lead, id, "A").unwrap_err().contains("retry-failure"));
        let after = get(&c, id).unwrap();
        for (before, after) in before.items.iter().zip(&after.items) {
            assert_eq!((&after.status, after.delegation_id, after.attempts), (&before.status, before.delegation_id, before.attempts));
        }
    }

    fn status(c: &Connection, plan: i64) -> Vec<(String, String)> {
        items_of(c, plan).unwrap().into_iter().map(|i| (i.key, i.status)).collect()
    }

    fn finish(c: &Connection, plan: i64, key: &str, how: &str) {
        let it = items_of(c, plan).unwrap().into_iter().find(|i| i.key == key).unwrap();
        let d = delegate::get(c, it.delegation_id.unwrap()).unwrap();
        if how == "done" {
            let w = delegate::caller(c, &d.to_agent).unwrap();
            delegate::report(c, &w, d.id, &format!("{key} bitti"), Some("docs/x.md")).unwrap();
        } else {
            delegate::set_status(c, d.id, how).unwrap();
        }
    }

    fn diamond(c: &Connection) -> i64 {
        let lead = delegate::caller(c, "timur").unwrap();
        create(c, &lead, new_plan(vec![
            task("A", "frontend", &[]),
            task("B", "backend", &[]),
            task("C", "qa", &["A"]),
            task("D", "backend", &["B", "C"]),
        ]))
        .unwrap()
    }

    #[test]
    fn rejects_bad_plans() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let err = |t: Vec<Value>| create(&c, &lead, new_plan(t)).unwrap_err();
        assert!(err(vec![task("A", "qa", &["B"]), task("B", "qa", &["A"])]).contains("döngü"));
        assert!(err(vec![task("A", "qa", &["A"])]).contains("kendine"));
        assert!(err(vec![task("A", "qa", &["Z"])]).contains("bilinmeyen key: Z"));
        assert!(err(vec![task("A", "qa", &[]), task("A", "qa", &[])]).contains("iki kez"));
        assert!(err(vec![task("A", "tasarımcı", &[])]).contains("eşleşen worker yok"));
        assert!(err(vec![]).contains("boş"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "nothing written on error");
        assert!(tasks::list(&c, Some("default")).unwrap().is_empty());
    }

    #[test]
    fn runs_in_dependency_order() {
        let c = mem();
        let p = diamond(&c);
        assert_eq!(tasks::list(&c, Some("default")).unwrap().len(), 4, "every item is on the board");
        let (started, _) = step(&c).unwrap();
        assert_eq!(started.len(), 2);
        assert_eq!(status(&c, p), [("A", "running"), ("B", "running"), ("C", "waiting"), ("D", "waiting")].map(|(a, b)| (a.into(), b.into())));
        assert!(step(&c).unwrap().0.is_empty(), "idempotent");

        finish(&c, p, "A", "done");
        let (started, _) = step(&c).unwrap();
        assert_eq!(started.len(), 1);
        assert!(started[0].prompt.contains("A bitti") && started[0].prompt.contains("docs/x.md"), "{}", started[0].prompt);
        assert_eq!(status(&c, p)[2].1, "running");
        assert_eq!(status(&c, p)[3].1, "waiting", "D still needs B");

        finish(&c, p, "C", "done");
        assert!(step(&c).unwrap().0.is_empty());
        finish(&c, p, "B", "done");
        assert_eq!(step(&c).unwrap().0.len(), 1);
        finish(&c, p, "D", "done");
        step(&c).unwrap();
        assert_eq!(get(&c, p).unwrap().status, "done");
        assert!(tasks::list(&c, Some("default")).unwrap().iter().all(|t| t.status == "done"));
        assert!(take_notes(&c, "timur").unwrap().unwrap().contains("tamamlandı"));
    }

    #[test]
    fn failed_prerequisite_blocks_then_late_report_unblocks() {
        let c = mem();
        let p = diamond(&c);
        step(&c).unwrap();
        finish(&c, p, "A", "done_no_report");
        assert!(step(&c).unwrap().0.is_empty());
        assert_eq!(status(&c, p).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["failed", "running", "blocked", "blocked"]);
        let n = take_notes(&c, "timur").unwrap().unwrap();
        assert!(n.contains("A (devretme") && n.contains("bekleyenler: C, D"), "{n}");
        assert!(take_notes(&c, "timur").unwrap().is_none(), "one-shot");
        let a_task = items_of(&c, p).unwrap()[0].task_id.unwrap();
        assert_eq!(tasks::get(&c, a_task).unwrap().status, "todo", "a failed item's card leaves 'doing'");

        finish(&c, p, "A", "done"); // late report
        let (started, _) = step(&c).unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(status(&c, p).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["done", "running", "running", "waiting"]);
    }

    /// Görev #283: a task's patronWords reach the board, the job and the retried job.
    #[test]
    fn patron_words_survive_board_and_retry() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let mut a = task("A", "backend", &[]);
        a["patronWords"] = json!("düzen değişmez");
        let p = create(&c, &lead, new_plan(vec![a, task("B", "backend", &[])])).unwrap();
        let items = items_of(&c, p).unwrap();
        let desc = tasks::get(&c, items[0].task_id.unwrap()).unwrap().description;
        assert!(desc.starts_with("## Patronun sözü\ndüzen değişmez\n\nA yap"), "{desc}");
        assert!(!tasks::get(&c, items[1].task_id.unwrap()).unwrap().description.contains("Patronun sözü"));

        let (started, _) = step(&c).unwrap();
        let first = started.iter().find(|d| d.prompt.starts_with("A yap")).unwrap();
        assert_eq!(first.patron_words.as_deref(), Some("düzen değişmez"));
        assert!(started.iter().find(|d| d.prompt.starts_with("B yap")).unwrap().patron_words.is_none());
        finish(&c, p, "A", "failed");
        step(&c).unwrap();
        retry(&c, &lead, p, "A").unwrap();
        let (again, _) = step(&c).unwrap();
        assert_eq!(again.len(), 1);
        assert_ne!(again[0].id, first.id);
        assert!(again[0].to_agent.is_empty(), "the older B job owns the worker until it reports");
        finish(&c,p,"B","done");
        step(&c).unwrap();
        let assigned = delegate::get(&c,again[0].id).unwrap();
        assert_eq!(assigned.to_agent,"ayla");
        assert!(delegate::worker_prompt(&c, &assigned).unwrap().contains("## Patronun sözü\ndüzen değişmez\n"));
    }

    /// A plan's initially valid request survives a route change while prerequisites finish.
    #[test]
    fn model_plan_keeps_request_when_route_changes_before_item_is_ready() {
        let c = mem();
        let lead = delegate::caller(&c,"timur").unwrap();
        let mut a = task("A","backend",&[]);
        a["model"]=json!("opus");
        a["modelReason"]=json!("Derin analiz");
        create(&c,&lead,new_plan(vec![a])).unwrap();
        c.execute("UPDATE agents SET active_provider='glm',active_model='GLM-5.3' WHERE id='ayla'",[]).unwrap();
        let started = step(&c).unwrap().0;
        assert_eq!(started.len(),1,"route change must not fail an initially valid model request");
        let d=&started[0];
        assert_eq!(d.model.as_deref(),Some("opus"));
        assert_eq!(d.model_approval.as_deref(),Some("pending"));
        let p=crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c,std::path::Path::new("."),p.id,true,None).unwrap();
        let mut a=crate::teams::get_agent(&c,"ayla").unwrap();
        assert!(!crate::job_model::apply(&c,&mut a,Some(d.id)).unwrap());
        assert_eq!(a.active_model.as_deref(),Some("GLM-5.3"));
    }

    #[test]
    fn dependent_model_plan_preserves_validated_route_for_chain_providers() {
        for pinned in [false, true] {
            for (engine, provider, model) in [("claude", "glm", "GLM-5.3"), ("claude", "muse", "muse-spark-1.3")] {
                let c = mem();
                let lead = delegate::caller(&c, "timur").unwrap();
                let mut b = task("B", "backend", &["A"]);
                if pinned { b["agentId"] = json!("ayla"); }
                b["model"] = json!("opus");
                b["modelReason"] = json!("Derin analiz");
                let plan = create(&c, &lead, new_plan(vec![task("A", "frontend", &[]), b])).unwrap();
                let stored = get(&c, plan).unwrap().items.remove(1).model_route.unwrap();
                assert_eq!((stored.engine.as_str(), stored.provider.as_str()), ("claude", "anthropic"));
                step(&c).unwrap();
                c.execute("UPDATE agents SET active_engine=?1,active_provider=?2,active_model=?3 WHERE id='ayla'",
                    params![engine, provider, model]).unwrap();
                finish(&c, plan, "A", "done");
                let d = step(&c).unwrap().0.remove(0);
                let p = crate::proposals::list_pending(&c).unwrap().remove(0);
                assert_eq!(p.provider, "anthropic", "approval carries the validated provider");
                crate::proposals::decide(&c, std::path::Path::new("."), p.id, true, None).unwrap();
                let mut a = crate::teams::get_agent(&c, "ayla").unwrap();
                assert!(!crate::job_model::apply(&c, &mut a, Some(d.id)).unwrap(), "must keep {engine}/{provider} chain model");
                assert_eq!(a.active_model.as_deref(), Some(model));
                crate::job_model::record_spawn(&c, "ayla", Some(d.id), engine, provider, model, None).unwrap();
                let note = take_notes(&c, "timur").unwrap().unwrap();
                assert!(note.contains("opus") && note.contains(model) && note.contains("Neden:"), "{note}");
                assert!(crate::job_model::can_start(&c, d.id).unwrap());
            }
        }
    }

    #[test]
    fn legacy_model_plan_without_proven_route_keeps_chain_model() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let mut item = task("A", "backend", &[]);
        item["model"] = json!("opus");
        item["modelReason"] = json!("analysis");
        let id = create(&c, &lead, new_plan(vec![item])).unwrap();
        // Reproduce a pre-migration waiting plan; no safe historical route can be inferred.
        c.execute("UPDATE plan_items SET model_engine=NULL,model_provider=NULL WHERE plan_id=?1", [id]).unwrap();
        c.execute("UPDATE agents SET active_provider='glm',active_model='GLM-5.3' WHERE id='ayla'", []).unwrap();
        let d = step(&c).unwrap().0.remove(0);
        let p = crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c, std::path::Path::new("."), p.id, true, None).unwrap();
        let mut a = crate::teams::get_agent(&c, "ayla").unwrap();
        assert!(!crate::job_model::apply(&c, &mut a, Some(d.id)).unwrap());
        assert_eq!(a.active_model.as_deref(), Some("GLM-5.3"));
        let note = take_notes(&c, "timur").unwrap().unwrap();
        assert!(note.contains("opus") && note.contains("GLM-5.3") && note.contains("rotası kayıtlı değil"), "{note}");
    }

    /// Model retries receive a new approval; cancellation withdraws a pending proposal.
    #[test]
    fn model_plan_waits_for_approval_retries_require_fresh_approval_and_cancel_withdraws() {
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let mut a = task("A", "backend", &[]);
        a["model"] = json!("opus");
        a["modelReason"] = json!("Daha derin analiz");
        let p = create(&c, &lead, new_plan(vec![a])).unwrap();
        let first = step(&c).unwrap().0.remove(0);
        assert_eq!(first.model.as_deref(), Some("opus"));
        assert_eq!(first.model_approval.as_deref(), Some("pending"));
        assert!(!crate::job_model::can_start(&c, first.id).unwrap());
        assert_eq!(requeue_interrupted(&c, false).unwrap(), 0, "restart preserves pending approval");
        let proposal = crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c, std::path::Path::new("."), proposal.id, false, None).unwrap();
        step(&c).unwrap();
        retry(&c, &lead, p, "A").unwrap();
        let again = step(&c).unwrap().0.remove(0);
        assert_ne!(again.id, first.id);
        assert_eq!(again.model.as_deref(), Some("opus"));
        assert_eq!(again.model_approval.as_deref(), Some("pending"));
        assert_eq!(crate::proposals::list_pending(&c).unwrap().len(), 1);
        cancel(&c, &lead, p).unwrap();
        assert!(crate::proposals::list_pending(&c).unwrap().is_empty());
    }

    /// #671: a task's effort reaches its job and the retried job; low/max are refused at parse.
    #[test]
    fn effort_survives_retry_and_only_three_levels_parse() {
        use crate::effort::Effort;
        let c = mem();
        let lead = delegate::caller(&c, "timur").unwrap();
        let mut a = task("A", "backend", &[]);
        a["effort"] = json!("xhigh");
        let p = create(&c, &lead, new_plan(vec![a, task("B", "backend", &[])])).unwrap();
        let (started, _) = step(&c).unwrap();
        let first = started.iter().find(|d| d.prompt.starts_with("A yap")).unwrap();
        assert_eq!(first.effort, Some(Effort::Xhigh));
        assert_eq!(started.iter().find(|d| d.prompt.starts_with("B yap")).unwrap().effort, None);
        finish(&c, p, "A", "failed");
        step(&c).unwrap();
        retry(&c, &lead, p, "A").unwrap();
        let (again, _) = step(&c).unwrap();
        assert_eq!(again[0].effort, Some(Effort::Xhigh));
        for bad in ["low", "max"] {
            let mut t = task("C", "backend", &[]);
            t["effort"] = json!(bad);
            let err = serde_json::from_value::<NewPlan>(json!({ "tasks": [t] })).unwrap_err().to_string();
            assert!(err.contains(bad) && err.contains("xhigh"), "{err}");
        }
    }

    #[test]
    fn retry_redelegates_failed_item_and_its_dependents_wait_again() {
        let c = mem();
        let p = diamond(&c);
        step(&c).unwrap();
        finish(&c, p, "A", "done_no_report");
        step(&c).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let old = items_of(&c, p).unwrap()[0].delegation_id.unwrap();
        assert!(retry(&c, &lead, p, "B").unwrap_err().contains("durumu running"));
        assert!(retry(&c, &lead, p, "Z").unwrap_err().contains("görev yok"));

        let out = retry(&c, &lead, p, "A").unwrap();
        assert_eq!(out["waitingAgain"], json!(["C", "D"]), "{out}");
        assert_eq!(status(&c, p).iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["waiting", "running", "waiting", "waiting"]);
        let (started, _) = step(&c).unwrap();
        assert_eq!(started.len(), 1, "only A: C and D wait for it");
        assert_ne!(started[0].id, old);
        assert!(started[0].prompt.contains("yarım kaldı"), "{}", started[0].prompt);
        let a = &items_of(&c, p).unwrap()[0];
        assert_eq!((a.status.as_str(), a.attempts), ("running", 2));
        assert_eq!(tasks::get(&c, a.task_id.unwrap()).unwrap().status, "doing");

        finish(&c, p, "A", "done");
        assert_eq!(step(&c).unwrap().0.len(), 1, "C follows as usual");
        assert_eq!(status(&c, p)[2].1, "running");
        cancel(&c, &lead, p).unwrap();
        assert!(retry(&c, &lead, p, "D").unwrap_err().contains("iptal"));
    }

    #[test]
    fn cancel_keeps_finished_and_withdraws_queued() {
        let c = mem();
        let p = diamond(&c);
        step(&c).unwrap();
        finish(&c, p, "A", "done");
        step(&c).unwrap(); // C starts (queued at its worker)
        let b = items_of(&c, p).unwrap()[1].delegation_id.unwrap();
        delegate::set_status(&c, b, "running").unwrap(); // B's pane acked
        let lead = delegate::caller(&c, "timur").unwrap();
        let out = cancel(&c, &lead, p).unwrap();
        assert_eq!(out["cancelled"], json!(["C", "D"]), "{out}");
        assert_eq!(out["stillRunning"], json!(["B"]), "{out}");
        let s = status(&c, p);
        assert_eq!(s[0].1, "done");
        assert_eq!(s[2].1, "cancelled");
        assert_eq!(s[3].1, "cancelled");
        assert_eq!(get(&c, p).unwrap().status, "cancelled");
        assert!(step(&c).unwrap().0.is_empty());
    }

    #[test]
    fn restart_requeues_in_flight_items() {
        let c = mem();
        diamond(&c);
        step(&c).unwrap();
        assert_eq!(requeue_interrupted(&c, false).unwrap(), 2);
        delegate::reconcile_on_start(&c, false).unwrap();
        let (started, _) = step(&c).unwrap();
        assert_eq!(started.len(), 2);
        assert!(started[0].prompt.contains("yarım kaldı"));
    }

    #[test]
    fn notes_merge_into_hook_answers() {
        let db = Db::new(mem());
        with(&db, |c| note(c, "timur", "Plan #1 tamamlandı")).unwrap();
        let prior = json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": "önce" } }).to_string();
        let out: Value = serde_json::from_str(&with_notes(&db, "timur", "UserPromptSubmit", Some(prior)).unwrap()).unwrap();
        let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.starts_with("önce") && ctx.contains("Plan #1 tamamlandı"), "{ctx}");
        assert!(with_notes(&db, "timur", "UserPromptSubmit", None).is_none());
        with(&db, |c| note(c, "timur", "x")).unwrap();
        let out: Value = serde_json::from_str(&with_notes(&db, "timur", "Stop", None).unwrap()).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(with_notes(&db, "timur", "PreToolUse", None).is_none());
    }

    #[test]
    fn worker_cannot_plan_or_propose() {
        let db = Db::new(mem());
        let ev = Events::new(crate::events::NullSink);
        let kaan = with(&db, |c| delegate::caller(c, "kaan")).unwrap();
        for name in ["plan", "plan_status", "plan_cancel", "plan_retry", "propose_member"] {
            let r = mcp_call(&db, &ev, &kaan, name, &json!({})).unwrap();
            assert!(r.unwrap_err().contains("yetki yok"), "{name}");
        }
        assert!(mcp_call(&db, &ev, &kaan, "task_list", &json!({})).is_none());
    }
}
