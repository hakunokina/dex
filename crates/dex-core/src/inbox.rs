//! Host-only personal assistant inbox. Shared content never enters the task board.
use serde::{Deserialize, Serialize};
use base64::Engine;
use crate::db::with;
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

// Additive, idempotent. Before commit: ROLLBACK. After commit: retain both tables and
// pending shares, revert only the application; never DROP or lower user_version.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS inbox_receipts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    client_id TEXT,
    reply TEXT NOT NULL,
    UNIQUE(team_id,client_id)
);
CREATE TABLE IF NOT EXISTS inbox_messages (
    receipt_id INTEGER PRIMARY KEY REFERENCES inbox_receipts(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    prompt TEXT NOT NULL,
    pasted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS inbox_messages_fifo ON inbox_messages(agent_id,receipt_id);";

pub const IMAGE_LIMIT: usize = 700 * 1024;
pub const TEXT_LIMIT: usize = 8 * 1024;
/// Missing exact UPS is ambiguous, never a reason to replay personal content (#712).
const UPS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
type DecodedImage = (Vec<u8>, &'static str);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Submit {
    pub team_id: String,
    pub client_id: Option<String>,
    pub text: Option<String>,
    pub image: Option<Image>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image { pub mime: String, pub base64: String }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Reply { pub ok: bool, pub agent_id: String, pub queued: bool }

#[derive(Debug, Serialize)]
pub struct Error { pub code: &'static str, pub message: String }

fn error(code: &'static str, message: &str) -> Error { Error { code, message: message.into() } }

fn validate(input: &Submit) -> Result<(String, Option<DecodedImage>), Error> {
    if input.team_id.is_empty() || input.team_id.len() > 128 { return Err(error("inbox_team", "teamId gerekli")); }
    if input.client_id.as_ref().is_some_and(|s| s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))) {
        return Err(error("inbox_client_id", "clientId 1–128 ASCII harf/rakam veya -_.: olmalı"));
    }
    let raw = input.text.as_deref().unwrap_or("");
    if raw.len() > TEXT_LIMIT { return Err(error("inbox_text_too_large", "metin çok büyük (8192 bayt)")); }
    let text: String = raw.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    let image = input.image.as_ref().map(|image| {
        let ext = match image.mime.as_str() {
            "image/png" => "png", "image/jpeg" => "jpg", "image/webp" => "webp",
            _ => return Err(error("inbox_mime", "yalnız image/png, image/jpeg, image/webp")),
        };
        if image.base64.len() > IMAGE_LIMIT.div_ceil(3) * 4 { return Err(error("inbox_image_too_large", "görsel çok büyük")); }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&image.base64).map_err(|_| error("inbox_base64", "geçersiz base64"))?;
        if bytes.len() > IMAGE_LIMIT { return Err(error("inbox_image_too_large", "görsel çok büyük")); }
        let matches = match ext {
            "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "jpg" => bytes.starts_with(b"\xff\xd8\xff"),
            _ => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        };
        if !matches { return Err(error("inbox_image", "görsel içeriği MIME ile eşleşmiyor")); }
        Ok((bytes, ext))
    }).transpose()?;
    if text.trim().is_empty() && image.is_none() { return Err(error("inbox_empty", "metin veya görsel gerekli")); }
    Ok((text, image))
}

impl crate::Core {
    /// Only trusted hosts call this API; it is deliberately absent from the agent MCP.
    pub fn inbox_submit(&self, input: Submit) -> Result<Reply, Error> {
        let (text, image) = validate(&input)?;
        let _gate = self.inbox_gate.lock().map_err(|_| error("inbox_unavailable", "gelen kutusu kilidi kullanılamıyor"))?;
        let stored = with(&self.db, |c| {
            let previous: Option<String> = c.query_row("SELECT reply FROM inbox_receipts WHERE team_id=?1 AND client_id=?2", params![input.team_id,input.client_id], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
            if let Some(reply) = previous {
                return Ok((None, serde_json::from_str::<Reply>(&reply).map_err(|e| e.to_string())?));
            }
            let agent: Option<String> = c.query_row("SELECT id FROM agents WHERE team_id=?1 AND role_slug='assistant' ORDER BY sort,id LIMIT 1", [&input.team_id], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
            let agent = agent.ok_or("inbox_no_assistant")?;
            let team = crate::teams::get_team(c, &self.paths.data_dir, &input.team_id)?;
            let mut prompt = format!("Patron paylaştı:\n{text}");
            let file = if let Some((bytes, ext)) = &image {
                let root = team.cwd.or(team.workdir).ok_or("inbox_root")?;
                let path = store_image(Path::new(&root), bytes, ext)?;
                prompt.push_str(&format!("\n[Görsel: {}]", path.display()));
                Some(path)
            } else { None };
            let reply = Reply { ok: true, agent_id: agent.clone(), queued: true };
            let inserted = (|| {
                let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
                tx.execute("INSERT INTO inbox_receipts(team_id,agent_id,client_id,reply) VALUES(?1,?2,?3,?4)", params![input.team_id,agent,input.client_id,serde_json::to_string(&reply).unwrap()]).map_err(|e| e.to_string())?;
                let id = tx.last_insert_rowid();
                tx.execute("INSERT INTO inbox_messages(receipt_id,agent_id,prompt) VALUES(?1,?2,?3)", params![id,agent,prompt]).map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                Ok((Some(id), reply))
            })();
            if inserted.is_err() { if let Some(file) = file { let _ = std::fs::remove_file(file); } }
            inserted
        }).map_err(|e| match e.as_str() {
            "inbox_no_assistant" => error("inbox_no_assistant", "bu takımda assistant üyesi yok"),
            "inbox_root" => error("inbox_root", "takımın çalışma klasörü yok"),
            _ => error("inbox_unavailable", "gelen kutusu kaydedilemedi"),
        })?;
        let (Some(id), mut reply) = stored else { return Ok(stored.1) };
        if self.inbox_deliver_locked(&reply.agent_id).ok().flatten() == Some(id) {
            reply.queued = false;
            with(&self.db, |c| c.execute("UPDATE inbox_receipts SET reply=?1 WHERE id=?2", params![serde_json::to_string(&reply).unwrap(),id]).map(|_| ()).map_err(|e| e.to_string()))
                .map_err(|_| error("inbox_unavailable", "gelen kutusu yanıtı kaydedilemedi"))?;
        }
        Ok(reply)
    }

    fn inbox_deliver_locked(&self, agent: &str) -> Result<Option<i64>, String> {
        let gate = self.delivery_gate(agent);
        let _delivery = gate.lock().map_err(|_| "teslim kilidi kullanılamıyor")?;
        let next = with(&self.db, |c| {
            let a = crate::teams::get_agent(c, agent)?;
            if a.role_slug.as_deref() != Some("assistant") { return Ok(None); }
            c.query_row("SELECT receipt_id,prompt,pasted FROM inbox_messages WHERE agent_id=?1 ORDER BY receipt_id LIMIT 1", [agent], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?))).optional().map_err(|e| e.to_string())
        })?;
        let Some((id, prompt, pasted)) = next else { return Ok(None) };
        if pasted { return Ok(None); }
        let Some(turn) = self.admit(agent) else { return Ok(None) };
        if !self.ptys.is_running(agent) {
            self.agent_spawn(agent, crate::agents::StartOptions::default())?;
            return Ok(None); // SessionStart/settled screen opens the Idle gate, never initial argv.
        }
        if self.agent_states.get(agent).status != crate::agents::Status::Idle || self.agent_states.typing(agent) || self.agent_states.pre_session(agent) {
            return Ok(None);
        }
        with(&self.db, |c| c.execute("UPDATE inbox_messages SET pasted=1 WHERE receipt_id=?1", [id]).map(|_| ()).map_err(|e| e.to_string()))?;
        if let Err(e) = self.paste_admitted(agent, &prompt, &turn) {
            with(&self.db, |c| c.execute("UPDATE inbox_messages SET pasted=0 WHERE receipt_id=?1", [id]).map(|_| ()).map_err(|e| e.to_string()))?;
            return Err(e);
        }
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || loop {
            std::thread::sleep(UPS_TIMEOUT);
            let Some(core) = weak.upgrade() else { return };
            // Receipt identity fences late timers: an accepted row or the next FIFO head is
            // never removed. On DB failure retry only cleanup, never the ambiguous paste.
            match with(&core.db, |c| c.execute("DELETE FROM inbox_messages WHERE receipt_id=?1 AND agent_id=?2 AND pasted=1", params![id,agent]).map_err(|e| e.to_string())) {
                Ok(removed) => {
                    if removed > 0 {
                        log::warn!("inbox: UPS süre aşımı; teslim belirsiz, içerik silindi");
                        core.inbox_wake(&agent);
                    }
                    return;
                }
                Err(_) => log::warn!("inbox: belirsiz teslim temizliği başarısız; temizlik yeniden denenecek"),
            }
        });
        Ok(Some(id))
    }

    pub(crate) fn inbox_prompt_received(&self, agent: &str, prompt: &str) {
        // Only the exact UPS is receipt evidence. Mismatches wait for the ambiguity timeout.
        let _ = with(&self.db, |c| c.execute("DELETE FROM inbox_messages WHERE agent_id=?1 AND pasted=1 AND prompt=?2", params![agent,prompt]).map(|_| ()).map_err(|e| e.to_string()));
    }

    pub(crate) fn inbox_wake(&self, agent: &str) {
        let pending = with(&self.db, |c| c.query_row("SELECT EXISTS(SELECT 1 FROM inbox_messages WHERE agent_id=?1)", [agent], |r| r.get::<_,bool>(0)).map_err(|e| e.to_string())).unwrap_or(false);
        if !pending { return; }
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(700));
            let Some(core) = weak.upgrade() else { return };
            let Ok(_gate) = core.inbox_gate.lock() else { return };
            let _ = core.inbox_deliver_locked(&agent);
        });
    }

    pub(crate) fn inbox_resume(&self) -> Result<(), String> {
        // Leader decision #712: an ambiguous pre-crash paste is never replayed. Remove its
        // personal text; receipts retain only idempotency metadata and the original reply.
        with(&self.db, |c| c.execute("DELETE FROM inbox_messages WHERE pasted=1", []).map(|_| ()).map_err(|e| e.to_string()))?;
        self.inbox_wake_pending()
    }

    pub(crate) fn inbox_wake_pending(&self) -> Result<(), String> {
        let agents = with(&self.db, |c| {
            c.prepare("SELECT DISTINCT agent_id FROM inbox_messages WHERE pasted=0").map_err(|e| e.to_string())?
                .query_map([], |r| r.get::<_,String>(0)).map_err(|e| e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())
        })?;
        for agent in agents { self.inbox_wake(&agent); }
        Ok(())
    }
}

