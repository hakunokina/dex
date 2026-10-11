//! #161 (C): a core that goes down to come back — drained, then restarted (an update) — so the
//! next one picks up where it left (.dex/results/188.md §3):
//!
//! - **Drain** ([`Core::drain`]): no new job goes to a worker (`dispatch` leaves it `queued` in the
//!   DB); [`Core::drain_blockers`] says what still runs — a running job, an agent mid-turn, an open
//!   `ask_leader`. The host restarts once that list is empty (or at once, if told so).
//! - **Marker** (`<data_dir>/dex.restart.json`, [`write`] just before such an exit): the next
//!   `Core::start` ([`take`]) keeps the queue (`reconcile_on_start` closes only `running` rows) and
//!   starts the leaders that ran, with `--resume` (their saved session). Workers are not reopened —
//!   every job starts in a fresh session anyway (#20); the kept queue starts them.
//! - Without a marker (the patron closed the core, its 120 s no-window rule, a crash) a start is
//!   as before: the queue fails and leaders stay closed. A marker older than [`FRESH_MS`] is
//!   ignored: a restart whose new core never came up must not reopen anything days later.
//! - #447 **Admission** ([`Core::admit`]): every path that starts an agent's turn — a dispatch, a
//!   paste (`paste_prompt`: chat, leader notes, a leader reset), a start with a prompt
//!   (`agent_start`) — holds a [`Turn`] until the turn shows (Working; a start's pending prompt) or
//!   fails. The drain is the gate: closed, chat waits in its queue (the marker carries it), leader
//!   notes stay unsent; the drain's end or the next core delivers them.

use crate::db::with;
use crate::{agents, teams, Core};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::Ordering;

const TEAM_IDLE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub(crate) struct TeamIdle {
    seen: std::time::Instant,
    viewers: std::collections::HashSet<String>,
    reset: std::sync::mpsc::Sender<Option<std::time::Instant>>,
}

impl Core {
    /// UI visibility/focus event. One resettable timer per team; viewing never starts its leader.
    pub fn team_viewed(&self, team: &str, viewed: bool) -> Result<(), String> {
        self.team_viewed_for(team, "local", viewed)
    }

    pub fn team_viewed_for(&self, team: &str, client: &str, viewed: bool) -> Result<(), String> {
        with(&self.db, |c| teams::get_team(c, &self.paths.data_dir, team))?;
        self.team_idle_update(team, Some((client, viewed)));
        Ok(())
    }

    pub(crate) fn team_idle_reset(&self, team: &str) {
        self.team_idle_update(team, None);
    }

    pub fn team_view_release(&self, client: &str) {
        let teams: Vec<String> = self.team_idle.lock().unwrap().iter()
            .filter(|(_, t)| t.viewers.contains(client)).map(|(team, _)| team.clone()).collect();
        for team in teams { self.team_idle_update(&team, Some((client, false))); }
    }

