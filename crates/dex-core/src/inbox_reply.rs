//! #1718: the answer of a turn a phone share opened — stored host-only, announced by a
//! content-free FCM ping (push.rs). Panel/terminal turns never create one. Markdown and preview
//! are personal data: never logged.
use crate::db::with;
use crate::inbox::Error;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// Additive, idempotent (IF NOT EXISTS in a versioned migration). Before commit: ROLLBACK.
// After commit: keep both tables; revert only the application (rows expire in 7 days anyway).
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS inbox_replies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    inbox_id INTEGER NOT NULL UNIQUE REFERENCES inbox_receipts(id) ON DELETE CASCADE,
    task TEXT CHECK (task IN ('repo','verify','ask')),
    preview TEXT NOT NULL,
    markdown TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS inbox_replies_team ON inbox_replies(team_id,id);
CREATE TABLE IF NOT EXISTS push_tokens (
    token TEXT PRIMARY KEY,
    platform TEXT NOT NULL CHECK (platform IN ('android')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);";

pub const KEEP: i64 = 50;
pub const KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const PREVIEW_CHARS: usize = 200;
const MARKDOWN_LIMIT: usize = 64 * 1024;
pub const EVENT: &str = "inbox-reply";

/// An accepted share's turn, until its final answer is stored.
struct Mark { receipt: i64, team: String, task: Option<&'static str>, preview: String, transcript: Option<PathBuf>, from: u64 }

#[derive(Default)]
pub(crate) struct Marks(Mutex<HashMap<String, Mark>>);

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: i64,
    pub task: Option<String>,
    pub preview: String,
    pub created_at: i64,
    pub agent_id: String,
    pub agent_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    pub markdown: String,
}

fn error(code: &'static str, message: &str) -> Error { Error { code, message: message.into() } }

/// The share inside inbox.rs's prompt (`Patron paylaştı:\n<text>[\n[Görsel: <path>]]`):
/// its task (first line `Görev: …`) and a one-line preview without the task line.
fn share_meta(prompt: &str) -> (Option<&'static str>, String) {
    let body = prompt.strip_prefix("Patron paylaştı:\n").unwrap_or(prompt);
    let (text, image) = match body.rsplit_once("\n[Görsel: ") {
        Some((text, path)) if path.ends_with(']') && !path.contains('\n') => (text, true),
        _ => (body, false),
    };
    let text = text.trim_start();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let task = match first.trim() {
        "Görev: Repo incele" => Some("repo"),
        "Görev: Kaynak doğrula" => Some("verify"),
        "Görev: Soru" => Some("ask"),
        _ => None,
    };
    let flat = if task.is_some() { rest } else { text }.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = flat.chars().take(PREVIEW_CHARS).collect();
    if flat.chars().count() > PREVIEW_CHARS { preview.push('…'); }
    if image { preview = if preview.is_empty() { "[görsel]".into() } else { format!("[görsel] {preview}") }; }
    (task, preview)
}

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MARKDOWN_LIMIT { return text.into(); }
    let mut cut = MARKDOWN_LIMIT - '…'.len_utf8();
    while !text.is_char_boundary(cut) { cut -= 1; }
    format!("{}…", &text[..cut])
}

/// Expired or beyond the newest [`KEEP`]: on every insert and at start.
pub(crate) fn prune(c: &rusqlite::Connection, now: i64) -> Result<(), String> {
    c.execute("DELETE FROM inbox_replies WHERE created_at < ?1
        OR id NOT IN (SELECT id FROM inbox_replies ORDER BY id DESC LIMIT ?2)", params![now - KEEP_MS, KEEP])
        .map(|_| ()).map_err(|e| e.to_string())
}

const SELECT: &str = "SELECT r.id,r.task,r.preview,r.created_at,r.agent_id,COALESCE(a.name,r.agent_id),r.markdown
    FROM inbox_replies r LEFT JOIN agents a ON a.id=r.agent_id";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Detail> {
    Ok(Detail {
        summary: Summary { id: r.get(0)?, task: r.get(1)?, preview: r.get(2)?, created_at: r.get(3)?, agent_id: r.get(4)?, agent_name: r.get(5)? },
        markdown: r.get(6)?,
    })
}

fn now() -> i64 { crate::diagnostics::now_ms() as i64 }

impl crate::Core {
    pub(crate) fn inbox_reply_mark(&self, agent: &str, receipt: i64, team: &str, prompt: &str) {
        let (task, preview) = share_meta(prompt);
        let transcript = self.agent_states.get(agent).transcript.map(PathBuf::from);
        let from = transcript.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        self.inbox_marks.0.lock().unwrap().insert(agent.into(), Mark { receipt, team: team.into(), task, preview, transcript, from });
    }

    pub(crate) fn inbox_reply_forget(&self, agent: &str) {
        self.inbox_marks.0.lock().unwrap().remove(agent);
    }

    /// A Stop that ended the turn (not blocked): Claude's `last_assistant_message` first, the
    /// transcript window second. No answer yet keeps the mark for the reconciled turn end.
    pub(crate) fn inbox_reply_on_stop(&self, agent: &str, last: Option<&str>) {
        let mut marks = self.inbox_marks.0.lock().unwrap();
        let Some(mark) = marks.get(agent) else { return };
        let answer = last.map(str::trim).filter(|t| !t.is_empty()).map(String::from)
            .or_else(|| mark.transcript.as_deref().and_then(|p| crate::chat::turn_reply(p, mark.from)));
        let Some(answer) = answer else { return };
        let mark = marks.remove(agent).unwrap();
        drop(marks);
        self.inbox_reply_store(agent, mark, &answer);
    }

    /// The reconciled turn end (transcript `turn_duration`, or a Stop the hook missed).
    pub(crate) fn inbox_reply_turn_ended(&self, agent: &str) {
        let Some(mark) = self.inbox_marks.0.lock().unwrap().remove(agent) else { return };
        if let Some(answer) = mark.transcript.as_deref().and_then(|p| crate::chat::turn_reply(p, mark.from)) {
            self.inbox_reply_store(agent, mark, &answer);
        }
    }

    fn inbox_reply_store(&self, agent: &str, mark: Mark, answer: &str) {
        let stored = with(&self.db, |c| {
            let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
            let n = tx.execute("INSERT OR IGNORE INTO inbox_replies(team_id,agent_id,inbox_id,task,preview,markdown,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![mark.team, agent, mark.receipt, mark.task, mark.preview, clip(answer), now()]).map_err(|e| e.to_string())?;
            let id = (n > 0).then(|| tx.last_insert_rowid());
            prune(&tx, now())?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(id)
        });
        match stored {
            Ok(Some(id)) => {
                let _ = self.events.emit(EVENT, &serde_json::json!({ "id": id, "teamId": mark.team }));
                self.push_notify(id);
            }
            Ok(None) => {}
            Err(_) => log::warn!("inbox: cevap kaydedilemedi"),
        }
    }

    pub(crate) fn inbox_reply_resume(&self) -> Result<(), String> {
        with(&self.db, |c| prune(c, now()))
    }

    /// Host-only (dexd/desktop); deliberately absent from the agent MCP.
    pub fn inbox_replies(&self, team_id: &str, limit: Option<u32>) -> Result<Vec<Summary>, Error> {
        if team_id.is_empty() || team_id.len() > 128 { return Err(error("inbox_team", "teamId gerekli")); }
        let limit = limit.unwrap_or(KEEP as u32).clamp(1, KEEP as u32);
        with(&self.db, |c| {
            let mut stmt = c.prepare(&format!("{SELECT} WHERE r.team_id=?1 AND r.created_at>=?2 ORDER BY r.id DESC LIMIT ?3")).map_err(|e| e.to_string())?;
            let rows = stmt.query_map(params![team_id, now() - KEEP_MS, limit], row).map_err(|e| e.to_string())?;
            rows.map(|r| r.map(|d| d.summary)).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        }).map_err(|_| error("inbox_unavailable", "cevaplar okunamadı"))
    }

    pub fn inbox_reply_get(&self, id: i64) -> Result<Detail, Error> {
        with(&self.db, |c| c.query_row(&format!("{SELECT} WHERE r.id=?1 AND r.created_at>=?2"), params![id, now() - KEEP_MS], row).optional().map_err(|e| e.to_string()))
            .map_err(|_| error("inbox_unavailable", "cevap okunamadı"))?
            .ok_or_else(|| error("inbox_reply_not_found", "cevap yok ya da süresi doldu"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn share_meta_reads_task_line_preview_and_image() {
        assert_eq!(share_meta("Patron paylaştı:\nGörev: Repo incele\nhttps://example.com/r\n\nbak"), (Some("repo"), "https://example.com/r bak".into()));
        assert_eq!(share_meta("Patron paylaştı:\nGörev: Kaynak doğrula\nx").0, Some("verify"));
        assert_eq!(share_meta("Patron paylaştı:\n Görev: Soru \nne?").0, Some("ask"));
        assert_eq!(share_meta("Patron paylaştı:\nGörev: Başka\nx"), (None, "Görev: Başka x".into()));
        assert_eq!(share_meta("Patron paylaştı:\n\n[Görsel: C:\\t\\.dex\\inbox\\1.png]"), (None, "[görsel]".into()));
        assert_eq!(share_meta("Patron paylaştı:\nbu ne\n[Görsel: /t/.dex/inbox/1.png]"), (None, "[görsel] bu ne".into()));
        let (_, long) = share_meta(&format!("Patron paylaştı:\n{}", "ü".repeat(300)));
        assert_eq!(long.chars().count(), PREVIEW_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    fn core(tag: &str) -> std::sync::Arc<crate::Core> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-reply-{tag}-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        core
    }

    /// A share accepted by its exact UPS: the receipt row the real `inbox_submit` would leave.
    fn accept(core: &crate::Core, receipt: i64, text: &str, transcript: &std::path::Path) {
        let prompt = format!("Patron paylaştı:\n{text}");
        with(&core.db, |c| {
            c.execute("INSERT INTO inbox_receipts(id,team_id,agent_id,client_id,reply) VALUES(?1,'default','ayla',NULL,'{}')", [receipt]).map_err(|e| e.to_string())?;
            c.execute("INSERT INTO inbox_messages(receipt_id,agent_id,prompt,pasted) VALUES(?1,'ayla',?2,1)", params![receipt, prompt]).map(|_| ()).map_err(|e| e.to_string())
        }).unwrap();
        core.agent_states.set_transcript("ayla", transcript.to_str().unwrap());
        core.inbox_prompt_received("ayla", &prompt);
    }

    fn rows(core: &crate::Core) -> i64 {
        with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM inbox_replies", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap()
    }

    fn line(v: serde_json::Value) -> String { format!("{v}\n") }

    #[test]
    fn inbox_turn_answer_is_stored_once_and_panel_turns_never() {
        let core = core("store");
        let dir = core.paths.data_dir.join("t");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(&path, line(json!({"type":"assistant","uuid":"old","message":{"content":[{"type":"text","text":"önceki tur"}]}}))).unwrap();
        accept(&core, 1, "Görev: Soru\nneden?", &path);
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        f.write_all(line(json!({"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"bakıyorum"},{"type":"tool_use","id":"t1","name":"WebFetch","input":{"url":"x"}}]}})).as_bytes()).unwrap();
        f.write_all(line(json!({"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}})).as_bytes()).unwrap();
        f.write_all(line(json!({"type":"assistant","uuid":"a2","message":{"content":[{"type":"thinking","thinking":"gizli"},{"type":"text","text":"## Cevap\n**evet**"}]}})).as_bytes()).unwrap();
        drop(f);
        // A blocked Stop does not reach here; an empty last message falls back to the transcript.
        core.inbox_reply_on_stop("ayla", Some("  "));
        core.inbox_reply_turn_ended("ayla");
        core.inbox_reply_on_stop("ayla", Some("ikinci kez"));
        let list = core.inbox_replies("default", None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].task.as_deref(), list[0].preview.as_str(), list[0].agent_id.as_str()), (Some("ask"), "neden?", "ayla"));
        assert!(!list[0].agent_name.is_empty());
        let detail = core.inbox_reply_get(list[0].id).unwrap();
        assert_eq!(detail.markdown, "## Cevap\n**evet**");
        assert!(serde_json::to_value(&list[0]).unwrap().get("markdown").is_none(), "list carries no markdown");
        assert_eq!(serde_json::to_value(&detail).unwrap()["createdAt"], list[0].created_at);
        // Panel prompt (no inbox row matches) clears; its Stop stores nothing.
        accept(&core, 2, "ikinci", &path);
        core.inbox_prompt_received("ayla", "panelden yazdım");
        core.inbox_reply_on_stop("ayla", Some("panel cevabı"));
        // A new paste's Enter forgets too.
        accept(&core, 3, "üçüncü", &path);
        core.inbox_reply_forget("ayla");
        core.inbox_reply_turn_ended("ayla");
        assert_eq!(rows(&core), 1);
        // Claude's own last message wins over the transcript.
        accept(&core, 4, "dördüncü", &path);
        core.inbox_reply_on_stop("ayla", Some("son mesaj"));
        assert_eq!(core.inbox_reply_get(core.inbox_replies("default", Some(1)).unwrap()[0].id).unwrap().markdown, "son mesaj");
        assert_eq!(core.inbox_reply_get(999).unwrap_err().code, "inbox_reply_not_found");
        assert_eq!(core.inbox_replies("", None).unwrap_err().code, "inbox_team");
        assert_eq!(crate::mcp::call_for_test(&core, "ayla", "inbox_replies")["isError"], true);
        core.shutdown();
    }

    #[test]
    fn hook_path_stores_share_answer_and_skips_panel_turn() {
        #[derive(Default)]
        struct Rec(Mutex<Vec<(String, serde_json::Value)>>);
        impl crate::events::EventSink for std::sync::Arc<Rec> {
            fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), payload));
                Ok(())
            }
        }
        let rec = std::sync::Arc::new(Rec::default());
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-reply-hook-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(rec.clone())).unwrap();
        with(&core.db, |c| c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        let prompt = "Patron paylaştı:\nGörev: Repo incele\nhttps://example.com/repo";
        with(&core.db, |c| {
            c.execute("INSERT INTO inbox_receipts(id,team_id,agent_id,reply) VALUES(5,'default','ayla','{}')", []).map_err(|e| e.to_string())?;
            c.execute("INSERT INTO inbox_messages(receipt_id,agent_id,prompt,pasted) VALUES(5,'ayla',?1,1)", [prompt]).map(|_| ()).map_err(|e| e.to_string())
        }).unwrap();
        core.delegation_on_hook("ayla", "UserPromptSubmit", &json!({"prompt": prompt}), false);
        core.delegation_on_hook("ayla", "Stop", &json!({"last_assistant_message": "# Özet\n- iyi"}), false);
        core.delegation_on_hook("ayla", "UserPromptSubmit", &json!({"prompt": "panelden"}), false);
        core.delegation_on_hook("ayla", "Stop", &json!({"last_assistant_message": "panel cevabı"}), false);
        let list = core.inbox_replies("default", None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].task.as_deref(), list[0].preview.as_str()), (Some("repo"), "https://example.com/repo"));
        assert_eq!(core.inbox_reply_get(list[0].id).unwrap().markdown, "# Özet\n- iyi");
        let events: Vec<_> = rec.0.lock().unwrap().iter().filter(|(e, _)| e == EVENT).map(|(_, p)| p.clone()).collect();
        assert_eq!(events, [json!({"id": list[0].id, "teamId": "default"})], "the event carries no content");
        core.shutdown();
    }

    #[test]
    fn interrupted_or_empty_turn_creates_no_reply() {
        let core = core("empty");
        let path = core.paths.data_dir.join("i.jsonl");
        std::fs::write(&path, "").unwrap();
        accept(&core, 1, "kesilecek", &path);
        std::fs::write(&path, [
            line(json!({"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"yarım"}]}})),
            line(json!({"type":"user","uuid":"u1","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}})),
        ].concat()).unwrap();
        core.inbox_reply_on_stop("ayla", None);
        core.inbox_reply_turn_ended("ayla");
        accept(&core, 2, "boş", &path);
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap()
            .write_all(line(json!({"type":"assistant","uuid":"a2","message":{"content":[{"type":"tool_use","id":"t","name":"WebSearch","input":{"query":"q"}}]}})).as_bytes()).unwrap();
        core.inbox_reply_on_stop("ayla", None);
        core.inbox_reply_turn_ended("ayla");
        assert_eq!(rows(&core), 0);
        core.shutdown();
    }

    #[test]
    fn migration_runs_twice_on_a_clean_db_and_keeps_rows() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-reply-mig-{}-{}", std::process::id(), crate::agents::next_seq()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dex.db");
        let count = |c: &rusqlite::Connection, sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        let c = crate::db::open(&path).unwrap();
        c.execute_batch("INSERT INTO inbox_receipts(id,team_id,agent_id,reply) VALUES(1,'default','ayla','{}');
            INSERT INTO inbox_replies(team_id,agent_id,inbox_id,preview,markdown,created_at) VALUES('default','ayla',1,'p','m',1);
            INSERT INTO push_tokens(token,platform,created_at,updated_at) VALUES('t','android',1,1);").unwrap();
        let version = count(&c, "PRAGMA user_version");
        drop(c);
        let c = crate::db::open(&path).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        assert_eq!(count(&c, "PRAGMA user_version"), version);
        assert_eq!((count(&c, "SELECT COUNT(*) FROM inbox_replies"), count(&c, "SELECT COUNT(*) FROM push_tokens"), count(&c, "SELECT COUNT(*) FROM inbox_receipts")), (1, 1, 1));
        assert!(c.execute("INSERT INTO push_tokens(token,platform,created_at,updated_at) VALUES('u','ios',1,1)", []).is_err(), "android only");
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retention_keeps_newest_fifty_within_seven_days() {
        let core = core("prune");
        let now = now();
        with(&core.db, |c| {
            for id in 1..=60 {
                c.execute("INSERT INTO inbox_receipts(id,team_id,agent_id,reply) VALUES(?1,'default','ayla','{}')", [id]).map_err(|e| e.to_string())?;
                let at = if id <= 3 { now - KEEP_MS - 1 } else { now };
                c.execute("INSERT INTO inbox_replies(team_id,agent_id,inbox_id,preview,markdown,created_at) VALUES('default','ayla',?1,'p','m',?2)", params![id, at]).map_err(|e| e.to_string())?;
            }
            Ok(())
        }).unwrap();
        assert_eq!(core.inbox_replies("default", Some(500)).unwrap().len(), 50, "limit is capped at 50");
        core.inbox_reply_resume().unwrap();
        assert_eq!(rows(&core), 50);
        let oldest: i64 = with(&core.db, |c| c.query_row("SELECT MIN(id) FROM inbox_replies", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap();
        assert_eq!(oldest, 11);
        with(&core.db, |c| c.execute("UPDATE inbox_replies SET created_at=?1 WHERE id=11", [now - KEEP_MS - 1]).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        assert_eq!(core.inbox_reply_get(11).unwrap_err().code, "inbox_reply_not_found", "expired rows are not served before the next prune");
        core.shutdown();
    }
}
