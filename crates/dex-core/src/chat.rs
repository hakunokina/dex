// S5-B: the agent's own session transcript as a chat. The Claude `~/.claude/projects/…jsonl`
// file is append-only, so one parse gives paged history
// (`chat.history`) and a directory watch + tail gives live lines (`chat-message` events —
// file watching, not polling). Sending rides the S2b delivery paths: offline → `agent_start`
// with the text as the first prompt, idle → bracketed-paste + Enter, busy → an in-memory
// queue delivered when the Stop hook ends the turn.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, Weak};

/// One normalized transcript message. `seq` is the 1-based line number of the row that produced
/// it — append-only files keep it stable, so it is the paging cursor. Tool cards keep the `seq`
/// of their call; the result updates the same `id` in place.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub seq: u64,
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub thinking: bool,
    pub ts: Option<u64>,
    pub tool: Option<ToolCard>,
}

/// `ok`: `None` = running, `Some(true)` = ✓, `Some(false)` = ✗. Input is a one-line summary;
/// the result keeps its line breaks, cut at `TOOL_RESULT_MAX` chars.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCard {
    pub name: String,
    pub input: String,
    pub result: Option<String>,
    pub ok: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPage {
    pub messages: Vec<ChatMessage>,
    pub cursor: Option<u64>,
    pub has_more: bool,
}

// ---- shared helpers ----

/// `2002-01-01T12:34:56.789Z` → epoch ms, without a date dep. Both engines write this shape.
pub(crate) fn parse_ts(v: &Value) -> Option<u64> {
    let s = v.as_str()?;
    let mut it = s.strip_suffix('Z')?.split(['-', ':', 'T', '.']);
    let y = it.next()?.parse::<i64>().ok()?;
    let mo = it.next()?.parse::<i64>().ok()?;
    let d = it.next()?.parse::<i64>().ok()?;
    let h = it.next()?.parse::<i64>().ok()?;
    let mi = it.next()?.parse::<i64>().ok()?;
    let sec = it.next()?.parse::<i64>().ok()?;
    let ms: i64 = match it.next() {
        Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
            let fraction = &f[..f.len().min(3)];
            fraction.parse::<i64>().ok()? * 10_i64.pow(3 - fraction.len() as u32)
        }
        Some(_) => return None,
        None => 0,
    };
    // days from civil (Hinnant); the transcript clock is UTC.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3_600 + mi * 60 + sec) * 1000 + ms).max(0) as u64)
}