    fn team_idle_update(&self, team: &str, view: Option<(&str, bool)>) {
        let now = std::time::Instant::now();
        let mut timers = self.team_idle.lock().unwrap();
        if let Some(timer) = timers.get_mut(team) {
            if let Some((client, viewed)) = view {
                let changed = if viewed { timer.viewers.insert(client.into()) } else { timer.viewers.remove(client) };
                if !changed { return; }
            }
            timer.seen = now;
            let _ = timer.reset.send(timer.viewers.is_empty().then_some(now + TEAM_IDLE));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let viewers = view.filter(|(_, v)| *v).map(|(client, _)| std::collections::HashSet::from([client.into()])).unwrap_or_default();
        let initial = viewers.is_empty().then_some(now + TEAM_IDLE);
        timers.insert(team.into(), TeamIdle { seen: now, viewers, reset: tx });
        let (weak, team) = (self.weak_self.clone(), team.to_string());
        std::thread::spawn(move || {
            let mut deadline = initial;
            loop {
                let reset = match deadline {
                    Some(at) => rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())),
                    None => rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                };
                match reset {
                    Ok(at) => deadline = at,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let Some(core) = weak.upgrade() else { return };
                        core.leader_close_unseen(&team);
                        // Busy at expiry: a final idle transition or worker event reevaluates.
                        deadline = None;
                    }
                }
            }
        });
    }

    /// Explicit patron close, including an already sleeping leader: cancel automatic wake.
    pub fn agent_stop(&self, id: &str) {
        let leader = with(&self.db, |c| teams::get_agent(c, id)).is_ok_and(|a| a.kind == "leader");
        let _gate = leader.then(|| self.agent_start_gate.lock().unwrap());
        self.sleeping_leaders.lock().unwrap().remove(id);
        let guarded = leader && self.chain_guard.lock().unwrap().insert(id.into());
        self.ptys.kill(id);
        if guarded { self.chain_guard.lock().unwrap().remove(id); }
    }

    pub(crate) fn leader_close_unseen(&self, team: &str) -> bool {
        if self.admitted.load(Ordering::SeqCst) > 0 { return false; }
        #[cfg(test)]
        if let Some(pause) = self.close_pause.lock().unwrap().take() { pause(); }
        let admission = self.admission_gate.lock().unwrap();
        let _gate = self.agent_start_gate.lock().unwrap();
        if self.admitted.load(Ordering::SeqCst) > 0 { return false; }
        // Keep the deadline lock through the close, so a concurrent view event wins or resets
        // after it; it cannot be lost between the check and kill.
        let timers = self.team_idle.lock().unwrap();
        if !timers.get(team).is_some_and(|t| t.viewers.is_empty() && t.seen.elapsed() >= TEAM_IDLE) { return false; }
        let leader = with(&self.db, |c| {
            let busy: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM delegations
                    WHERE to_agent IN (SELECT id FROM agents WHERE team_id=?1) AND status IN ('running','queued'))
                    OR EXISTS(SELECT 1 FROM delegations
                    WHERE from_agent IN (SELECT id FROM agents WHERE team_id=?1) AND status IN ('running','queued'))",
                [team], |r| r.get(0)).map_err(|e| e.to_string())?;
            if busy { return Ok(None); }
            c.query_row("SELECT id FROM agents WHERE team_id=?1 AND kind='leader'", [team], |r| r.get::<_, String>(0))
                .map(Some).map_err(|e| e.to_string())
        }).ok().flatten();
        let Some(id) = leader else { return false };
        let state = self.agent_states.get(&id);
        if !self.ptys.is_running(&id) || state.in_turn() || state.status != agents::Status::Idle
            || self.agent_states.pre_session(&id) || self.agent_states.typing(&id)
            || self.chain_guard.lock().unwrap().contains(&id)
            || self.chat_queues.lock().unwrap().get(&id).is_some_and(|q| !q.is_empty())
            || self.leader_has_events(&id)
        { return false; }
        self.sleeping_leaders.lock().unwrap().insert(id.clone());
        // This voluntary exit must not trigger a chain return or queued dispatch under the gate.
        self.chain_guard.lock().unwrap().insert(id.clone());
        let closed = self.ptys.kill(&id);
        self.chain_guard.lock().unwrap().remove(&id);
        if !closed { self.sleeping_leaders.lock().unwrap().remove(&id); }
        drop(timers);
        drop(_gate);
        drop(admission);
        // A result can land after the pending-events check but before the sleep mark. Recheck
        // after parking so its event cannot be lost in that window.
        if closed { self.leader_notes_wake(&id); }
        closed
    }

    pub(crate) fn leader_has_events(&self, id: &str) -> bool {
        with(&self.db, |c| c.query_row(
            "SELECT EXISTS(SELECT 1 FROM leader_notes WHERE agent_id=?1 AND notified=0)
                OR EXISTS(SELECT 1 FROM questions WHERE leader=?1 AND status='open' AND leader_delivered_at IS NULL)
                OR EXISTS(SELECT 1 FROM delegations WHERE from_agent=?1 AND notified=0 AND status IN ('done','done_no_report','failed'))",
            [id], |r| r.get::<_, bool>(0)).map_err(|e| e.to_string())).unwrap_or(true)
    }

    /// Event watcher delivers open questions to idle leaders as well as resuming sleeping ones.
    /// Manual closes stay closed; questions retain their delivery stamp across hook/paste races.
    pub(crate) fn wake_sleeping_leaders(&self) {
        let mut ids = self.sleeping_leaders.lock().unwrap().clone();
        match with(&self.db, |c| {
            c.prepare("SELECT leader FROM questions WHERE status='open' AND leader_delivered_at IS NULL
                UNION SELECT agent_id FROM leader_notes WHERE notified=0
                UNION SELECT from_agent FROM delegations WHERE notified=0 AND status IN ('done','done_no_report','failed')")
                .map_err(|e| e.to_string())?.query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        }) {
            Ok(pending) => ids.extend(pending),
            Err(_) => log::warn!("pending leader question lookup failed"),
        }
        for id in ids { self.leader_notes_wake(&id); }
        let teams: Vec<String> = self.team_idle.lock().unwrap().keys().cloned().collect();
        for team in teams { self.leader_close_unseen(&team); }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::{delegate, events, pty, shell, Events, Paths, PtySink};
    use std::sync::Arc;

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }
    fn core() -> Arc<Core> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("lifecycle-{}-{}", std::process::id(), agents::next_seq()));
        Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap()
    }
    fn open(c: &Core, id: &str) {
        let (sink, wait_ready) = agents::shell_sink_with_ready_for_test(c, id, Sink);
        c.pty_spawn(pty::Spawn { id: id.into(), cmd: shell::SHELL_CMD.into(), cols:80, rows:24, ..Default::default() }, sink).unwrap();
        wait_ready();
        c.agent_states.hook(id, Some(agents::Status::Idle), None);
    }
    fn job(c: &Core, status: &str) -> delegate::Delegation {
        with(&c.db, |db| {
            let from = delegate::caller(db, "timur")?;
            let d = delegate::insert(db, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })?;
            delegate::set_status(db, d.id, status)?;
            delegate::get(db, d.id)
        }).unwrap()
    }
    fn finish(c: &Core) {
        for p in c.ptys.list() { c.agent_stop(&p.id); }
        c.shutdown();
    }
    fn post(c: &Core, agent: &str, body: &serde_json::Value) -> String {
        use std::io::{Read, Write};
        let text = body.to_string();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", c.hooks.port)).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        write!(stream, "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {}\r\nContent-Length: {}\r\n\r\n{text}", c.hooks.token, c.mcp.key_for(agent), text.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    fn accept_pending(c: &Core, timeout: std::time::Duration) -> String {
        let end = std::time::Instant::now() + timeout;
        loop {
            let prompt = c.leader_pastes.lock().unwrap().get("timur").map(|p| p.text.clone());
            if let Some(prompt) = prompt {
                return post(c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
            }
            assert!(std::time::Instant::now() < end, "no pending leader paste");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    #[test]
    fn delivered_worker_closes_only_after_stop_and_preserves_session() {
        let c = core(); open(&c, "kaan");
        with(&c.db, |db| agents::record_session(db, "kaan", "saved", "claude", None)).unwrap();
        let d = job(&c, "running");
        c.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":delegate::trigger_line(d.id)}), false);
        c.agent_states.hook("kaan", Some(agents::Status::Working), None);
        let reply = crate::mcp::call_with_args_for_test(&c, "kaan", "report", serde_json::json!({"delegationId":d.id,"summary":"bitti: test"}));
        assert_ne!(reply["isError"], true, "{reply}");
        assert!(!c.worker_close_delivered("kaan"));
        assert!(!crate::is_blocked(&c.delegation_on_hook("kaan", "Stop", &serde_json::json!({}), false)));
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while c.ptys.is_running("kaan") && std::time::Instant::now() < limit { std::thread::sleep(std::time::Duration::from_millis(25)); }
        assert!(!c.ptys.is_running("kaan"));
        assert_eq!(with(&c.db, |db| agents::resumable_session(db, "kaan", "claude")).unwrap().as_deref(), Some("saved"));
        finish(&c);
    }
    #[test]
    fn worker_queue_unreported_and_patron_conversation_stay_open() {
        let c = core(); open(&c, "kaan");
        job(&c, "done_no_report");
        assert!(!c.worker_close_delivered("kaan"));
        let d = job(&c, "done"); c.worker_delivered(&d);
        let queued = job(&c, "queued");
        assert!(!c.worker_close_delivered("kaan"));
        with(&c.db, |db| delegate::set_status(db, queued.id, "failed")).unwrap();
        c.agent_input("kaan", true);
        assert!(!c.worker_close_delivered("kaan"));
        c.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":"patron"}), false);
        assert!(!c.worker_close_delivered("kaan"));
        finish(&c);
    }
    fn expire(c: &Core) {
        c.team_viewed("default", false).unwrap();
        c.team_idle.lock().unwrap().get_mut("default").unwrap().seen = std::time::Instant::now() - TEAM_IDLE;
    }
    #[test]
    fn unseen_leader_waits_for_its_turn_and_workers_then_closes() {
        let c = core(); open(&c, "timur"); expire(&c);
        c.agent_states.hook("timur", Some(agents::Status::Working), None);
        assert!(!c.leader_close_unseen("default"));
        c.agent_states.hook("timur", Some(agents::Status::Idle), None);
        let d = job(&c, "queued");
        assert!(!c.leader_close_unseen("default"));
        with(&c.db, |db| delegate::set_status(db, d.id, "running")).unwrap();
        assert!(!c.leader_close_unseen("default"));
        with(&c.db, |db| { delegate::set_status(db, d.id, "done")?; db.execute("UPDATE delegations SET notified=1 WHERE id=?1", [d.id]).map_err(|e|e.to_string())?; Ok(()) }).unwrap();
        c.team_viewed("default", true).unwrap();
        assert!(!c.leader_close_unseen("default"), "viewing resets the deadline");
        c.team_viewed("default", false).unwrap(); expire(&c);
        assert!(c.leader_close_unseen("default"));
        assert!(c.sleeping_leaders.lock().unwrap().contains("timur"));
        c.team_viewed("default", true).unwrap();
        assert!(!c.ptys.is_running("timur"), "viewing does not start a leader");
        c.agent_stop("timur");
        assert!(!c.sleeping_leaders.lock().unwrap().contains("timur"), "manual close cancels auto wake");
        finish(&c);
    }
    #[test]
    fn sleeping_leader_resumes_with_pending_events_but_manual_close_stays_closed() {
        let c = core(); open(&c, "timur"); expire(&c);
        with(&c.db, |db| agents::record_session(db, "timur", "leader-saved", "claude", None)).unwrap();
        assert!(c.leader_close_unseen("default"));
        // Keep the asynchronous watcher away: insert directly, wake synchronously under CAPTURE.
        let d = job(&c, "done");
        agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
        c.leader_notes_wake("timur");
        let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
        assert_eq!(starts.len(), 1);
        assert!(starts[0].0.args.windows(2).any(|a| a == ["--resume", "leader-saved"]));
        assert!(starts[0].0.args.last().unwrap().starts_with("[Dex]"));
        assert!(!c.sleeping_leaders.lock().unwrap().contains("timur"));
        assert!(c.team_idle.lock().unwrap()["default"].seen.elapsed() < TEAM_IDLE);
        assert!(delegate::on_session_start(&c.db, &c.events, "timur").unwrap().contains(&format!("#{}", d.id)));
        c.agent_stop("timur");
        c.leader_notes_wake("timur");
        assert!(!c.ptys.is_running("timur"));
        finish(&c);
    }
    #[test]
    fn visibility_is_per_client_and_disconnect_rearms_only_after_last_viewer() {
        let c = core(); open(&c, "timur");
        c.team_viewed_for("default", "one", true).unwrap();
        c.team_viewed_for("default", "two", true).unwrap();
        c.team_idle.lock().unwrap().get_mut("default").unwrap().seen = std::time::Instant::now() - TEAM_IDLE;
        assert!(!c.leader_close_unseen("default"), "continuous viewing holds the leader");
        c.team_view_release("one");
        assert_eq!(c.team_idle.lock().unwrap()["default"].viewers.len(), 1);
        assert!(!c.leader_close_unseen("default"));
        c.team_view_release("two");
        let seen = c.team_idle.lock().unwrap()["default"].seen;
        assert!(seen.elapsed() < TEAM_IDLE);
        assert!(c.team_idle.lock().unwrap()["default"].viewers.is_empty());
        c.team_viewed_for("default", "two", false).unwrap();
        assert_eq!(c.team_idle.lock().unwrap()["default"].seen, seen, "repeated hidden is idempotent");
        expire(&c);
        assert!(c.leader_close_unseen("default"));
        assert!(c.team_viewed_for("missing", "one", true).is_err());
        finish(&c);
    }

    #[test]
    fn sleeping_leader_resumes_for_a_question_and_plan_note() {
        for question in [true, false] {
            let c = core(); open(&c, "timur"); expire(&c);
            with(&c.db, |db| agents::record_session(db, "timur", "saved", "claude", None)).unwrap();
            assert!(c.leader_close_unseen("default"));
            if question {
                let d = job(&c, "running");
                with(&c.db, |db| {
                    let worker = delegate::caller(db, "kaan")?;
                    crate::questions::ask(db, &worker, &serde_json::json!({"delegationId":d.id,"question":"karar?"}))
                }).unwrap();
            } else { with(&c.db, |db| crate::plan::note(db, "timur", "Plan bildirimi")).unwrap(); }
            agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
            c.leader_notes_wake("timur");
            let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
            assert_eq!(starts.len(), 1);
            if question {
                assert!(starts[0].0.args.windows(2).any(|a| a == ["--resume", "saved"]));
                let reply = post(&c, "timur", &serde_json::json!({"hook_event_name":"SessionStart","session_id":"saved","source":"resume"}));
                assert!(reply.starts_with("HTTP/1.1 200 OK") && reply.contains("karar?"), "{reply}");
                assert!(crate::questions::on_session_start(&c.db, &c.events, "timur").is_none());
            } else {
                assert!(crate::plan::with_notes(&c.db, "timur", "UserPromptSubmit", None).unwrap().contains("Plan bildirimi"));
            }
            finish(&c);
        }
    }
    struct QuestionSink(std::sync::mpsc::Sender<()>);
    impl PtySink for QuestionSink {
        fn data(&mut self, bytes: &[u8]) -> bool {
            // ConPTY waits for a cursor report before accepting any input.
            if bytes.windows(4).any(|b| b == b"\x1b[6n") {
                let _ = self.0.send(());
            }
            true
        }
        fn exit(&mut self) {}
    }

    #[test]
    fn idle_leader_gets_worker_question_text_without_a_ui_once() {
        let c = core();
        let (cmd, args) = if cfg!(windows) { ("cmd.exe", vec!["/Q".into()]) } else { ("/bin/sh", vec![]) };
        let (tx, rx) = std::sync::mpsc::channel();
        c.pty_spawn(pty::Spawn { id:"timur".into(), cmd:cmd.into(), args, cols:200, rows:80, ..Default::default() }, agents::shell_sink_for_test(&c, "timur", QuestionSink(tx))).unwrap();
        if cfg!(windows) {
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        }
        c.agent_states.hook("timur", Some(agents::Status::Idle), None);
        let d = job(&c, "running");
        let q = with(&c.db, |db| crate::questions::ask(db, &delegate::caller(db, "kaan")?,
            &serde_json::json!({"delegationId":d.id,"question":"probe decision?","options":["yes","no"],"context":"probe context"}))).unwrap();
        let _ = c.events.emit("questions-changed", &());
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        accept_pending(&c, std::time::Duration::from_secs(3));
        let state = c.agent_states.get("timur").status;
        let mut screen = c.ptys.screen("timur").unwrap();
        while !screen.contains("probe context") && std::time::Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(25));
            screen = c.ptys.screen("timur").unwrap();
        }
        let delivered = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
        c.agent_states.hook("timur", Some(agents::Status::Idle), None);
        c.leader_notes_wake("timur");
        let repeated = c.delegation_on_hook("timur", "UserPromptSubmit", &serde_json::json!({"prompt":"[Dex]"}), false);
        let after = c.agent_states.get("timur").status;
        finish(&c);
        assert_eq!(state, agents::Status::Working, "a question must start a turn without the frontend");
        assert!(screen.contains("probe decision?") && screen.contains("yes | no") && screen.contains("probe context"), "{screen}");
        assert!(delivered.is_some());
        assert_eq!(after, agents::Status::Idle, "same question must not start a second turn");
        assert!(!repeated.unwrap_or_default().contains("probe decision?"));
    }

    #[test]
    fn drain_end_delivers_held_worker_question_text_once() {
        let c = core();
        let (cmd, args) = if cfg!(windows) { ("cmd.exe", vec!["/Q".into()]) } else { ("/bin/sh", vec![]) };
        let (tx, rx) = std::sync::mpsc::channel();
        c.pty_spawn(pty::Spawn { id:"timur".into(), cmd:cmd.into(), args, cols:200, rows:80, ..Default::default() }, agents::shell_sink_for_test(&c, "timur", QuestionSink(tx))).unwrap();
        if cfg!(windows) {
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        }
        c.agent_states.hook("timur", Some(agents::Status::Idle), None);
        let d = job(&c, "running");
        c.drain(true);
        let q = with(&c.db, |db| crate::questions::ask(db, &delegate::caller(db, "kaan")?,
            &serde_json::json!({"delegationId":d.id,"question":"drain decision?","options":["yes","no"],"context":"drain context"}))).unwrap();
        // Run the question event's wake synchronously; no other event or hook retries it.
        c.wake_sleeping_leaders();
        assert_eq!(c.agent_states.get("timur").status, agents::Status::Idle);
        assert!(with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at.is_none());
        c.drain(false);
        assert!(with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at.is_none(), "Enter alone is not delivery");
        accept_pending(&c, std::time::Duration::from_secs(3));
        let state = c.agent_states.get("timur").status;
        let delivered = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut screen = c.ptys.screen("timur").unwrap();
        while !screen.contains("drain context") && std::time::Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(25));
            screen = c.ptys.screen("timur").unwrap();
        }
        c.agent_states.hook("timur", Some(agents::Status::Idle), None);
        c.drain(true);
        c.drain(false);
        let after = c.agent_states.get("timur").status;
        let repeated_stamp = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
        let repeated = c.delegation_on_hook("timur", "UserPromptSubmit", &serde_json::json!({"prompt":"[Dex]"}), false);
        finish(&c);
        assert_eq!(state, agents::Status::Working, "drain end must deliver without another event");
        assert!(screen.contains("drain decision?") && screen.contains("yes | no") && screen.contains("drain context"), "{screen}");
        assert!(delivered.is_some());
        assert_eq!(after, agents::Status::Idle, "a delivered question must not start another turn");
        assert_eq!(repeated_stamp, delivered, "delivery stamp must be preserved");
        assert!(!repeated.unwrap_or_default().contains("drain decision?"));
    }

    #[test]
    fn leader_result_waits_for_session_ready_and_prompt_acceptance() {
        let c = core(); open(&c, "timur");
        c.agent_states.spawned("timur");
        // SessionStart has read its context; a result arrives before the ready status/event.
        assert!(delegate::on_session_start(&c.db, &c.events, "timur").is_none());
        let d = job(&c, "done_no_report");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        c.wake_sleeping_leaders();
        let early = rx.try_recv().is_ok();
        c.agent_states.hook("timur", Some(agents::Status::Idle), Some("ready-session"));
        let _ = c.events.emit("dex-hook", &serde_json::json!({"agentId":"timur","event":"SessionStart","status":"idle"}));
        let pasted = rx.recv_timeout(std::time::Duration::from_secs(3)).ok();
        let enter = rx.recv_timeout(std::time::Duration::from_secs(1)).ok();
        let before = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
        let prompt = c.leader_pastes.lock().unwrap().get("timur").map(|p| p.text.clone()).unwrap_or_default();
        let response = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","session_id":"ready-session","prompt":prompt}));
        let after = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
        finish(&c);
        assert!(!early, "startup must not paste before readiness");
        assert!(pasted.is_some_and(|p| p.starts_with(b"\x1b[200~[Dex]")), "SessionStart must wake the pending result without a UI timer");
        assert_eq!(enter.as_deref(), Some(b"\r".as_slice()), "ready delivery includes Enter");
        assert!(before.is_none(), "a PTY write is not prompt acceptance");
        assert!(response.contains("RAPORSUZ"), "UPS must carry the pending result: {response}");
        assert!(after.is_some());
    }

    struct InputProbe {
        writes: std::sync::mpsc::Sender<Vec<u8>>,
        fail_enter: bool,
        _original: Box<dyn std::io::Write + Send>,
    }
    impl std::io::Write for InputProbe {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.writes.send(bytes.to_vec());
            if bytes == b"\r" && std::mem::take(&mut self.fail_enter) {
                return Err(std::io::Error::other("injected Enter failure"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn leader_enter_failure_or_missing_acceptance_retries_only_enter_once() {
        for fail_enter in [true, false] {
            let c = core(); open(&c, "timur");
            let (tx, rx) = std::sync::mpsc::channel();
            c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter, _original: original }));
            let d = job(&c, "done");
            c.leader_notes_wake("timur");
            let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
            let enter = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
            let before = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
            let retry = rx.recv_timeout(std::time::Duration::from_secs(4)).unwrap();
            let prompt = c.leader_pastes.lock().unwrap().get("timur").unwrap().text.clone();
            let reply = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
            let stamp = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
            c.agent_states.hook("timur", Some(agents::Status::Idle), None);
            c.leader_notes_wake("timur");
            let extra = rx.recv_timeout(std::time::Duration::from_millis(2300)).is_ok();
            let repeated = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
            let after = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
            finish(&c);
            assert!(text.starts_with(b"\x1b[200~[Dex]"));
            assert_eq!(enter, b"\r");
            assert_eq!(retry, b"\r", "retry must not append a second copy of the text");
            assert!(before.is_none());
            assert!(reply.contains("biten devretmeler") && stamp.is_some());
            assert!(!extra, "acceptance must cancel the acknowledgement deadline");
            assert!(!repeated.contains("biten devretmeler"));
            assert_eq!(stamp, after, "one delivery stamp");
        }
    }

    #[test]
    fn patron_input_cancels_leader_paste_until_context_delivery_once() {
        // The UI reports the same empty draft for Backspace, Esc and Ctrl+U;
        // a nonempty draft must relinquish ownership just as well.
        for (key, draft) in [("Backspace", false), ("Esc", false), ("Ctrl+U", false), ("typing", true)] {
            let c = core(); open(&c, "timur");
            let (tx, rx) = std::sync::mpsc::channel();
            c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
            let d = job(&c, "done");
            let work = job(&c, "running");
            let q = with(&c.db, |db| crate::questions::ask(db, &delegate::caller(db, "kaan")?,
                &serde_json::json!({"delegationId":work.id,"question":"pending decision"}))).unwrap();
            with(&c.db, |db| crate::plan::note(db, "timur", "pending note")).unwrap();
            c.leader_notes_wake("timur");
            let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
            assert!(text.starts_with(b"\x1b[200~[Dex]"));
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), b"\r");
            assert!(with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at.is_none());

            c.agent_input("timur", draft);
            let extra_before = rx.recv_timeout(std::time::Duration::from_millis(7500)).is_ok();
            let pending = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at.is_none();
            let prompt = "patron's next message";
            let reply = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
            let stamp = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
            c.agent_states.hook("timur", Some(agents::Status::Idle), None);
            c.leader_notes_wake("timur");
            let extra = rx.recv_timeout(std::time::Duration::from_millis(2300)).is_ok();
            let repeated = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
            let after = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
            let question_stamp = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
            finish(&c);
            assert!(!extra_before, "{key}: patron input must cancel both paste and Enter retries");
            assert!(pending, "input must not acknowledge delivery");
            assert!(reply.contains("biten devretmeler") && stamp.is_some());
            assert!(reply.contains("pending decision") && reply.contains("pending note") && question_stamp.is_some());
            assert!(!extra, "acceptance must cancel further retries");
            assert!(!repeated.contains("biten devretmeler"));
            assert!(!repeated.contains("pending decision") && !repeated.contains("pending note"));
            assert_eq!(stamp, after, "one delivery stamp");
        }
    }

    #[test]
    fn session_start_context_cancels_old_paste_and_keeps_only_pending_notes() {
        let c = core(); open(&c, "timur");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        let d = job(&c, "done");
        with(&c.db, |db| crate::plan::note(db, "timur", "remaining note")).unwrap();
        c.leader_notes_wake("timur");
        let old = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), b"\r");
        let reply = post(&c, "timur", &serde_json::json!({"hook_event_name":"SessionStart","session_id":"new-session"}));
        let next = rx.recv_timeout(std::time::Duration::from_secs(4)).unwrap();
        let next_text = String::from_utf8_lossy(&next);
        let stamp = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
        finish(&c);
        assert!(String::from_utf8_lossy(&old).contains("Devretme sonucu"));
        assert!(reply.contains("biten devretmeler") && stamp.is_some());
        assert!(next_text.contains("remaining note"), "the new session must get a new paste: {next_text:?}");
        assert!(!next_text.contains("Devretme sonucu"), "context delivery must not be repeated");
    }

    #[test]
    fn patron_prompt_delivers_a_replaced_leader_paste_through_context() {
        let c = core(); open(&c, "timur");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        let d = job(&c, "done");
        let work = job(&c, "running");
        let q = with(&c.db, |db| {
            crate::questions::ask(db, &delegate::caller(db, "kaan")?, &serde_json::json!({"delegationId":work.id,"question":"retained question"}))
        }).unwrap();
        with(&c.db, |db| crate::plan::note(db, "timur", "retained note")).unwrap();
        c.leader_notes_wake("timur");
        let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), b"\r");
        let reply = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":"patron prompt"}));
        c.leader_notes_wake("timur");
        let busy_write = rx.try_recv().is_ok();
        crate::hooks::on_interrupt(&c, "timur");
        let retried = rx.recv_timeout(std::time::Duration::from_millis(2300)).is_ok();
        let repeated = post(&c, "timur", &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":"another patron prompt"}));
        let stamp = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
        let delivered = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
        finish(&c);
        assert!(text.starts_with(b"\x1b[200~[Dex]"));
        assert!(reply.contains("retained note") && reply.contains("retained question") && reply.contains("biten devretmeler"));
        assert!(!busy_write, "do not paste into the patron's working turn");
        assert!(!retried, "the replaced draft must never be pasted again");
        assert!(stamp.is_some() && delivered.is_some());
        assert!(!repeated.contains("retained") && !repeated.contains("biten devretmeler"));
    }

    #[test]
    fn patron_cancellation_waits_for_an_enter_already_being_written() {
        struct EnterProbe {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
            _original: Box<dyn std::io::Write + Send>,
        }
        impl std::io::Write for EnterProbe {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes == b"\r" {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let c = core(); open(&c, "timur");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(EnterProbe { entered: entered_tx, release: release_rx, _original: original }));
        let _d = job(&c, "done");
        let sending = c.clone();
        let sender = std::thread::spawn(move || sending.leader_notes_wake("timur"));
        entered_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let input = c.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let patron = std::thread::spawn(move || { input.agent_input("timur", true); done_tx.send(()).unwrap(); });
        // agent_input updates typing before trying to cancel the paste: it reached the lock.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !c.agent_states.typing("timur") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let typing = c.agent_states.typing("timur");
        let cancelled_before_write = done_rx.recv_timeout(std::time::Duration::from_millis(100)).is_ok();
        release_tx.send(()).unwrap();
        sender.join().unwrap();
        patron.join().unwrap();
        finish(&c);
        assert!(typing, "patron input must reach cancellation while Enter is being written");
        assert!(!cancelled_before_write, "cancellation must serialize after an Enter that already owns the paste");
    }

    #[test]
    fn patron_input_at_enter_submission_cancels_the_write() {
        let c = core(); open(&c, "timur");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        let d = job(&c, "done");
        let input = c.clone();
        *c.paste_enter_pause.lock().unwrap() = Some(Box::new(move || input.agent_input("timur", false)));
        c.leader_notes_wake("timur");
        let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        let extra = rx.recv_timeout(std::time::Duration::from_millis(2300)).is_ok();
        let pending = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at.is_none();
        finish(&c);
        assert!(text.starts_with(b"\x1b[200~[Dex]"));
        assert!(pending, "cancelled input must keep the result pending");
        assert!(!extra, "patron input at submission must cancel Enter and its retry");
    }

    #[test]
    fn acceptance_during_redraw_cancels_the_late_enter() {
        let c = core(); open(&c, "timur");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        let d = job(&c, "done");
        let (paused_tx, paused_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || { paused_tx.send(()).unwrap(); go_rx.recv().unwrap(); }));
        let copying = c.clone();
        let sender = std::thread::spawn(move || copying.leader_notes_wake("timur"));
        paused_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        // The patron manually submitted the already pasted text while Dex waited for redraw.
        accept_pending(&c, std::time::Duration::from_secs(1));
        go_tx.send(()).unwrap();
        sender.join().unwrap();
        let extra = rx.recv_timeout(std::time::Duration::from_millis(2300)).is_ok();
        let stamp = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at;
        finish(&c);
        assert!(text.starts_with(b"\x1b[200~[Dex]"));
        assert!(stamp.is_some());
        assert!(!extra, "a confirmed prompt must cancel both the late Enter and timeout retry");
    }

    #[test]
    fn patron_input_during_redraw_cancels_enter_and_delivers_at_stop() {
        let c = core(); open(&c, "timur");
        let (tx, rx) = std::sync::mpsc::channel();
        c.ptys.input_writer_for_test("timur", |original| Box::new(InputProbe { writes: tx, fail_enter: false, _original: original }));
        let d = job(&c, "done");
        with(&c.db, |db| crate::plan::note(db, "timur", "stop note")).unwrap();
        let (paused_tx, paused_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || { paused_tx.send(()).unwrap(); go_rx.recv().unwrap(); }));
        let copying = c.clone();
        let sender = std::thread::spawn(move || copying.leader_notes_wake("timur"));
        paused_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let text = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        c.agent_input("timur", false);
        go_tx.send(()).unwrap();
        sender.join().unwrap();
        let extra = rx.recv_timeout(std::time::Duration::from_millis(7500)).is_ok();
        let pending = with(&c.db, |db| delegate::get(db, d.id)).unwrap().delivered_at.is_none();
        c.agent_states.hook("timur", Some(agents::Status::Working), Some("leader-session"));
        let reply = c.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false).unwrap();
        // The CLI's transcript confirms acceptance before the continuation hook.
        crate::claude_turn::tests::accepted_stop(&c, "timur");
        c.claude_hook_received("timur", "PreToolUse", Some("leader-session"), agents::next_seq());
        let repeated = c.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false).unwrap_or_default();
        finish(&c);
        assert!(text.starts_with(b"\x1b[200~[Dex]"));
        assert!(!extra, "input during redraw cancels the late Enter and all retries");
        assert!(pending);
        assert!(reply.contains("biten devretmeler") && reply.contains("stop note"));
        assert!(!repeated.contains("biten devretmeler") && !repeated.contains("stop note"));
    }

    #[test]
    fn worker_question_waits_for_patron_draft_then_busy_turn_stop() {
        let c = core(); open(&c, "timur");
        let d = job(&c, "running");
        let ask = |text: &str| with(&c.db, |db| crate::questions::ask(db, &delegate::caller(db, "kaan")?,
            &serde_json::json!({"delegationId":d.id,"question":text}))).unwrap();
        c.agent_input("timur", true);
        let q = ask("draft decision?");
        let _ = c.events.emit("questions-changed", &());
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(c.agent_states.get("timur").status, agents::Status::Idle);
        assert!(with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at.is_none());
        c.agent_input("timur", false);
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(12);
        accept_pending(&c, limit.saturating_duration_since(std::time::Instant::now()));
        let after_draft = c.agent_states.get("timur").status;
        let first_delivered = with(&c.db, |db| crate::questions::get(db, q.id)).unwrap().leader_delivered_at;
        let busy = ask("busy decision?");
        let _ = c.events.emit("questions-changed", &());
        std::thread::sleep(std::time::Duration::from_millis(150));
        let busy_delivered = with(&c.db, |db| crate::questions::get(db, busy.id)).unwrap().leader_delivered_at;
        c.agent_states.hook("timur", Some(agents::Status::Working), Some("leader-session"));
        let reply = c.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false).unwrap();
        // The CLI's transcript confirms acceptance before the continuation hook.
        crate::claude_turn::tests::accepted_stop(&c, "timur");
        c.claude_hook_received("timur", "PreToolUse", Some("leader-session"), agents::next_seq());
        let repeated = c.delegation_on_hook("timur", "Stop", &serde_json::json!({}), false);
        finish(&c);
        assert_eq!(after_draft, agents::Status::Working);
        assert!(first_delivered.is_some());
        assert!(busy_delivered.is_none(), "a running patron turn must not be pasted into");
        assert!(crate::is_blocked(&Some(reply.clone())) && reply.contains("busy decision?"));
        assert!(!reply.contains("draft decision?"));
        assert!(!repeated.unwrap_or_default().contains("busy decision?"));
    }
    #[test]
    fn timer_expires_without_polling_and_manual_start_clears_sleep_mark() {
        let c = core(); open(&c, "timur"); expire(&c);
        c.team_idle.lock().unwrap()["default"].reset.send(Some(std::time::Instant::now())).unwrap();
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while c.ptys.is_running("timur") && std::time::Instant::now() < limit { std::thread::sleep(std::time::Duration::from_millis(25)); }
        assert!(!c.ptys.is_running("timur"));
        agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
        c.agent_start("timur", agents::StartOptions::default()).unwrap();
        let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
        assert_eq!(starts.len(), 1);
        assert!(!c.sleeping_leaders.lock().unwrap().contains("timur"));
        finish(&c);
    }

    #[test]
    fn admitted_turn_and_unread_result_keep_the_leader_open() {
        let c = core(); open(&c, "timur"); expire(&c);
        let turn = c.admit("timur").unwrap();
        assert!(!c.leader_close_unseen("default"));
        drop(turn);
        job(&c, "done");
        assert!(!c.leader_close_unseen("default"), "a result still needs the leader");
        finish(&c);
    }

    #[test]
    fn reviewer_closes_after_valid_verdict_and_final_stop_only() {
        let c = core(); open(&c, "kaan");
        let target = job(&c, "done");
        let review = job(&c, "running");
        with(&c.db, |db| {
            db.execute("INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(?1,?2,'fixture-sha',1)", rusqlite::params![review.id,target.id]).map_err(|e|e.to_string())?;
            Ok(())
        }).unwrap();
        c.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":delegate::trigger_line(review.id)}), false);
        let report = |summary| crate::mcp::call_with_args_for_test(&c, "kaan", "report", serde_json::json!({"delegationId":review.id,"summary":summary}));
        assert_eq!(report("bitti: eksik karar")["isError"], true);
        assert!(!c.reported_workers.lock().unwrap().contains("kaan"));
        assert_ne!(report("karar: onay")["isError"], true);
        assert_eq!(with(&c.db, |db| crate::review::get(db, review.id)).unwrap().unwrap().verdict.as_deref(), Some("onay"));
        // The transcript's Interrupt/Settled substitute may dispatch a queue, but cannot kill
        // a reported worker before its real Stop (memory writes can still be in progress).
        c.delegation_after_stop("kaan");
        std::thread::sleep(std::time::Duration::from_millis(850));
        assert!(c.ptys.is_running("kaan"));
        with(&c.db, |db| crate::plan::note(db, "kaan", "son not")).unwrap();
        c.agent_states.hook("kaan", Some(agents::Status::Working), Some("reviewer-session"));
        let blocked = c.delegation_on_hook("kaan", "Stop", &serde_json::json!({}), false);
        assert!(crate::is_blocked(&blocked));
        assert!(c.ptys.is_running("kaan"));
        crate::claude_turn::tests::accepted_stop(&c, "kaan");
        c.claude_hook_received("kaan", "PreToolUse", Some("reviewer-session"), agents::next_seq());
        let out = c.delegation_on_hook("kaan", "Stop", &serde_json::json!({}), false);
        assert!(!crate::is_blocked(&out), "{out:?}");
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while c.ptys.is_running("kaan") && std::time::Instant::now() < limit { std::thread::sleep(std::time::Duration::from_millis(25)); }
        assert!(!c.ptys.is_running("kaan"));
        finish(&c);
    }

    #[test]
    fn an_outbound_service_job_keeps_its_requesting_leader_open() {
        let c = core(); open(&c, "timur"); expire(&c);
        let service = c.team_create(teams::NewTeam { name:"servis".into(),kind:"service".into(),cwd:None,worker_roles:None,members:None }).unwrap();
        let worker = c.agent_list().unwrap().into_iter().find(|a|a.team_id==service.id && a.kind=="worker").unwrap();
        with(&c.db, |db| {
            let from = delegate::caller(db, "timur")?;
            delegate::insert(db, delegate::NewDelegation {from:&from,to_agent:worker.id.clone(),team_id:"default".into(),service_team_id:Some(service.id.clone()),prompt:"iş".into(),task_id:None,patron_words:None,effort:None})
        }).unwrap();
        assert!(!c.leader_close_unseen("default"));
        finish(&c);
    }

    #[test]
    fn expired_leader_closes_after_transient_stop_failure() {
        let c = core(); open(&c, "timur"); expire(&c);
        with(&c.db, |db| agents::record_session(db, "timur", "leader-saved", "claude", None)).unwrap();
        c.agent_states.hook("timur", Some(agents::Status::Working), None);
        assert!(!c.leader_close_unseen("default"), "expiry waits for the turn");
        let body = serde_json::json!({"hook_event_name":"StopFailure", "error":"server_error", "last_assistant_message":"API Error: 503 Service Unavailable"});
        assert!(c.chain_plan("timur", &body).is_none());
        let response = post(&c, "timur", &body);
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while c.ptys.is_running("timur") && std::time::Instant::now() < limit { std::thread::sleep(std::time::Duration::from_millis(25)); }
        let running = c.ptys.is_running("timur");
        let sleeping = c.sleeping_leaders.lock().unwrap().contains("timur");
        let saved = with(&c.db, |db| agents::resumable_session(db, "timur", "claude")).unwrap();
        finish(&c);
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
        assert!(!running, "a transient StopFailure must reevaluate the expired idle leader");
        assert!(sleeping);
        assert_eq!(saved.as_deref(), Some("leader-saved"));
    }

    #[test]
    fn expired_leader_closes_after_stop_interrupt_and_settled() {
        for event in ["Stop", "Interrupt", "Settled"] {
            let c = core(); open(&c, "timur"); expire(&c);
            c.agent_states.hook("timur", Some(agents::Status::Working), None);
            assert!(!c.leader_close_unseen("default"));
            if event == "Stop" {
                assert!(post(&c, "timur", &serde_json::json!({"hook_event_name":event})).starts_with("HTTP/1.1 204"));
            } else { crate::hooks::turn_ended(&c, "timur", event); }
            let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while c.ptys.is_running("timur") && std::time::Instant::now() < limit { std::thread::sleep(std::time::Duration::from_millis(25)); }
            let running = c.ptys.is_running("timur");
            let sleeping = c.sleeping_leaders.lock().unwrap().contains("timur");
            finish(&c);
            assert!(!running, "{event} must reevaluate the expired leader");
            assert!(sleeping, "{event}");
        }
    }

    #[test]
    fn reported_worker_stop_failure_still_requires_a_real_stop() {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        c.agent_states.hook("kaan", Some(agents::Status::Working), None);
        let response = post(&c, "kaan", &serde_json::json!({"hook_event_name":"StopFailure", "error":"server_error", "last_assistant_message":"API Error: 503 Service Unavailable"}));
        std::thread::sleep(std::time::Duration::from_millis(850));
        let running = c.ptys.is_running("kaan");
        let idle = c.agent_states.get("kaan").status == agents::Status::Idle;
        finish(&c);
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
        assert!(idle);
        assert!(running, "agent-idle cannot close a report without a real Stop");
    }

    #[test]
    fn reported_worker_closes_during_another_agents_paste() {
        let c = core(); open(&c, "kaan"); open(&c, "timur");
        let d = job(&c, "done"); c.worker_delivered(&d);
        let (written, wait_written) = std::sync::mpsc::channel();
        let (resume, wait_resume) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || {
            written.send(()).unwrap(); wait_resume.recv().unwrap();
        }));
        let pasting = c.clone();
        let paster = std::thread::spawn(move || pasting.paste_prompt("timur", "[Dex] teslim"));
        wait_written.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let closed = c.worker_close_delivered("kaan");
        let running = c.ptys.is_running("kaan");
        resume.send(()).unwrap();
        let delivered = paster.join().unwrap();
        finish(&c);
        assert!(closed, "another agent's delivery must not defer the report close");
        assert!(!running, "the reported worker must close before the other Enter");
        assert!(delivered.is_ok(), "{delivered:?}");
    }

    #[test]
    fn reported_worker_retries_close_when_its_last_delivery_finishes() {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        let (written, wait_written) = std::sync::mpsc::channel();
        let (resume, wait_resume) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || {
            written.send(()).unwrap(); wait_resume.recv().unwrap();
        }));
        let pasting = c.clone();
        let paster = std::thread::spawn(move || {
            let turn = pasting.admit("kaan").unwrap();
            let delivered = pasting.paste_admitted("kaan", "[Dex] teslim", &turn);
            // The shell has no hooks: settle after Enter, without sending another Stop.
            pasting.agent_states.hook("kaan", Some(agents::Status::Idle), None);
            delivered
        });
        wait_written.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let extra_turn = c.admit("kaan").unwrap();
        let closed_during_paste = c.worker_close_delivered("kaan");
        drop(extra_turn);
        let running_during_paste = c.ptys.is_running("kaan");
        resume.send(()).unwrap();
        let delivered = paster.join().unwrap();
        let running_after_delivery = c.ptys.is_running("kaan");
        finish(&c);
        assert!(!closed_during_paste, "the worker must stay open through its Enter");
        assert!(running_during_paste, "only the last admitted delivery can retry close");
        assert!(delivered.is_ok(), "{delivered:?}");
        assert!(!running_after_delivery, "Turn release must retry close without another Stop");
    }

    #[test]
    fn reported_worker_keeps_queued_chat_until_enter_is_delivered() {
        assert_reported_worker_delivers_queued_chat("[Dex] teslim", true);
    }

    #[test]
    fn reported_worker_keeps_settled_patron_chat_open() {
        assert_reported_worker_delivers_queued_chat("patron teslim", false);
    }

    fn assert_reported_worker_delivers_queued_chat(prompt: &str, dex: bool) {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "running");
        c.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":delegate::trigger_line(d.id)}), false);
        c.agent_states.hook("kaan", Some(agents::Status::Working), None);
        assert_eq!(c.chat_send("kaan", prompt), Ok("queued"));
        with(&c.db, |db| delegate::set_status(db, d.id, "done")).unwrap();
        c.worker_delivered(&d);
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        // Exercise the real queue delivery, pausing after text but before Enter.
        let (written, wait_written) = std::sync::mpsc::channel();
        let (resume, wait_resume) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || {
            written.send(()).unwrap(); wait_resume.recv().unwrap();
        }));
        c.chat_on_stop("kaan");
        wait_written.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let empty = c.chat_queues.lock().unwrap()["kaan"].is_empty();
        let closed_during_paste = c.worker_close_delivered("kaan");
        resume.send(()).unwrap();
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while c.admitted.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let delivered = c.agent_states.get("kaan").status == agents::Status::Working;
        let owner = c.turns.dex_turn("kaan");
        let queued = c.chat_queues.lock().unwrap()["kaan"].len();
        // The shell fixture has no hooks: settle the delivered turn explicitly.
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let closed_after_delivery = c.worker_close_delivered("kaan");
        let running_after_delivery = c.ptys.is_running("kaan");
        finish(&c);
        assert!(empty, "delivery has already removed the text from the queue");
        assert!(!closed_during_paste, "report must keep the PTY through Enter");
        assert!(delivered, "Enter must succeed");
        assert_eq!(queued, 0, "the message must not be stranded in the queue");
        assert_eq!(owner, Some(dex), "Enter must claim the queued prompt's owner without a hook");
        // Paste now records ownership: only a Dex turn may close a reported worker.
        // A settled patron conversation must remain open for follow-up messages.
        assert_eq!(closed_after_delivery, dex, "only a settled Dex turn can close after delivery");
        assert_eq!(running_after_delivery, !dex, "a settled patron conversation stays open");
    }

    #[test]
    fn reported_worker_keeps_chat_queued_until_delivery_is_admitted() {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        c.agent_states.hook("kaan", Some(agents::Status::Working), None);
        assert_eq!(c.chat_send("kaan", "patron teslim"), Ok("queued"));
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let closing = c.weak_self.clone();
        let (checked, wait_checked) = std::sync::mpsc::channel();
        *c.admit_pause.lock().unwrap() = Some(Box::new(move || {
            checked.send(closing.upgrade().unwrap().worker_close_delivered("kaan")).unwrap();
        }));
        c.chat_on_stop("kaan");
        let closed = wait_checked.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while c.agent_states.get("kaan").status != agents::Status::Working
            && std::time::Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let delivered = c.agent_states.get("kaan").status == agents::Status::Working;
        finish(&c);
        assert!(!closed, "queue protection must overlap admission protection");
        assert!(delivered, "the queued message must reach Enter");
    }

    #[test]
    fn reported_worker_dispatches_a_job_arriving_during_close_without_reentering_admission() {
        struct QueueOnClose(std::sync::Weak<Core>, bool);
        impl PtySink for QueueOnClose {
            fn data(&mut self, bytes: &[u8]) -> bool {
                if !self.1 && String::from_utf8_lossy(bytes).contains("iş teslim edildi") {
                    self.1 = true;
                    job(&self.0.upgrade().unwrap(), "queued");
                }
                true
            }
            fn exit(&mut self) {}
        }
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        c.ptys.subscribe("kaan", QueueOnClose(c.weak_self.clone(), false)).unwrap();
        let closing = c.clone();
        let (done, wait_done) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
            let closed = closing.worker_close_delivered("kaan");
            let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
            done.send((closed, starts.len())).unwrap();
        });
        let result = wait_done.recv_timeout(std::time::Duration::from_secs(5));
        finish(&c);
        assert!(result.is_ok(), "the synchronous exit must not reacquire admission: {result:?}");
        closer.join().unwrap();
        assert_eq!(result.unwrap(), (true, 1), "the late job must start after close");
    }

    #[test]
    fn patron_chat_starts_a_worker_closed_just_before_admission() {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        with(&c.db, |db| agents::record_session(db, "kaan", "worker-saved", "claude", None)).unwrap();
        let closing = c.weak_self.clone();
        *c.admit_pause.lock().unwrap() = Some(Box::new(move || {
            assert!(closing.upgrade().unwrap().worker_close_delivered("kaan"));
        }));
        agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
        let delivered = c.chat_send("kaan", "patron mesajı");
        let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
        let running = c.ptys.is_running("kaan");
        finish(&c);
        assert_eq!(delivered, Ok("started"));
        assert!(running);
        assert_eq!(starts.len(), 1);
        assert!(starts[0].0.args.windows(2).any(|a| a == ["--resume", "worker-saved"]));
        assert_eq!(starts[0].0.args.last().unwrap(), "patron mesajı");
    }

    #[test]
    fn a_patron_paste_admitted_after_the_report_close_check_prevents_close() {
        let c = core(); open(&c, "kaan");
        let d = job(&c, "done"); c.worker_delivered(&d);
        let (checked, wait_checked) = std::sync::mpsc::channel();
        let (continue_close, wait_close) = std::sync::mpsc::channel();
        *c.close_pause.lock().unwrap() = Some(Box::new(move || {
            checked.send(()).unwrap(); wait_close.recv().unwrap();
        }));
        let closing = c.clone();
        let closer = std::thread::spawn(move || closing.worker_close_delivered("kaan"));
        wait_checked.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let (written, wait_written) = std::sync::mpsc::channel();
        let (continue_paste, wait_paste) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || {
            written.send(()).unwrap(); wait_paste.recv().unwrap();
        }));
        let pasting = c.clone();
        let paster = std::thread::spawn(move || pasting.chat_send("kaan", "patron teslim"));
        wait_written.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        continue_close.send(()).unwrap();
        let closed = closer.join().unwrap();
        continue_paste.send(()).unwrap();
        let delivered = paster.join().unwrap();
        c.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":"patron teslim"}), false);
        c.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let conversation_closed = c.worker_close_delivered("kaan");
        finish(&c);
        assert!(!closed, "an admitted patron paste keeps the PTY through Enter");
        assert_eq!(delivered, Ok("sent"));
        assert!(!conversation_closed, "the patron's conversation still stays open");
    }

    #[test]
    fn patron_chat_resumes_a_leader_closed_just_before_admission() {
        let c = core(); open(&c, "timur"); expire(&c);
        with(&c.db, |db| agents::record_session(db, "timur", "leader-saved", "claude", None)).unwrap();
        let closing = c.weak_self.clone();
        *c.admit_pause.lock().unwrap() = Some(Box::new(move || {
            assert!(closing.upgrade().unwrap().leader_close_unseen("default"));
        }));
        agents::CAPTURE.with(|cap| *cap.borrow_mut() = Some(Vec::new()));
        let delivered = c.chat_send("timur", "patron mesajı");
        let starts = agents::CAPTURE.with(|cap| cap.borrow_mut().take().unwrap());
        let running = c.ptys.is_running("timur");
        let sleeping = c.sleeping_leaders.lock().unwrap().contains("timur");
        finish(&c);
        assert_eq!(delivered, Ok("started"));
        assert!(running);
        assert!(!sleeping);
        assert_eq!(starts.len(), 1);
        assert!(starts[0].0.args.windows(2).any(|a| a == ["--resume", "leader-saved"]));
        assert_eq!(starts[0].0.args.last().unwrap(), "patron mesajı");
    }

    #[test]
    fn a_paste_admitted_after_the_first_idle_check_prevents_close() {
        let c = core(); open(&c, "timur"); expire(&c);
        let (checked, wait_checked) = std::sync::mpsc::channel();
        let (continue_close, wait_close) = std::sync::mpsc::channel();
        *c.close_pause.lock().unwrap() = Some(Box::new(move || {
            checked.send(()).unwrap(); wait_close.recv().unwrap();
        }));
        let closing = c.clone();
        let closer = std::thread::spawn(move || closing.leader_close_unseen("default"));
        wait_checked.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        let (written, wait_written) = std::sync::mpsc::channel();
        let (continue_paste, wait_paste) = std::sync::mpsc::channel();
        *c.paste_pause.lock().unwrap() = Some(Box::new(move || {
            written.send(()).unwrap(); wait_paste.recv().unwrap();
        }));
        let pasting = c.clone();
        let paster = std::thread::spawn(move || {
            let turn = pasting.admit("timur").unwrap();
            pasting.paste_admitted("timur", "[Dex] teslim", &turn)
        });
        wait_written.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        continue_close.send(()).unwrap();
        let closed = closer.join().unwrap();
        continue_paste.send(()).unwrap();
        let delivered = paster.join().unwrap();
        // Release and join both threads before assertions, including on RED.
        assert!(!closed, "an admitted paste keeps the PTY");
        assert!(delivered.is_ok(), "{delivered:?}");
        assert!(c.ptys.is_running("timur"));
        finish(&c);
    }
}