fn store_image(root: &Path, bytes: &[u8], ext: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let dex = root.join(".dex");
    let inbox = dex.join("inbox");
    for dir in [&dex, &inbox] {
        if dir.exists() && dir.canonicalize().map_err(|e| e.to_string())? != *dir { return Err("inbox yolu bağlantı olamaz".into()); }
        crate::private_fs::create_dir(dir).map_err(|e| e.to_string())?;
    }
    let mut random = [0u8;16];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let random = random.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let path = inbox.join(format!("{}-{random}.{ext}", crate::diagnostics::now_ms()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).map_err(|e| e.to_string())?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if result.is_err() { let _ = std::fs::remove_file(&path); }
    result.map_err(|e| e.to_string())?;
    let ordinary = crate::teams::normalize_dir(&inbox.to_string_lossy())?;
    Ok(PathBuf::from(ordinary).join(path.file_name().ok_or("inbox dosya adı yok")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    #[test]
    fn assistant_reads_real_store_image_with_unprefixed_team_context() {
        let f = crate::projects::tests::Fixture::new();
        let root = crate::teams::normalize_dir(f.0.to_str().unwrap()).unwrap();
        let core = crate::Core::start(crate::Paths { data_dir: f.0.with_extension("data"), config_dir: f.0.with_extension("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd=?1 WHERE id='default'", [&root]).map_err(|e| e.to_string())?;
            c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        let image = store_image(Path::new(&root), b"\x89PNG\r\n\x1a\n", "png").unwrap();
        let ctx = with(&core.db, |c| Ok(core.permit_ctx(c, "ayla", &serde_json::json!({}))?.unwrap().0)).unwrap();
        let verdict = crate::permit::decide("Read", &serde_json::json!({"file_path": image}), &ctx);
        core.shutdown();
        drop(core);
        assert_eq!(verdict, crate::permit::Verdict::Allow);
        assert!(!image.to_string_lossy().starts_with(r"\\?\"), "prompt image path must be ordinary absolute path");
        assert_eq!(crate::permit::decide("Read", &serde_json::json!({"file_path": image.canonicalize().unwrap()}), &ctx), crate::permit::Verdict::Allow);
    }

    #[test]
    fn assistant_permit_context_reads_only_inbox_shared_and_own_private_memory() {
        let f = crate::projects::tests::Fixture::new();
        let root = crate::teams::normalize_dir(f.0.to_str().unwrap()).unwrap();
        let memory = f.0.join(".dex/memory");
        let core = crate::Core::start(crate::Paths { data_dir: f.0.with_extension("data"), config_dir: f.0.with_extension("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        let own = with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd=?1,team_memory_root=?2,agent_memory_root=?2 WHERE id='default'", params![root,memory.to_string_lossy()]).map_err(|e| e.to_string())?;
            c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map_err(|e| e.to_string())?;
            crate::slug::memory_dir(c, &memory, "ayla")
        }).unwrap();
        let ctx = with(&core.db, |c| Ok(core.permit_ctx(c, "ayla", &serde_json::json!({}))?.unwrap().0)).unwrap();
        let allowed = [f.0.join(".dex/inbox/image.png"), memory.join("shared/note.md"), own.join("private.md")];
        let denied = [f.0.join("crates/core.rs"), f.0.join(".dex/results/1702.md"), memory.join("agents/kaan/private.md"), f.0.join(".dex/inbox-other/private.md"), f.0.join(".dex/inbox/.env")];
        for path in allowed.iter().chain(&denied) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fixture").unwrap();
        }
        core.shutdown();
        drop(core);
        for tool in ["Read", "Glob", "Grep", "LS"] {
            for path in &allowed {
                assert_eq!(crate::permit::decide(tool, &serde_json::json!({"file_path":path,"path":path}), &ctx), crate::permit::Verdict::Allow, "{tool}: {path:?}");
            }
            for path in &denied {
                assert!(matches!(crate::permit::decide(tool, &serde_json::json!({"file_path":path,"path":path}), &ctx), crate::permit::Verdict::Deny(_)), "{tool}: {path:?}");
            }
            assert!(matches!(crate::permit::decide(tool, &serde_json::json!({}), &ctx), crate::permit::Verdict::Deny(_)), "implicit repo cwd: {tool}");
        }
    }

    #[test]
    fn missing_ups_expires_without_replay_and_wakes_next_fifo_share() {
        use crate::agents::Status;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-timeout-{}-{}", std::process::id(), crate::diagnostics::now_ms()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        crate::agents::CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.agent_spawn("ayla", Default::default()).unwrap();
        crate::agents::CAPTURE.with(|c| c.borrow_mut().take());
        core.agent_states.hook("ayla", Some(Status::Idle), Some("inbox-timeout"));
        let mut first = input(); first.text = Some("lost".into()); first.client_id = Some("lost-share".into());
        assert!(!core.inbox_submit(first).unwrap().queued);
        let mut next = input(); next.text = Some("next".into());
        assert!(core.inbox_submit(next).unwrap().queued);
        core.inbox_prompt_received("ayla", "mismatched UPS");
        core.agent_states.hook("ayla", Some(Status::Idle), None);
        let rows = || with(&core.db, |c| {
            let mut stmt = c.prepare("SELECT prompt,pasted FROM inbox_messages ORDER BY receipt_id").map_err(|e| e.to_string())?;
            let mapped = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,bool>(1)?))).map_err(|e| e.to_string())?;
            mapped.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())
        }).unwrap();
        let expected = vec![("Patron paylaştı:\nnext".to_string(), true)];
        let end = std::time::Instant::now() + UPS_TIMEOUT + std::time::Duration::from_secs(3);
        while rows() != expected && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let actual = rows();
        let mut duplicate = input(); duplicate.client_id = Some("lost-share".into());
        assert!(!core.inbox_submit(duplicate).unwrap().queued, "receipt remains idempotent after ambiguity");
        let after_duplicate = rows();
        core.inbox_prompt_received("ayla", "Patron paylaştı:\nnext");
        core.ptys.kill("ayla");
        core.shutdown();
        assert_eq!(actual, expected, "unaccepted personal text must expire and the next share must paste");
        assert_eq!(after_duplicate, expected, "ambiguous share must never be retried");
    }

    #[test]
    fn inbox_and_chat_serialize_before_enter_and_keep_fifo() {
        use crate::agents::Status;
        use std::{io::Write, sync::{Arc, Mutex, mpsc}, time::Duration};
        struct Stop(Arc<crate::Core>);
        impl Drop for Stop {
            fn drop(&mut self) { self.0.ptys.kill("ayla"); self.0.shutdown(); }
        }
        struct Input {
            _original: Box<dyn Write + Send>,
            bytes: Arc<Mutex<Vec<u8>>>,
            written: mpsc::Sender<()>,
        }
        impl Write for Input {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                let _ = self.written.send(());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-chat-{}-{}", std::process::id(), crate::diagnostics::now_ms()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        let _stop = Stop(core.clone());
        with(&core.db, |c| c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        crate::agents::CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.agent_spawn("ayla", Default::default()).unwrap();
        crate::agents::CAPTURE.with(|c| c.borrow_mut().take());
        core.agent_states.hook("ayla", Some(Status::Idle), Some("inbox-chat"));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (written_tx, written_rx) = mpsc::channel();
        core.ptys.input_writer_for_test("ayla", |original| Box::new(Input { _original: original, bytes: bytes.clone(), written: written_tx }));
        let (paused_tx, paused_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *core.paste_pause.lock().unwrap() = Some(Box::new(move || {
            paused_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }));
        let first = core.clone();
        let inbox = std::thread::spawn(move || {
            let mut share = input(); share.text = Some("first".into());
            first.inbox_submit(share).unwrap()
        });
        paused_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        written_rx.recv_timeout(Duration::from_secs(1)).unwrap(); // First paste, before Enter.
        let second = core.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let panel = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            second.chat_send("ayla", "panel").unwrap()
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let interleaved = written_rx.recv_timeout(Duration::from_millis(300)).is_ok();
        release_tx.send(()).unwrap();
        let first = inbox.join().unwrap();
        let panel = panel.join().unwrap();
        assert!(!interleaved, "panel text reached PTY before inbox Enter");
        assert!(!first.queued);
        assert_eq!(panel, "queued", "readiness must be rechecked after the first Enter");
        for text in ["second", "third"] {
            let mut share = input(); share.text = Some(text.into());
            assert!(core.inbox_submit(share).unwrap().queued);
        }
        core.inbox_prompt_received("ayla", "Patron paylaştı:\nfirst");
        core.agent_states.hook("ayla", Some(Status::Idle), None);
        core.chat_on_stop("ayla");
        let end = std::time::Instant::now() + Duration::from_secs(4);
        while bytes.lock().unwrap().iter().filter(|b| **b == b'\r').count() < 2 && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        for text in ["second", "third"] {
            core.agent_states.hook("ayla", Some(Status::Idle), None);
            assert!(core.inbox_deliver_locked("ayla").unwrap().is_some());
            core.inbox_prompt_received("ayla", &format!("Patron paylaştı:\n{text}"));
        }
        let actual = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let expected = ["Patron paylaştı:\nfirst", "panel", "Patron paylaştı:\nsecond", "Patron paylaştı:\nthird"]
            .map(|p| format!("\x1b[200~{p}\x1b[201~\r")).join("");
        assert_eq!(actual, expected, "each paste must have its own Enter, preserving inbox FIFO");
        assert_eq!(with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM inbox_messages", [], |r| r.get::<_,i64>(0)).map_err(|e| e.to_string())).unwrap(), 0);
    }
    fn input() -> Submit { Submit { team_id: "default".into(), client_id: None, text: Some("bir\niki\u{1b}[201~\u{0}".into()), image: None } }
    #[test]
    fn validates_size_mime_base64_and_cleans_controls_without_accepting_paths() {
        assert_eq!(validate(&input()).unwrap().0, "bir\niki[201~");
        let mut s = input(); s.text = None;
        assert_eq!(validate(&s).unwrap_err().code, "inbox_empty");
        s.text = Some("ü".repeat(TEXT_LIMIT / 2 + 1));
        assert_eq!(validate(&s).unwrap_err().code, "inbox_text_too_large");
        s.text = None; s.image = Some(Image { mime: "image/gif".into(), base64: "AA==".into() });
        assert_eq!(validate(&s).unwrap_err().code, "inbox_mime");
        s.image.as_mut().unwrap().mime = "image/png".into();
        s.image.as_mut().unwrap().base64 = "?".into();
        assert_eq!(validate(&s).unwrap_err().code, "inbox_base64");
        s.image.as_mut().unwrap().base64 = base64::engine::general_purpose::STANDARD.encode(vec![0; IMAGE_LIMIT + 1]);
        assert_eq!(validate(&s).unwrap_err().code, "inbox_image_too_large");
        assert!(serde_json::from_value::<Submit>(serde_json::json!({"teamId":"default","text":"x","path":"C:/evil"})).is_err());
        assert!(serde_json::from_value::<Submit>(serde_json::json!({"teamId":"default","image":{"mime":"image/png","base64":"AA==","path":"evil"}})).is_err());
    }

    #[test]
    fn busy_inbox_is_durable_idempotent_and_host_only_without_board_rows() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-busy-{}-{}", std::process::id(), crate::diagnostics::now_ms()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        let mut s = input(); s.client_id = Some("share-1".into());
        assert_eq!(core.inbox_submit(s).unwrap_err().code, "inbox_no_assistant");
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        // Draining is a real admission barrier: the accepted share must remain queued.
        core.draining.store(true, std::sync::atomic::Ordering::Release);
        let submit = || { let mut s = input(); s.client_id = Some("share-1".into()); s };
        let reply = core.inbox_submit(submit()).unwrap();
        assert_eq!(reply, Reply { ok: true, agent_id: "ayla".into(), queued: true });
        assert_eq!(core.inbox_submit(submit()).unwrap(), reply);
        let c = core.db.0.lock().unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM inbox_messages", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(c);
        assert_eq!(crate::mcp::tools_list_for_test(&core, "ayla").iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["memory_search"]);
        assert_eq!(crate::mcp::call_for_test(&core, "ayla", "inbox_submit")["isError"], true);
        assert_eq!(crate::mcp::call_for_test(&core, "ayla", "task_create")["isError"], true);
        assert_eq!(crate::mcp::call_for_test(&core, "timur", "inbox_submit")["isError"], true);
        for (tool, expected) in [("WebFetch","allow"),("WebSearch","allow"),("AskUserQuestion","allow"),("Bash","deny"),("Write","deny")] {
            let reply=core.delegation_on_hook("ayla","PreToolUse",&serde_json::json!({"tool_name":tool,"tool_input":{"command":"git status","file_path":"C:/evil"}}),false).unwrap();
            let reply:serde_json::Value=serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"],expected,"{tool}");
        }
        crate::db::with(&core.db,|c| {
            c.execute_batch("UPDATE inbox_messages SET pasted=1;
                INSERT INTO inbox_receipts(id,team_id,agent_id,client_id,reply) VALUES(2,'default','ayla','pending','{}');
                INSERT INTO inbox_messages(receipt_id,agent_id,prompt) VALUES(2,'ayla','pending after restart');").map_err(|e|e.to_string())
        }).unwrap();
        core.inbox_resume().unwrap();
        crate::db::with(&core.db,|c| {
            assert_eq!(c.query_row("SELECT COUNT(*) FROM inbox_messages WHERE pasted=1",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM inbox_receipts",[],|r|r.get::<_,i64>(0)).unwrap(),2);
            Ok(())
        }).unwrap();
        core.shutdown();
        drop(core);
        let reopened = crate::db::open(&root.join("data/dex.db")).unwrap();
        assert_eq!(reopened.query_row("SELECT COUNT(*) FROM inbox_messages", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn idle_pastes_one_share_and_ups_erases_text_before_the_next_share() {
        use crate::agents::Status;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("inbox-paste-{}-{}",std::process::id(),crate::diagnostics::now_ms()));
        let core = crate::Core::start(crate::Paths {data_dir:root.join("data"),config_dir:root.join("config")},crate::Events::new(crate::events::NullSink)).unwrap();
        crate::db::with(&core.db, |c| { c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).map_err(|e|e.to_string())?; Ok(()) }).unwrap();
        crate::agents::CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        let first = core.inbox_submit(input()).unwrap();
        let starts = crate::agents::CAPTURE.with(|c| c.borrow_mut().take().unwrap());
        assert_eq!(starts.len(),1);
        assert!(starts[0].0.args.iter().all(|a| !a.contains("Patron paylaştı")), "share is pasted only after Idle");
        assert!(first.queued);
        core.agent_states.hook("ayla", Some(Status::Working), None);
        let mut second = input(); second.text=Some("ikinci paylaşım".into()); second.client_id=Some("second".into());
        assert!(core.inbox_submit(second).unwrap().queued);
        core.agent_states.hook("ayla", Some(Status::Idle), None);
        let delivered = core.inbox_deliver_locked("ayla").unwrap().unwrap();
        let count = || crate::db::with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM inbox_messages", [], |r|r.get::<_,i64>(0)).map_err(|e|e.to_string())).unwrap();
        assert_eq!(count(),2);
        assert!(core.inbox_deliver_locked("ayla").unwrap().is_none(), "no duplicate before UPS");
        core.inbox_prompt_received("ayla", "wrong"); assert_eq!(count(),2);
        core.inbox_prompt_received("ayla", "Patron paylaştı:\nbir\niki[201~"); assert_eq!(count(),1);
        core.agent_states.hook("ayla", Some(Status::Idle), None);
        assert_ne!(core.inbox_deliver_locked("ayla").unwrap(), Some(delivered));
        core.inbox_prompt_received("ayla", "Patron paylaştı:\nikinci paylaşım"); assert_eq!(count(),0);
        let mut duplicate = input(); duplicate.client_id=Some("second".into());
        assert!(core.inbox_submit(duplicate).unwrap().queued, "original response retained after delivery");
        assert_eq!(count(),0);
        core.drain(true);
        core.agent_states.hook("ayla",Some(Status::Idle),None);
        let mut held=input(); held.text=Some("drain sonrası".into());
        assert!(core.inbox_submit(held).unwrap().queued);
        core.drain(false);
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
        let pasted=|| crate::db::with(&core.db,|c| c.query_row("SELECT pasted FROM inbox_messages",[],|r|r.get::<_,bool>(0)).map_err(|e|e.to_string())).unwrap();
        while !pasted() && std::time::Instant::now()<deadline { std::thread::sleep(std::time::Duration::from_millis(25)); }
        assert!(pasted(),"cancelled drain must release the inbox queue");
        core.inbox_prompt_received("ayla","Patron paylaştı:\ndrain sonrası");
        core.ptys.kill("ayla"); core.shutdown();
    }

    #[test]
    fn stored_image_has_generated_name_for_each_supported_mime() {
        let f = crate::projects::tests::Fixture::new();
        let mut s=input(); s.text=None;
        for (mime, header, ext) in [("image/png",b"\x89PNG\r\n\x1a\n".as_slice(),"png"),("image/jpeg",b"\xff\xd8\xff".as_slice(),"jpg"),("image/webp",b"RIFF0000WEBP".as_slice(),"webp")] {
            s.image=Some(Image{mime:mime.into(),base64:base64::engine::general_purpose::STANDARD.encode(header)});
            let (_,image)=validate(&s).unwrap();
            let (bytes,extension)=image.unwrap();
            let path=store_image(&f.0,&bytes,extension).unwrap();
            assert_eq!(path.parent().unwrap(),PathBuf::from(crate::teams::normalize_dir(f.0.to_str().unwrap()).unwrap()).join(".dex/inbox"));
            assert_eq!(path.extension().unwrap(),ext);
            assert_eq!(std::fs::read(&path).unwrap(),header);
        }
    }

    #[test]
    fn store_image_rejects_a_linked_inbox_parent() {
        let f=crate::projects::tests::Fixture::new();
        let root=f.0.join("team"); let outside=f.0.join("elsewhere");
        std::fs::create_dir_all(&root).unwrap(); std::fs::create_dir_all(&outside).unwrap();
        #[cfg(windows)]
        let link=std::os::windows::fs::symlink_dir(&outside,root.join(".dex"));
        #[cfg(unix)]
        let link=std::os::unix::fs::symlink(&outside,root.join(".dex"));
        if link.is_err() { return; }
        assert!(store_image(&root,b"fixture","png").is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(),0);
    }
}
