//! Görev #51: the leader's "Temiz oturum" asks for a hand-over note first. Dex sends the leader a
//! `[Dex]` prompt (queued while it is busy or the patron types), the leader writes its memory and
//! calls the leader-only MCP tool `reset_ready`; then Dex closes the PTY and starts it `fresh`.
//! No note within [`Resets::timeout`] → the phase turns `timeout` and the patron decides (force /
//! cancel). One reset per leader at a time; everything is driven by hooks and the MCP call.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::agents::Status;
use crate::db::with;
use crate::Core;

/// How long the leader has for its note before the patron is asked.
pub const NOTE_TIMEOUT: Duration = Duration::from_secs(180);

/// The patron's typing window ([`crate::agents::States::typing`]) plus a margin.
const TYPING_RETRY: Duration = Duration::from_millis(5_100);

/// The `reset_ready` answer reaches the session (and its transcript) before the PTY closes.
const READY_GRACE: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The prompt is out (or queued); the leader is writing its note.
    Writing,
    /// No `reset_ready` in time — the patron forces or cancels. A late `reset_ready` still counts.
    Timeout,
    /// The PTY is being closed and the fresh session started.
    Restarting,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub agent_id: String,
    /// `None`: no reset (finished, cancelled or failed).
    pub phase: Option<Phase>,
    /// Whether the prompt reached the leader (else it waits for its turn to end).
    pub sent: bool,
    pub error: Option<String>,
}

struct Reset {
    gen: u64,
    phase: Phase,
    sent: bool,
    prompt: String,
}

pub struct Resets {
    map: Mutex<HashMap<String, Reset>>,
    pub(crate) timeout: Mutex<Duration>,
    #[cfg(test)]
    pub(crate) started: Mutex<Vec<String>>,
}

impl Default for Resets {
    fn default() -> Self {
        Self {
            map: Mutex::default(),
            timeout: Mutex::new(NOTE_TIMEOUT),
            #[cfg(test)]
            started: Mutex::default(),
        }
    }
}

fn prompt(memory: &str) -> String {
    format!(
        "[{}] Oturumun sıfırlanacak (patron temiz oturum istedi). Açık konuları, patron kararlarını ve süren işleri \
         hafızana ({memory}) devir notu olarak yaz ya da güncelle. Bitince dex MCP'sinin reset_ready aracını çağır; \
         Dex oturumu kapatıp temiz açar, yeni oturum hafıza indeksini başta alır.",
        crate::dexmsg::SISTEM
    )
}

impl Core {
    /// `begin` | `force` | `cancel` | `get` — the patron's side (Tauri command / dexd method).
    pub fn leader_reset(&self, id: &str, action: &str) -> Result<Option<View>, String> {
        match action {
            "begin" => self.leader_reset_begin(id),
            "force" => {
                let phase = self.leader_resets.map.lock().unwrap().get(id).map(|r| r.phase);
                match phase {
                    None => Err("sürmekte olan bir sıfırlama yok".into()),
                    Some(Phase::Restarting) => Ok(Some(self.reset_view(id, None))),
                    Some(_) => self.leader_reset_restart(id).map(|_| None),
                }
            }
            "cancel" => {
                let mut map = self.leader_resets.map.lock().unwrap();
                if map.get(id).is_some_and(|r| r.phase == Phase::Restarting) {
                    return Err("oturum zaten yeniden başlatılıyor".into());
                }
                map.remove(id);
                drop(map);
                self.emit_reset(id, None);
                Ok(None)
            }
            "get" => {
                let pending = self.leader_resets.map.lock().unwrap().contains_key(id);
                Ok(pending.then(|| self.reset_view(id, None)))
            }
            _ => Err(format!("bilinmeyen sıfırlama eylemi: {action}")),
        }
    }

