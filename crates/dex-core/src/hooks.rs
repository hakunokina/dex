// Claude Code hook bridge: a loopback HTTP receiver that turns hook events into agent status.
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::Core;

pub struct Hooks {
    pub port: u16,
    pub token: String,
    pub settings: PathBuf,
    /// #164 P3: workers' settings — PreToolUse fails closed (see [`settings_json`]).
    pub worker_settings: PathBuf,
    /// Legacy PTY registration; SessionStart resolves the explicit memory anchors for the index.
    pub memory: Mutex<HashMap<String, PathBuf>>,
    /// #164 P3: claude workers spawned with `worker_settings`, set by pty_spawn (in memory: no DB
    /// needed to know them when the DB is what fails).
    pub fail_closed: Mutex<HashSet<String>>,
    /// #242: rate limit + start/end pairing for the `agent.step` event.
    pub steps: crate::step::Limiter,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HookEvent {
    agent_id: String,
    event: String,
    status: Option<&'static str>,
    session_id: Option<String>,
    /// DEX-28: extra silence the stall watchdog allows while this tool runs ([`tool_grace_ms`]).
    tool_grace_ms: Option<u64>,
}

const MAX_BODY: usize = 32 * 1024 * 1024;

/// FIX-1: how long a drawn pre-session screen must stay still to count as a dialog. Measured on
/// Windows (claude 2.x): a trusted start stays blank ~3.1 s, then draws and fires SessionStart
/// ~1 s later with output flowing throughout; the trust dialog draws and then goes silent.
const PRE_SESSION_SETTLE: Duration = Duration::from_millis(2_000);

/// FIX-1: Claude's folder-trust dialog comes before any hook (SessionStart follows it), so no
/// hook can report it. The marker is structural, not its wording: no hook yet + a drawn screen +
/// silence → the process waits on the patron. It goes out like a PermissionRequest (status and
/// notification); the first real hook, or the exit, ends the phase. Codex is not watched: its
/// SessionStart may come only with the first turn, and Dex marks its workdir trusted anyway.
pub(crate) fn watch_pre_session(core: std::sync::Weak<Core>, agent: String, stream: std::sync::Arc<crate::pty_stream::PtyStream>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let Some(core) = core.upgrade() else { return };
        if stream.closed() || !core.agent_states.pre_session(&agent) {
            return;
        }
        if stream.settled_for().is_some_and(|d| d >= PRE_SESSION_SETTLE) {
            if let Some(state) = core.agent_states.blocked(&agent) {
                let event = HookEvent { agent_id: agent.clone(), event: "PreSessionPrompt".into(), status: Some("waiting"), session_id: None, tool_grace_ms: None };
                let _ = core.events.emit("dex-hook", &event);
                let _ = core.events.emit("agent-status", &state);
            }
        }
    });
}

/// Görev #15: Esc ends a claude turn without a Stop hook; the transcript's interrupt row (seen by
/// the chat watch) stands in for it. A busy agent turns idle — status, stall watchdog, first-reply
/// watch — and the chat queue and a queued delegation go in as after a Stop. A running delegation
/// stays running: the session goes on, and its next Stop settles the row.
pub(crate) fn on_interrupt(core: &Core, agent: &str) {
    core.claude_interrupt(agent);
    core.agent_states.interrupt_team_turn(agent);
    turn_ended(core, agent, "Interrupt");
}

/// A turn that ended with no usable Stop (`Interrupt`; #125 `Settled`: the watchdog found the
/// transcript's turn over while the status still said working) is handled as after a Stop.
pub(crate) fn turn_ended(core: &Core, agent: &str, event: &str) {
    use crate::agents::Status;
    if !matches!(core.agent_states.get(agent).status, Status::Working | Status::Waiting) {
        return;
    }
    let state = core.agent_states.hook(agent, Some(Status::Idle), None);
    after_turn_ended(core, agent, event, &state);
}

/// Also used after an atomic, turn-matched rollout completion has already set Idle.
pub(crate) fn after_turn_ended(core: &Core, agent: &str, event: &str, state: &crate::agents::State) {
    if matches!(event, "Stop" | "Settled") {
        let _ = core.team_turn_ended(state);
    }
    let hook = HookEvent { agent_id: agent.into(), event: event.into(), status: Some("idle"), session_id: None, tool_grace_ms: None };
    core.hooks.steps.forget(agent);
    let _ = core.events.emit("dex-hook", &hook);
    let _ = core.events.emit("agent-status", state);
    core.first_reply_on_hook(agent, event);
    core.chat_on_stop(agent);
    core.delegation_after_stop(agent);
    let _ = core.events.emit("agent-idle", &agent);
}

// One settings file per Dex process: a second instance (same identifier) must not
// repoint the first one's sessions at its own port, which dies with it.
fn settings_path(dir: &std::path::Path, pid: u32) -> PathBuf {
    dir.join(format!("claude-hooks-{pid}.json"))
}

