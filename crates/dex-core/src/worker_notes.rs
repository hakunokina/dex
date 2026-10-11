//! Durable leader instructions, bound to a job rather than its provisional recipient.
use crate::delegate::{self, Caller, Delegation};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

#[derive(Default)]
pub(crate) struct Receipts(Mutex<HashMap<String, (PathBuf, u64, u64)>>);

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS worker_notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    delegation_id INTEGER NOT NULL REFERENCES delegations(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    delivered_at INTEGER
);
CREATE INDEX IF NOT EXISTS worker_notes_pending ON worker_notes(delegation_id, id) WHERE delivered_at IS NULL;";

pub(crate) struct Note {
    pub id: i64,
    pub delegation_id: i64,
    pub text: String,
    created_at: i64,
}

pub(crate) fn send(
    conn: &Connection,
    caller: &Caller,
    id: i64,
    text: &str,
) -> Result<Value, String> {
    if !caller.is_leader() {
        return Err("yetki yok: note yalnız takım liderine açık".into());
    }
    let d = delegate::get(conn, id)?;
    if d.team_id != caller.team_id || (d.service_team_id.is_some() && d.from_agent != caller.id) {
        return Err(format!(
            "devretme #{id} senin takımının ya da verdiğin hizmet işi değil"
        ));
    }
    if !matches!(d.status.as_str(), "queued" | "running") {
        return Err(format!("devretme #{id} açık değil ({})", d.status));
    }
    if !d.to_agent.is_empty() {
        let target = delegate::caller(conn, &d.to_agent)?;
        if target.kind != "worker"
            || target.team_id != d.service_team_id.as_deref().unwrap_or(&d.team_id)
        {
            return Err(
                "devretmenin hedefi kendi takımında ya da hizmet takımında bir worker olmalı"
                    .into(),
            );
        }
    }
    if text.trim().is_empty() {
        return Err("text boş olamaz".into());
    }
    conn.execute(
        "INSERT INTO worker_notes(delegation_id,text,created_at) VALUES(?1,?2,?3)",
        params![id, text, crate::diagnostics::now_ms()],
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"ok":true,"delegationId":id,"noteId":conn.last_insert_rowid(),"status":d.status,
        "note":if d.status == "running" { "Worker çalışıyor, ilk uygun hook sınırında iletilecek." }
            else { "Kuyruğa alındı; iş başlarken worker'a iletilecek." }}),
    )
}

pub(crate) fn pending(conn: &Connection, worker: &str) -> Result<Vec<Note>, String> {
    for_worker(conn, worker, false)
}