    fn leader_reset_begin(&self, id: &str) -> Result<Option<View>, String> {
        let memory = with(&self.db, |c| {
            let a = crate::teams::get_agent(c, id)?;
            if a.kind != "leader" {
                return Err(format!("{} lider değil; temiz oturum yalnız liderler için", a.name));
            }
            let team = crate::teams::get_team(c, &self.paths.data_dir, &a.team_id)?;
            team.memory_dir.map(|m| crate::slug::memory_dir(c, std::path::Path::new(&m), id).map(|p| p.to_string_lossy().into_owned())).transpose()
        })?;
        // No session: nothing to hand over, the fresh start is all there is.
        if !self.ptys.is_running(id) {
            self.leader_reset_start(id)?;
            return Ok(None);
        }
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut map = self.leader_resets.map.lock().unwrap();
            if map.contains_key(id) {
                return Err("bu lider için sıfırlama zaten sürüyor".into());
            }
            let prompt = prompt(memory.as_deref().unwrap_or(".dex/memory/agents/<ad>/"));
            map.insert(id.into(), Reset { gen, phase: Phase::Writing, sent: false, prompt });
        }
        self.emit_reset(id, None);
        self.leader_reset_deliver(id);
        let (weak, agent, wait) = (self.weak_self.clone(), id.to_string(), *self.leader_resets.timeout.lock().unwrap());
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            let Some(core) = weak.upgrade() else { return };
            let mut map = core.leader_resets.map.lock().unwrap();
            match map.get_mut(&agent) {
                Some(r) if r.gen == gen && r.phase == Phase::Writing => r.phase = Phase::Timeout,
                _ => return,
            }
            drop(map);
            core.emit_reset(&agent, None);
        });
        Ok(Some(self.reset_view(id, None)))
    }

    /// The prompt goes in once the leader sits idle at its input box with the patron not typing.
    /// Called on begin, after a Stop / Esc, and after the patron's draft ends; anything else waits
    /// for the next of those (a busy leader always ends its turn with one).
    pub(crate) fn leader_reset_deliver(&self, id: &str) {
        {
            let map = self.leader_resets.map.lock().unwrap();
            if !map.get(id).is_some_and(|r| !r.sent && r.phase != Phase::Restarting) {
                return;
            }
        }
        if !self.ptys.is_running(id) || self.chat_queues.lock().unwrap().get(id).is_some_and(|q| !q.is_empty()) {
            return; // the chat queue goes first; its turn's Stop brings us back
        }
        let status = match self.agent_states.get(id).status {
            Status::Offline => Status::Idle,
            s => s,
        };
        if status != Status::Idle {
            return;
        }
        if self.agent_states.typing(id) {
            self.leader_reset_deliver_later(id);
            return;
        }
        let text = {
            let mut map = self.leader_resets.map.lock().unwrap();
            match map.get_mut(id) {
                Some(r) if !r.sent => {
                    r.sent = true;
                    r.prompt.clone()
                }
                _ => return, // another trigger won the race
            }
        };
        if let Err(err) = self.paste_prompt(id, &text) {
            eprintln!("dex: {id}: reset prompt: {err}");
            if let Some(r) = self.leader_resets.map.lock().unwrap().get_mut(id) {
                r.sent = false;
            }
            return;
        }
        self.emit_reset(id, None);
    }

    /// #447: a drain taken back — every reset prompt the gate held tries again.
    pub(crate) fn leader_reset_deliver_all_later(&self) {
        let ids: Vec<String> = self.leader_resets.map.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.leader_reset_deliver_later(&id);
        }
    }

    pub(crate) fn leader_reset_deliver_later(&self, id: &str) {
        if self.leader_resets.map.lock().unwrap().get(id).is_none_or(|r| r.sent) {
            return;
        }
        let (weak, agent) = (self.weak_self.clone(), id.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(TYPING_RETRY);
            if let Some(core) = weak.upgrade() {
                core.leader_reset_deliver(&agent);
            }
        });
    }

    /// MCP `reset_ready` (leader-only, checked by the caller): only answers a reset Dex asked for.
    pub(crate) fn leader_reset_ready(&self, id: &str) -> Result<serde_json::Value, String> {
        match self.leader_resets.map.lock().unwrap().get(id).map(|r| r.phase) {
            None => return Err("Dex senden bir sıfırlama istemedi (ya da patron iptal etti); oturumun sürüyor".into()),
            Some(Phase::Restarting) => return Ok(serde_json::json!({ "ok": true, "note": "oturum zaten yeniden başlatılıyor" })),
            Some(_) => {}
        }
        // Answer first: the session closes a moment after this reply lands.
        if let Some(r) = self.leader_resets.map.lock().unwrap().get_mut(id) {
            r.phase = Phase::Restarting;
        }
        self.emit_reset(id, None);
        let (weak, agent) = (self.weak_self.clone(), id.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(READY_GRACE);
            if let Some(core) = weak.upgrade() {
                let _ = core.leader_reset_restart(&agent);
            }
        });
        Ok(serde_json::json!({ "ok": true, "note": "Devir notu alındı; oturumun kapanıp temiz açılacak. Başka bir şey yapma." }))
    }

    /// Close the PTY (its exit settles synchronously, see `Ptys::kill`) and start fresh.
    fn leader_reset_restart(&self, id: &str) -> Result<(), String> {
        {
            let mut map = self.leader_resets.map.lock().unwrap();
            let Some(r) = map.get_mut(id) else { return Err("sıfırlama iptal edildi".into()) };
            r.phase = Phase::Restarting;
        }
        self.emit_reset(id, None);
        self.ptys.kill(id);
        crate::diagnostics::event("pty_kill", serde_json::json!({"agent": id, "reason": "reset"}));
        let result = self.leader_reset_start(id);
        self.leader_resets.map.lock().unwrap().remove(id);
        self.emit_reset(id, result.as_ref().err().cloned());
        result
    }

    fn leader_reset_start(&self, id: &str) -> Result<(), String> {
        #[cfg(test)]
        {
            self.leader_resets.started.lock().unwrap().push(id.into());
            Ok(())
        }
        #[cfg(not(test))]
        {
            self.agent_start(id, crate::agents::StartOptions { fresh: true, ..Default::default() }).map(|_| ())
        }
    }

    fn reset_view(&self, id: &str, error: Option<String>) -> View {
        let map = self.leader_resets.map.lock().unwrap();
        let r = map.get(id);
        View { agent_id: id.into(), phase: r.map(|r| r.phase), sent: r.is_some_and(|r| r.sent), error }
    }

    /// `leader-reset` carries the whole view; `phase: null` ends it (with `error` on a failed start).
    fn emit_reset(&self, id: &str, error: Option<String>) {
        let _ = self.events.emit("leader-reset", &self.reset_view(id, error));
    }
}

