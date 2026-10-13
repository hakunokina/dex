//! FIX-3 — first-reply watchdog. A turn starts with UserPromptSubmit; if no other hook of that
//! agent (tool, permission, Stop, StopFailure…) follows within the provider's threshold
//! ([`crate::settings::first_reply_secs`]; built in only for the local LLM), the agent gets a
//! "yanıt gecikiyor" mark: `dex-first-reply` (pane line + amber mark + notification in the UI) and,
//! on a delegated job, a note for its leader. Nothing is stopped; the patron picks
//! [`Core::agent_first_reply_retry`] (Esc + the same prompt again) or [`Core::agent_first_reply_cancel`].
//!
//! Separate from DEX-28's stall clock (frontend, minutes of silence at any point of a turn): this
//! one watches only the gap before the first event, where a local model switch hides.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::db::with;

pub const EVENT: &str = "dex-first-reply";

/// Claude puts an interrupted prompt back into its input box when nothing was answered yet
/// (see src/agents.ts `noteInput`); Enter after this pause sends that same prompt again.
const RESUBMIT_AFTER: Duration = Duration::from_millis(700);

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub agent_id: String,
    /// false = the mark is gone (a reply came, a new turn, retry/cancel, exit).
    pub late: bool,
    pub seconds: u64,
    pub text: Option<String>,
    /// Leader of the running delegation, told via `plan::note`; the UI pings it when idle.
    pub leader: Option<String>,
}

impl Mark {
    fn cleared(agent: &str) -> Self {
        Self { agent_id: agent.into(), late: false, seconds: 0, text: None, leader: None }
    }
}

pub struct Watch {
    turns: Mutex<HashMap<String, Turn>>,
    /// Length of one threshold "second" in ms; tests shrink it.
    unit_ms: AtomicU64,
}

struct Turn {
    gen: u64,
    late: bool,
}

impl Default for Watch {
    fn default() -> Self {
        Self { turns: Mutex::default(), unit_ms: AtomicU64::new(1_000) }
    }
}

impl Watch {
    /// A new turn: returns its generation and whether an earlier turn was marked late.
    fn prompt(&self, id: &str) -> (u64, bool) {
        let mut turns = self.turns.lock().unwrap();
        let t = turns.entry(id.into()).or_insert(Turn { gen: 0, late: false });
        t.gen += 1;
        (t.gen, std::mem::replace(&mut t.late, false))
    }

    /// Any other event ends the wait. `true` = it was marked late (the mark must be cleared).
    fn answered(&self, id: &str) -> bool {
        self.turns.lock().unwrap().remove(id).is_some_and(|t| t.late)
    }

    /// The timer of turn `gen` ran out: `true` once, if that turn still waits.
    fn due(&self, id: &str, gen: u64) -> bool {
        let mut turns = self.turns.lock().unwrap();
        match turns.get_mut(id) {
            Some(t) if t.gen == gen && !t.late => {
                t.late = true;
                true
            }
            _ => false,
        }
    }

    pub fn is_late(&self, id: &str) -> bool {
        self.turns.lock().unwrap().get(id).is_some_and(|t| t.late)
    }

    #[cfg(test)]
    pub(crate) fn set_unit(&self, d: Duration) {
        self.unit_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }
}

/// "Yerel LLM yanıt vermiyor (95 sn). Sunucu model yüklüyor: qwen3-coder-next."
pub fn text(label: &str, provider: &str, secs: u64, loading: Option<&str>) -> String {
    match (provider, loading) {
        (_, Some(l)) => format!("{label} yanıt vermiyor ({secs} sn). Sunucu {}.", l.replacen("model yükleniyor", "model yüklüyor", 1)),
        ("local", None) => format!("{label} yanıt vermiyor ({secs} sn). Sunucu model yüklüyor olabilir."),
        _ => format!("{label} yanıt vermiyor ({secs} sn)."),
    }
}

impl crate::Core {
    /// Hook receiver side, after the status update.
    pub(crate) fn first_reply_on_hook(&self, agent: &str, event: &str) {
        match event {
            "UserPromptSubmit" => {
                let (gen, was_late) = self.first_reply.prompt(agent);
                if was_late {
                    let _ = self.events.emit(EVENT, &Mark::cleared(agent));
                }
                let Ok(a) = with(&self.db, |c| crate::teams::get_agent(c, agent)) else { return };
                let secs = with(&self.db, |c| Ok(crate::settings::first_reply_secs(c, &a.provider))).unwrap_or(0);
                if secs == 0 {
                    return;
                }
                let wait = Duration::from_millis(secs as u64 * self.first_reply.unit_ms.load(Ordering::Relaxed));
                let (weak, agent) = (self.weak_self.clone(), agent.to_string());
                std::thread::spawn(move || {
                    std::thread::sleep(wait);
                    if let Some(core) = weak.upgrade().filter(|c| c.first_reply.due(&agent, gen)) {
                        core.first_reply_late(&agent, secs as u64, &a.provider, &a.model);
                    }
                });
            }
            "SessionStart" => {}
            _ => self.first_reply_clear(agent),
        }
    }