fn for_worker(conn: &Connection, worker: &str, include_queued: bool) -> Result<Vec<Note>, String> {
    let mut st = conn.prepare("SELECT n.id,n.delegation_id,n.text,n.created_at FROM delegations d JOIN worker_notes n ON n.delegation_id=d.id
        WHERE d.to_agent=?1 AND d.status IN ('queued','running') AND (d.status='running' OR ?2) AND n.delivered_at IS NULL ORDER BY n.id").map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params![worker, include_queued], |r| {
            Ok(Note {
                id: r.get(0)?,
                delegation_id: r.get(1)?,
                text: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub(crate) fn text(notes: &[Note]) -> String {
    notes.iter().map(render).collect::<Vec<_>>().join("\n")
}

fn render(n: &Note) -> String {
    format!(
        "[Dex lider notu #{}]\nLiderden not (#{}): {}\n",
        n.id, n.delegation_id, n.text
    )
}

pub(crate) fn for_job(conn: &Connection, id: i64) -> Result<Vec<Note>, String> {
    let mut st = conn.prepare("SELECT id,delegation_id,text,created_at FROM worker_notes WHERE delegation_id=?1 AND delivered_at IS NULL ORDER BY id").map_err(|e| e.to_string())?;
    let rows = st
        .query_map([id], |r| {
            Ok(Note {
                id: r.get(0)?,
                delegation_id: r.get(1)?,
                text: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Avoid repeating notes already in the incoming prompt or prepared start context.
/// Neither preparing nor returning context acknowledges delivery.
pub(crate) fn prepare(
    conn: &Connection,
    worker: &str,
    body: &Value,
    out: &Option<String>,
) -> Result<Vec<Note>, String> {
    let v: Value = out
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(json!({}));
    let submitted = body["prompt"].as_str().unwrap_or("");
    let context = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(pending(conn, worker)?
        .into_iter()
        .filter(|n| {
            let rendered = render(n);
            !submitted.contains(&rendered) && !context.contains(&rendered)
        })
        .collect())
}

/// Only text received from the CLI, never text prepared for its response, is a receipt.
pub(crate) fn accept_text(
    conn: &Connection,
    worker: &str,
    text: &str,
    at: u64,
) -> Result<(), String> {
    let mut ids = Vec::new();
    for n in for_worker(conn, worker, true)? {
        if n.created_at >= 0 && at >= n.created_at as u64 && text.contains(&render(&n)) {
            ids.push(n.id);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    accept(conn, worker, &ids)
}

fn context_texts<'a>(engine: &str, row: &'a Value) -> Vec<&'a str> {
    if engine == "claude"
        && row["type"] == "attachment"
        && row["attachment"]["type"] == "hook_additional_context"
        && matches!(
            row["attachment"]["hookEvent"].as_str(),
            Some("UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure")
        )
    {
        return row["attachment"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
    }
    let p = &row["payload"];
    if engine == "codex"
        && row["type"] == "response_item"
        && p["type"] == "message"
        && p["role"] == "developer"
    {
        if let (Some(content), Some(kinds)) = (
            p["content"].as_array(),
            p["internal_chat_message_metadata_passthrough"]["content_item_kinds"].as_array(),
        ) {
            if content.len() == kinds.len() {
                return content
                    .iter()
                    .zip(kinds)
                    .filter(|(c, kind)| {
                        c["type"] == "input_text" && **kind == "hooks.additional_context"
                    })
                    .filter_map(|(c, _)| c["text"].as_str())
                    .collect();
            }
        }
    }
    Vec::new()
}

impl crate::Core {
    /// A receipt belongs to the currently watched CLI process and session. Its exact
    /// note identity, rather than a successful socket write, proves context delivery.
    pub(crate) fn worker_notes_observed(
        &self,
        agent: &str,
        engine: &str,
        path: &std::path::Path,
        row: &Value,
        process: Option<u64>,
    ) {
        let texts = context_texts(engine, row);
        if texts.is_empty() {
            return;
        }
        let Some((current, started)) = self
            .ptys
            .process(agent)
            .filter(|(current, _)| Some(*current) == process)
        else {
            return;
        };
        let state = self.agent_states.get(agent);
        let Some(session) = state.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return;
        };
        if state.status == crate::agents::Status::Offline
            || state.engine.as_deref() != Some(engine)
            || state.transcript.as_deref() != path.to_str()
            || row["isSidechain"] == true
        {
            return;
        }
        if engine == "claude" {
            if row["sessionId"]
                .as_str()
                .or_else(|| row["session_id"].as_str())
                != Some(session)
            {
                return;
            }
        } else if !path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .ends_with(&format!("-{session}.jsonl"))
        }) {
            return;
        }
        let Some(at) = crate::chat::parse_ts(&row["timestamp"]).filter(|at| *at >= started) else {
            return;
        };
        let result = crate::db::with(&self.db, |c| {
            // A process replaced while waiting for the connection cannot consume its successor's notes.
            if self.ptys.generation(agent) != Some(current)
                || self.agent_states.get(agent).session_id.as_deref() != Some(session)
            {
                return Ok(());
            }
            for text in texts {
                accept_text(c, agent, text, at)?;
            }
            Ok(())
        });
        if let Err(err) = result {
            eprintln!("dex worker note receipt: {err}");
        }
    }

    /// The watcher may lag the next hook. Reconcile durable receipts before preparing
    /// another payload; malformed or unavailable records leave the notes pending.
    pub(crate) fn worker_notes_reconcile(&self, agent: &str) {
        use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
        if !crate::db::with(&self.db, |c| Ok(!for_worker(c, agent, true)?.is_empty()))
            .unwrap_or(false)
        {
            return;
        }
        let state = self.agent_states.get(agent);
        let (Some(engine), Some(path), Some(process)) = (
            state.engine.as_deref(),
            state.transcript.as_deref(),
            self.ptys.generation(agent),
        ) else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return;
        };
        let mut receipts = self.worker_note_receipts.0.lock().unwrap();
        let cursor = receipts
            .entry(agent.into())
            .or_insert_with(|| (PathBuf::from(path), process, 0));
        if cursor.0 != std::path::Path::new(path) || cursor.1 != process || cursor.2 > len {
            *cursor = (PathBuf::from(path), process, 0);
        }
        if file.seek(SeekFrom::Start(cursor.2)).is_err() {
            return;
        }
        // Freeze the file end and retain partial JSONL rows for the next hook.
        let mut reader = BufReader::new(file.take(len - cursor.2));
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => return,
                Ok(_) if bytes.last() != Some(&b'\n') => return,
                Ok(_) => cursor.2 += bytes.len() as u64,
            }
            if !bytes
                .windows(b"[Dex lider notu #".len())
                .any(|w| w == b"[Dex lider notu #")
            {
                continue;
            }
            if let Ok(row) = serde_json::from_slice(&bytes) {
                self.worker_notes_observed(
                    agent,
                    engine,
                    std::path::Path::new(path),
                    &row,
                    Some(process),
                );
            }
        }
    }

    pub(crate) fn worker_notes_prompt_received(&self, agent: &str, body: &Value) {
        let state = self.agent_states.get(agent);
        let Some(process) = self.ptys.generation(agent) else {
            return;
        };
        if state.status == crate::agents::Status::Offline
            || state.session_id.is_none()
            || state.session_id.as_deref() != body["session_id"].as_str()
        {
            return;
        }
        let Some(prompt) = body["prompt"].as_str() else {
            return;
        };
        if let Err(err) = crate::db::with(&self.db, |c| {
            if self.ptys.generation(agent) != Some(process)
                || self.agent_states.get(agent).session_id != state.session_id
            {
                return Ok(());
            }
            accept_text(c, agent, prompt, crate::diagnostics::now_ms())
        }) {
            eprintln!("dex worker note prompt receipt: {err}");
        }
    }
}

pub(crate) fn accept(conn: &Connection, worker: &str, ids: &[i64]) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for id in ids {
        tx.execute("UPDATE worker_notes SET delivered_at=?3 WHERE id=?1 AND delivered_at IS NULL
            AND delegation_id IN (SELECT id FROM delegations WHERE to_agent=?2 AND status IN ('queued','running'))",
            params![id,worker,crate::diagnostics::now_ms()]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Extend context without replacing a permission decision already produced by the hook.
pub(crate) fn merge(
    event: &str,
    out: Option<String>,
    text: &str,
) -> Result<Option<String>, String> {
    let mut v: Value = match out {
        Some(s) => serde_json::from_str(&s).map_err(|e| e.to_string())?,
        None => json!({}),
    };
    if event == "Stop" {
        let reason = v["reason"]
            .as_str()
            .map(|r| format!("{r}\n\n{text}"))
            .unwrap_or_else(|| text.into());
        v["decision"] = json!("block");
        v["reason"] = json!(reason);
    } else {
        let ctx = v
            .pointer("/hookSpecificOutput/additionalContext")
            .and_then(Value::as_str)
            .map(|s| format!("{s}\n{text}"))
            .unwrap_or_else(|| text.into());
        if !v["hookSpecificOutput"].is_object() {
            v["hookSpecificOutput"] = json!({});
        }
        v["hookSpecificOutput"]["hookEventName"] = json!(event);
        v["hookSpecificOutput"]["additionalContext"] = json!(ctx);
    }
    Ok(Some(v.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status<'a> {
    #[serde(flatten)]
    delegation: &'a Delegation,
    pending_notes: i64,
}

pub(crate) fn status<'a>(conn: &Connection, d: &'a Delegation) -> Result<Status<'a>, String> {
    let pending_notes = conn
        .query_row(
            "SELECT COUNT(*) FROM worker_notes WHERE delegation_id=?1 AND delivered_at IS NULL",
            [d.id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(Status {
        delegation: d,
        pending_notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{with, Db},
        events::{Events, NullSink},
    };
    use std::io::Write;

    fn job(conn: &Connection, worker: &str) -> Delegation {
        let lead = delegate::caller(conn, "timur").unwrap();
        delegate::insert(
            conn,
            delegate::NewDelegation {
                from: &lead,
                to_agent: worker.into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "work".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn unreceived_context_retries_the_same_notes_on_the_next_hook() {
        for event in ["UserPromptSubmit", "PostToolUse", "PostToolUseFailure"] {
            let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
            let d = job(&conn, "kaan");
            delegate::set_status(&conn, d.id, "running").unwrap();
            let lead = delegate::caller(&conn, "timur").unwrap();
            send(&conn, &lead, d.id, "retry-canary").unwrap();
            let db = Db::new(conn);
            let ev = Events::new(NullSink);
            let out =
                crate::questions::on_hook(&db, &ev, "kaan", event, &json!({}), || None).unwrap();
            assert!(out.contains("retry-canary"));
            // Preparing the response cannot prove that the CLI received it: the
            // connection can close or the hook can time out before it is written.
            drop(out);
            assert_eq!(
                with(&db, |c| pending(c, "kaan")).unwrap().len(),
                1,
                "{event}"
            );
            let retry =
                crate::questions::on_hook(&db, &ev, "kaan", "PostToolUse", &json!({}), || None)
                    .unwrap();
            assert!(retry.contains("retry-canary"), "{event}");
            assert_eq!(with(&db, |c| pending(c, "kaan")).unwrap().len(), 1);
        }
    }

    struct Sink;
    impl crate::pty_stream::PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool {
            true
        }
        fn exit(&mut self) {}
    }

    #[test]
    fn timed_out_http_context_remains_pending_for_the_next_hook() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Mutex,
        };
        use std::time::Duration;
        struct Delay {
            event: &'static str,
            armed: AtomicBool,
            prepared: mpsc::Sender<()>,
            resume: Mutex<mpsc::Receiver<()>>,
        }
        impl crate::events::EventSink for Delay {
            fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
                if event == "dex-hook"
                    && payload["event"] == self.event
                    && self.armed.swap(false, Ordering::SeqCst)
                {
                    self.prepared.send(()).unwrap();
                    self.resume
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap();
                }
                Ok(())
            }
        }
        for event in ["UserPromptSubmit", "PostToolUse", "PostToolUseFailure"] {
            let (prepared, wait) = mpsc::channel();
            let (resume, release) = mpsc::channel();
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.dex/tmp/note-timeout-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
            let core = crate::Core::start(
                crate::Paths {
                    data_dir: root.join("data"),
                    config_dir: root.join("config"),
                },
                Events::new(Delay {
                    event,
                    armed: AtomicBool::new(true),
                    prepared,
                    resume: Mutex::new(release),
                }),
            )
            .unwrap();
            let id = with(&core.db, |c| {
                let d = job(c, "kaan");
                delegate::set_status(c, d.id, "running")?;
                send(c, &delegate::caller(c, "timur")?, d.id, "timeout-canary")?;
                Ok(d.id)
            })
            .unwrap();
            let request = core.clone();
            let client = std::thread::spawn(move || {
                ureq::post(&format!("http://127.0.0.1:{}/hook", request.hooks.port))
                    .timeout(Duration::from_millis(200))
                    .set("Authorization", &format!("Bearer {}", request.hooks.token))
                    .set("X-Dex-Agent", "kaan").set("X-Dex-Key", &request.mcp.key_for("kaan"))
                    .send_json(json!({"hook_event_name":event,"session_id":"timeout-session","prompt":delegate::trigger_line(id)}))
            });
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            let timed_out = client.join().unwrap().is_err();
            let pending_before_retry = with(&core.db, |c| pending(c, "kaan")).unwrap().len();
            resume.send(()).unwrap();
            let retry = ureq::post(&format!("http://127.0.0.1:{}/hook", core.hooks.port))
                .timeout(Duration::from_secs(2))
                .set("Authorization", &format!("Bearer {}", core.hooks.token))
                .set("X-Dex-Agent", "kaan")
                .set("X-Dex-Key", &core.mcp.key_for("kaan"))
                .send_json(json!({"hook_event_name":"PostToolUse","session_id":"timeout-session"}))
                .unwrap()
                .into_string()
                .unwrap();
            let still_pending = with(&core.db, |c| pending(c, "kaan")).unwrap().len();
            core.shutdown();
            assert!(
                timed_out,
                "{event}: CLI did not receive the prepared response"
            );
            assert_eq!(pending_before_retry, 1, "{event}");
            assert!(retry.contains("timeout-canary"), "{event}");
            assert_eq!(
                still_pending, 1,
                "successful write alone is not a CLI receipt"
            );
        }
    }

    #[test]
    fn transcript_receipt_accepts_only_the_exact_note_and_stops_repeating_it() {
        for engine in ["claude", "codex"] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.dex/tmp/note-receipt-{engine}-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
            let core = crate::Core::start(
                crate::Paths {
                    data_dir: root.join("data"),
                    config_dir: root.join("config"),
                },
                Events::new(NullSink),
            )
            .unwrap();
            core.pty_spawn(
                crate::pty::Spawn {
                    id: "kaan".into(),
                    cmd: crate::shell::SHELL_CMD.into(),
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                },
                crate::agents::shell_sink_for_test(&core, "kaan", Sink),
            )
            .unwrap();
            core.agent_states.started("kaan", engine, true);
            core.agent_states.hook(
                "kaan",
                Some(crate::agents::Status::Working),
                Some("note-session"),
            );
            let transcript = root.join("rollout-note-session.jsonl");
            std::fs::write(&transcript, "").unwrap();
            core.agent_states
                .set_transcript("kaan", transcript.to_str().unwrap());
            let (id, rendered) = with(&core.db, |c| {
                let d = job(c, "kaan");
                delegate::set_status(c, d.id, "running")?;
                let lead = delegate::caller(c, "timur")?;
                send(c, &lead, d.id, "same-text")?;
                send(c, &lead, d.id, "same-text")?;
                let notes = pending(c, "kaan")?;
                Ok((notes[0].id, render(&notes[0])))
            })
            .unwrap();
            let out = core
                .delegation_on_hook(
                    "kaan",
                    "PostToolUse",
                    &json!({"session_id":"note-session"}),
                    false,
                )
                .unwrap();
            assert!(out.contains("same-text"));
            core.chat_watches
                .ensure(core.weak_self.clone(), "kaan", engine, &transcript);
            let row = if engine == "claude" {
                json!({"type":"attachment","sessionId":"note-session","timestamp":"9999-01-01T00:00:00.000Z",
                    "attachment":{"type":"hook_additional_context","hookEvent":"PostToolUse","content":[rendered]}})
            } else {
                json!({"type":"response_item","timestamp":"9999-01-01T00:00:00.000Z","payload":{
                    "type":"message","role":"developer","content":[{"type":"input_text","text":rendered}],
                    "internal_chat_message_metadata_passthrough":{"content_item_kinds":["hooks.additional_context"]}}})
            };
            let process = core.ptys.generation("kaan");
            let mut invalid = Vec::new();
            let mut old = row.clone();
            old["timestamp"] = json!("2000-01-01T00:00:00.000Z");
            invalid.push(old);
            let mut missing_time = row.clone();
            missing_time["timestamp"] = Value::Null;
            invalid.push(missing_time);
            let mut untrusted = row.clone();
            if engine == "claude" {
                untrusted["attachment"]["type"] = json!("hook_success");
                let mut other_session = row.clone();
                other_session["sessionId"] = json!("other-session");
                invalid.push(other_session);
            } else {
                untrusted["payload"]["internal_chat_message_metadata_passthrough"] = Value::Null;
                let mut user = row.clone();
                user["payload"]["role"] = json!("user");
                invalid.push(user);
            }
            invalid.push(untrusted);
            let mut wrong_content = row.clone();
            if engine == "claude" {
                wrong_content["attachment"]["content"] =
                    json!([rendered.replace("same-text", "changed-text")]);
            } else {
                wrong_content["payload"]["content"][0]["text"] =
                    json!(rendered.replace("same-text", "changed-text"));
            }
            invalid.push(wrong_content);
            for record in invalid {
                core.worker_notes_observed("kaan", engine, &transcript, &record, process);
                assert_eq!(with(&core.db, |c| pending(c, "kaan")).unwrap().len(), 2);
            }
            core.worker_notes_observed("kaan", engine, &transcript, &row, process.map(|p| p + 1));
            core.worker_notes_observed(
                "kaan",
                engine,
                &root.join("rollout-other-session.jsonl"),
                &row,
                process,
            );
            core.agent_states.hook(
                "kaan",
                Some(crate::agents::Status::Working),
                Some("other-session"),
            );
            core.worker_notes_observed("kaan", engine, &transcript, &row, process);
            assert_eq!(with(&core.db, |c| pending(c, "kaan")).unwrap().len(), 2);
            core.agent_states.hook(
                "kaan",
                Some(crate::agents::Status::Working),
                Some("note-session"),
            );
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript)
                .unwrap();
            writeln!(file, "{row}").unwrap();
            file.flush().unwrap();
            drop(file);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while with(&core.db, |c| pending(c, "kaan")).unwrap().len() == 2
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let notes = with(&core.db, |c| pending(c, "kaan")).unwrap();
            let stamp: i64 = with(&core.db, |c| {
                c.query_row(
                    "SELECT delivered_at FROM worker_notes WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();
            core.worker_notes_observed("kaan", engine, &transcript, &row, process);
            assert_eq!(
                with(&core.db, |c| c
                    .query_row(
                        "SELECT delivered_at FROM worker_notes WHERE id=?1",
                        [id],
                        |r| r.get::<_, i64>(0)
                    )
                    .map_err(|e| e.to_string()))
                .unwrap(),
                stamp
            );
            let retry = core.delegation_on_hook(
                "kaan",
                "PostToolUse",
                &json!({"session_id":"note-session"}),
                false,
            );
            core.chat_watches.drop_watch("kaan");
            let submitted = render(&notes[0]);
            core.worker_notes_prompt_received(
                "kaan",
                &json!({"session_id":"other-session","prompt":submitted}),
            );
            assert_eq!(with(&core.db, |c| pending(c, "kaan")).unwrap().len(), 1);
            core.worker_notes_prompt_received(
                "kaan",
                &json!({"session_id":"note-session","prompt":submitted}),
            );
            assert!(with(&core.db, |c| pending(c, "kaan")).unwrap().is_empty());
            let partial_note = with(&core.db, |c| {
                send(
                    c,
                    &delegate::caller(c, "timur")?,
                    notes[0].delegation_id,
                    "partial-canary",
                )?;
                Ok(render(&pending(c, "kaan")?[0]))
            })
            .unwrap();
            let mut partial_row = row.clone();
            if engine == "claude" {
                partial_row["attachment"]["content"] = json!([partial_note]);
            } else {
                partial_row["payload"]["content"][0]["text"] = json!(partial_note);
            }
            let serialized = partial_row.to_string();
            let split = serialized.len() / 2;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript)
                .unwrap();
            file.write_all(&serialized.as_bytes()[..split]).unwrap();
            file.flush().unwrap();
            core.worker_notes_reconcile("kaan");
            assert_eq!(
                with(&core.db, |c| pending(c, "kaan")).unwrap().len(),
                1,
                "partial record cannot acknowledge a note"
            );
            file.write_all(&serialized.as_bytes()[split..]).unwrap();
            file.write_all(b"\n").unwrap();
            file.flush().unwrap();
            core.worker_notes_reconcile("kaan");
            assert!(
                with(&core.db, |c| pending(c, "kaan")).unwrap().is_empty(),
                "completed receipt is reconciled before the next response"
            );
            drop(file);
            core.ptys.kill("kaan");
            core.shutdown();
            assert_eq!(
                notes.len(),
                1,
                "{engine}: an exact receipt acknowledges only one note"
            );
            assert_ne!(notes[0].id, id);
            let retry = retry.unwrap();
            assert!(!retry.contains(&format!("[Dex lider notu #{id}]")));
            assert!(retry.contains(&format!("[Dex lider notu #{}]", notes[0].id)));
        }
    }

    #[test]
    fn queued_note_arrives_in_start_prompt_once_and_late_note_is_kept() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let d = job(&conn, "kaan");
        let lead = delegate::caller(&conn, "timur").unwrap();
        send(&conn, &lead, d.id, "başlangıç bilgisi").unwrap();
        let req = delegate::start_request(&conn, &d).unwrap();
        assert!(req
            .prompt
            .contains(&format!("Liderden not (#{}): başlangıç bilgisi", d.id)));
        assert_eq!(
            status(&conn, &d).unwrap().pending_notes,
            1,
            "preparing a prompt is not delivery"
        );
        send(&conn, &lead, d.id, "geç gelen bilgi").unwrap();
        let db = Db::new(conn);
        let ev = Events::new(NullSink);
        delegate::started(&db, &ev, d.id).unwrap();
        let body = json!({"prompt":req.prompt});
        let out = crate::questions::on_hook(&db, &ev, "kaan", "UserPromptSubmit", &body, || {
            delegate::on_hook(&db, &ev, "kaan", "UserPromptSubmit", &body)
        })
        .unwrap();
        assert!(
            !out.contains("başlangıç bilgisi"),
            "already present in the submitted prompt"
        );
        assert!(out.contains("geç gelen bilgi"));
        with(&db, |c| {
            accept_text(
                c,
                "kaan",
                body["prompt"].as_str().unwrap(),
                crate::diagnostics::now_ms(),
            )?;
            assert_eq!(
                pending(c, "kaan")?.len(),
                1,
                "prepared response is still pending"
            );
            let receipt: Value = serde_json::from_str(&out).unwrap();
            accept_text(
                c,
                "kaan",
                receipt["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap(),
                crate::diagnostics::now_ms(),
            )
        })
        .unwrap();
        assert!(with(&db, |c| pending(c, "kaan")).unwrap().is_empty());
        assert!(
            crate::questions::on_hook(&db, &ev, "kaan", "PostToolUse", &json!({}), || None)
                .is_none()
        );
    }

    #[test]
    fn stop_notes_stay_pending_until_continuation_in_fifo_order() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let d = job(&conn, "kaan");
        delegate::set_status(&conn, d.id, "running").unwrap();
        let lead = delegate::caller(&conn, "timur").unwrap();
        send(&conn, &lead, d.id, "birinci").unwrap();
        send(&conn, &lead, d.id, "ikinci").unwrap();
        let db = Db::new(conn);
        let ev = Events::new(NullSink);
        let staged = crate::hook_delivery::Pending::default();
        let out = crate::questions::on_hook_pending(
            &db,
            &ev,
            "kaan",
            "Stop",
            &json!({}),
            Some(&staged),
            || delegate::on_hook(&db, &ev, "kaan", "Stop", &json!({})),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let reason = v["reason"].as_str().unwrap();
        assert_eq!(v["decision"], "block");
        assert!(reason.find("birinci").unwrap() < reason.find("ikinci").unwrap());
        with(&db, |c| {
            assert_eq!(
                delegate::get(c, d.id)?.status,
                "running",
                "notes keep the job open"
            );
            assert_eq!(
                pending(c, "kaan")?.len(),
                2,
                "Stop response alone is not acceptance"
            );
            staged.0.lock().unwrap().accept(c, "kaan")?;
            assert!(pending(c, "kaan")?.is_empty());
            Ok(())
        })
        .unwrap();
        assert!(
            crate::questions::on_hook(&db, &ev, "kaan", "PostToolUse", &json!({}), || None)
                .is_none()
        );
    }

    #[test]
    fn note_authorization_rejects_foreign_closed_worker_and_non_worker_target() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let d = job(&conn, "kaan");
        let lead = delegate::caller(&conn, "timur").unwrap();
        let worker = delegate::caller(&conn, "kaan").unwrap();
        let foreign = Caller {
            team_id: "elsewhere".into(),
            ..lead.clone()
        };
        assert!(send(&conn, &foreign, d.id, "x")
            .unwrap_err()
            .contains("takım"));
        assert!(send(&conn, &worker, d.id, "x")
            .unwrap_err()
            .contains("yetki"));
        assert!(send(&conn, &lead, d.id, "  \n").is_err());
        assert!(send(&conn, &lead, -1, "x").is_err());
        for state in ["done", "done_no_report", "failed"] {
            delegate::set_status(&conn, d.id, state).unwrap();
            assert!(send(&conn, &lead, d.id, "x")
                .unwrap_err()
                .contains("açık değil"));
        }
        let leader_job = job(&conn, "timur");
        assert!(send(&conn, &lead, leader_job.id, "x")
            .unwrap_err()
            .contains("worker"));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM worker_notes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn unassigned_role_note_follows_the_claimed_worker_at_start() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let d = job(&conn, "ayla");
        delegate::queue_role(&conn, d.id, "backend", None).unwrap();
        let lead = delegate::caller(&conn, "timur").unwrap();
        send(&conn, &lead, d.id, "rol bilgisi").unwrap();
        assert!(pending(&conn, "ayla").unwrap().is_empty());
        assert!(pending(&conn, "kaan").unwrap().is_empty());
        let assigned = delegate::assign_role_job(&conn, d.id).unwrap();
        assert_eq!(assigned.to_agent, "ayla");
        let prompt = delegate::start_request(&conn, &assigned).unwrap().prompt;
        assert!(prompt.contains(&format!("Liderden not (#{}): rol bilgisi", d.id)));
        let db = Db::new(conn);
        let ev = Events::new(NullSink);
        let body = json!({"prompt":delegate::trigger_line(d.id)});
        let out = crate::questions::on_hook(&db, &ev, "ayla", "UserPromptSubmit", &body, || {
            delegate::on_hook(&db, &ev, "ayla", "UserPromptSubmit", &body)
        })
        .unwrap();
        assert_eq!(out.matches("rol bilgisi").count(), 1);
        with(&db, |c| {
            assert_eq!(
                status(c, &assigned)?.pending_notes,
                1,
                "start context is only prepared"
            );
            let receipt: Value = serde_json::from_str(&out).unwrap();
            accept_text(
                c,
                "ayla",
                receipt["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap(),
                crate::diagnostics::now_ms(),
            )?;
            assert_eq!(status(c, &assigned)?.pending_notes, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn database_restart_preserves_unassigned_pending_note() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "worker-note-restart-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("dex.db");
        let c = crate::db::open(&path).unwrap();
        let d = job(&c, "ayla");
        delegate::queue_role(&c, d.id, "backend", None).unwrap();
        send(
            &c,
            &delegate::caller(&c, "timur").unwrap(),
            d.id,
            "kalıcı bilgi",
        )
        .unwrap();
        drop(c);
        let c = crate::db::open(&path).unwrap();
        delegate::reconcile_on_start(&c, false).unwrap();
        let d = delegate::get(&c, d.id).unwrap();
        assert_eq!(d.status, "queued");
        assert!(d.to_agent.is_empty());
        assert_eq!(status(&c, &d).unwrap().pending_notes, 1);
        let d = delegate::assign_role_job(&c, d.id).unwrap();
        assert!(delegate::worker_prompt(&c, &d)
            .unwrap()
            .contains("kalıcı bilgi"));
    }

    #[test]
    fn service_note_is_only_for_the_requesting_leader() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        conn.execute_batch("INSERT INTO teams(id,name,kind,created_at) VALUES('svc','svc','service',1);
            INSERT INTO agents(id,team_id,name,kind,model,shirt,hair,skin) VALUES('svc-worker','svc','svc-worker','worker','sonnet','','','');").unwrap();
        let d = job(&conn, "kaan");
        conn.execute(
            "UPDATE delegations SET to_agent='svc-worker',service_team_id='svc' WHERE id=?1",
            [d.id],
        )
        .unwrap();
        let lead = delegate::caller(&conn, "timur").unwrap();
        send(&conn, &lead, d.id, "hizmet bilgisi").unwrap();
        let other = Caller {
            id: "other-leader".into(),
            ..lead.clone()
        };
        assert!(send(&conn, &other, d.id, "x").is_err());
        let foreign = Caller {
            team_id: "svc".into(),
            ..lead.clone()
        };
        assert!(send(&conn, &foreign, d.id, "x").is_err());
        delegate::set_status(&conn, d.id, "running").unwrap();
        assert!(pending(&conn, "kaan").unwrap().is_empty());
        assert_eq!(pending(&conn, "svc-worker").unwrap().len(), 1);
    }

    #[test]
    fn permission_hook_does_not_consume_notes_or_change_the_decision() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let d = job(&conn, "kaan");
        delegate::set_status(&conn, d.id, "running").unwrap();
        send(
            &conn,
            &delegate::caller(&conn, "timur").unwrap(),
            d.id,
            "bilgi",
        )
        .unwrap();
        let db = Db::new(conn);
        for event in ["PreToolUse", "PermissionRequest"] {
            let decision =
                json!({"hookSpecificOutput":{"hookEventName":event,"permissionDecision":"deny"}})
                    .to_string();
            assert_eq!(
                crate::questions::on_hook(
                    &db,
                    &Events::new(NullSink),
                    "kaan",
                    event,
                    &json!({}),
                    || Some(decision.clone())
                ),
                Some(decision)
            );
        }
        assert_eq!(with(&db, |c| pending(c, "kaan")).unwrap().len(), 1);
    }
}