// Files left by crashed instances (Exit never ran) plus the pre-per-process shared file.
fn sweep_stale(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = match name.strip_prefix("claude-hooks-").and_then(|r| r.strip_suffix(".json")).map(|r| r.trim_start_matches("worker-")) {
            Some(pid) => pid.parse::<u32>().is_ok_and(|pid| !crate::proc::alive(pid)),
            None => name == "claude-hooks.json",
        };
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Binds the loopback receiver and writes `<config_dir>/claude-hooks-<pid>.json`. The caller
/// runs [`accept`] once the core exists; the host removes the file on exit.
pub fn start(config_dir: &Path) -> Result<(Hooks, TcpListener), String> {
    let e = |e: std::io::Error| e.to_string();
    let listener = TcpListener::bind("127.0.0.1:0").map_err(e)?;
    let port = listener.local_addr().map_err(e)?.port();
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

    std::fs::create_dir_all(config_dir).map_err(e)?;
    sweep_stale(config_dir);
    let settings = settings_path(config_dir, std::process::id());
    let worker_settings = config_dir.join(format!("claude-hooks-worker-{}.json", std::process::id()));
    for (path, fail_closed) in [(&settings, false), (&worker_settings, true)] {
        let text = serde_json::to_vec_pretty(&settings_json(port, fail_closed)).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(e)?;
    }
    Ok((Hooks { port, token, settings, worker_settings, memory: Mutex::default(), fail_closed: Mutex::default(), steps: Default::default() }, listener))
}

impl Hooks {
    pub fn remove_settings(&self) {
        let _ = std::fs::remove_file(&self.settings);
        let _ = std::fs::remove_file(&self.worker_settings);
    }

    pub fn is_fail_closed(&self, agent: &str) -> bool {
        self.fail_closed.lock().unwrap().contains(agent)
    }
}

const MAX_CONNECTIONS: usize = 64;

struct ConnectionSlot(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl ConnectionSlot {
    fn take(active: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Option<Self> {
        use std::sync::atomic::Ordering::Relaxed;
        active.fetch_update(Relaxed, Relaxed, |n| (n < MAX_CONNECTIONS).then_some(n + 1)).ok()?;
        Some(Self(active.clone()))
    }
}
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// At most 64 connection threads; excess sockets close without spawning a thread.
pub fn accept(listener: TcpListener, core: std::sync::Arc<Core>) {
    std::thread::spawn(move || {
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for stream in listener.incoming().flatten() {
            let Some(slot) = ConnectionSlot::take(&active) else { continue };
            let deadline = crate::local_http::Deadline::new(crate::local_http::REQUEST_TIMEOUT);
            let core = core.clone();
            // #125: arrival order, taken before the thread races the others for locks.
            let seq = crate::agents::next_seq();
            let _ = std::thread::Builder::new().spawn(move || {
                let _slot = slot;
                serve(stream, core, seq, deadline);
            });
        }
    });
}

// Hooks are additive to the user's own settings (--settings merges, it doesn't replace).
// Port is baked in (per-process file, see settings_path); token and agent id come from the
// pane's env so the file holds no secret. SessionStart only allows command hooks, so it
// uses curl in exec form (no shell) with curl's own env expansion (curl >= 8.3).
//
// #164 P3 `fail_closed` (workers): an HTTP hook that cannot reach Dex, times out or gets a non-2xx
// is a non-blocking error in Claude Code, and so is a command hook's own timeout — the tool would
// run unjudged. Only a command hook's exit 2 blocks, so workers' PreToolUse is a shell-form
// command (Git Bash on Windows, verified on Claude Code 2.1.289): curl `-f`, its own `-m` well
// inside the hook's timeout, and any failure (Dex closed, hung, 401) exits 2 with a reason.
fn settings_json(port: u16, fail_closed: bool) -> Value {
    let url = format!("http://127.0.0.1:{port}/hook");
    let http = json!([{ "hooks": [{
        "type": "http",
        "url": url,
        "timeout": 5,
        "headers": {
            "Authorization": "Bearer $DEX_HOOK_TOKEN",
            "X-Dex-Agent": "$DEX_AGENT_ID",
            "X-Dex-Key": "$DEX_MCP_KEY"
        },
        "allowedEnvVars": ["DEX_HOOK_TOKEN", "DEX_AGENT_ID", "DEX_MCP_KEY"]
    }]}]);
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let pre_tool = if fail_closed {
        json!([{ "hooks": [{
            "type": "command",
            "command": format!(
                "{curl} -sS -f -m 8 -H 'Content-Type: application/json' \
                 --variable %DEX_HOOK_TOKEN --variable %DEX_AGENT_ID --variable %DEX_MCP_KEY \
                 --expand-header 'Authorization: Bearer {{{{DEX_HOOK_TOKEN}}}}' \
                 --expand-header 'X-Dex-Agent: {{{{DEX_AGENT_ID}}}}' \
                 --expand-header 'X-Dex-Key: {{{{DEX_MCP_KEY}}}}' \
                 --data-binary @- {url} || {{ echo 'Dex izin denetimi yanıt vermedi (Dex kapalı ya da meşgul); araç engellendi. Biraz sonra aynı adımı yeniden dene; sürerse raporunda belirt.' >&2; exit 2; }}"
            ),
            "timeout": 15
        }]}])
    } else {
        http.clone()
    };
    json!({ "hooks": {
        "SessionStart": [{ "hooks": [{
            "type": "command",
            "command": curl,
            "args": [
                "-sS", "-m", "3", "-H", "Content-Type: application/json",
                "--variable", "%DEX_HOOK_TOKEN", "--variable", "%DEX_AGENT_ID", "--variable", "%DEX_MCP_KEY",
                "--expand-header", "Authorization: Bearer {{DEX_HOOK_TOKEN}}",
                "--expand-header", "X-Dex-Agent: {{DEX_AGENT_ID}}",
                "--expand-header", "X-Dex-Key: {{DEX_MCP_KEY}}",
                "--data-binary", "@-", url
            ],
            "timeout": 5
        }]}],
        "UserPromptSubmit": http,
        "PreToolUse": pre_tool,
        "PostToolUse": http,
        // #242: a failed tool fires this instead of PostToolUse; it ends the `agent.step`.
        "PostToolUseFailure": http,
        "PermissionRequest": http,
        "Notification": http,
        "Stop": http,
        "StopFailure": http,
        "SessionEnd": http
    }, "skipDangerousModePermissionPrompt": true })
}

fn status_for(event: &str, body: &Value) -> Option<&'static str> {
    let field = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("");
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => Some("working"),
        // PermissionRequest fires immediately; Notification(permission_prompt) only after ~6 s idle.
        "PermissionRequest" => Some("waiting"),
        "Notification" => match field("notification_type") {
            "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input" => {
                Some("waiting")
            }
            _ => None,
        },
        "SessionStart" | "Stop" | "StopFailure" => Some("idle"),
        // clear/resume end one session but the process lives on; SessionStart follows.
        "SessionEnd" => match field("reason") {
            "clear" | "resume" => None,
            _ => Some("offline"),
        },
        _ => None,
    }
}

/// DEX-28: how long the tool that just started may legitimately stay silent (no hook fires until
/// its PostToolUse). Only Bash declares a bound: its `timeout` (Claude Code default 2 min, max 10),
/// after which Claude Code kills it; a background run returns at once. Other tools: None.
fn tool_grace_ms(event: &str, body: &Value) -> Option<u64> {
    if event != "PreToolUse" || body["tool_name"] != "Bash" || body["tool_input"]["run_in_background"] == true {
        return None;
    }
    Some(body["tool_input"]["timeout"].as_u64().unwrap_or(120_000).min(600_000))
}

fn serve(mut s: TcpStream, core: std::sync::Arc<Core>, seq: u64, deadline: crate::local_http::Deadline) {
    let token = core.hooks.token.as_str();
    if s.set_write_timeout(Some(crate::local_http::REQUEST_TIMEOUT)).is_err() { return; }
    // Same port + token serve the Dex MCP endpoint (DEX-4).
    match crate::mcp::is_mcp(&s, deadline) {
        Ok(true) => return crate::mcp::serve(s, &core, &core.mcp, token, deadline),
        Ok(false) => {},
        Err(code) => {
            let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    }
    let code = match read_request(&mut s, token, deadline) {
        Ok((agent, key, body)) => {
            if !core.mcp.verifies(&agent, &key) {
                let _ = write!(s, "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                return;
            }
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let event = body.get("hook_event_name").and_then(Value::as_str).unwrap_or("").to_string();
            core.observations.hook(&agent, seq, &event, false, core.agent_states.get(&agent).status);
            let session_id = body.get("session_id").and_then(Value::as_str).map(String::from);
            let process = core.ptys.generation(&agent);
            let stop = core.claude_hook_received(&agent, &event, session_id.as_deref(), seq);
            if let Some(path) = body["transcript_path"].as_str() {
                core.agent_states.set_transcript(&agent, path);
            }
            let memory = (event == "SessionStart")
                .then(|| core.hooks.memory.lock().unwrap().get(&agent).cloned())
                .flatten();
            let roster = (event == "SessionStart")
                .then(|| team_roster_block(&core, &agent))
                .flatten();
            // The PTY registration gates the index; the saved session context picks the scopes and
            // the registered root is the last fallback (slug folder, #415/#475, in session_context_index).
            let context = memory.map(|root| session_context_index(&core, &agent, &root));
            // DEX-19: results that finished while the leader was closed join the memory index.
            let pending = (event == "SessionStart")
                .then(|| crate::delegate::on_session_start(&core.db, &core.events, &agent))
                .flatten();
            // DEX-24: worker questions that came while the leader was closed join too.
            let asked = (event == "SessionStart")
                .then(|| crate::questions::on_session_start(&core.db, &core.events, &agent))
                .flatten();
            let pending = match (pending, asked) {
                (Some(p), Some(q)) => Some(format!("{p}
{q}")),
                (p, q) => p.or(q),
            };
            let context = match (context, pending) {
                (Some(m), Some(p)) => Some(format!("{m}\n{p}")),
                (m, p) => m.or(p),
            };
            // Kadro bloğu hafıza indeksinin ÖNÜNE gelir (indeksin 8 KB sınırına sayılmaz).
            let context = match (roster, context) {
                (Some(r), Some(c)) => Some(format!("{r}\n{c}")),
                (r, c) => r.or(c),
            };
            // Delegations (DEX-4): Stop delivers queued work, UserPromptSubmit tells the leader what finished.
            // DEX-17: StopFailure önce zincir kararını verir — geçiş olacaksa devretme `failed`
            // OLMAZ (aynı iş sürer). Karar spawn içermez (DB + kasa okuma), yanıtı geciktirmez.
            let chain = (event == "StopFailure").then(|| core.chain_plan(&agent, &body)).flatten();
            let delegated = core.delegation_hook_prepared(&agent, &event, &body, chain.is_some(), stop.as_ref());
            // DEX-13: an API error ended the turn → plain-Turkish warning for the pane and hover card.
            if event == "StopFailure" {
                crate::providers::on_stop_failure(&core.db, &core.events, &agent, &body);
            }
            // A Stop answered with `decision: block` continues the turn: it is not idle (DEX-19).
            let blocked = event == "Stop"
                && delegated
                    .as_deref()
                    .and_then(|o| serde_json::from_str::<Value>(o).ok())
                    .is_some_and(|v| v["decision"] == "block");
            // Görev #23: a permission request Dex answered never waits on the patron.
            let answered = event == "PermissionRequest" && delegated.is_some();
            let status = if blocked || answered { Some("working") } else { status_for(&event, &body) };
            let state = if let Some(stop) = stop.as_ref() {
                core.claude_stop_status(&agent, stop, status.and_then(crate::agents::Status::from_hook).unwrap_or(crate::agents::Status::Idle))
            } else {
                core.agent_states.hook_seq(&agent, status.and_then(crate::agents::Status::from_hook), session_id.as_deref(), seq)
            };
            let deferred_session = session_id.clone();
            let chain_engine = chain.as_ref().map(|_| "claude");
            let current_stop = if event == "Stop" && session_id.is_some() && state.engine.as_deref() != Some("codex") {
                stop.as_ref().is_some_and(|stop| core.claude_stop_is_current(&agent, stop))
            } else { true };
            let status = status.map(|_| match state.status {
                crate::agents::Status::Offline => "offline",
                crate::agents::Status::Idle => "idle",
                crate::agents::Status::Working => "working",
                crate::agents::Status::Waiting => "waiting",
            });
            // #447: after the status: a restart's look sees one or the other.
            if event != "SessionStart" {
                core.agent_states.prompt_seen(&agent);
            }
            core.observations.hook(&agent, seq, &event, true, state.status);
            let tool_grace_ms = tool_grace_ms(&event, &body);
            // FIX-1: dex-hook first — the webview notifies on a change *to* waiting, and agent-status
            // applied before it would already read "waiting" (no desktop notification ever).
            let (agent_id, event_name) = (agent.clone(), event.clone());
            let _ = core.events.emit("dex-hook", &HookEvent { status, agent_id: agent, event, session_id, tool_grace_ms });
            let _ = core.events.emit("agent-status", &state);
            // #242: agent.step. The answer (permit, budget, queue) is final above; this only reads the
            // body, swallows any failure and cannot change what the hook replies.
            let denied = event_name == "PreToolUse"
                && delegated
                    .as_deref()
                    .and_then(|o| serde_json::from_str::<Value>(o).ok())
                    .is_some_and(|v| v["hookSpecificOutput"]["permissionDecision"] == "deny");
            if current_stop {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| emit_step(&core, &agent_id, &event_name, &body, denied)));
            }
            // Reevaluate an expired leader after a final idle transition, including transient
            // StopFailure. A chain transition owns its session instead; workers still need Stop.
            // Codex Stop is final; Claude waits for its turn-matched transcript evidence.
            if event_name == "Stop" && state.engine.as_deref() == Some("codex") && !blocked {
                let _ = core.team_turn_ended(&state);
            }
            if chain.is_none() && state.status == crate::agents::Status::Idle
                && matches!(event_name.as_str(), "Stop" | "StopFailure")
            {
                let _ = core.events.emit("agent-idle", &agent_id);
            }
            if chain.is_none() && state.status == crate::agents::Status::Idle && current_stop {
                core.inbox_wake(&agent_id);
            }
            // FIX-3: UserPromptSubmit arms the first-reply watch, any later event ends it.
            if current_stop { core.first_reply_on_hook(&agent_id, &event_name); }
            // Finish the response before usage ingestion, context refresh or watch setup.
            let reply = delegated.or_else(|| context.map(|ctx| json!({ "hookSpecificOutput": {
                "hookEventName": "SessionStart", "additionalContext": ctx
            }}).to_string()));
            let written = if let Some(out) = reply {
                write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len())
            } else {
                write!(s, "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            };
            let _ = s.shutdown(std::net::Shutdown::Write);
            drop(s);
            crate::diagnostics::event("leader_delivery", json!({"agent":agent_id, "seq":seq,
                "reason":if written.is_ok() { "hook_reply" } else { "write_error" }}));
            if let Some(session) = deferred_session.as_deref().filter(|session|
                core.agent_states.get(&agent_id).session_id.as_deref() == Some(*session)
                    && core.ptys.generation(&agent_id) == process)
            {
                core.session_on_hook(&agent_id, &event_name, session,
                    body["transcript_path"].as_str(), chain_engine);
            }
            // DEX-17: transition only after the source session/transcript is durably recorded.
            // Spawn remains background work, so the hook response is not held by PTY startup.
            if let Some(plan) = chain {
                let (core, agent) = (core.clone(), agent_id.clone());
                std::thread::spawn(move || core.chain_execute(&agent, plan));
            }
            // A timeout may have ended the turn while the response was being prepared.
            // Wake again after releasing its payload, so no row waits for patron input.
            if event_name == "Stop" && core.agent_states.get(&agent_id).status == crate::agents::Status::Idle {
                let _ = core.events.emit("agent-idle", &agent_id);
            }
            return;
        }
        Err(code) => code,
    };
    let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

/// #242: `agent.step` for a tool hook (see [`crate::step`]). Emitted for every agent — leader or
/// worker — the limiter keeps it to a few `start`s a second each. A turn that ends leaves no step
/// open ([`crate::step::Limiter::forget`]).
fn emit_step(core: &Core, agent: &str, event: &str, body: &Value, denied: bool) {
    if matches!(event, "Stop" | "StopFailure" | "SessionEnd") {
        core.hooks.steps.forget(agent);
        return;
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    if let Some(step) = crate::step::on_hook(&core.hooks.steps, agent, event, body, denied, std::time::Instant::now(), ts) {
        let _ = core.events.emit(crate::step::EVENT, &step);
    }
}

const INDEX_LIMIT: usize = 8 * 1024;
const MEMORY_HYGIENE_THRESHOLD: usize = 100;

/// "Sana özel" bölümünün garantisi: ortak bölüm tek başına sınırı dolduramaz.
/// Bir bölüm kotasını kullanmazsa artanı diğeri alır (boşa harcanmaz).
const INDEX_PRIVATE_MIN: usize = 3 * 1024;

/// Kadro bloğunun kendi sınırı; hafıza indeksinin 8 KB sınırına sayılmaz, onu kesmez.
const ROSTER_LIMIT: usize = 2 * 1024;

/// YALNIZ LİDER SessionStart'ı için "# Takım kadrosu" bloğu: oturum anındaki DB'den
/// okunur (yeni üye bir sonraki oturumda görünür). Worker/bilinmeyen ajana None.
/// Satır başına: ad (id) — rol/unvan — motor/model — açık/kapalı. Sır içermez.
fn team_roster_block(core: &Core, agent: &str) -> Option<String> {
    let rows: Vec<crate::status::RosterEntry> = crate::db::with(&core.db, |conn| {
        let c = match crate::delegate::caller(conn, agent) {
            Ok(c) if c.is_leader() => c,
            _ => return Err(String::new()),
        };
        crate::status::team_roster(conn, Some(&c.team_id))
    })
    .ok()?;
    let mut out = String::from("# Takım kadrosu\n");
    let mut dropped = 0;
    for a in &rows {
        let role = match &a.role_slug {
            Some(s) => format!("{s}/{}", a.title),
            None => a.title.clone(),
        };
        let state = if matches!(
            core.agent_states.get(&a.id).status,
            crate::agents::Status::Offline
        ) {
            "kapalı"
        } else {
            "açık"
        };
        let line = format!(
            "- {} ({}) — {} — {}/{} — {}\n",
            a.name, a.id, role, a.engine, a.model, state
        );
        if out.len() + line.len() > ROSTER_LIMIT {
            dropped += 1;
        } else {
            out.push_str(&line);
        }
    }
    if dropped > 0 {
        out.push_str(&format!("… {dropped} üye sığmadı; dex_status ile bak.\n"));
    }
    if let Ok(projects) = crate::db::with(&core.db, |c| {
        let caller = crate::delegate::caller(c, agent)?;
        crate::rules::project_list_block(c, &caller.team_id, 2048)
    }) {
        out.push_str(&projects);
    }
    Some(out)
}

/// SessionStart index under the PTY-registered `registered` root. Saved session context first;
/// if that fails (no saved row with several bound projects, archived/deleted saved project),
/// the project-less team context; if that fails too, the legacy index of `registered`. Only the
/// context and the agent's own folder are resolved under the DB mutex; files are read outside it.
pub fn session_context_index(core: &Core, agent: &str, registered: &Path) -> String {
    enum Plan {
        Scoped(Box<IndexPlan>),
        Legacy(String),
    }
    let plan = crate::db::with(&core.db, |conn| {
        let scoped = session_context(conn, &core.paths.data_dir, agent, false)
            .map_err(|e| e.code())
            .and_then(|ctx| index_plan(conn, ctx).map_err(|_| "index plan"));
        Ok(match scoped {
            Ok(plan) => Plan::Scoped(Box::new(plan)),
            Err(why) => {
                log::debug!("SessionStart index: team context failed ({why}); registered root");
                Plan::Legacy(crate::slug::memory_name(conn, registered, agent).unwrap_or_else(|_| crate::slug::of(agent)))
            }
        })
    });
    match plan {
        Ok(Plan::Scoped(plan)) => render_index(&plan, INDEX_LIMIT),
        Ok(Plan::Legacy(own)) => memory_index(registered, &own, INDEX_LIMIT),
        Err(_) => {
            log::debug!("SessionStart index: db unavailable; registered root");
            memory_index(registered, &crate::slug::of(agent), INDEX_LIMIT)
        }
    }
}

/// #838 G: the agent session's context — its saved launch binding (agents.rs writes it before the
/// CLI starts), the spawn and SessionStart alike. `strict` (a bound session): its resolver error
/// stands (fail-closed); otherwise the project-less team context is the fallback (E, #866).
pub(crate) fn session_context(
    conn: &rusqlite::Connection,
    data_dir: &Path,
    agent: &str,
    strict: bool,
) -> Result<crate::project_context::ProjectContext, crate::projects::ProjectError> {
    use crate::project_context::{resolve, ContextMode, ResolveRequest};
    let caller = crate::delegate::caller(conn, agent).map_err(|_| crate::projects::ProjectError::NotFound)?;
    let at = |mode| {
        let request = ResolveRequest { team_id: &caller.team_id, agent_id: Some(agent), delegation_id: None, project_id: None, mode };
        resolve(conn, data_dir, request)
    };
    at(ContextMode::Session).or_else(|e| {
        if strict {
            return Err(e);
        }
        log::debug!("session context {}; team context", e.code());
        at(ContextMode::Team)
    })
}

/// #838 G: an agent's memory folders from its context's anchors (one choice for the spawn's folders,
/// the engine's grant and the worker's permit). Write: team shared, its own private folder (slug, or
/// the old id folder while its move is pending) and, in its own team's project, the project shared.
/// Read: those anchors' roots, as before. A service never gets the requesting project's memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryScope {
    /// The PTY registration (SessionStart's gate and last fallback) and `DEX_MEMORY_DIR`.
    pub root: PathBuf,
    pub write: Vec<PathBuf>,
    pub read: Vec<PathBuf>,
}

pub(crate) fn memory_scope(conn: &rusqlite::Connection, ctx: &crate::project_context::ProjectContext) -> Result<MemoryScope, String> {
    let agent = ctx.agent_id.as_deref().ok_or("hafıza için ajan gerekli")?;
    let project = ctx.project_memory_root.as_ref().filter(|_| ctx.project_team_id.as_deref().is_none_or(|t| t == ctx.team_id));
    let mut scope = MemoryScope { root: ctx.agent_memory_root.clone(), ..Default::default() };
    let own = crate::slug::memory_dir(conn, &ctx.agent_memory_root, agent)?;
    for dir in [Some(ctx.team_memory_root.join("shared")), Some(own), project.map(|p| p.join("shared"))].into_iter().flatten() {
        if !scope.write.iter().any(|d| crate::memory::same_dir(d, &dir)) {
            scope.write.push(dir);
        }
    }
    for dir in [Some(&ctx.team_memory_root), Some(&ctx.agent_memory_root), project].into_iter().flatten() {
        if !scope.read.iter().any(|d| crate::memory::same_dir(d, dir)) {
            scope.read.push(dir.clone());
        }
    }
    Ok(scope)
}

/// Index derived from each memory file's frontmatter (no hand-kept MEMORY.md), newest first
/// (equal timestamps use descending filename order),
/// team-shared before agent-specific. Both sections share `limit`: "Sana özel" is guaranteed
/// [`INDEX_PRIVATE_MIN`], the rest goes to whichever section needs it (a section that uses less
/// leaves the remainder to the other). Each section notes its own dropped count.
pub fn memory_index(root: &Path, agent: &str, limit: usize) -> String {
    let sections = [("Takım ortak", root.join("shared")), ("Sana özel", root.join("agents").join(agent))];
    legacy_index(root, &sections, limit)
}

fn legacy_index(root: &Path, sections: &[(&str, PathBuf)], limit: usize) -> String {
    let root_canon = root.canonicalize().ok();
    let entries: Vec<(String, Vec<String>)> = sections
        .iter()
        .map(|(title, dir)| {
            let lines = root_canon.as_deref().map(|rc| memory_entries(rc, dir)).unwrap_or_default();
            (format!("\n## {title} — {}\n", dir.display()), lines)
        })
        .collect();
    legacy_index_entries(&entries, limit)
}

fn legacy_index_entries(entries: &[(String, Vec<String>)], limit: usize) -> String {
    let head = String::from("# Dex hafıza indeksi\nAyrıntı gerekirse dosyayı oku; kurallar sistem isteminde.\n");
    // Fixed overhead: header + section titles + "(boş)" markers. What remains is the entry budget.
    let fixed = head.len() + entries.iter().map(|(h, l)| h.len() + if l.is_empty() { "(boş)\n".len() } else { 0 }).sum::<usize>();
    let budget = limit.saturating_sub(fixed);
    let need: Vec<usize> = entries.iter().map(|(_, l)| l.iter().map(String::len).sum()).collect();
    // Private first so its guarantee holds no matter how full shared is; shared takes the rest.
    let quota_p = need[1].min(INDEX_PRIVATE_MIN.min(budget).max(budget / 2));
    let used_p: usize = fit(&entries[1].1, quota_p).iter().map(|l| l.len()).sum();
    let quota_s = need[0].min(budget.saturating_sub(used_p));
    let quotas = [quota_s, quota_p];
    let mut out = head;
    for ((header, lines), quota) in entries.iter().zip(quotas) {
        out.push_str(header);
        if lines.is_empty() {
            out.push_str("(boş)\n");
            continue;
        }
        let shown = fit(lines, quota);
        for line in &shown {
            out.push_str(line);
        }
        let dropped = lines.len() - shown.len();
        if dropped > 0 {
            out.push_str(&dropped_note(dropped));
        }
    }
    out
}

/// DB part of a context index: the context plus what only the DB knows.
struct IndexPlan {
    ctx: crate::project_context::ProjectContext,
    own: PathBuf,
    single: bool,
    leader: bool,
    search_scope: crate::memory::SearchScope,
    task_query: Option<String>,
}

fn index_plan(conn: &rusqlite::Connection, ctx: crate::project_context::ProjectContext) -> Result<IndexPlan, String> {
    use rusqlite::OptionalExtension;
    let agent = ctx.agent_id.as_deref().ok_or("hafıza için ajan gerekli")?;
    let search_scope = crate::memory::SearchScope::for_context(conn, &ctx)?;
    let own = search_scope.own.clone();
    let leader = crate::delegate::caller(conn, agent)?.is_leader();
    let task_query = if leader {
        None
    } else if let Some(id) = ctx.delegation_id {
        conn.query_row(
            "SELECT COALESCE(t.title,'') || char(10) || COALESCE(d.patron_words,'') || char(10) || d.prompt
             FROM delegations d LEFT JOIN tasks t ON t.id=d.task_id
             WHERE d.id=?1 AND d.to_agent=?2 AND COALESCE(d.service_team_id,d.team_id)=?3 AND d.status IN ('queued','running')",
            rusqlite::params![id, agent, ctx.team_id], |r| r.get(0),
        ).optional().map_err(|e| e.to_string())?
    } else {
        None
    };
    let single = crate::projects::list(conn, &ctx.team_id, false).map_err(|e| e.to_string())?.iter().filter(|p| p.root_path.is_some()).count() <= 1;
    Ok(IndexPlan { ctx, own, single, leader, search_scope, task_query })
}

/// G supplies the trusted launch/session context; requesting project memory is excluded for services.
pub fn memory_index_for_context(conn: &rusqlite::Connection, ctx: &crate::project_context::ProjectContext, limit: usize) -> Result<String, String> {
    Ok(render_index(&index_plan(conn, ctx.clone())?, limit))
}

/// Filesystem part of a context index; needs no DB.
fn render_index(plan: &IndexPlan, limit: usize) -> String {
    let IndexPlan { ctx, own, single, leader, search_scope, task_query } = plan;
    let limit = limit.min(INDEX_LIMIT);
    let ranks: HashMap<_, _> = task_query.as_deref().map(|q| crate::memory::search(search_scope, q, usize::MAX))
        .unwrap_or_default().into_iter().enumerate().map(|(i, hit)| (hit.path, i)).collect();
    let team = ctx.team_memory_root.join("shared");
    let project = ctx
        .project_memory_root
        .as_ref()
        .filter(|_| ctx.project_team_id.as_deref().is_none_or(|t| t == ctx.team_id))
        .map(|r| r.join("shared"))
        .filter(|p| !crate::memory::same_dir(p, &team));
    let load = |dir: &Path| {
        let root = if dir == own { Some(ctx.agent_memory_root.as_path()) } else { dir.parent() };
        root.and_then(|r| r.canonicalize().ok())
            .map(|rc| memory_entries_ranked(&rc, dir, (dir == own).then_some(ctx.project_id.as_deref()), &ranks))
            .unwrap_or_default()
    };
    let team_lines = load(&team);
    let warning = if *leader && team_lines.len() > MEMORY_HYGIENE_THRESHOLD {
        format!("⚠ Takım ortak hafıza {} kayıt (eşik {MEMORY_HYGIENE_THRESHOLD}): hijyen işi aç\n", team_lines.len())
    } else {
        String::new()
    };
    let warning = if warning.len() <= limit { warning } else { String::new() };
    let limit = limit - warning.len();
    let legacy = *single && (project.is_none() || team_lines.is_empty());
    let mut sections = Vec::new();
    if legacy {
        let shared = project.as_ref().unwrap_or(&team);
        let entries =
            [(format!("\n## Takım ortak — {}\n", shared.display()), load(shared)), (format!("\n## Sana özel — {}\n", own.display()), load(own))];
        // Reuse the old two-section allocation and visibility, including the id fallback.
        let old = legacy_index_entries(&entries, limit);
        if old.len() <= limit {
            let mut old = if project.is_some() { old.replacen("\n## Takım ortak", "\n## Proje ortak", 1) } else { old };
            old.push_str(&warning);
            return old;
        }
        sections.push((if project.is_some() { "Proje ortak" } else { "Takım ortak" }, shared.clone(), 2 * 1024));
    } else {
        if let Some(p) = project {
            sections.push(("Proje ortak", p, 3 * 1024));
        }
        sections.push(("Takım ortak", team, 2 * 1024));
    }
    sections.push(("Sana özel", own.clone(), 1024));
    let entries: Vec<_> = sections.iter().map(|(title, dir, weight)| (format!("\n## {title} — {}\n", dir.display()), load(dir), *weight)).collect();
    let mut index = bounded_index(&entries, limit, legacy);
    index.push_str(&warning);
    index
}

fn dropped_note(count: usize) -> String {
    format!("… {count} kayıt daha sığmadı; memory_search ile ara.\n")
}

/// Reserve titles and worst-case drop notes before distributing entry bytes. Never split a line.
fn bounded_index(entries: &[(String, Vec<String>, usize)], limit: usize, legacy: bool) -> String {
    let mut out = String::from("# Dex hafıza indeksi\nAyrıntı gerekirse dosyayı oku; kurallar sistem isteminde.\n");
    let overhead =
        out.len() + entries.iter().map(|(h, l, _)| h.len() + if l.is_empty() { "(boş)\n".len() } else { dropped_note(l.len()).len() }).sum::<usize>();
    if overhead > limit {
        let header = "# Dex hafıza indeksi\n";
        return if header.len() <= limit { header.into() } else { String::new() };
    }
    let budget = limit - overhead;
    let mut quotas = vec![0; entries.len()];
    let mut remaining = budget;
    if legacy {
        let private = entries.len() - 1;
        quotas[private] = INDEX_PRIVATE_MIN.min(budget).max(budget / 2);
        remaining -= quotas[private];
        quotas[0] = remaining;
    } else {
        for (i, (_, lines, weight)) in entries.iter().enumerate() {
            let need: usize = lines.iter().map(String::len).sum();
            quotas[i] = need.min(*weight).min(remaining);
            remaining -= quotas[i];
        }
    }
    // Reclaim unused entry slots, then offer the pool in project/team/private order.
    let mut selected: Vec<Vec<bool>> = entries
        .iter()
        .zip(&quotas)
        .map(|((_, lines, _), &quota)| {
            let mut left = quota;
            lines
                .iter()
                .map(|line| {
                    if line.len() <= left {
                        left -= line.len();
                        true
                    } else {
                        false
                    }
                })
                .collect()
        })
        .collect();
    let used: usize = entries
        .iter()
        .zip(&selected)
        .map(|((_, lines, _), flags)| lines.iter().zip(flags).filter(|(_, yes)| **yes).map(|(line, _)| line.len()).sum::<usize>())
        .sum();
    remaining = budget - used;
    for ((_, lines, _), flags) in entries.iter().zip(&mut selected) {
        for (line, yes) in lines.iter().zip(flags) {
            if !*yes && line.len() <= remaining {
                *yes = true;
                remaining -= line.len();
            }
        }
    }
    for ((header, lines, _), flags) in entries.iter().zip(selected) {
        out.push_str(header);
        if lines.is_empty() {
            out.push_str("(boş)\n");
            continue;
        }
        let mut shown = 0;
        for (line, yes) in lines.iter().zip(flags) {
            if yes {
                out.push_str(line);
                shown += 1;
            }
        }
        if shown < lines.len() {
            out.push_str(&dropped_note(lines.len() - shown));
        }
    }
    out
}

/// Lines from `lines` that fit in `quota` (order preserved).
fn fit(lines: &[String], quota: usize) -> Vec<&String> {
    let mut used = 0;
    lines
        .iter()
        .filter(|l| {
            if used + l.len() <= quota {
                used += l.len();
                true
            } else {
                false
            }
        })
        .collect()
}

fn memory_entries(root_canon: &Path, dir: &Path) -> Vec<String> {
    memory_entries_filtered(root_canon, dir, None)
}

fn memory_entries_filtered(root_canon: &Path, dir: &Path, project: Option<Option<&str>>) -> Vec<String> {
    memory_entries_ranked(root_canon, dir, project, &HashMap::new())
}

fn memory_entries_ranked(root_canon: &Path, dir: &Path, project: Option<Option<&str>>, ranks: &HashMap<PathBuf, usize>) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .filter(|p| crate::memory::inside(root_canon, p))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("MEMORY.md")))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by(|(ta, pa), (tb, pb)| {
        ranks.get(pa).copied().unwrap_or(usize::MAX).cmp(&ranks.get(pb).copied().unwrap_or(usize::MAX))
            .then_with(|| tb.cmp(ta))
            .then_with(|| pb.cmp(pa))
    });
    files
        .into_iter()
        .filter_map(|(_, p)| {
            let mut text = String::new();
            let _ = std::fs::File::open(&p).map(|f| f.take(8 * 1024).read_to_string(&mut text));
            if project.is_some_and(|project| !crate::memory::visible_in_project(&text, project)) {
                return None;
            }
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            Some(match frontmatter(&text) {
                Some((name, desc, kind)) => {
                    let name = if name.is_empty() { stem } else { name };
                    let kind = kind.map(|k| format!(" [{k}]")).unwrap_or_default();
                    let desc = if desc.is_empty() { "⚠ description eksik".into() } else { desc };
                    format!("- {name}{kind} — {desc} — {}\n", p.display())
                }
                None => format!("- {stem} — ⚠ frontmatter eksik — {}\n", p.display()),
            })
        })
        .collect()
}

/// (name, description, metadata.type) from a leading `---` block; a flat line scan, no YAML dep.
fn frontmatter(text: &str) -> Option<(String, String, Option<String>)> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let (mut name, mut desc, mut kind, mut in_meta) = (String::new(), String::new(), None, false);
    let clean = |v: &str| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    for line in lines {
        if line.trim_end() == "---" {
            return Some((name, desc, kind));
        }
        let indented = line.starts_with([' ', '\t']);
        let Some((k, v)) = line.split_once(':') else { continue };
        match (indented, k.trim()) {
            (false, "name") => name = clean(v),
            (false, "description") => desc = clean(v),
            (false, "metadata") => in_meta = true,
            (false, _) => in_meta = false,
            (true, "type") if in_meta => kind = Some(clean(v)).filter(|k| !k.is_empty()),
            _ => {}
        }
    }
    None
}

fn read_request(s: &mut TcpStream, token: &str, deadline: crate::local_http::Deadline) -> Result<(String, String, Vec<u8>), &'static str> {
    let (mut buf, head_end) = crate::local_http::read_head(s, deadline)?;
    let mut chunk = [0u8; 8192];

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    if !lines.next().is_some_and(|l| l.starts_with("POST /hook ")) {
        return Err("404 Not Found");
    }
    let (mut len, mut auth, mut agent, mut key) = (0usize, "", "", "");
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.parse().map_err(|_| "400 Bad Request")?,
                "authorization" => auth = v,
                "x-dex-agent" => agent = v,
                "x-dex-key" => key = v,
                _ => {}
            }
        }
    }
    if !eq_ct(auth.strip_prefix("Bearer ").unwrap_or("").as_bytes(), token.as_bytes()) {
        return Err("401 Unauthorized");
    }
    if agent.is_empty() {
        return Err("400 Bad Request");
    }
    if len > MAX_BODY {
        return Err("413 Payload Too Large");
    }

    let mut body = buf.split_off(head_end);
    while body.len() < len {
        let n = deadline.read(s, &mut chunk)?;
        if n == 0 {
            return Err("400 Bad Request");
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok((agent.to_string(), key.to_string(), body))
}

fn eq_ct(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn connection_slots_enforce_limit_and_recover_on_drop() {
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut slots: Vec<_> = (0..MAX_CONNECTIONS).map(|_| ConnectionSlot::take(&active).unwrap()).collect();
        assert!(ConnectionSlot::take(&active).is_none());
        slots.pop();
        assert!(ConnectionSlot::take(&active).is_some());
        drop(slots);
        assert_eq!(active.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
    #[test]
    fn idle_connection_is_closed_before_request_timeout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("idle-hook-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(7))).unwrap();
        let result = client.read(&mut [0; 256]);
        core.shutdown();
        assert!(matches!(result, Ok(_)), "idle peer must receive a response or EOF: {result:?}");
    }

    #[test]
    fn hook_reply_precedes_slow_context_refresh() {
        struct Slow;
        impl crate::events::EventSink for Slow {
            fn emit(&self, event: &str, _: serde_json::Value) -> Result<(), String> {
                if event == "agent-context" { std::thread::sleep(std::time::Duration::from_millis(5_500)); }
                Ok(())
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.dex/tmp/hook-slow-context-{}", std::process::id()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Slow),
        ).unwrap();
        let reply = ureq::post(&format!("http://127.0.0.1:{}/hook", core.hooks.port))
            .timeout(std::time::Duration::from_secs(1))
            .set("Authorization", &format!("Bearer {}", core.hooks.token))
            .set("X-Dex-Agent", "timur").set("X-Dex-Key", &core.mcp.key_for("timur"))
            .send_json(serde_json::json!({"hook_event_name":"Stop", "session_id":"slow-context"}));
        core.shutdown();
        assert!(reply.is_ok(), "context refresh must not hold the hook reply: {reply:?}");
    }
    #[test]
    fn stop_result_is_pending_until_cli_continues() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.dex/tmp/stop-result-pending-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        ).unwrap();
        core.agent_states.hook("timur", Some(crate::agents::Status::Working), None);
        let id = crate::db::with(&core.db, |c| {
            let d = crate::delegate::insert(c, crate::delegate::NewDelegation {
                from: &crate::delegate::caller(c, "timur")?, to_agent: "kaan".into(),
                team_id: "default".into(), service_team_id: None, prompt: "work".into(),
                task_id: None, patron_words: None, effort: None,
            })?;
            crate::delegate::set_status(c, d.id, "done")?;
            Ok(d.id)
        }).unwrap();
        let out = core.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["decision"], "block");
        let d = crate::db::with(&core.db, |c| crate::delegate::get(c, id)).unwrap();
        assert_eq!(d.delivered_at, None, "producing a block is not CLI acceptance");
        core.shutdown();
    }
    use super::*;

    #[test]
    fn delegated_index_ranks_task_matches_before_newest_within_budget() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let mut ctx = crate::project_context::resolve(&c, &f.0, crate::project_context::ResolveRequest {
            team_id: "default", agent_id: Some("ayla"), delegation_id: None, project_id: None,
            mode: crate::project_context::ContextMode::Team,
        }).unwrap();
        ctx.team_memory_root = f.0.join("team");
        ctx.agent_memory_root = ctx.team_memory_root.clone();
        ctx.project_memory_root = Some(f.0.join("project"));
        let own = crate::slug::memory_dir(&c, &ctx.agent_memory_root, "ayla").unwrap();
        for dir in [ctx.team_memory_root.join("shared"), ctx.project_memory_root.as_ref().unwrap().join("shared"), own] {
            put(&dir.join("old.md"), "---\nname: sqlite\ndescription: migration trap\n---\n");
            std::thread::sleep(Duration::from_millis(20));
            for i in 0..50 {
                put(&dir.join(format!("new-{i}.md")), &format!("---\nname: newest-{i}\ndescription: {}\n---\n", "unrelated ".repeat(12)));
            }
        }
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(1,'timur','ayla','default','sqlite migration','running',1,1)", []).unwrap();
        let plain = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        ctx.delegation_id = Some(1);
        let index = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(index.len() <= INDEX_LIMIT);
        assert_eq!(index.matches("- sqlite").count(), 3, "{index}");
        for section in index.split("\n## ").skip(1) {
            assert!(section.lines().find(|l| l.starts_with("- ")).unwrap().starts_with("- sqlite"));
        }
        assert!(index.contains("memory_search ile ara") && index.contains("- newest-"));
        assert_ne!(index, plain);
        ctx.delegation_id = None;
        assert_eq!(memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap(), plain);
    }

    #[test]
    fn hygiene_warning_only_for_leader_above_one_hundred_team_records() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let mut ctx = crate::project_context::resolve(&c, &f.0, crate::project_context::ResolveRequest {
            team_id: "default", agent_id: Some("timur"), delegation_id: None, project_id: None,
            mode: crate::project_context::ContextMode::Team,
        }).unwrap();
        ctx.team_memory_root = f.0.join("team");
        ctx.agent_memory_root = ctx.team_memory_root.clone();
        ctx.project_memory_root = None;
        for i in 0..100 {
            put(&ctx.team_memory_root.join(format!("shared/n{i}.md")), "---\nname: fixture\ndescription: hygiene fixture\n---\n");
        }
        let at_threshold = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(!at_threshold.contains("hijyen işi aç"));
        put(&ctx.team_memory_root.join("shared/over.md"), "body without frontmatter");
        let lead = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(lead.contains("⚠ Takım ortak hafıza 101 kayıt (eşik 100): hijyen işi aç"), "{lead}");
        assert!(lead.len() <= INDEX_LIMIT);
        ctx.agent_id = Some("ayla".into());
        assert!(!memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap().contains("hijyen işi aç"));
        ctx.agent_id = Some("timur".into());
        for limit in [0, 20, 128, 512, INDEX_LIMIT] {
            assert!(memory_index_for_context(&c, &ctx, limit).unwrap().len() <= limit);
        }
    }

    #[test]
    fn scoped_index_total_budget_and_legacy_visibility() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let mut ctx = crate::project_context::resolve(
            &c,
            &f.0,
            crate::project_context::ResolveRequest {
                team_id: "default",
                agent_id: Some("ayla"),
                delegation_id: None,
                project_id: None,
                mode: crate::project_context::ContextMode::Team,
            },
        )
        .unwrap();
        ctx.team_memory_root = f.0.join("team");
        ctx.agent_memory_root = f.0.join("team");
        ctx.project_memory_root = Some(ctx.team_memory_root.clone());
        let own = crate::slug::memory_dir(&c, &ctx.agent_memory_root, "ayla").unwrap();
        put(&ctx.team_memory_root.join("shared/old.md"), "---\nname: old\ndescription: old fixture\n---\n");
        put(&own.join("habit.md"), "---\nname: habit\ndescription: private fixture\n---\n");
        let legacy = memory_index(&ctx.team_memory_root, "ayla", INDEX_LIMIT);
        let scoped = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert_eq!(legacy, scoped, "same legacy entries and allocation");
        ctx.project_memory_root = Some(f.0.join("project"));
        for dir in [ctx.team_memory_root.join("shared"), ctx.project_memory_root.as_ref().unwrap().join("shared"), own.clone()] {
            for i in 0..70 {
                put(&dir.join(format!("n{i}.md")), &format!("---\nname: n{i}\ndescription: {}\n---\n", "üğ".repeat(50)));
            }
        }
        let scoped = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(scoped.len() <= INDEX_LIMIT, "{} bytes", scoped.len());
        assert!(scoped.contains("Proje ortak") && scoped.contains("Takım ortak") && scoped.contains("Sana özel"));
        assert_eq!(scoped.matches("kayıt daha sığmadı").count(), 3);
        ctx.project_memory_root = Some(ctx.team_memory_root.clone());
        let dedup = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(!dedup.contains("Proje ortak"));
        ctx.project_team_id = Some("requester".into());
        ctx.project_memory_root = Some(f.0.join("project"));
        let service = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(!service.contains("Proje ortak") && !service.contains(&f.0.join("project").display().to_string()));
        for name in ["one", "two"] {
            let root = f.dir(name);
            let prepared = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &prepared).unwrap();
        }
        ctx.project_memory_root = None;
        ctx.project_team_id = None;
        let multi_team = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        let private = &multi_team[multi_team.find("## Sana özel").unwrap()..];
        let private_bytes: usize = private.lines().filter(|l| l.starts_with("- ")).map(|l| l.len() + 1).sum();
        assert!(private_bytes < 2048, "unselected multi-project context uses the 1 KiB private share: {private_bytes}");
    }

    #[test]
    fn legacy_split_roots_filter_private_and_reuse_unused_budget() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("db")).unwrap();
        let mut ctx = crate::project_context::resolve(
            &c,
            &f.0,
            crate::project_context::ResolveRequest {
                team_id: "default",
                agent_id: Some("ayla"),
                delegation_id: None,
                project_id: None,
                mode: crate::project_context::ContextMode::Team,
            },
        )
        .unwrap();
        ctx.team_memory_root = f.0.join("team");
        ctx.agent_memory_root = f.0.join("private");
        ctx.project_memory_root = Some(f.0.join("project"));
        ctx.project_id = Some("selected".into());
        let own = ctx.agent_memory_root.join("agents/ayla");
        put(&own.join("habit.md"), "---\nname: habit\ndescription: kept\n---\n");
        put(&own.join("hidden.md"), "---\nname: hidden\ndescription: wrong repo\nmetadata:\n  project: other\n---\n");
        put(&ctx.project_memory_root.as_ref().unwrap().join("shared/one.md"), "---\nname: one\ndescription: kept\n---\n");
        let index = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(index.contains("- habit") && index.contains("- one") && !index.contains("- hidden"), "{index}");
        let old_root = f.0.join("old-view");
        put(&old_root.join("shared/fixture.md"), "---\nname: fixture\ndescription: old shared\n---\n");
        put(&old_root.join("agents/ayla/fixture.md"), "---\nname: private-fixture\ndescription: old private\n---\n");
        c.execute("UPDATE agents SET slug='stable-own' WHERE id='ayla'", []).unwrap();
        let legacy_ctx = crate::project_context::ProjectContext {
            team_memory_root: f.0.join("empty-team"),
            agent_memory_root: old_root.clone(),
            project_memory_root: Some(old_root.clone()),
            ..ctx.clone()
        };
        assert_eq!(
            memory_index_for_context(&c, &legacy_ctx, INDEX_LIMIT).unwrap(),
            memory_index(&old_root, "ayla", INDEX_LIMIT).replacen("\n## Takım ortak", "\n## Proje ortak", 1),
            "empty team share preserves legacy fixture and id fallback"
        );
        c.execute("UPDATE agents SET slug='ayla' WHERE id='ayla'", []).unwrap();
        for i in 0..80 {
            put(
                &ctx.project_memory_root.as_ref().unwrap().join(format!("shared/x{i}.md")),
                &format!("---\nname: x{i}\ndescription: {}\n---\n", "ü".repeat(100)),
            );
        }
        let index = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(index.len() <= INDEX_LIMIT && index.len() > 6 * 1024, "{} bytes", index.len());
        assert!(index.contains("- habit") && !index.contains("- hidden"));
        for limit in [0, 1, 20, 21, 27, 100, 300] {
            assert!(memory_index_for_context(&c, &ctx, limit).unwrap().len() <= limit);
        }
        let outside = f.0.join("outside");
        put(&outside.join("escaped.md"), "---\nname: escaped\ndescription: secret\n---\n");
        std::fs::create_dir_all(&ctx.team_memory_root).unwrap();
        assert!(crate::memory::tests::link_dir(&outside, &ctx.team_memory_root.join("shared")));
        assert!(!memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap().contains("- escaped"));
        #[cfg(windows)]
        std::fs::remove_dir(ctx.team_memory_root.join("shared")).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(ctx.team_memory_root.join("shared")).unwrap();
    }

    #[test]
    fn legacy_overflow_counts_notes_in_exact_budget() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("db")).unwrap();
        let mut ctx = crate::project_context::resolve(
            &c,
            &f.0,
            crate::project_context::ResolveRequest {
                team_id: "default",
                agent_id: Some("ayla"),
                delegation_id: None,
                project_id: None,
                mode: crate::project_context::ContextMode::Team,
            },
        )
        .unwrap();
        ctx.team_memory_root = f.0.join("memory");
        ctx.agent_memory_root = ctx.team_memory_root.clone();
        let shared = ctx.team_memory_root.join("shared");
        let file = shared.join("a.md");
        put(&file, "---\nname: a\ndescription: x\n---\n");
        let row_len = memory_entries(&ctx.team_memory_root.canonicalize().unwrap(), &shared)[0].len();
        let shared_header = format!("\n## Takım ortak — {}\n", shared.display());
        let private_header = format!("\n## Sana özel — {}\n", ctx.agent_memory_root.join("agents/ayla").display());
        let head = "# Dex hafıza indeksi\nAyrıntı gerekirse dosyayı oku; kurallar sistem isteminde.\n";
        let row_budget = INDEX_LIMIT - head.len() - shared_header.len() - private_header.len() - "(boş)\n".len();
        put(&file, &format!("---\nname: a\ndescription: {}\n---\n", "x".repeat(row_budget - row_len + 1)));
        put(&shared.join("b.md"), &format!("---\nname: b\ndescription: {}\n---\n", "y".repeat(row_budget + 1)));
        let old = memory_index(&ctx.team_memory_root, "ayla", INDEX_LIMIT);
        assert!(old.len() > INDEX_LIMIT, "legacy notes were outside the budget");
        let strict = memory_index_for_context(&c, &ctx, INDEX_LIMIT).unwrap();
        assert!(strict.len() <= INDEX_LIMIT);
        assert!(strict.contains("2 kayıt daha sığmadı") && !strict.contains("- a") && !strict.contains("- b"));
    }

    fn put(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn bash_grace_follows_its_timeout() {
        let pre = |input: Value| json!({ "tool_name": "Bash", "tool_input": input });
        assert_eq!(tool_grace_ms("PreToolUse", &pre(json!({ "command": "cargo build" }))), Some(120_000));
        assert_eq!(tool_grace_ms("PreToolUse", &pre(json!({ "timeout": 540000 }))), Some(540_000));
        assert_eq!(tool_grace_ms("PreToolUse", &pre(json!({ "timeout": 9_000_000 }))), Some(600_000));
        assert_eq!(tool_grace_ms("PreToolUse", &pre(json!({ "run_in_background": true }))), None);
        assert_eq!(tool_grace_ms("PostToolUse", &pre(json!({}))), None);
        assert_eq!(tool_grace_ms("PreToolUse", &json!({ "tool_name": "Read", "tool_input": {} })), None);
    }

    #[test]
    fn every_hook_shape_carries_the_agent_key() {
        for fail_closed in [false, true] {
            let settings = settings_json(4242, fail_closed);
            let start = &settings["hooks"]["SessionStart"][0]["hooks"][0];
            assert!(start["args"].as_array().unwrap().iter().any(|v| v == "X-Dex-Key: {{DEX_MCP_KEY}}"));
            for event in ["UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest", "Notification", "Stop", "StopFailure", "SessionEnd"] {
                let hook = &settings["hooks"][event][0]["hooks"][0];
                if fail_closed && event == "PreToolUse" {
                    assert!(hook["command"].as_str().unwrap().contains("'X-Dex-Key: {{DEX_MCP_KEY}}'"), "{hook}");
                    continue;
                }
                assert_eq!(hook["headers"]["X-Dex-Key"], "$DEX_MCP_KEY", "{event}");
                assert!(hook["allowedEnvVars"].as_array().unwrap().iter().any(|v| v == "DEX_MCP_KEY"), "{event}");
            }
        }
    }

    fn run_resume_command(port: u16, token: &str, key: &str) -> std::process::Output {
        let hook = settings_json(port, false)["hooks"]["SessionStart"][0]["hooks"][0].clone();
        let mut child = std::process::Command::new(hook["command"].as_str().unwrap())
            .args(hook["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()))
            .args(["--write-out", "\n%{http_code}"])
            .env("DEX_HOOK_TOKEN", token).env("DEX_AGENT_ID", "timur").env("DEX_MCP_KEY", key)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"SessionStart","source":"resume"}"#).unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn session_start_command_reports_a_closed_port() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = run_resume_command(closed, "fixture", "fixture");
        assert!(!out.status.success());
        assert_eq!(out.stdout, b"\n000");
        assert!(!out.stderr.is_empty(), "transport errors must reach the pane instead of No stderr output");
    }

    #[test]
    fn session_start_resume_command_returns_200_and_context() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("resume-command-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir:root.join("data"), config_dir:root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let out = run_resume_command(core.hooks.port, &core.hooks.token, &core.mcp.key_for("timur"));
        core.shutdown();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        let text = String::from_utf8(out.stdout).unwrap();
        let (body, status) = text.rsplit_once('\n').unwrap();
        assert_eq!(status, "200");
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert!(body["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("# Takım kadrosu"));
    }

    /// #164 P3: the workers' PreToolUse blocks (exit 2) when Dex cannot answer; leaders keep the
    /// HTTP hook. Runs the real command against a closed port and a 401, in the shell Claude uses.
    #[test]
    fn worker_pre_tool_hook_fails_closed() {
        let leader = settings_json(4242, false);
        assert_eq!(leader["hooks"]["PreToolUse"][0]["hooks"][0]["type"], "http");
        let hook = |port: u16| settings_json(port, true)["hooks"]["PreToolUse"][0]["hooks"][0].clone();
        let h = hook(4242);
        assert_eq!(h["type"], "command");
        let cmd = h["command"].as_str().unwrap();
        assert!(cmd.contains(" -f ") && cmd.contains("-m 8") && cmd.ends_with("exit 2; }"), "{cmd}");
        assert!(h["timeout"].as_u64().unwrap() > 8, "curl's own timeout must fire first");
        let sh = if cfg!(windows) { r"C:\Program Files\Git\bin\bash.exe" } else { "sh" };
        if cfg!(windows) && !Path::new(sh).exists() {
            return;
        }
        let run = |port: u16| {
            let cmd = hook(port)["command"].as_str().unwrap().to_string();
            let mut child = std::process::Command::new(sh)
                .args(["-c", &cmd])
                .env("DEX_HOOK_TOKEN", "t").env("DEX_AGENT_ID", "kaan").env("DEX_MCP_KEY", "k")
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
                .spawn().unwrap();
            child.stdin.take().unwrap().write_all(b"{}").unwrap();
            let out = child.wait_with_output().unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned(), String::from_utf8_lossy(&out.stdout).into_owned())
        };
        // Nothing listens: blocked with the reason.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (code, err, _) = run(closed);
        assert_eq!(code, Some(2), "{err}");
        assert!(err.contains("araç engellendi"), "{err}");
        // A 401 (wrong key) blocks too; a 200 passes its JSON answer through.
        for (status, want) in [("401 Unauthorized", Some(2)), ("200 OK", Some(0))] {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body = "{\"ok\":1}";
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                head
            });
            let (code, err, out) = run(port);
            let head = server.join().unwrap();
            assert_eq!(code, want, "{status}: {err}");
            assert!(head.contains("Authorization: Bearer t") && head.contains("X-Dex-Agent: kaan") && head.contains("X-Dex-Key: k"), "{head}");
            if want == Some(0) {
                assert_eq!(out, "{\"ok\":1}");
            }
        }
    }

    /// #242: through the real hook entry — the reply (permit allow / deny / no answer) is exactly what
    /// `delegation_on_hook` gives without any emission, and `agent.step` follows the tool's life.
    #[test]
    fn agent_step_rides_the_hook_without_touching_the_decision() {
        use crate::events::EventSink;
        use std::sync::Arc;
        #[derive(Default)]
        struct Rec(Mutex<Vec<(String, Value)>>);
        impl EventSink for Arc<Rec> {
            fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), payload));
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("dex-step-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let rec = Arc::new(Rec::default());
        let core = Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(rec.clone()),
        )
        .unwrap();
        crate::db::with(&core.db, |c| c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let steps = |agent: &str| -> Vec<Value> {
            rec.0.lock().unwrap().iter().filter(|(e, p)| e == crate::step::EVENT && p["agentId"] == agent).map(|(_, p)| p.clone()).collect()
        };
        // (status line, body) of a real POST /hook.
        let post = |agent: &str, body: &Value| -> (String, String) {
            let text = body.to_string();
            let mut s = TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
            write!(
                s,
                "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {}\r\nContent-Length: {}\r\n\r\n{text}",
                core.hooks.token,
                core.mcp.key_for(agent),
                text.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
            (head.lines().next().unwrap_or("").to_string(), body.to_string())
        };
        let pre = |id: &str, tool: &str, input: Value| json!({ "hook_event_name": "PreToolUse", "tool_use_id": id, "tool_name": tool, "tool_input": input, "cwd": proj.to_string_lossy(), "permission_mode": "auto" });

        // Allowed (worker): the reply is the policy's allow, byte for byte; a start goes out.
        let safe = pre("t1", "Bash", json!({ "command": "cargo test -p dex-core --lib chat" }));
        let direct = core.delegation_on_hook("kaan", "PreToolUse", &safe, false).unwrap();
        let (status, reply) = post("kaan", &safe);
        assert!(status.contains("200"), "{status}");
        assert_eq!(reply, direct);
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["hookSpecificOutput"]["permissionDecision"], "allow");
        let got = steps("kaan");
        assert_eq!((got[0]["phase"].as_str(), got[0]["tool"].as_str(), got[0]["label"].as_str()), (Some("start"), Some("Bash"), Some("komut · cargo test")));
        assert!(got[0].get("ok").is_none() && got[0]["ts"].as_u64().unwrap() > 0);
        // Its PostToolUse ends it ok; a failed tool ends not-ok.
        post("kaan", &json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1", "tool_name": "Bash", "tool_input": { "command": "cargo test" } }));
        post("kaan", &pre("t2", "Read", json!({ "file_path": proj.join("src").join("lib.rs").to_string_lossy() })));
        post("kaan", &json!({ "hook_event_name": "PostToolUseFailure", "tool_use_id": "t2", "tool_name": "Read", "tool_input": { "file_path": "x" }, "error": "yok" }));
        let got = steps("kaan");
        assert_eq!(got.iter().map(|s| (s["phase"].as_str().unwrap(), s["ok"].as_bool())).collect::<Vec<_>>(),
            vec![("start", None), ("end", Some(true)), ("start", None), ("end", Some(false))]);
        assert_eq!(got[2]["label"], "dosya okuyor · lib.rs");

        // Denied: the deny is the reply as ever, and no step — the tool never runs.
        let bad = pre("t3", "Bash", json!({ "command": "git push server HEAD" }));
        let direct = core.delegation_on_hook("kaan", "PreToolUse", &bad, false).unwrap();
        let (_, reply) = post("kaan", &bad);
        assert_eq!(reply, direct);
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(steps("kaan").len(), 4, "no start for a denied call");

        // No answer (the leader's own call): 204 as before, and the leader's step is out.
        let gray = pre("l1", "Bash", json!({ "command": "curl -sI https://example.com" }));
        assert!(core.delegation_on_hook("timur", "PreToolUse", &gray, false).is_none());
        let (status, reply) = post("timur", &gray);
        assert!(status.contains("204") && reply.is_empty(), "{status} {reply}");
        let leader = steps("timur");
        assert_eq!(leader.len(), 1);
        assert_eq!(leader[0]["label"], "komut · curl");

        // The rate limit holds through the real entry: a burst of 10 starts → at most 4 go out.
        for i in 0..10 {
            post("ayla", &pre(&format!("b{i}"), "Read", json!({ "file_path": proj.join("a.rs").to_string_lossy() })));
        }
        let burst = steps("ayla");
        assert!(!burst.is_empty() && burst.len() <= 4 + 1, "{} steps", burst.len());
        // A Stop leaves nothing open: a late PostToolUse of a started call makes no end.
        post("ayla", &json!({ "hook_event_name": "Stop" }));
        let before = steps("ayla").len();
        post("ayla", &json!({ "hook_event_name": "PostToolUse", "tool_use_id": "b0", "tool_name": "Read", "tool_input": {} }));
        assert_eq!(steps("ayla").len(), before);
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_from_frontmatter_with_limit() {
        let root = std::env::temp_dir().join(format!("dex-mem-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        put(
            &root.join("shared/port.md"),
            "---\nname: port\ndescription: \"Hook portu süreç başına\"\nmetadata:\n  type: project\n---\nbody [[x]]\n",
        );
        put(&root.join("shared/MEMORY.md"), "- elle tutulan satır\n");
        put(&root.join("shared/bare.md"), "no frontmatter\n");
        put(&root.join("agents/ann-1/pref.md"), "---\nname: pref\ndescription: Kısa cevap\n---\n");
        put(&root.join("agents/bob-2/secret.md"), "---\nname: secret\ndescription: başkasının\n---\n");

        let idx = memory_index(&root, "ann-1", INDEX_LIMIT);
        assert!(idx.contains("- port [project] — Hook portu süreç başına — "), "{idx}");
        assert!(idx.contains("- bare — ⚠ frontmatter eksik"), "{idx}");
        assert!(idx.contains("- pref — Kısa cevap"), "{idx}");
        assert!(!idx.contains("elle tutulan") && !idx.contains("secret"), "{idx}");
        assert!(idx.find("Takım ortak").unwrap() < idx.find("Sana özel").unwrap());
        assert!(memory_index(&root, "nobody", INDEX_LIMIT).contains("(boş)"));

        let cut = memory_index(&root, "ann-1", 300);
        assert!(cut.contains("sığmadı"), "{cut}");
        assert!(!cut.contains("- pref"), "{cut}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Bölüm kotası: iki bölüm de doluyken ikisinden de kayıt gelir, her bölüm kendi
    /// kesme notunu doğru sayıyla yazar, toplam sınır (notlar hariç) aşılmaz.
    #[test]
    fn index_quota_both_sections_present() {
        let root = std::env::temp_dir().join(format!("dex-mem-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for i in 0..30 {
            put(
                &root.join(format!("shared/s{i:02}.md")),
                &format!("---\nname: s{i:02}\ndescription: ortak kayıt numara {i:02} doldurma metni doldurma doldurma doldurma\nmetadata:\n  type: project\n---\n"),
            );
            put(
                &root.join(format!("agents/ann-1/p{i:02}.md")),
                &format!("---\nname: p{i:02}\ndescription: özel kayıt numara {i:02} doldurma metni doldurma doldurma doldurma\n---\n"),
            );
        }
        let idx = memory_index(&root, "ann-1", INDEX_LIMIT);
        assert!(idx.contains("- s29 [project]"), "{idx}");
        assert!(idx.contains("- p29"), "{idx}");
        // Her bölümün kendi kesme notu var.
        assert_eq!(idx.matches("kayıt daha sığmadı; memory_search ile ara.").count(), 2, "{idx}");
        // Kesme notundaki sayılar doğru: gösterilen + düşen = toplam.
        for (section, total) in [("Takım ortak", 30), ("Sana özel", 30)] {
            let start = idx.find(section).unwrap();
            let end = idx[start..].find("## ").map(|i| start + i).unwrap_or(idx.len());
            let body = &idx[start..end.min(idx.len()).max(start)];
            let shown = body.matches("\n- ").count();
            let dropped: usize = body
                .rsplit("… ")
                .next()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(shown + dropped, total, "{section}: {body}");
        }
        // Özel bölüm garantisi: en az ~3 KB özel kayıt.
        let priv_start = idx.find("Sana özel").unwrap();
        let priv_shown: usize = idx[priv_start..]
            .lines()
            .filter(|l| l.starts_with("- p"))
            .map(|l| l.len() + 1)
            .sum();
        assert!(priv_shown >= INDEX_PRIVATE_MIN - 512, "özel {priv_shown} bayt");
        // Girdi baytları sınır içinde (kesme notları hariç).
        let notes: usize = idx
            .lines()
            .filter(|l| l.contains("kayıt daha sığmadı"))
            .map(|l| l.len() + 1)
            .sum();
        assert!(idx.len() - notes <= INDEX_LIMIT, "toplam {}", idx.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ranking precedes recency; tied timestamps use a stable filename order.
    #[test]
    fn memory_entry_order_uses_rank_then_mtime_then_filename() {
        let root = std::env::temp_dir().join(format!("dex-mem-order-{}", std::process::id()));
        for (name, seconds) in [("a", 50), ("b", 200), ("c", 100), ("z", 100)] {
            let path = root.join(format!("shared/{name}.md"));
            put(&path, &format!("---\nname: {name}\ndescription: order\n---\n"));
            std::fs::File::options().write(true).open(path).unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)).unwrap();
        }
        let canonical = root.canonicalize().unwrap();
        let names = |lines: Vec<String>| lines.iter().map(|l| l.split_whitespace().nth(1).unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(memory_entries(&canonical, &root.join("shared"))), ["b", "z", "c", "a"]);
        let ranks = HashMap::from([(root.join("shared/a.md"), 0)]);
        assert_eq!(names(memory_entries_ranked(&canonical, &root.join("shared"), None, &ranks)), ["a", "b", "z", "c"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Özel bölüm boşken ortak tüm sınırı kullanır (artanı devralır), özelde not olmaz.
    #[test]
    fn index_quota_unused_passes_to_other() {
        let root = std::env::temp_dir().join(format!("dex-mem-quota2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for i in 0..60 {
            let path = root.join(format!("shared/s{i:02}.md"));
            put(
                &path,
                &format!("---\nname: s{i:02}\ndescription: ortak kayıt numara {i:02} doldurma metni doldurma doldurma doldurma\nmetadata:\n  type: project\n---\n"),
            );
            // Coarse filesystem timestamps must not decide which tied entry survives the cut.
            std::fs::File::options().write(true).open(path).unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(100)).unwrap();
        }
        // Budget exactly 40 rows plus the real headers: changing TEMP's path length cannot
        // change this quota test's cut. All 60 mtimes tie; filename order selects s59 first.
        let full = memory_index(&root, "nobody", usize::MAX);
        let rows: Vec<_> = full.lines().filter(|l| l.starts_with("- s")).collect();
        let row_budget: usize = rows[..40].iter().map(|l| l.len() + 1).sum();
        let fixed = full.len() - rows.iter().map(|l| l.len() + 1).sum::<usize>();
        let limit = fixed + row_budget;
        let idx = memory_index(&root, "nobody", limit);
        assert!(idx.contains("(boş)"), "{idx}");
        assert!(idx.contains("- s59 [project]"), "{idx}");
        assert_eq!(idx.matches("kayıt daha sığmadı; memory_search ile ara.").count(), 1, "{idx}");
        // Ortak, özelin kullanmadığı kotayı da aldı: gösterilen girdi > yarı bütçe.
        let shared_start = idx.find("Takım ortak").unwrap();
        let shared_shown: usize = idx[shared_start..]
            .lines()
            .take_while(|l| !l.starts_with("## "))
            .filter(|l| l.starts_with("- s"))
            .map(|l| l.len() + 1)
            .sum();
        assert_eq!(idx.lines().filter(|l| l.starts_with("- s")).count(), 40, "{idx}");
        assert!(shared_shown > row_budget / 2, "ortak {shared_shown} bayt");
        let notes: usize = idx
            .lines()
            .filter(|l| l.contains("kayıt daha sığmadı"))
            .map(|l| l.len() + 1)
            .sum();
        assert!(idx.len() - notes <= limit, "toplam {}", idx.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_skips_links_escaping_root() {
        let base = std::env::temp_dir().join(format!("dex-mem-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, out) = (base.join("root"), base.join("out"));
        put(&root.join("shared/ok.md"), "---
name: ok
description: içeride
---
");
        put(&out.join("secret.md"), "---
name: secret
description: dışarıda
---
");
        std::fs::create_dir_all(root.join("agents")).unwrap();
        assert!(crate::memory::tests::link_dir(&out, &root.join("agents/ann-1")));
        let idx = memory_index(&root, "ann-1", INDEX_LIMIT);
        assert!(idx.contains("- ok — içeride"), "{idx}");
        assert!(!idx.contains("secret"), "{idx}");
        let _ = std::fs::remove_dir(root.join("agents/ann-1"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn roster_block_only_for_leaders_and_reads_db_live() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "roster-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        let core = Core::start(
            crate::Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let lead = team_roster_block(&core, "timur").expect("lider kadroyu görür");
        assert!(lead.starts_with("# Takım kadrosu\n"), "{lead}");
        assert!(lead.contains("- Timur (timur)"), "{lead}");
        assert!(lead.contains("claude/opus"), "{lead}");
        assert!(lead.contains("açık") || lead.contains("kapalı"), "{lead}");
        assert!(out_len_within(&lead, ROSTER_LIMIT), "{lead}");
        assert_eq!(team_roster_block(&core, "kaan"), None, "worker kadro almaz");
        assert_eq!(
            team_roster_block(&core, "yok-ajan"),
            None,
            "bilinmeyen ajan kadro almaz"
        );
        // Yeni üye bir sonraki oturum başında görünür (blok her seferinde DB'den okunur).
        crate::db::with(&core.db, |conn| {
            conn.execute(
                "INSERT INTO agents (id, team_id, name, kind, title, provider, model, engine, role_slug, shirt, hair, skin, sort) \
                 VALUES ('canvas-1','default','Canvas-1','worker','Tasarımcı','anthropic','sonnet','claude','frontend','#111','#222','#333',4)",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .unwrap();
        let lead2 = team_roster_block(&core, "timur").unwrap();
        assert!(lead2.contains("- Canvas-1 (canvas-1)"), "{lead2}");
        core.shutdown();
    }

    #[test]
    fn session_start_index_survives_resolver_failures() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!("session-index-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let repo = |name: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(["init", "--quiet", "--initial-branch=fixture-main"]).status().unwrap().success());
            dir
        };
        let db = |sql: &str, p: &[&dyn rusqlite::ToSql]| crate::db::with(&core.db, |c| c.execute(sql, p).map_err(|e| e.to_string())).unwrap();
        // An old single-project team: cwd, default project and both memory anchors on one repo.
        let old = repo("old");
        let mem = old.join(".dex").join("memory");
        let old_s = old.to_string_lossy().into_owned();
        db("UPDATE teams SET cwd=?1,team_memory_root=?2,agent_memory_root=?2 WHERE id='default'", &[&old_s, &mem.to_string_lossy().into_owned()]);
        db(
            "UPDATE projects SET root_path=?1,root_key=?2 WHERE id=(SELECT default_project_id FROM teams WHERE id='default')",
            &[&old_s, &crate::projects::path_key(&old_s)],
        );
        put(&mem.join("shared/old.md"), "---\nname: old\ndescription: team fixture\n---\n");
        put(&mem.join("agents/kaan/habit.md"), "---\nname: habit\ndescription: private fixture\n---\n");
        put(&mem.join("agents/timur/lead.md"), "---\nname: lead\ndescription: leader fixture\n---\n");
        for agent in ["kaan", "timur", "yok-ajan", "ayla"] {
            core.hooks.memory.lock().unwrap().insert(agent.into(), mem.clone());
        }
        let session_start = |agent: &str| -> String {
            let text = json!({ "hook_event_name": "SessionStart", "session_id": format!("s-{agent}"), "source": "startup" }).to_string();
            let mut s = TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
            write!(
                s,
                "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {}\r\nContent-Length: {}\r\n\r\n{text}",
                core.hooks.token,
                core.mcp.key_for(agent),
                text.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
            serde_json::from_str::<Value>(body).unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
        };
        // Live serve path, single project: byte for byte the pre-project index.
        assert_eq!(session_start("kaan"), memory_index(&mem, "kaan", INDEX_LIMIT));
        let lead = session_start("timur");
        assert!(lead.ends_with(&memory_index(&mem, "timur", INDEX_LIMIT)) && lead.starts_with("# Takım kadrosu"), "{lead}");

        // A distinct registered root tells step 2 (team anchors) from step 3 (registered root).
        let reg = root.join("registered");
        put(&reg.join("shared/reg.md"), "---\nname: reg\ndescription: registered root\n---\n");
        core.hooks.memory.lock().unwrap().insert("timur".into(), reg.clone());
        // Step 2: two bound projects and a leader without an agent_sessions row (Session → Required).
        let ready = crate::projects::prepare_root(repo("second").to_str().unwrap(), None).unwrap();
        let second = crate::db::with(&core.db, |c| {
            crate::projects::create(c, "default", crate::projects::NewProject { name: "Second".into(), worktrees: None }, &ready).map_err(|e| e.to_string())
        })
        .unwrap();
        db("DELETE FROM agent_sessions WHERE agent_id='timur'", &[]);
        let multi = session_start("timur");
        assert!(multi.contains("# Dex hafıza indeksi") && multi.contains("- old —") && multi.contains("- lead —"), "{multi}");
        assert!(!multi.contains("Proje ortak") && !multi.contains("- reg —"), "project-less team context: {multi}");

        // Step 2: the saved project was archived (Unavailable) or deleted (NotFound).
        db("DELETE FROM agent_sessions WHERE agent_id='timur'", &[]);
        db("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id) VALUES('timur','s','claude',1,?1)", &[&second.id]);
        db("UPDATE projects SET archived_at=1 WHERE id=?1", &[&second.id]);
        let archived = session_context_index(&core, "timur", &reg);
        assert!(archived.contains("- old —") && archived.contains("- lead —") && !archived.contains("- reg —"), "{archived}");
        db("PRAGMA foreign_keys=OFF", &[]);
        db("DELETE FROM projects WHERE id=?1", &[&second.id]);
        let deleted = session_context_index(&core, "timur", &reg);
        assert!(deleted.contains("- old —") && deleted.contains("- lead —") && !deleted.contains("- reg —"), "{deleted}");

        // Step 3: no team context either → the registered root's legacy index.
        assert_eq!(session_context_index(&core, "yok-ajan", &reg), memory_index(&reg, "yok-ajan", INDEX_LIMIT), "unknown agent");
        db("UPDATE agents SET team_id='ghost' WHERE id='ayla'", &[]);
        assert_eq!(session_context_index(&core, "ayla", &reg), memory_index(&reg, "ayla", INDEX_LIMIT), "team row missing");
        db("PRAGMA foreign_keys=ON", &[]);
        core.shutdown();
    }

    fn out_len_within(out: &str, limit: usize) -> bool {
        out.len() <= limit + 256
    }
}