/// One line, `…` past `max` (on a char boundary).
fn short(text: &str, max: usize) -> String {
    let flat: String = text.trim().chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Multi-line, `…` past `max` chars — tool output keeps its line breaks so the chat can fold it.
fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut: String = t.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Claude `tool_use.input` objects: the interesting key on one line.
fn input_summary(input: &Value) -> String {
    const KEYS: &[&str] = &["command", "file_path", "path", "pattern", "url", "query", "prompt", "description", "script", "content", "cmd", "name"];
    for k in KEYS {
        if let Some(v) = input.get(*k) {
            let text = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return short(&text, 160);
        }
    }
    short(&input.to_string(), 160)
}

/// Claude `tool_result.content`: a string or a list of `{type:"text"}` blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Tool output kept per card (line breaks included); the chat folds long ones.
const TOOL_RESULT_MAX: usize = 4000;

// ---- parsing ----

/// Collects messages across lines so a tool result can patch the card of its earlier call.
struct Collector {
    out: Vec<ChatMessage>,
    tools: HashMap<String, usize>,
    seq: u64,
}

impl Collector {
    fn push(&mut self, id: String, role: &str, text: String, thinking: bool, ts: Option<u64>, tool: Option<ToolCard>) {
        if tool.is_none() && text.trim().is_empty() {
            return;
        }
        // Same row can carry several messages (an assistant turn with text + calls): keep ids unique.
        // The first push keeps the exact id (tool results patch by it); later twins get a suffix.
        let base = id.clone();
        let mut id = id;
        let mut n = 1;
        while self.out.iter().any(|m| m.id == id) {
            n += 1;
            id = format!("{base}#{n}");
            if n > 50 {
                break;
            }
        }
        if let Some(t) = tool.clone() {
            self.tools.insert(id.clone(), self.out.len());
            self.out.push(ChatMessage { id, seq: self.seq, role: role.into(), text, thinking, ts, tool: Some(t) });
            return;
        }
        self.out.push(ChatMessage { id, seq: self.seq, role: role.into(), text, thinking, ts, tool: None });
    }

    fn tool_result(&mut self, call_id: &str, result: String, ok: bool) {
        if let Some(&i) = self.tools.get(call_id) {
            if let Some(t) = self.out[i].tool.as_mut() {
                t.result = Some(clip(&result, TOOL_RESULT_MAX));
                t.ok = Some(ok);
            }
        }
    }
}

fn parse_claude(v: &Value, c: &mut Collector) {
    if v["isSidechain"].as_bool().unwrap_or(false) {
        return;
    }
    let uuid = v["uuid"].as_str().unwrap_or("").to_string();
    let ts = parse_ts(&v["timestamp"]);
    let row_id = |c: &Collector, n: usize| if uuid.is_empty() { format!("{}-{n}", c.seq) } else { uuid.clone() };
    match v["type"].as_str() {
        Some("user") => {
            let content = &v["message"]["content"];
            match content {
                Value::String(s) if !v["isMeta"].as_bool().unwrap_or(false) => {
                    c.push(row_id(c, 1), "user", s.clone(), false, ts, None);
                }
                Value::Array(blocks) => {
                    // Row-level `isMeta` (synthetic reminders) hides the whole row's text;
                    // tool results still patch their cards.
                    let row_meta = v["isMeta"].as_bool().unwrap_or(false);
                    for b in blocks {
                        match b["type"].as_str() {
                            Some("text") if !row_meta && !b["isMeta"].as_bool().unwrap_or(false) => {
                                if let Some(t) = b["text"].as_str() {
                                    c.push(row_id(c, 2), "user", t.into(), false, ts, None);
                                }
                            }
                            Some("tool_result") => {
                                let text = result_text(&b["content"]);
                                let ok = !b["is_error"].as_bool().unwrap_or(false);
                                if let Some(id) = b["tool_use_id"].as_str() {
                                    c.tool_result(id, text, ok);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some("assistant") => {
            let blocks = v["message"]["content"].as_array().cloned().unwrap_or_default();
            for b in &blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str() {
                            c.push(row_id(c, 3), "assistant", t.into(), false, ts, None);
                        }
                    }
                    Some("thinking") => {
                        if let Some(t) = b["thinking"].as_str() {
                            c.push(row_id(c, 4), "assistant", t.into(), true, ts, None);
                        }
                    }
                    Some("tool_use") => {
                        let (id, name) = (b["id"].as_str().unwrap_or("").to_string(), b["name"].as_str().unwrap_or("tool").to_string());
                        if id.is_empty() {
                            continue;
                        }
                        let tool = ToolCard { name, input: input_summary(&b["input"]), result: None, ok: None };
                        c.push(id, "tool", String::new(), false, ts, Some(tool));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Görev #19: the last assistant text of a claude transcript (its tail only), cut to `max_chars`
/// — what the leader gets when a job ends without `report`. The tail's cut first line is not
/// JSON; `collect` skips it.
pub(crate) fn last_assistant_text(path: &Path, max_chars: usize) -> Option<String> {
    const TAIL: u64 = 512 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let m = collect(&String::from_utf8_lossy(&bytes))
        .into_iter()
        .rev()
        .find(|m| m.role == "assistant" && !m.thinking && m.tool.is_none())?;
    Some(short(&m.text, max_chars))
}

/// #125: the claude transcript's last turn is over — its last conversation row (user / assistant /
/// `system` `turn_duration`, sidechains skipped) is `turn_duration`, which claude writes once a turn
/// really ends (a blocked Stop goes on with an assistant row). Meta rows (title, mode, file history)
/// may follow it. Unreadable, or no such row in the tail → false.
pub(crate) fn claude_turn_ended(path: &Path) -> bool {
    const TAIL: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return false };
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).is_err() {
        return false;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return false;
    }
    for line in String::from_utf8_lossy(&bytes).lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue }; // the tail's cut first line
        if v["isSidechain"].as_bool().unwrap_or(false) {
            continue;
        }
        match v["type"].as_str() {
            Some("user" | "assistant") => return false,
            Some("system") if v["subtype"] == "turn_duration" => return true,
            _ => {}
        }
    }
    false
}

/// Only recent complete metadata rows are needed to reconcile hook acceptance.
pub(crate) fn claude_tail_rows(bytes: &[u8]) -> impl Iterator<Item = Value> + '_ {
    bytes[bytes.len().saturating_sub(64 * 1024)..].split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|row| row["type"] == "system" || row["type"] == "attachment"
            || row["type"] == "user" && row["isMeta"] == true)
}

/// Görev #15: Esc ends a claude turn with no hook at all; the transcript gets a user row
/// `[Request interrupted by user]` (`… for tool use]` when a tool was running or refused).
fn claude_interrupted(v: &Value) -> bool {
    if v["type"] != "user" || v["isSidechain"].as_bool().unwrap_or(false) {
        return false;
    }
    let marked = |t: &str| t.starts_with("[Request interrupted by user");
    match &v["message"]["content"] {
        Value::String(s) => marked(s),
        Value::Array(blocks) => blocks.iter().any(|b| b["type"] == "text" && b["text"].as_str().is_some_and(marked)),
        _ => false,
    }
}

/// Whole-file parse: every JSONL line, malformed ones skipped, tool results joined to their calls.
pub fn collect(text: &str) -> Vec<ChatMessage> {
    let mut c = Collector { out: Vec::new(), tools: HashMap::new(), seq: 0 };
    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            c.seq += 1;
            continue;
        }
        c.seq += 1;
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            parse_claude(&v, &mut c);
        }
    }
    c.out
}

/// Zincir geçişi (ve resume düşüşü) için deterministik, sır taşımayan devir özeti.
/// Thinking blokları dışarıda kalır; son kullanıcı isteği ile son görünür cevap/araç adımları
/// taşınır. Kota bitmiş motora ikinci bir özet çağrısı yapılamadığı için transcript doğrudan
/// kısaltılır. `max_bytes` UTF-8 sınırında uygulanır.
pub(crate) fn handoff_summary_from_text(text: &str, max_bytes: usize) -> String {
    let messages = collect(text);
    let request = redact_handoff(messages
        .iter()
        .rev()
        .find(|m| m.role == "user" && !m.thinking && !m.text.trim().is_empty())
        .map(|m| m.text.trim())
        .unwrap_or("Transcript'te kullanıcı isteği bulunamadı."));
    let mut progress = messages
        .iter()
        .filter(|m| !m.thinking && (m.role == "assistant" || m.role == "tool"))
        .rev()
        .take(10)
        .collect::<Vec<_>>();
    progress.reverse();
    let progress = progress
        .into_iter()
        .map(|m| {
            if let Some(tool) = &m.tool {
                // Tool input/output can contain commands, environment variables, credentials or
                // personal data. A provider-boundary handoff carries only operation and outcome.
                let outcome = match tool.ok {
                    Some(true) => "başarılı",
                    Some(false) => "başarısız",
                    None => "sonucu belirsiz",
                };
                format!("- araç {}: {outcome}", redact_handoff(&tool.name))
            } else {
                format!("- {}", redact_handoff(m.text.trim()))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let progress = if progress.is_empty() { "- Transcript'te tamamlanmış adım bulunamadı." } else { &progress };
    let summary = format!(
        "Önceki oturumdan devir özeti\n\nSon kullanıcı isteği:\n{request}\n\nYapılanlar:\n{progress}\n\n\
Açık kalanlar:\n- Son kullanıcı isteğinin tamamlanmamış kısmını sürdür. Bitmiş adımları tekrarlama; önce mevcut dosya/durumu doğrula."
    );
    truncate_utf8(&summary, max_bytes)
}

/// Conservative provider-boundary scrub. It intentionally favors context loss over moving a
/// credential or common personal identifier to a different provider.
fn redact_handoff(text: &str) -> String {
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if ["authorization", "api_key", "api-key", "password", "passwd", "secret", "bearer ", "token="]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                return "[GİZLİ SATIR]".to_string();
            }
            line.split_whitespace().map(redact_handoff_token).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_handoff_token(token: &str) -> String {
    let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '@' | '.' | '_' | '-'));
    let lower = bare.to_ascii_lowercase();
    let is_email = bare.contains('@') && bare.rsplit_once('@').is_some_and(|(_, host)| host.contains('.'));
    let digits = bare.chars().filter(|c| c.is_ascii_digit()).count();
    let alpha = bare.chars().any(|c| c.is_ascii_alphabetic());
    let long_opaque = bare.len() >= 32 && digits > 0 && alpha;
    let secret_prefix = ["sk-", "ghp_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "eyj"]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    let user_path = lower.starts_with("c:\\users\\") || lower.starts_with("/users/") || lower.starts_with("/home/");
    if is_email {
        "[E-POSTA]".into()
    } else if digits >= 9 {
        "[KİŞİSEL_VERİ]".into()
    } else if secret_prefix || long_opaque {
        "[GİZLİ]".into()
    } else if user_path {
        "[KULLANICI_YOLU]".into()
    } else {
        token.to_string()
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    let suffix = "…";
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = text[..cut].to_string();
    if max_bytes >= suffix.len() {
        out.push_str(suffix);
    }
    out
}

// ---- transcript source ----

fn claude_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join(".claude").join("projects")
}

/// Claude names the file after the session; the project folder varies with the cwd.
fn find_claude(session: &str) -> Option<PathBuf> {
    let dir = claude_dir();
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for project in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = project.path().join(format!("{session}.jsonl"));
        if let Ok(meta) = std::fs::metadata(&p) {
            let m = meta.modified().ok()?;
            if best.as_ref().is_none_or(|(t, _)| m > *t) {
                best = Some((m, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Where this agent's current conversation lives. `None` = no session yet (empty chat).
pub(crate) fn transcript_of(session: Option<&str>, known: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = known.filter(|p| Path::new(p).is_file()) {
        return Some(PathBuf::from(p));
    }
    find_claude(session?)
}

// ---- live watch (directory events + tail; no polling) ----

type Watch = (PathBuf, Option<u64>, mpsc::Sender<()>);

#[derive(Default)]
pub(crate) struct Watches(Mutex<HashMap<String, Watch>>);

impl Watches {
    /// Keeps one watcher per agent on `path`; a different path replaces it. #1033: returns once the
    /// new watcher listens and has taken the file's end — a line written after this is seen.
    pub(crate) fn ensure(&self, core: Weak<crate::Core>, agent: &str, engine: &str, path: &Path) {
        let process = core.upgrade().and_then(|core| core.ptys.process(agent));
        let mut map = self.0.lock().unwrap();
        if let Some((p, generation, _)) = map.get(agent) {
            if p == path && *generation == process.map(|p| p.0) {
                return;
            }
            map.remove(agent);
        }
        let (tx, rx) = mpsc::channel::<()>();
        map.insert(agent.into(), (path.into(), process.map(|p| p.0), tx));
        drop(map);
        let (agent, engine, path): (String, String, PathBuf) = (agent.into(), engine.into(), path.into());
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || watch_loop(core, agent, engine, path, process, rx, ready_tx));
        // A failed watcher drops the sender (no wait); the bound only guards a stalled notify setup.
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(2));
    }

    pub(crate) fn drop_watch(&self, agent: &str) -> bool {
        self.0.lock().unwrap().remove(agent).is_some()
    }
}

/// Replaced watches exit on the next wakeup; the timeout bounds a parked thread to ~30 s
/// after its sender is gone (a file event usually ends it sooner).
fn watch_loop(core: Weak<crate::Core>, agent: String, engine: String, path: PathBuf, process: Option<(u64, u64)>, rx: mpsc::Receiver<()>, ready: mpsc::Sender<()>) {
    use notify::Watcher;
    let (event_tx, event_rx) = mpsc::channel::<()>();
    let Ok(mut watcher) = notify::recommended_watcher(move |r: Result<notify::Event, _>| {
        // Any create/modify in the watched directory wakes the tail; the read is idempotent.
        if r.is_ok() {
            let _ = event_tx.send(());
        }
    }) else {
        return;
    };
    // Watch the parent: robust against the file being created later and against editors that
    // swap files.
    let watched_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone());
    if watcher.watch(&watched_dir, notify::RecursiveMode::NonRecursive).is_err() {
        return;
    }
    // Skip what history already showed; count lines so live `seq` matches the file's.
    let head = std::fs::read(&path).unwrap_or_default();
    let (mut offset, mut seq) = (head.len() as u64, head.iter().filter(|b| **b == b'\n').count() as u64);
    let mut partial = String::new();
    if engine == "claude" {
        if let Some(core) = core.upgrade() {
            for row in claude_tail_rows(&head) {
                core.claude_transcript_row(&agent, &row, process.map(|p| p.0));
            }
        }
    }
    if let Some(core) = core.upgrade() {
        for line in head.split(|b| *b == b'\n') {
            if let Ok(row) = serde_json::from_slice::<Value>(line) {
                core.worker_notes_observed(&agent, &engine, &path, &row, process.map(|p| p.0));
            }
        }
    }
    drop(head);
    let _ = ready.send(());
    loop {
        match event_rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(()) => {
                while event_rx.try_recv().is_ok() {}
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return, // core is gone
        }
        if matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)) {
            return; // replaced
        }
        let Some(core) = core.upgrade() else { return };
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let len = meta.len();
        if len < offset {
            // Truncated or replaced: the client refetches history; we resume from the new end.
            let _ = core.events.emit("chat-reset", &serde_json::json!({ "agentId": agent }));
            offset = len;
            seq = std::fs::read(&path).map(|b| b.iter().filter(|x| **x == b'\n').count() as u64).unwrap_or(0);
            partial.clear();
            continue;
        }
        if len == offset {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(&path) else { continue };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        offset += bytes.len() as u64;
        partial.push_str(&String::from_utf8_lossy(&bytes));
        let emit = |text: &str, seq: u64| {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                core.worker_notes_observed(&agent, &engine, &path, &v, process.map(|p| p.0));
                let mut c = Collector { out: Vec::new(), tools: HashMap::new(), seq };
                core.claude_transcript_row(&agent, &v, process.map(|p| p.0));
                parse_claude(&v, &mut c);
                for m in c.out {
                    let _ = core.events.emit("chat-message", &serde_json::json!({ "agentId": agent, "message": m }));
                }
                if claude_interrupted(&v) {
                    crate::hooks::on_interrupt(&core, &agent);
                }
            }
        };
        while let Some(pos) = partial.find('\n') {
            let line: String = partial.drain(..=pos).collect();
            seq += 1;
            let line = line.trim();
            if !line.is_empty() {
                emit(line, seq);
            }
        }
    }
}

// ---- Core API ----

impl crate::Core {
    /// The agent's engine + session, from live state first, the saved session second.
    fn chat_source(&self, agent: &str) -> Result<(String, Option<String>, Option<String>), String> {
        let state = self.agent_states.get(agent);
        with_engine(self, agent, state.engine.clone()).map(|engine| {
            let session = state.session_id.clone().or_else(|| {
                crate::db::with(&self.db, |c| crate::agents::resumable_session(c, agent, &engine)).ok().flatten()
            });
            let known = state.transcript.clone().or_else(|| {
                crate::db::with(&self.db, |c| saved_transcript(c, agent, &engine)).ok().flatten()
            });
            (engine, session, known)
        })
    }

    /// Aktif oturum transcript'inden yeni motora taşınacak 2–4 kB'lik bağlamı üretir.
    pub(crate) fn chain_handoff_summary(&self, agent: &str, max_bytes: usize) -> Result<String, String> {
        let (_, session, known) = self.chat_source(agent)?;
        self.chain_handoff_summary_from(session.as_deref(), known.as_deref(), max_bytes)
    }

    /// Build from the exact session carried by StopFailure, not mutable active-agent state.
    pub(crate) fn chain_handoff_summary_from(
        &self,
        session: Option<&str>,
        known: Option<&str>,
        max_bytes: usize,
    ) -> Result<String, String> {
        let path = transcript_of(session, known)
            .ok_or_else(|| "aktif transcript bulunamadı".to_string())?;
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(handoff_summary_from_text(&text, max_bytes))
    }

    pub fn chat_history(&self, agent: &str, before: Option<u64>, limit: Option<u32>) -> Result<ChatPage, String> {
        with_engine(self, agent, None)?; // unknown agent / no db row
        let (engine, session, known) = self.chat_source(agent)?;
        let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
        let empty = ChatPage { messages: Vec::new(), cursor: None, has_more: false };
        let Some(path) = transcript_of(session.as_deref(), known.as_deref()) else {
            return Ok(empty);
        };
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let all = collect(&text);
        let younger: Vec<_> = all.into_iter().filter(|m| before.is_none_or(|b| m.seq < b)).collect();
        let has_more = younger.len() > limit;
        let start = younger.len().saturating_sub(limit);
        let messages: Vec<ChatMessage> = younger[start..].to_vec();
        let cursor = messages.first().map(|m| m.seq).filter(|_| has_more || before.is_some());
        self.chat_watches.ensure(self.weak_self.clone(), agent, &engine, &path);
        Ok(ChatPage { messages, cursor, has_more })
    }

    /// Sends patron text to the agent over the S2b delivery paths. `chat-queue` events keep
    /// every client's "sırada" view in sync.
    pub fn chat_send(&self, agent: &str, text: &str) -> Result<&'static str, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("boş mesaj".into());
        }
        if text.chars().count() > 50_000 {
            return Err("mesaj çok uzun (en çok 50.000 karakter)".into());
        }
        with_engine(self, agent, None)?; // unknown agent / no db row
        let gate = self.delivery_gate(agent);
        let _delivery = gate.lock().map_err(|_| "teslim kilidi kullanılamıyor")?;
        // #447: admit before reading live state: an unseen leader may close until this gate.
        // A drain holds the text in the queue (the drain's end or the next core delivers it).
        let turn = self.admit(agent);
        let mut status = self.agent_states.get(agent).status;
        if !self.ptys.is_running(agent) {
            status = crate::agents::Status::Offline;
        } else if status == crate::agents::Status::Offline {
            status = crate::agents::Status::Idle;
        }
        let typing = self.agent_states.typing(agent);
        let queued_count = {
            let mut queues = self.chat_queues.lock().unwrap();
            let queue = queues.entry(agent.into()).or_default();
            let delivery = match turn {
                Some(_) => crate::agents::delivery(status, typing, false),
                None => crate::agents::Delivery::Queue,
            };
            match delivery {
                crate::agents::Delivery::Queue => {
                    queue.push_back(text.into());
                    self.emit_queue(agent, queue.make_contiguous());
                    return Ok("queued");
                }
                crate::agents::Delivery::Start => {
                    // Anything queued earlier rides along with the first prompt.
                    queue.push_back(text.into());
                    let prompt = queue.iter().cloned().collect::<Vec<_>>().join("\n\n");
                    queue.clear();
                    Some(prompt)
                }
                crate::agents::Delivery::Paste => None,
            }
        };
        let turn = turn.expect("held: queued and returned above");
        match queued_count {
            Some(prompt) => {
                self.agent_spawn(
                    agent,
                    crate::agents::StartOptions { prompt: Some(prompt), ..Default::default() },
                )?;
                // The queue rode along with the first prompt — now empty.
                self.emit_queue(agent, &[]);
                Ok("started")
            }
            None => {
                self.paste_admitted(agent, text, &turn)?;
                Ok("sent")
            }
        }
    }

    pub(crate) fn delivery_gate(&self, agent: &str) -> std::sync::Arc<Mutex<()>> {
        self.delivery_gates.lock().unwrap().entry(agent.into()).or_default().clone()
    }

    /// Bracketed-paste + Enter (the S2b idle path) and mark the agent working. #447: past the gate
    /// only ([`crate::restart::HELD`] when a drain holds it; nothing is written then).
    pub(crate) fn paste_prompt(&self, agent: &str, text: &str) -> Result<(), String> {
        let turn = self.admit(agent).ok_or(crate::restart::HELD)?;
        self.paste_admitted(agent, text, &turn)
    }

    /// [`Core::paste_prompt`] for a path that holds its turn already ([`crate::Core::admit`]).
    pub(crate) fn paste_admitted(&self, agent: &str, text: &str, _turn: &crate::restart::Turn) -> Result<(), String> {
        self.paste_text(agent, text)?;
        self.paste_enter(agent).map(|_| {
            // The sender already knows the prompt. Do not wait for a hook to preserve
            // the patron's conversation.
            let _ = crate::db::with(&self.db, |_| {
                self.turns.prompt(agent, text);
                Ok(())
            });
        })
    }

    pub(crate) fn paste_text(&self, agent: &str, text: &str) -> Result<(), String> {
        self.ptys.write(agent, format!("\x1b[200~{text}\x1b[201~").as_bytes())
    }

    /// Separate Enter allows a leader's unaccepted paste to retry without duplicating its text.
    pub(crate) fn paste_enter(&self, agent: &str) -> Result<(), String> {
        self.paste_enter_with(|| self.claude_prompt_submit(agent, true, None, || self.ptys.write(agent, b"\r")))
    }

    /// Run the final check and write together, after the redraw delay.
    pub(crate) fn paste_enter_with(&self, submit: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        #[cfg(test)]
        if let Some(pause) = self.paste_pause.lock().unwrap().take() {
            pause();
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        #[cfg(test)]
        if let Some(pause) = self.paste_enter_pause.lock().unwrap().take() {
            pause();
        }
        submit()
    }

    /// A Stop that no delegation continued: the turn really ended, the TUI sits at its input
    /// box. Deliver the chat queue after the redraw settles. Any doubt (busy again, patron
    /// typing, process gone) leaves the queue for the next Stop.
    pub(crate) fn chat_on_stop(&self, agent: &str) {
        if self.chat_queues.lock().unwrap().get(agent).is_none_or(|q| q.is_empty()) {
            return;
        }
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            let Some(core) = weak.upgrade() else { return };
            let gate = core.delivery_gate(&agent);
            let Ok(_delivery) = gate.lock() else { return };
            // Keep queue protection until admission takes over, then hold the turn through Enter.
            let Some(turn) = core.admit(&agent) else { return };
            let state = core.agent_states.get(&agent);
            let idle = core.ptys.is_running(&agent)
                && state.status == crate::agents::Status::Idle
                && !core.agent_states.typing(&agent);
            if !idle {
                return;
            }
            let prompt = {
                let mut queues = core.chat_queues.lock().unwrap();
                let Some(queue) = queues.get_mut(&agent) else { return };
                if queue.is_empty() {
                    return;
                }
                let prompt = queue.iter().cloned().collect::<Vec<_>>().join("\n\n");
                queue.clear();
                core.emit_queue(&agent, &[]);
                prompt
            };
            if core.paste_admitted(&agent, &prompt, &turn).is_err() {
                // The PTY died in the race window: back into the queue for the next start.
                let mut queues = core.chat_queues.lock().unwrap();
                for part in prompt.split("\n\n") {
                    queues.entry(agent.clone()).or_default().push_back(part.into());
                }
            }
        });
    }

    /// A new session (or a saved one coming back): the old file is history now. If the resolved
    /// transcript changed, the live watch follows it and open chats refetch.
    pub(crate) fn chat_rescan(&self, agent: &str) {
        let Ok((engine, session, known)) = self.chat_source(agent) else { return };
        let Some(path) = transcript_of(session.as_deref(), known.as_deref()) else {
            if self.chat_watches.drop_watch(agent) {
                let _ = self.events.emit("chat-reset", &serde_json::json!({ "agentId": agent }));
            }
            return;
        };
        self.chat_watches.ensure(self.weak_self.clone(), agent, &engine, &path);
    }

    pub(crate) fn emit_queue(&self, agent: &str, messages: &[String]) {
        let _ = self.events.emit("chat-queue", &serde_json::json!({ "agentId": agent, "messages": messages }));
    }

    /// The PTY's visible screen as text — the waiting question shown in the chat card.
    pub fn chat_screen(&self, agent: &str) -> Result<String, String> {
        self.ptys.screen(agent)
    }
}

#[cfg(test)]
mod patron_turn_tests {
    use super::*;
    use crate::{agents::Status, db::with, delegate};
    use serde_json::json;

    fn core(tag: &str) -> (std::sync::Arc<crate::Core>, PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("patron-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        ).unwrap();
        (core, root)
    }

    struct Sink;
    impl crate::pty_stream::PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    fn live(core: &crate::Core, agent: &str) {
        let (sink, wait_ready) = crate::agents::shell_sink_with_ready_for_test(core, agent, Sink);
        core.pty_spawn(crate::pty::Spawn {
            id: agent.into(), cmd: crate::shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default()
        }, sink).unwrap();
        wait_ready();
    }

    #[test]
    fn live_fixture_answers_terminal_queries() {
        use std::{io::Write, sync::{Arc, Mutex}};
        struct Input {
            original: Box<dyn Write + Send>,
            bytes: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for Input {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let written = self.original.write(bytes)?;
                self.bytes.lock().unwrap().extend_from_slice(&bytes[..written]);
                Ok(written)
            }
            fn flush(&mut self) -> std::io::Result<()> { self.original.flush() }
        }
        let (core, root) = core("terminal-queries");
        live(&core, "timur");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        core.ptys.input_writer_for_test("timur", |original| Box::new(Input { original, bytes: bytes.clone() }));
        // Inject the queries through the live fixture's output, independently of the host OS.
        core.ptys.note("timur", "\x1b[6n\x1b[c\x1b]10;?\x1b]11;?");
        let reply = bytes.lock().unwrap().clone();
        core.ptys.kill("timur");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(root);
        assert!(reply.ends_with(b"\x1b[1;1R\x1b[?1;2c\x1b]10;rgb:c9c9/cdcd/d2d2\x1b\\\x1b]11;rgb:1313/1414/1616\x1b\\"), "the live fixture must answer terminal queries: {reply:?}");
    }

    #[test]
    fn claude_timeout_releases_leader_without_patron_input() {
        let (core, root) = core("claude-timeout");
        live(&core, "timur");
        let path = root.join("transcript.jsonl");
        std::fs::write(&path, "").unwrap();
        core.agent_states.hook("timur", Some(Status::Working), Some("session-a"));
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), crate::agents::next_seq());
        core.agent_states.set_transcript("timur", path.to_str().unwrap());
        core.chat_rescan("timur");
        std::fs::write(&path, format!("{}\n", json!({
            "type":"attachment", "sessionId":"session-a", "timestamp":"9999-01-01T00:00:00.000Z",
            "attachment":{"type":"hook_cancelled", "hookEvent":"Stop", "timedOut":true, "timeoutMs":5000}
        }))).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while core.agent_states.get("timur").status != Status::Idle && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = core.agent_states.get("timur").status;
        core.ptys.kill("timur");
        core.shutdown();
        assert_eq!(status, Status::Idle, "the CLI timeout must release the leader");
    }

    #[test]
    fn patron_turn_keeps_leader_results_and_questions_pending_until_stop() {
        let (core, _) = core("leader-claude");
        live(&core, "timur");
        core.delegation_on_hook("timur", "UserPromptSubmit", &json!({"prompt":"patron sorusu"}), false);
        core.agent_states.hook("timur", Some(Status::Working), None);
        let (result, question) = with(&core.db, |conn| {
            let leader = delegate::caller(conn, "timur")?;
            let make = |worker: &str| delegate::insert(conn, delegate::NewDelegation {
                from: &leader, to_agent: worker.into(), team_id: "default".into(), service_team_id: None,
                prompt: "is".into(), task_id: None, patron_words: None, effort: None,
            });
            let result = make("kaan")?;
            delegate::set_status(conn, result.id, "done")?;
            let job = make("ayla")?;
            delegate::set_status(conn, job.id, "running")?;
            let worker = delegate::caller(conn, "ayla")?;
            let question = crate::questions::ask(conn, &worker, &json!({"delegationId":job.id,"question":"Karar bekliyor"}))?;
            Ok((result.id, question.id))
        }).unwrap();
        assert_eq!(core.agent_states.get("timur").status, Status::Working, "UI pingable also sees busy");
        assert_eq!(core.turns.dex_turn("timur"), Some(false));
        assert!(with(&core.db, |c| delegate::get(c, result)).unwrap().delivered_at.is_none());
        assert!(with(&core.db, |c| crate::questions::get(c, question)).unwrap().leader_delivered_at.is_none());
        // Keep automatic idle question delivery parked; this case verifies hook context.
        core.agent_states.input("timur", true);
        core.agent_states.hook("timur", Some(Status::Working), Some("leader-session"));
        let out = core.delegation_on_hook("timur", "Stop", &json!({"prompt":"[Dex] bekleyen bildirimler"}), false).unwrap();
        assert!(out.contains(&format!("#{result}")) && out.contains("Karar bekliyor"), "{out}");
        assert!(with(&core.db, |c| delegate::get(c, result)).unwrap().delivered_at.is_none());
        assert!(with(&core.db, |c| crate::questions::get(c, question)).unwrap().leader_delivered_at.is_none());
        crate::claude_turn::tests::accepted_stop(&core, "timur");
        core.claude_hook_received("timur", "PreToolUse", Some("leader-session"), crate::agents::next_seq());
        assert!(with(&core.db, |c| delegate::get(c, result)).unwrap().delivered_at.is_some());
        assert!(with(&core.db, |c| crate::questions::get(c, question)).unwrap().leader_delivered_at.is_some());
        assert!(core.delegation_on_hook("timur", "UserPromptSubmit", &json!({"prompt":"[Dex] yeniden"}), false).is_none(), "exactly once");
        core.ptys.kill("timur");
        core.shutdown();
    }
}