pub const FILE: &str = "dex.restart.json";
/// A restart's new core is up within seconds; a marker older than this is a restart that failed.
pub const FRESH_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub at_ms: u64,
    /// Leaders whose session ran: started again with `--resume`.
    pub leaders: Vec<String>,
    /// #447: the patron's chat still queued (held by the gate, or the agent was busy): the next
    /// core starts each agent with it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub chat: std::collections::BTreeMap<String, Vec<String>>,
}

pub fn write(data_dir: &Path, m: &Marker) -> Result<(), String> {
    let path = data_dir.join(FILE);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// The marker, once: it is removed whatever it says. None: none, unreadable, or stale.
pub fn take(data_dir: &Path) -> Option<Marker> {
    let path = data_dir.join(FILE);
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let m: Marker = serde_json::from_slice(&bytes).ok()?;
    (crate::diagnostics::now_ms().saturating_sub(m.at_ms) <= FRESH_MS).then_some(m)
}

/// Per-agent deliveries and the report close deferred until their final release.
#[derive(Default)]
pub(crate) struct Admission {
    turns: usize,
    pub(crate) close_pending: bool,
}

/// #427/#447: one turn being started, past the gate ([`Core::admit`]), counted while it lives
/// ([`Core::drain_blockers`]). Its holder drops it once the turn shows, or failed.
pub(crate) struct Turn<'a> {
    core: &'a Core,
    agent: String,
}