#[cfg(test)]
mod tests {
    use crate::*;
    use std::time::{Duration, Instant};

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    /// The default team's leader `timur` with a shell PTY standing in for claude.
    fn core(tag: &str) -> (Arc<Core>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dex-lreset-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
        core.pty_spawn(pty::Spawn { id: "timur".into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, agents::shell_sink_for_test(&core, "timur", Sink)).unwrap();
        // A pasted reset prompt would make the shell look busy; the test drives the status itself.
        core.agent_states.hook("timur", Some(agents::Status::Idle), None);
        (core, root)
    }

    fn done(core: Arc<Core>, root: std::path::PathBuf) {
        core.ptys.kill("timur");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    fn phase(core: &Core) -> Option<Phase> {
        core.leader_reset("timur", "get").unwrap().and_then(|v| v.phase)
    }

    fn sent(core: &Core) -> bool {
        core.leader_reset("timur", "get").unwrap().is_some_and(|v| v.sent)
    }

    fn within(limit: Duration, f: impl Fn() -> bool) -> bool {
        let end = Instant::now() + limit;
        while !f() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    use super::Phase;

    #[test]
    fn reset_prompt_then_ready_closes_the_session_and_starts_it_fresh_once() {
        let (core, root) = core("ready");
        let v = core.leader_reset("timur", "begin").unwrap().unwrap();
        assert_eq!((v.phase, v.sent), (Some(Phase::Writing), true), "idle leader gets the prompt at once");
        assert!(core.leader_reset("timur", "begin").is_err(), "no double trigger");
        assert!(core.ptys.is_running("timur"), "the session stays until the note is written");

        let reply = core.leader_reset_ready("timur").unwrap();
        assert_eq!(reply["ok"], true);
        assert!(within(Duration::from_secs(5), || phase(&core).is_none()), "restart finished");
        assert!(!core.ptys.is_running("timur"), "old session closed");
        assert_eq!(*core.leader_resets.started.lock().unwrap(), vec!["timur".to_string()], "one fresh start");
        assert!(core.leader_reset_ready("timur").is_err(), "a stray reset_ready is refused");
        done(core, root);
    }

    #[test]
    fn busy_leader_gets_the_prompt_after_its_stop() {
        let (core, root) = core("busy");
        core.agent_states.hook("timur", Some(agents::Status::Working), None);
        core.leader_reset("timur", "begin").unwrap();
        assert!(!sent(&core), "working: queued");
        core.leader_reset_deliver("timur"); // a stray trigger mid-turn changes nothing
        assert!(!sent(&core));
        core.agent_states.hook("timur", Some(agents::Status::Idle), None);
        core.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false);
        assert!(within(Duration::from_secs(3), || sent(&core)), "delivered after the Stop");
        done(core, root);
    }

    #[test]
    fn patrons_draft_holds_the_prompt_until_it_ends() {
        let (core, root) = core("draft");
        core.agent_input("timur", true);
        core.leader_reset("timur", "begin").unwrap();
        assert!(!sent(&core), "the patron is typing");
        core.agent_input("timur", false); // emptied the prompt
        assert!(!sent(&core), "not inside the typing window");
        assert!(within(Duration::from_secs(8), || sent(&core)), "goes once the window is over");
        done(core, root);
    }

    #[test]
    fn no_note_in_time_asks_the_patron_and_cancel_keeps_the_session() {
        let (core, root) = core("timeout");
        *core.leader_resets.timeout.lock().unwrap() = Duration::from_millis(200);
        core.leader_reset("timur", "begin").unwrap();
        assert!(within(Duration::from_secs(3), || phase(&core) == Some(Phase::Timeout)), "timed out");
        assert!(core.leader_reset("timur", "cancel").unwrap().is_none());
        assert!(phase(&core).is_none() && core.ptys.is_running("timur"), "cancelled: session kept");
        assert!(core.leader_resets.started.lock().unwrap().is_empty());
        assert!(core.leader_reset_ready("timur").is_err(), "a late note after cancel restarts nothing");

        // Timeout → force: restarts without the note.
        core.leader_reset("timur", "begin").unwrap();
        assert!(within(Duration::from_secs(3), || phase(&core) == Some(Phase::Timeout)));
        core.leader_reset("timur", "force").unwrap();
        assert!(phase(&core).is_none() && !core.ptys.is_running("timur"));
        assert_eq!(core.leader_resets.started.lock().unwrap().len(), 1);
        done(core, root);
    }

    #[test]
    fn a_worker_cannot_be_reset_nor_call_reset_ready() {
        let (core, root) = core("worker");
        let worker = with(&core.db, |c| Ok(teams::list_agents(c)?.into_iter().find(|a| a.kind == "worker").unwrap().id)).unwrap();
        assert!(core.leader_reset(&worker, "begin").unwrap_err().contains("lider değil"));
        // MCP: the tool is leader-only before it reaches the reset state.
        let out = mcp::call_for_test(&core, &worker, "reset_ready");
        assert_eq!(out["isError"], true, "{out}");
        assert!(out["content"][0]["text"].as_str().unwrap().contains("yalnız takım liderine"), "{out}");
        // The leader's own call without a pending reset is refused too.
        let out = mcp::call_for_test(&core, "timur", "reset_ready");
        assert_eq!(out["isError"], true, "{out}");
        done(core, root);
    }
}