fn with_engine(core: &crate::Core, agent: &str, live: Option<String>) -> Result<String, String> {
    if let Some(e) = live.filter(|e| !e.is_empty()) {
        return Ok(e);
    }
    crate::db::with(&core.db, |c| {
        crate::teams::get_agent(c, agent).map(|a| a.active_engine.unwrap_or(a.engine))
    })
}

fn saved_transcript(conn: &rusqlite::Connection, agent: &str, engine: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT transcript FROM agent_sessions WHERE agent_id = ?1 AND engine = ?2",
        rusqlite::params![agent, engine],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic transcript: UTC clocks, names, paths and ids are examples; CLI row shapes stay intact.
    const CLAUDE: &str = r##"{"type":"last-prompt","lastPrompt":"x"}
{"parentUuid":null,"isSidechain":false,"isMeta":false,"type":"user","uuid":"u1","timestamp":"2002-01-01T00:00:00.000Z","message":{"role":"user","content":"npm test koş"}}
{"parentUuid":"u1","isSidechain":false,"type":"assistant","uuid":"a1","timestamp":"2002-01-01T00:00:02.123Z","message":{"content":[{"type":"thinking","thinking":"testleri bulayım"},{"type":"text","text":"Testleri **koşuyorum**"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"npm test","timeout":60000}}]}}
{"parentUuid":"a1","isSidechain":false,"isMeta":false,"type":"user","uuid":"u2","timestamp":"2002-01-01T00:00:05.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"3 passed\n0 failed"}]}}
{"parentUuid":"u2","isSidechain":false,"isMeta":true,"type":"user","uuid":"u3","timestamp":"2002-01-01T00:00:05.100Z","message":{"role":"user","content":[{"type":"text","text":"bağlam gürültüsü"}]}}
{"isSidechain":true,"type":"user","uuid":"u4","message":{"role":"user","content":"yan zincir"}}
{"parentUuid":"u2","type":"assistant","uuid":"a2","timestamp":"2002-01-01T00:00:06.000Z","message":{"content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"C:\\proje\\src\\app.ts"}}]}}
{"parentUuid":"a2","isMeta":false,"type":"user","uuid":"u5","message":{"content":[{"tool_use_id":"toolu_2","type":"tool_result","is_error":true,"content":"dosya yok"}]}}
{"type":"assistant","uuid":"a3","message":{"content":[{"type":"tool_use","id":"toolu_3","name":"Bash","input":{"command":"XXX300"}}]}}
{"type":"system","subtype":"turn_duration","uuid":"s1","content":"ignore"}
{"type":"assistant","uuid":"a4","timestamp":"2002-01-01T00:00:09.000Z","message":{"content":[{"type":"text","text":"tamamlandı"}]}}
"##;

    #[test]
    fn claude_tail_includes_stop_feedback_context_receipt() {
        let rows = br##"{"type":"user","isMeta":true,"message":{"role":"user","content":"Stop hook feedback:\n# Dex: fixture\n"}}
{"type":"attachment","attachment":{"type":"hook_blocking_error","hookEvent":"Stop"}}
{"type":"system","subtype":"stop_hook_summary","hasOutput":true,"preventedContinuation":false}
{"type":"user","message":{"role":"user","content":"patron prompt"}}
"##;
        let tail: Vec<_> = claude_tail_rows(rows).collect();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0]["type"], "user");
        assert_eq!(tail[0]["isMeta"], true);
    }

    /// Synthetic turn end using Claude CLI 2.1.289 row shapes; trailing metadata must not hide the end.
    #[test]
    fn claude_turn_end_is_the_last_conversation_row() {
        let dir = std::env::temp_dir().join(format!("dex-turn-end-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, extra: &[&str]| {
            let p = dir.join(name);
            std::fs::write(&p, format!("{CLAUDE}{}\n", extra.join("\n"))).unwrap();
            p
        };
        let ended = [
            r#"{"type":"system","subtype":"stop_hook_summary","hookCount":1,"preventedContinuation":false}"#,
            r#"{"type":"system","subtype":"turn_duration","durationMs":9000}"#,
            r#"{"type":"ai-title","aiTitle":"x"}"#,
            r#"{"type":"file-history-delta"}"#,
            r#"{"isSidechain":true,"type":"assistant","message":{"content":[]}}"#,
        ];
        assert!(!claude_turn_ended(&file("mid.jsonl", &[])), "ends with an assistant row: mid-turn");
        assert!(claude_turn_ended(&file("ended.jsonl", &ended)), "meta and sidechain rows after the end");
        // A blocked Stop: the summary, then the turn goes on.
        let blocked = [r#"{"type":"system","subtype":"stop_hook_summary","preventedContinuation":false}"#];
        assert!(!claude_turn_ended(&file("blocked.jsonl", &blocked)));
        let mut next = ended.to_vec();
        next.push(r#"{"type":"user","message":{"role":"user","content":"[Dex] sentetik yeni iş"}}"#);
        assert!(!claude_turn_ended(&file("next.jsonl", &next)), "a new prompt opened the next turn");
        assert!(!claude_turn_ended(&dir.join("missing.jsonl")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_transcript_normalizes() {
        // A 300-char command: the parser shortens it to one line with an ellipsis.
        let claude = CLAUDE.replace("XXX300", &"x".repeat(300));
        let m = collect(&claude);
        let roles: Vec<(&str, bool)> = m.iter().map(|x| (x.role.as_str(), x.thinking)).collect();
        assert_eq!(
            roles,
            vec![
                ("user", false),          // prompt
                ("assistant", true),      // thinking (collapsed in the UI)
                ("assistant", false),     // reply
                ("tool", false),          // Bash call
                ("tool", false),          // Read call
                ("tool", false),          // long Bash call (input shortened)
                ("assistant", false),     // final
            ]
        );
        assert_eq!(m[0].text, "npm test koş");
        assert_eq!(m[0].ts, Some(1_009_843_200_000));
        assert_eq!(m[2].text, "Testleri **koşuyorum**");
        // meta text and sidechain rows never show up
        assert!(!m.iter().any(|x| x.text.contains("gürültü") || x.text.contains("yan zincir")));
        // results joined onto their cards
        let bash = m.iter().find(|x| x.id == "toolu_1").unwrap();
        assert_eq!(bash.tool.as_ref().unwrap().input, "npm test");
        assert_eq!(bash.tool.as_ref().unwrap().result.as_deref(), Some("3 passed\n0 failed"));
        assert_eq!(bash.tool.as_ref().unwrap().ok, Some(true));
        let read = m.iter().find(|x| x.id == "toolu_2").unwrap();
        assert_eq!(read.tool.as_ref().unwrap().ok, Some(false), "is_error → ✗");
        // a 300-char command is shortened to one line with an ellipsis
        let long = m.iter().find(|x| x.id == "toolu_3").unwrap();
        let input = &long.tool.as_ref().unwrap().input;
        assert!(input.chars().count() <= 160 && input.ends_with('…'), "{input}");
        // ids unique, seq = row order
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "id {id} tekrar");
        }
        assert_eq!(m[0].seq, 2);
    }

    #[test]
    fn tool_result_keeps_lines_and_is_capped() {
        let big = format!("a
b
{}", "x".repeat(TOOL_RESULT_MAX));
        let line = |id: &str, body: &str| {
            format!(
                r#"{{"type":"user","uuid":"{id}","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":{body}}}]}}}}"#
            )
        };
        let call = r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let text = format!("{call}
{}", line("u1", &serde_json::to_string(&big).unwrap()));
        let r = collect(&text)[0].tool.as_ref().unwrap().result.clone().unwrap();
        assert!(r.starts_with("a
b
"));
        assert_eq!(r.chars().count(), TOOL_RESULT_MAX);
        assert!(r.ends_with('…'));
    }

    #[test]
    fn interrupt_rows_are_recognized_and_nothing_else() {
        let row = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert!(claude_interrupted(&row(r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#)));
        assert!(claude_interrupted(&row(r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#)));
        assert!(claude_interrupted(&row(r#"{"type":"user","message":{"content":"[Request interrupted by user]"}}"#)));
        assert!(!claude_interrupted(&row(r#"{"type":"user","isSidechain":true,"message":{"content":"[Request interrupted by user]"}}"#)), "subagent");
        assert!(!claude_interrupted(&row(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#)));
        assert!(!claude_interrupted(&row(r#"{"type":"user","message":{"content":"neden [Request interrupted by user] yazıyor?"}}"#)), "quoted by the patron");
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let mixed = "not json\n{\"type\":\"user\",\"uuid\":\"u9\",\"message\":{\"role\":\"user\",\"content\":\"selam\"}}\n{ broken\n";
        let m = collect(mixed);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "selam");
        assert_eq!(m[0].seq, 2);
    }

    #[test]
    fn handoff_summary_keeps_request_progress_and_budget() {
        let summary = handoff_summary_from_text(CLAUDE, 2_400);
        assert!(summary.contains("Son kullanıcı isteği") && summary.contains("npm test koş"), "{summary}");
        assert!(summary.contains("Yapılanlar") && summary.contains("npm test"), "{summary}");
        assert!(summary.contains("Açık kalanlar"), "{summary}");
        assert!(!summary.contains("testleri bulayım"), "thinking devre taşınmamalı: {summary}");
        assert!(summary.len() <= 2_400, "{} bayt", summary.len());
    }

    #[test]
    fn handoff_summary_redacts_provider_boundary_data() {
        // Sentetik redaksiyon personası; ardışık rakamlı telefon yalnız tespit biçimini sınar.
        let transcript = r#"{"type":"user","message":{"role":"user","content":"yaz: ada@example.com Authorization: Bearer sk-secretsecretsecretsecretsecret 05321234567 C:\\Users\\Ada\\repo"}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo API_KEY=sk-leak"}}]}}
{"type":"user","tool_use_result":{"stdout":"token=ghp_abcdefghijklmnopqrstuvwxyz123456"},"tool_use_id":"t1"}"#;
        let summary = handoff_summary_from_text(transcript, 2_400);
        assert!(summary.contains("[GİZLİ SATIR]") || summary.contains("[E-POSTA]"), "{summary}");
        for leaked in ["ada@example.com", "sk-secret", "05321234567", "C:\\Users\\Ada", "API_KEY", "ghp_"] {
            assert!(!summary.contains(leaked), "sızıntı: {leaked}: {summary}");
        }
        assert!(summary.contains("araç Bash") && !summary.contains("echo"), "{summary}");
    }

    #[test]
    fn claude_file_discovery_and_ts() {
        assert_eq!(parse_ts(&serde_json::json!("2002-01-01T00:00:00.000Z")), Some(1_009_843_200_000));
        assert_eq!(parse_ts(&serde_json::json!(null)), None);
        assert!(transcript_of(Some("no-such-session"), None).is_none());
    }

    /// No PTY, no CLI: unknown agents fail, a sessionless agent reads empty, bad input is refused.
    #[test]
    fn chat_api_empty_history_and_bad_input() {
        let root = std::env::temp_dir().join(format!("dex-chat-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        assert!(core.chat_history("yok-ajan", None, None).is_err());
        let agent = core.agent_list().unwrap().into_iter().next().expect("seed crew");
        let page = core.chat_history(&agent.id, None, None).unwrap();
        assert!(page.messages.is_empty() && !page.has_more && page.cursor.is_none());
        assert!(core.chat_send(&agent.id, "   ").is_err(), "boş mesaj");
        assert!(core.chat_send(&agent.id, &"x".repeat(50_001)).is_err(), "çok uzun mesaj");
        assert!(core.chat_send("yok-ajan", "selam").is_err());
        assert!(core.chat_screen(&agent.id).is_err(), "çalışmayan PTY'nin ekranı yok");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}