    fn first_reply_late(&self, agent: &str, secs: u64, provider: &str, model: &str) {
        let label = crate::providers::get(provider).map_or("Claude", |p| p.label);
        // Network (≤ ~25 s) — this runs on the timer thread, never on a hook connection.
        let loading = (provider == "local").then(|| crate::providers::local_loading(model)).flatten();
        let text = text(label, provider, secs, loading.as_deref());
        let leader = with(&self.db, |c| crate::delegate::slow_reply(c, agent, &text)).unwrap_or(None);
        // A reply may have come during the health probe: then the mark is already gone.
        if self.first_reply.is_late(agent) {
            let _ = self.events.emit(EVENT, &Mark { agent_id: agent.into(), late: true, seconds: secs, text: Some(text), leader });
        }
    }

    fn first_reply_clear(&self, agent: &str) {
        if self.first_reply.answered(agent) {
            let _ = self.events.emit(EVENT, &Mark::cleared(agent));
        }
    }

    pub(crate) fn first_reply_exited(&self, agent: &str) {
        self.first_reply_clear(agent);
    }

    /// Esc ends the waiting turn (the PTY lives), then Enter sends the prompt Claude put back.
    /// The next UserPromptSubmit arms a fresh watch.
    pub fn agent_first_reply_retry(&self, id: &str) -> Result<(), String> {
        self.first_reply_interrupt(id, true, &|b| self.ptys.write(id, b))
    }

    /// Esc only: the turn ends, the prompt stays in Claude's input box (a draft — no delegation
    /// paste lands on it) and the agent is idle.
    pub fn agent_first_reply_cancel(&self, id: &str) -> Result<(), String> {
        self.first_reply_interrupt(id, false, &|b| self.ptys.write(id, b))
    }

    fn first_reply_interrupt(&self, id: &str, resend: bool, write: &dyn Fn(&[u8]) -> Result<(), String>) -> Result<(), String> {
        if !self.ptys.is_running(id) {
            return Err("ajanın oturumu kapalı".into());
        }
        self.first_reply_clear(id);
        write(b"\x1b")?;
        if resend {
            std::thread::sleep(RESUBMIT_AFTER);
            return write(b"\r");
        }
        self.agent_states.input(id, true);
        let state = self.agent_states.hook(id, Some(crate::agents::Status::Idle), None);
        let _ = self.events.emit("agent-status", &state);
        Ok(())
    }

    pub fn first_reply_get(&self) -> Result<std::collections::BTreeMap<String, u32>, String> {
        with(&self.db, |c| Ok(crate::settings::first_reply_view(c)))
    }