/// #447: what a turn-starting path returns when the gate held it ([`Core::admit`]).
pub(crate) const HELD: &str = "çekirdek yeniden başlamak üzere; tur başlatılmadı";

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut admitted = self.core.admission_gate.lock().unwrap();
        let admission = admitted.get_mut(&self.agent).expect("live admitted turn");
        admission.turns -= 1;
        let retry_close = admission.turns == 0 && admission.close_pending;
        if admission.turns == 0 { admitted.remove(&self.agent); }
        self.core.admitted.fetch_sub(1, Ordering::SeqCst);
        drop(admitted);
        // The final release owns the retry; close/exit may admit the next job itself.
        if retry_close { self.core.worker_close_delivered(&self.agent); }
    }
}

impl Core {
    /// Drain on: jobs wait in the queue. Off: the queue is delivered again.
    pub fn drain(&self, on: bool) {
        let was = self.draining.swap(on, Ordering::SeqCst);
        crate::diagnostics::event("drain", serde_json::json!({ "on": on }));
        if was && !on {
            self.dispatch_queued();
            self.deliver_held();
        }
    }

    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// #447: the one gate of every path that starts an agent's turn. Counted before the drain check
    /// (both SeqCst; [`Core::blockers`] reads them the other way round): a drain on before it holds
    /// the turn (None); one that comes on after it sees this turn until it shows.
    pub(crate) fn admit(&self, agent: &str) -> Option<Turn<'_>> {
        #[cfg(test)]
        if let Some(pause) = self.admit_pause.lock().unwrap().take() { pause(); }
        let mut admitted = self.admission_gate.lock().unwrap();
        self.admitted.fetch_add(1, Ordering::SeqCst);
        admitted.entry(agent.into()).or_default().turns += 1;
        let turn = Turn { core: self, agent: agent.into() };
        let held = self.draining();
        drop(admitted);
        (!held).then_some(turn)
    }

    /// What keeps the core from a clean restart; empty = drained. Short lines for the patron.
    pub fn drain_blockers(&self) -> Result<Vec<String>, String> {
        self.blockers(false)
    }

    /// #400: what keeps a core from a restart that holds nothing back (a dev update waits for this
    /// instead of draining): [`Core::drain_blockers`] in every team, and jobs still in the queue.
    pub fn quiet_blockers(&self) -> Result<Vec<String>, String> {
        self.blockers(true)
    }

    /// #427: a turn past the gate comes first ([`Core::admit`]: its job is not `running`, its agent
    /// not Working yet) — read before the states its holder sets; the jobs (running, and queued if
    /// `queued`) and open questions are one read of the DB, so a job moving from the queue to
    /// running in between can't slip past both.
    fn blockers(&self, queued: bool) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let n = self.admitted.load(Ordering::SeqCst);
        if n > 0 {
            out.push(format!("{n} tur başlatılıyor"));
        }
        let (jobs, questions) = with(&self.db, |c| {
            let e = |e: rusqlite::Error| e.to_string();
            let jobs = c
                .prepare(
                    "SELECT id, to_agent, status FROM delegations
                     WHERE status = 'running' OR (?1 AND status = 'queued') ORDER BY status = 'queued', id",
                )
                .map_err(e)?
                .query_map([queued], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                .map_err(e)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(e)?;
            let questions = c
                .prepare("SELECT id FROM questions WHERE status IN ('open','escalated') ORDER BY id")
                .map_err(e)?
                .query_map([], |r| r.get::<_, i64>(0))
                .map_err(e)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(e)?;
            Ok((jobs, questions))
        })?;
        let (running, waiting): (Vec<_>, Vec<_>) = jobs.into_iter().partition(|(_, _, s)| s == "running");
        out.extend(running.into_iter().map(|(id, to, _)| format!("#{id} {to} üzerinde koşuyor")));
        out.extend(questions.into_iter().map(|id| format!("soru #{id} cevap bekliyor")));
        for s in self.agent_states.list() {
            if s.in_turn() && self.ptys.is_running(&s.agent_id) {
                out.push(format!("{} çalışıyor", s.agent_id));
            }
        }
        out.extend(self.chain_guard.lock().unwrap().iter().map(|id| format!("{id} sağlayıcı değiştiriyor")));
        out.extend(waiting.into_iter().map(|(id, to, _)| format!("#{id} {to} için kuyrukta")));
        Ok(out)
    }

    /// Just before a restart's exit: the marker, with the leaders running now.
    pub fn restart_mark(&self) -> Result<Marker, String> {
        let leaders = with(&self.db, |c| {
            Ok(self
                .ptys
                .list()
                .into_iter()
                .filter(|p| p.kind == "agent" && teams::get_agent(c, &p.id).is_ok_and(|a| a.kind == "leader"))
                .map(|p| p.id)
                .collect::<Vec<_>>())
        })?;
        let chat = self
            .chat_queues
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(a, q)| (a.clone(), q.iter().cloned().collect()))
            .collect();
        let m = Marker { at_ms: crate::diagnostics::now_ms(), leaders, chat };
        write(&self.paths.data_dir, &m)?;
        crate::diagnostics::event("restart_mark", serde_json::json!({ "leaders": m.leaders, "chat": m.chat.len() }));
        Ok(m)
    }

    /// After a start that found a marker: its leaders again (their saved session; their queued chat
    /// is the prompt, else their unsent notes go in once they are up), the other agents with queued
    /// chat (#447), then the kept queue.
    pub(crate) fn restart_resume(&self, mut m: Marker) {
        for id in &m.leaders {
            let leader = with(&self.db, |c| Ok(teams::get_agent(c, id).is_ok_and(|a| a.kind == "leader"))).unwrap_or(false);
            if !leader {
                continue;
            }
            let r = self.chat_start(id, m.chat.remove(id));
            crate::diagnostics::event("restart_resume", serde_json::json!({ "agent": id, "ok": r.is_ok() }));
            match r {
                Ok(false) => self.leader_notes_wake(id),
                Ok(true) => {}
                Err(e) => eprintln!("dex: {id}: yeniden başlatma sonrası açılamadı: {e}"),
            }
        }
        for (id, chat) in m.chat {
            if let Err(e) = self.chat_start(&id, Some(chat)) {
                eprintln!("dex: {id}: sıradaki sohbet yeniden başlatma sonrası iletilemedi: {e}");
            }
        }
        self.dispatch_queued();
    }

    /// #447: `chat` (queued before a restart, or while the gate held it) goes in as the start's
    /// prompt; `true` if there was some. Not started: back into the agent's queue.
    fn chat_start(&self, id: &str, chat: Option<Vec<String>>) -> Result<bool, String> {
        let chat = chat.filter(|c| !c.is_empty());
        let prompt = chat.as_ref().map(|c| c.join("\n\n"));
        match self.agent_start(id, agents::StartOptions { prompt, ..Default::default() }) {
            Ok(_) => Ok(chat.is_some()),
            Err(e) => {
                if let Some(chat) = chat {
                    let mut queues = self.chat_queues.lock().unwrap();
                    let queue = queues.entry(id.into()).or_default();
                    queue.extend(chat);
                    self.emit_queue(id, queue.make_contiguous());
                }
                Err(e)
            }
        }
    }

    /// #447: a drain taken back — what the gate held goes in now: chat (an open agent's next idle
    /// moment, a closed agent's start), unsent leader notes/questions, leader reset prompts.
    fn deliver_held(&self) {
        let chat: Vec<String> = self.chat_queues.lock().unwrap().iter().filter(|(_, q)| !q.is_empty()).map(|(a, _)| a.clone()).collect();
        for a in chat {
            if self.ptys.is_running(&a) {
                self.chat_on_stop(&a);
                continue;
            }
            let queued = self.chat_queues.lock().unwrap().get_mut(&a).map(|q| q.drain(..).collect());
            self.emit_queue(&a, &[]);
            if let Err(e) = self.chat_start(&a, queued) {
                eprintln!("dex: {a}: sıradaki sohbet iletilemedi: {e}");
            }
        }
        let leaders = with(&self.db, |c| {
            c.prepare("SELECT agent_id FROM leader_notes WHERE notified = 0
                UNION SELECT leader FROM questions WHERE status='open' AND leader_delivered_at IS NULL
                UNION SELECT from_agent FROM delegations WHERE notified=0 AND status IN ('done','done_no_report','failed')
                ORDER BY agent_id")
                .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
                .map_err(|e| e.to_string())
        })
        .unwrap_or_default();
        for l in leaders {
            self.leader_notes_wake(&l);
        }
        self.leader_reset_deliver_all_later();
    }

    /// Every worker's oldest queued job (one per worker; its Stop / exit takes the next).
    fn dispatch_queued(&self) {
        let agents = with(&self.db, |c| {
            c.prepare("SELECT DISTINCT to_agent FROM delegations WHERE status = 'queued' ORDER BY to_agent")
                .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
                .map_err(|e| e.to_string())
        })
        .unwrap_or_default();
        for a in agents {
            self.delegation_dispatch_next(&a, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dex-restart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn marker_is_taken_once_and_a_stale_one_is_ignored() {
        let d = dir("marker");
        assert_eq!(take(&d), None);
        let m = Marker { at_ms: crate::diagnostics::now_ms(), leaders: vec!["timur".into()], ..Default::default() };
        write(&d, &m).unwrap();
        assert_eq!(take(&d), Some(m));
        assert_eq!(take(&d), None, "once");
        write(&d, &Marker { at_ms: crate::diagnostics::now_ms() - FRESH_MS - 1, leaders: vec!["timur".into()], ..Default::default() }).unwrap();
        assert_eq!(take(&d), None, "a restart whose core never came up");
        assert!(!d.join(FILE).exists(), "and it goes");
        // #447: a marker written before chat rode in it still reads; one with chat keeps it.
        std::fs::write(d.join(FILE), format!(r#"{{"atMs":{},"leaders":["timur"]}}"#, crate::diagnostics::now_ms())).unwrap();
        assert!(take(&d).is_some_and(|m| m.chat.is_empty()));
        let m = Marker { at_ms: crate::diagnostics::now_ms(), chat: [("kaan".to_string(), vec!["a".to_string(), "b".to_string()])].into(), ..Default::default() };
        write(&d, &m).unwrap();
        assert_eq!(take(&d), Some(m));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The whole round on a real Core pair: a restart keeps the queue, closes what ran, and starts
    /// the leader that ran with its saved session; a start with no marker (the patron's "Çekirdeği
    /// kapat", the 120 s rule) reopens nothing. `cmd` stands in for claude: it ignores the
    /// arguments and stays up (Windows only).
    #[cfg(windows)]
    #[test]
    fn a_restart_keeps_the_queue_and_resumes_its_leaders_a_plain_start_does_not() {
        use crate::{delegate, events, Events, Paths};
        use std::time::{Duration, Instant};
        let root = dir("round");
        let paths = || Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        let start = || Core::start(paths(), Events::new(events::NullSink)).unwrap();
        let wait = |what: &dyn Fn() -> bool| {
            let end = Instant::now() + Duration::from_secs(10);
            while !what() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(100));
            }
            what()
        };
        let stop = |core: std::sync::Arc<Core>| {
            for p in core.ptys.list() {
                core.ptys.kill(&p.id);
            }
            core.shutdown();
        };

        let a = start();
        let (queued, running) = with(&a.db, |c| {
            // Test only: past the engine CHECKs (claude/codex).
            c.execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE agents SET engine = 'cmd' WHERE id IN ('timur','kaan');").map_err(|e| e.to_string())?;
            agents::record_session(c, "timur", "sess-1", "cmd", None)?;
            c.execute_batch("PRAGMA ignore_check_constraints = OFF;").map_err(|e| e.to_string())?;
            // #838 G: the restarted Core records the kept job's launch binding (its worker's engine)
            // before the spawn; the next open's schema lets the stand-in engine into that row too.
            let version: i64 = c.query_row("PRAGMA schema_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            c.execute_batch(&format!(
                "PRAGMA writable_schema = ON;
                 UPDATE sqlite_master SET sql = replace(sql, 'CHECK (engine IN (''claude'',''codex''))', '') WHERE type = 'table' AND name = 'agent_sessions';
                 PRAGMA schema_version = {};
                 PRAGMA writable_schema = OFF;",
                version + 1
            ))
            .map_err(|e| e.to_string())?;
            let from = delegate::caller(c, "timur")?;
            let new = |p: &str| delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: p.into(), task_id: None, patron_words: None, effort: None };
            let r = delegate::insert(c, new("koşan"))?;
            delegate::set_status(c, r.id, "running")?;
            let q = delegate::insert(c, new("kuyrukta"))?;
            Ok((q.id, r.id))
        })
        .unwrap();
        a.agent_start("timur", agents::StartOptions::default()).unwrap();
        // #447: the patron's chat the gate held rides in the marker.
        a.chat_queues.lock().unwrap().entry("timur".into()).or_default().extend(["bir".to_string(), "iki".to_string()]);
        let m = a.restart_mark().unwrap();
        assert_eq!(m.leaders, vec!["timur".to_string()]);
        assert_eq!(m.chat.get("timur"), Some(&vec!["bir".to_string(), "iki".to_string()]));
        stop(a);

        let b = start();
        let status = |core: &Core, id: i64| with(&core.db, |c| delegate::get(c, id)).unwrap().status;
        assert_eq!(status(&b, running), "done_no_report", "what ran is closed as before");
        assert_ne!(status(&b, queued), "failed", "the queue is kept");
        assert!(wait(&|| b.ptys.is_running("timur")), "the leader came back");
        assert!(b.resume_watch.lock().unwrap().contains_key("timur"), "with --resume (its saved session, sess-1)");
        assert!(b.agent_states.get("timur").in_turn(), "and its queued chat as the prompt");
        assert!(b.chat_queues.lock().unwrap().get("timur").is_none_or(|q| q.is_empty()));
        assert!(wait(&|| status(&b, queued) == "running"), "the kept job went to its worker: {}", status(&b, queued));
        // A plain exit next: no marker.
        let q2 = with(&b.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(c, delegate::NewDelegation { from: &from, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None })
        })
        .unwrap()
        .id;
        stop(b);

        let c = start();
        assert_eq!(status(&c, q2), "failed", "no marker: the queue fails as before");
        std::thread::sleep(Duration::from_millis(1_000));
        assert!(!c.ptys.is_running("timur"), "no marker: leaders stay closed");
        stop(c);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #284: "Temiz oturum" and then a restart before the fresh session said a word (no Stop, so
    /// no new id recorded): the leader comes back fresh, not on the session the patron cleared.
    #[cfg(windows)]
    #[test]
    fn a_restart_after_a_fresh_start_does_not_resume_the_cleared_session() {
        use crate::{events, Events, Paths};
        use std::time::{Duration, Instant};
        let root = dir("fresh");
        let paths = || Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        let start = || Core::start(paths(), Events::new(events::NullSink)).unwrap();
        let saved = |core: &Core| with(&core.db, |c| agents::resumable_session(c, "timur", "cmd")).unwrap();
        let stop = |core: std::sync::Arc<Core>| {
            for p in core.ptys.list() {
                core.ptys.kill(&p.id);
            }
            core.shutdown();
        };

        let a = start();
        with(&a.db, |c| {
            // Test only: past the engine CHECKs (claude/codex).
            c.execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE agents SET engine = 'cmd' WHERE id = 'timur';").map_err(|e| e.to_string())?;
            agents::record_session(c, "timur", "sess-old", "cmd", None)?;
            c.execute_batch("PRAGMA ignore_check_constraints = OFF;").map_err(|e| e.to_string())
        })
        .unwrap();
        a.agent_start("timur", agents::StartOptions { fresh: true, ..Default::default() }).unwrap();
        assert!(!a.resume_watch.lock().unwrap().contains_key("timur"), "fresh: no --resume");
        assert_eq!(saved(&a), None, "the cleared session is no longer the one to resume");
        assert_eq!(a.restart_mark().unwrap().leaders, vec!["timur".to_string()]);
        stop(a);

        let b = start();
        let end = Instant::now() + Duration::from_secs(10);
        while !b.ptys.is_running("timur") && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(b.ptys.is_running("timur"), "the leader came back");
        assert!(!b.resume_watch.lock().unwrap().contains_key("timur"), "fresh again, not --resume sess-old");
        stop(b);
        let _ = std::fs::remove_dir_all(&root);
    }
}