    pub fn first_reply_set(&self, provider: &str, secs: u32) -> Result<std::collections::BTreeMap<String, u32>, String> {
        with(&self.db, |c| crate::settings::set_first_reply(c, provider, secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pty, shell, teams, Core, EventSink, Events, Paths, PtySink};
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    #[derive(Default)]
    struct Rec(Mutex<Vec<(String, Value)>>);
    impl EventSink for Arc<Rec> {
        fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
            self.0.lock().unwrap().push((event.into(), payload));
            Ok(())
        }
    }
    impl Rec {
        fn marks(&self, agent: &str) -> Vec<Value> {
            let got = self.0.lock().unwrap();
            got.iter().filter(|(e, p)| e == EVENT && p["agentId"] == agent).map(|(_, p)| p.clone()).collect()
        }
    }

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    fn hook(core: &Core, agent: &str, event: &str) {
        let body = format!(r#"{{"hook_event_name":"{event}","prompt":"merhaba"}}"#);
        let mut s = TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {}\r\nContent-Length: {}\r\n\r\n{body}",
            core.hooks.token,
            core.mcp.key_for(agent),
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
    }

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn plain_turkish_text() {
        assert_eq!(text("Yerel LLM", "local", 95, None), "Yerel LLM yanıt vermiyor (95 sn). Sunucu model yüklüyor olabilir.");
        assert_eq!(
            text("Yerel LLM", "local", 95, Some("model yükleniyor: qwen3-coder-next")),
            "Yerel LLM yanıt vermiyor (95 sn). Sunucu model yüklüyor: qwen3-coder-next."
        );
        assert_eq!(text("GLM", "glm", 120, None), "GLM yanıt vermiyor (120 sn).");
    }

    #[test]
    fn watch_generations() {
        let w = Watch::default();
        let (g1, _) = w.prompt("a");
        let (g2, was) = w.prompt("a");
        assert!(!was);
        assert!(!w.due("a", g1), "an older turn's timer is stale");
        assert!(w.due("a", g2));
        assert!(!w.due("a", g2), "once");
        assert!(w.answered("a"), "late → clear");
        assert!(!w.answered("a"));
        let (g3, _) = w.prompt("a");
        assert!(!w.answered("a"), "answered in time: nothing to clear");
        assert!(!w.due("a", g3));
    }

    /// Through the real hook receiver: threshold overrun → mark; a reply clears it; retry → Esc +
    /// Enter; cancel → Esc, idle, draft; exit clears. (Leader note: `delegate` tests.)
    #[test]
    fn overrun_marks_and_retry_cancel_act_on_the_pty() {
        let root = std::env::temp_dir().join(format!("dex-first-reply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rec = Arc::new(Rec::default());
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(rec.clone()),
        )
        .unwrap();
        core.first_reply.set_unit(Duration::from_millis(5)); // "90 sn" = 450 ms
        let team = core
            .team_create(teams::NewTeam { profile: None, name: "FR".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None })
            .unwrap();
        let agent = core.agent_list().unwrap().into_iter().find(|a| a.team_id == team.id).unwrap();
        let id = agent.id.clone();

        // Anthropic (default provider): off.
        hook(&core, &id, "UserPromptSubmit");
        std::thread::sleep(Duration::from_millis(600));
        assert!(rec.marks(&id).is_empty(), "anthropic has no threshold");
        hook(&core, &id, "Stop");

        // A threshold for the agent's provider, set through the same call a host exposes.
        core.first_reply_set("anthropic", 20).unwrap(); // 100 ms at this unit
        hook(&core, &id, "UserPromptSubmit");
        hook(&core, &id, "PreToolUse");
        std::thread::sleep(Duration::from_millis(250));
        assert!(rec.marks(&id).is_empty(), "a tool event in time: no mark");
        hook(&core, &id, "Stop");

        hook(&core, &id, "UserPromptSubmit");
        assert!(wait_for(|| rec.marks(&id).len() == 1), "overrun → mark");
        let m = &rec.marks(&id)[0];
        assert_eq!(m["late"], true);
        assert_eq!(m["text"], "Claude yanıt vermiyor (20 sn).");
        assert!(core.first_reply.is_late(&id));
        hook(&core, &id, "Stop");
        assert!(wait_for(|| rec.marks(&id).len() == 2));
        assert_eq!(rec.marks(&id)[1]["late"], false, "the late reply clears it");

        // Retry / cancel need a live session (a shell stands in for claude); the bytes they send
        // are recorded through the writer seam.
        assert!(core.agent_first_reply_retry(&id).is_err(), "no session");
        core.pty_spawn(pty::Spawn { id: id.clone(), cmd: shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() }, crate::agents::shell_sink_for_test(&core, &id, Sink))
            .unwrap();
        let sent = Mutex::new(Vec::<Vec<u8>>::new());
        let rec_write = |b: &[u8]| {
            sent.lock().unwrap().push(b.to_vec());
            Ok(())
        };
        // The timer marks state before publishing its event. Wait for both before interrupting.
        let marked_late = || core.first_reply.is_late(&id) && rec.marks(&id).last().is_some_and(|m| m["late"] == true);
        hook(&core, &id, "UserPromptSubmit");
        assert!(wait_for(&marked_late));
        core.first_reply_interrupt(&id, true, &rec_write).unwrap();
        assert!(!core.first_reply.is_late(&id), "retry clears the mark");
        assert_eq!(rec.marks(&id).last().unwrap()["late"], false);
        assert_eq!(*sent.lock().unwrap(), vec![b"\x1b".to_vec(), b"\r".to_vec()], "Esc, then Enter on the put-back prompt");

        hook(&core, &id, "UserPromptSubmit"); // the resubmitted prompt arms a fresh watch
        assert!(wait_for(&marked_late));
        sent.lock().unwrap().clear();
        core.first_reply_interrupt(&id, false, &rec_write).unwrap();
        assert!(!core.first_reply.is_late(&id));
        assert_eq!(*sent.lock().unwrap(), vec![b"\x1b".to_vec()], "cancel: Esc only");
        assert_eq!(core.agent_states.get(&id).status, crate::agents::Status::Idle, "cancel → idle");
        assert!(core.agent_states.typing(&id), "the put-back prompt is a draft");
        assert!(core.ptys.is_running(&id), "the PTY is never killed");
        // The real writers reach the live PTY.
        core.agent_first_reply_cancel(&id).unwrap();

        // Exit drops a pending watch silently; a late one is cleared.
        hook(&core, &id, "UserPromptSubmit");
        assert!(wait_for(&marked_late));
        core.ptys.kill(&id);
        assert!(wait_for(|| rec.marks(&id).last().unwrap()["late"] == false));
        assert!(core.agent_first_reply_cancel(&id).is_err(), "no session");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}
