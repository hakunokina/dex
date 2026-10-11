//! Claude hook replies are proposals; transcript evidence reconciles the live turn.
use crate::{
    agents::{next_seq, Status},
    hook_delivery::Rows,
    Core,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stop {
    session: String,
    process: Option<u64>,
    seq: u64,
    at: u64,
    after: u64,
    transcript_from: u64,
}

struct Turn {
    session: String,
    process: Option<u64>,
    after: u64,
    transcript_from: u64,
    stop: Option<Stop>,
    ended: bool,
    continued: bool,
    cancelled: bool,
    interrupted: bool,
    accepted: bool,
    pending: Option<Rows>,
    latest_seq: u64,
    end_seq: u64,
}

#[derive(Default)]
pub(crate) struct Turns(Mutex<HashMap<String, Turn>>);

fn audit(agent: &str, seq: Option<u64>, reason: &str) {
    crate::diagnostics::event(
        "leader_delivery",
        json!({"agent":agent, "seq":seq, "reason":reason}),
    );
}

impl Core {
    /// Ingress precedes DB/context work. A newer UPS fences every old Stop callback.
    pub(crate) fn claude_hook_received(
        &self,
        agent: &str,
        event: &str,
        session: Option<&str>,
        seq: u64,
    ) -> Option<Stop> {
        if self.agent_states.get(agent).engine.as_deref() == Some("codex") {
            if event == "UserPromptSubmit" {
                self.agent_states.prompt_seq(agent, session, seq);
            }
            return None;
        }
        if matches!(
            event,
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "Stop"
        ) {
            self.claude_reconcile_unread_stop(agent);
        }
        let session = session
            .map(str::to_owned)
            .or_else(|| self.agent_states.get(agent).session_id)?;
        let process = self.ptys.generation(agent);
        let now = crate::diagnostics::now_ms();
        let transcript_from = self
            .agent_states
            .get(agent)
            .transcript
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let mut turns = self.claude_turns.0.lock().unwrap();
        let turn = turns.entry(agent.into()).or_insert_with(|| Turn {
            session: session.clone(),
            process,
            after: now,
            transcript_from,
            stop: None,
            ended: false,
            continued: false,
            cancelled: false,
            accepted: false,
            pending: None,
            latest_seq: seq,
            end_seq: 0,
            interrupted: false,
        });
        if seq < turn.latest_seq || (event != "UserPromptSubmit" && seq < turn.end_seq) {
            return None;
        }
        if turn.session != session
            || turn.process != process
            || event == "UserPromptSubmit"
            || event == "SessionStart" && !turn.interrupted
        {
            *turn = Turn {
                session: session.clone(),
                process,
                after: now,
                transcript_from,
                stop: None,
                ended: false,
                continued: false,
                cancelled: false,
                accepted: false,
                pending: None,
                latest_seq: seq,
                end_seq: 0,
                interrupted: false,
            };
        }
        turn.latest_seq = seq;
        if event == "UserPromptSubmit" {
            self.agent_states
                .prompt_seq(agent, Some(&session), next_seq());
        } else if matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure") {
            turn.continued = true;
            turn.ended = false;
            turn.after = now;
            turn.transcript_from = transcript_from;
            self.agent_states
                .hook_seq(agent, Some(Status::Working), Some(&session), seq);
        } else if event == "Stop" {
            let stop = Stop {
                session,
                process,
                seq,
                at: now,
                after: turn.after,
                transcript_from: turn.transcript_from,
            };
            turn.stop = Some(stop.clone());
            // Stop processing can lag the CLI's completion; only UPS/continuation
            // establishes a new live-turn boundary. The attempt has its own `at`.
            turn.ended = false;
            turn.continued = false;
            turn.cancelled = false;
            turn.accepted = false;
            turn.pending = None;
            return Some(stop);
        }
        None
    }

    /// Reconcile the whole Stop evidence window before a hook replaces its boundary.
    /// Read in file order with a fixed I/O buffer, retaining only one JSONL row at a time.
    /// A large interval must not lose acceptance when the next hook replaces the Stop.
    fn claude_reconcile_unread_stop(&self, agent: &str) {
        use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
        let Some(stop) = self
            .claude_turns
            .0
            .lock()
            .unwrap()
            .get(agent)
            .and_then(|t| t.stop.clone())
        else {
            return;
        };
        let Some(path) = self.agent_states.get(agent).transcript else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return;
        };
        if len < stop.transcript_from || file.seek(SeekFrom::Start(stop.transcript_from)).is_err() {
            return;
        }
        // Freeze the end: a concurrently growing transcript cannot extend this hook's read.
        let mut reader = BufReader::with_capacity(64 * 1024, file.take(len - stop.transcript_from));
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let Ok(row) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            // This hook already proves liveness: never paste through a transient Idle here.
            self.claude_reconcile_row(agent, &row, stop.process, Some(&stop));
        }
    }

    fn accept_stop_rows(&self, agent: &str, turn: &mut Turn) {
        let Some(rows) = turn.pending.as_ref() else {
            return;
        };
        match crate::db::with(&self.db, |c| rows.accept(c, agent)) {
            Ok(()) => {
                turn.pending = None;
                audit(agent, turn.stop.as_ref().map(|s| s.seq), "stop_accept");
                let _ = self.events.emit("delegations-changed", &());
                let _ = self.events.emit("questions-changed", &());
            }
            Err(_) => audit(agent, turn.stop.as_ref().map(|s| s.seq), "db_error"),
        }
    }

    pub(crate) fn claude_stage_stop(&self, agent: &str, stop: &Stop, rows: Rows) {
        let mut turns = self.claude_turns.0.lock().unwrap();
        let Some(turn) = turns
            .get_mut(agent)
            .filter(|t| t.stop.as_ref() == Some(stop) && !t.ended && !t.cancelled && !t.interrupted)
        else {
            audit(agent, Some(stop.seq), "stop_retry");
            return;
        };
        turn.pending = Some(rows);
        audit(agent, Some(stop.seq), "stop_prepare");
        if turn.accepted {
            self.accept_stop_rows(agent, turn);
        }
    }

    pub(crate) fn claude_stop_is_current(&self, agent: &str, stop: &Stop) -> bool {
        self.claude_turns
            .0
            .lock()
            .unwrap()
            .get(agent)
            .is_some_and(|t| t.stop.as_ref() == Some(stop) && !t.ended && !t.continued && !t.interrupted)
            && self.ptys.generation(agent) == stop.process
    }

    pub(crate) fn claude_stop_status(
        &self,
        agent: &str,
        stop: &Stop,
        status: Status,
    ) -> crate::agents::State {
        let turns = self.claude_turns.0.lock().unwrap();
        if turns
            .get(agent)
            .is_some_and(|t| t.stop.as_ref() == Some(stop) && !t.ended && !t.continued && !t.interrupted)
            && self.ptys.generation(agent) == stop.process
        {
            self.agent_states
                .hook_seq(agent, Some(status), Some(&stop.session), stop.seq)
        } else {
            audit(agent, Some(stop.seq), "stale_stop");
            self.agent_states.get(agent)
        }
    }

    pub(crate) fn claude_transcript_row(&self, agent: &str, row: &Value, process: Option<u64>) {
        self.claude_reconcile_row(agent, row, process, None);
    }

    pub(crate) fn claude_interrupt(&self, agent: &str) {
        if let Some(turn) = self.claude_turns.0.lock().unwrap().get_mut(agent) {
            turn.interrupted = true;
            turn.pending = None;
            turn.accepted = false;
        }
    }

    fn claude_reconcile_row(
        &self,
        agent: &str,
        row: &Value,
        process: Option<u64>,
        ingress: Option<&Stop>,
    ) {
        if process.is_none() || self.ptys.generation(agent) != process || row["isSidechain"] == true
        {
            return;
        }
        let Some(session) = row["sessionId"]
            .as_str()
            .or_else(|| row["session_id"].as_str())
        else {
            return;
        };
        let state = self.agent_states.get(agent);
        if state.session_id.as_deref() != Some(session) {
            return;
        }
        let Some(timestamp) = crate::chat::parse_ts(&row["timestamp"]) else {
            return;
        };
        let ended = row["type"] == "system" && row["subtype"] == "turn_duration"
            || row["type"] == "attachment"
                && row["attachment"]["type"] == "hook_cancelled"
                && row["attachment"]["hookEvent"] == "Stop"
                && row["attachment"]["timedOut"] == true;
        let continued = row["type"] == "system"
            && row["subtype"] == "stop_hook_summary"
            && row["preventedContinuation"] == true;
        if !ended && !continued {
            return;
        }
        let mut turns = self.claude_turns.0.lock().unwrap();
        let turn = turns.entry(agent.into()).or_insert_with(|| Turn {
            session: session.into(),
            process,
            after: crate::diagnostics::now_ms(),
            transcript_from: 0,
            stop: None,
            ended: false,
            continued: false,
            cancelled: false,
            accepted: false,
            pending: None,
            latest_seq: 0,
            end_seq: 0,
            interrupted: false,
        });
        if turn.session != session
            || turn.process != process
            || ingress.is_some_and(|stop| turn.stop.as_ref() != Some(stop))
            || turn.interrupted
        {
            return;
        }
        let state = self.agent_states.get(agent);
        if state.session_id.as_deref() != Some(session)
            || self.ptys.generation(agent) != process
            || state.status == Status::Offline
        {
            return;
        }
        // Delivery evidence belongs to the Stop attempt, even after a tool hook advanced
        // the live turn. A general continuation cannot acknowledge Dex's cancelled reply.
        let cancellation = row["type"] == "attachment" && ended;
        if cancellation && turn.stop.as_ref().is_some_and(|s| timestamp >= s.after) {
            turn.cancelled = true;
            turn.accepted = false;
            turn.pending = None;
        }
        if continued && turn.stop.as_ref().is_some_and(|s| timestamp >= s.at) && !turn.cancelled {
            turn.accepted = true;
            self.accept_stop_rows(agent, turn);
        }
        if timestamp < turn.after
            || (turn.ended && !continued)
            || timestamp == turn.after && (turn.stop.is_none() || turn.continued)
        {
            return;
        }
        if continued && turn.stop.is_some() {
            turn.ended = false;
            turn.continued = true;
            turn.after = timestamp;
            if ingress.is_some() {
                return;
            }
            let fence = next_seq();
            self.agent_states
                .hook_seq(agent, Some(Status::Working), Some(session), fence);
            return;
        }
        if !ended || ingress.is_some() {
            return;
        }
        turn.ended = true;
        turn.cancelled |= cancellation;
        turn.pending = None;
        audit(agent, turn.stop.as_ref().map(|s| s.seq), "turn_end");
        let was_busy = matches!(state.status, Status::Working | Status::Waiting);
        let fence = next_seq();
        turn.end_seq = fence;
        let state = self
            .agent_states
            .hook_seq(agent, Some(Status::Idle), Some(session), fence);
        drop(turns);
        if was_busy {
            crate::hooks::after_turn_ended(self, agent, "Settled", &state);
        } else {
            // The Stop hook may already have published Idle before transcript completion.
            let _ = self.team_turn_ended(&state);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        db::with,
        delegate,
        events::{EventSink, NullSink},
        Events, Paths,
    };
    use std::{
        io::Write,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    struct Sink;
    impl crate::pty_stream::PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool {
            true
        }
        fn exit(&mut self) {}
    }
    struct Input {
        bytes: Arc<Mutex<Vec<u8>>>,
        _original: Box<dyn Write + Send>,
    }
    impl Write for Input {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn core(tag: &str, sink: impl EventSink + 'static) -> (Arc<Core>, Arc<Mutex<Vec<u8>>>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.dex/tmp/claude-{tag}-{}-{}",
            std::process::id(),
            next_seq()
        ));
        let core = Core::start(
            Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            Events::new(sink),
        )
        .unwrap();
        core.pty_spawn(
            crate::pty::Spawn {
                id: "timur".into(),
                cmd: crate::shell::SHELL_CMD.into(),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Sink,
        )
        .unwrap();
        let input = Arc::new(Mutex::new(Vec::new()));
        core.ptys.input_writer_for_test("timur", |original| {
            Box::new(Input {
                bytes: input.clone(),
                _original: original,
            })
        });
        core.agent_states
            .hook("timur", Some(Status::Working), Some("session-a"));
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
        (core, input)
    }
    fn finish(core: &Core) {
        core.ptys.kill("timur");
        core.shutdown();
    }
    struct Finish(Arc<Core>);
    impl Drop for Finish {
        fn drop(&mut self) {
            finish(&self.0);
        }
    }
    fn result(core: &Core) -> i64 {
        with(&core.db, |c| {
            let d = delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &delegate::caller(c, "timur")?,
                    to_agent: "kaan".into(),
                    team_id: "default".into(),
                    service_team_id: None,
                    prompt: "work".into(),
                    task_id: None,
                    patron_words: None,
                    effort: None,
                },
            )?;
            delegate::set_status(c, d.id, "done")?;
            c.execute(
                "UPDATE delegations SET summary='result-canary' WHERE id=?1",
                [d.id],
            )
            .map_err(|e| e.to_string())?;
            Ok(d.id)
        })
        .unwrap()
    }
    fn post(
        core: &Core,
        event: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<ureq::Response, ureq::Error> {
        ureq::post(&format!("http://127.0.0.1:{}/hook", core.hooks.port))
            .timeout(timeout)
            .set("Authorization", &format!("Bearer {}", core.hooks.token))
            .set("X-Dex-Agent", "timur")
            .set("X-Dex-Key", &core.mcp.key_for("timur"))
            .send_json(json!({"hook_event_name":event,"session_id":"session-a","prompt":prompt}))
    }
    // Inverse of chat::parse_ts's civil calendar conversion, for actual race clocks.
    fn timestamp(ms: u64) -> String {
        let days = (ms / 86_400_000) as i64 + 719468;
        let era = days / 146097;
        let doe = days - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = mp + if mp < 10 { 3 } else { -9 };
        let y = y + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
            ms / 3_600_000 % 24,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    }
    fn row(subtype: &str, ms: u64) -> Value {
        json!({"type":"system","subtype":subtype,"sessionId":"session-a","timestamp":timestamp(ms)})
    }
    /// Existing delivery fixtures need a live process and an actual CLI acceptance row.
    pub(crate) fn accepted_stop(core: &Core, agent: &str) {
        assert!(core.ptys.generation(agent).is_some());
        let summary = json!({"type":"system","subtype":"stop_hook_summary",
            "sessionId":core.agent_states.get(agent).session_id.unwrap(),
            "timestamp":timestamp(crate::diagnostics::now_ms()),"preventedContinuation":true});
        let transcript = core.paths.data_dir.join(format!("accepted-{agent}.jsonl"));
        std::fs::write(&transcript, format!("{summary}\n")).unwrap();
        core.agent_states
            .set_transcript(agent, transcript.to_str().unwrap());
    }
    fn observe(core: &Core, row: Value) {
        core.claude_transcript_row("timur", &row, core.ptys.generation("timur"));
    }
    fn wait_for(check: impl Fn() -> bool, duration: Duration) {
        let end = Instant::now() + duration;
        while !check() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(check(), "deadline expired");
    }

    #[derive(Default)]
    struct DoneEvents(Mutex<Vec<Value>>);
    impl EventSink for Arc<DoneEvents> {
        fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
            if event == "team_turn_done" {
                self.0.lock().unwrap().push(payload);
            }
            Ok(())
        }
    }

    #[test]
    fn team_turn_done_once_per_completed_leader_turn() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-done", events.clone());
        let _finish = Finish(core.clone());
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        let end = row("turn_duration", crate::diagnostics::now_ms());
        observe(&core, end.clone());
        observe(&core, end);
        {
            let done = events.0.lock().unwrap();
            assert_eq!(done.len(), 1);
            assert_eq!(done[0]["teamId"], "default");
            assert_eq!(done[0]["leaderId"], "timur");
            assert!(done[0]["timestamp"].as_u64().unwrap() > 0);
        }
        std::thread::sleep(Duration::from_millis(2));
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 2);
    }

    fn team_turn_done_waits_for_delegation(status: &str) {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-busy", events.clone());
        let _finish = Finish(core.clone());
        let id = result(&core);
        with(&core.db, |c| delegate::set_status(c, id, status)).unwrap();
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert!(events.0.lock().unwrap().is_empty());

        with(&core.db, |c| delegate::set_status(c, id, "done")).unwrap();
        core.events.emit("delegations-changed", &()).unwrap();
        core.agent_states.hook("kaan", Some(Status::Working), None);
        crate::hooks::turn_ended(&core, "kaan", "Settled");
        assert!(events.0.lock().unwrap().is_empty(), "worker completion cannot finish a team turn");
        let idle = core.agent_states.get("timur");
        crate::hooks::after_turn_ended(&core, "timur", "Settled", &idle);
        assert!(events.0.lock().unwrap().is_empty(), "a busy end stays consumed after its worker finishes");

        std::thread::sleep(Duration::from_millis(2));
        let context = post(&core, "UserPromptSubmit", "process result", Duration::from_secs(3))
            .unwrap().into_string().unwrap();
        assert!(context.contains("result-canary"), "the leader must receive the result before completing");
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn team_turn_done_waits_for_running_delegation() {
        team_turn_done_waits_for_delegation("running");
    }

    #[test]
    fn team_turn_done_waits_for_queued_delegation() {
        team_turn_done_waits_for_delegation("queued");
    }

    fn team_turn_done_waits_for_plan_item(status: &str) {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-plan", events.clone());
        let _finish = Finish(core.clone());
        let plan = with(&core.db, |c| {
            c.execute("INSERT INTO plans(team_id, leader, created_at, updated_at) VALUES('default', 'timur', 0, 0)", [])
                .map_err(|e| e.to_string())?;
            let plan = c.last_insert_rowid();
            c.execute("INSERT INTO plan_items(plan_id, key, title, prompt, target, status) VALUES(?1, 'A', 'A', 'work', 'kaan', ?2)", rusqlite::params![plan, status])
                .map_err(|e| e.to_string())?;
            Ok(plan)
        }).unwrap();
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert!(events.0.lock().unwrap().is_empty());

        with(&core.db, |c| {
            c.execute("UPDATE plans SET status='cancelled' WHERE id=?1", [plan]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1, "inactive plans do not block completion");
    }

    #[test]
    fn team_turn_done_waits_for_waiting_plan_item() {
        team_turn_done_waits_for_plan_item("waiting");
    }

    #[test]
    fn team_turn_done_waits_for_running_plan_item() {
        team_turn_done_waits_for_plan_item("running");
    }

    #[test]
    fn team_turn_done_waits_for_reconciliation_after_stop_hook_idle() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-hook", events.clone());
        let _finish = Finish(core.clone());
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert_eq!(core.agent_states.get("timur").status, Status::Idle);
        assert!(events.0.lock().unwrap().is_empty(), "Stop reply alone is not a reconciled end");
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn team_turn_done_deduplicates_shared_end_paths_and_rearms_on_work() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-dedup", events.clone());
        let _finish = Finish(core.clone());
        crate::hooks::turn_ended(&core, "timur", "Settled");
        let first_end = core.agent_states.get("timur");
        crate::hooks::after_turn_ended(&core, "timur", "Settled", &first_end);
        assert_eq!(events.0.lock().unwrap().len(), 1);

        core.agent_states.hook("timur", Some(Status::Working), None);
        crate::hooks::after_turn_ended(&core, "timur", "Settled", &first_end);
        assert_eq!(events.0.lock().unwrap().len(), 1, "stale end cannot complete a new working turn");
        crate::hooks::turn_ended(&core, "timur", "Settled");
        assert_eq!(events.0.lock().unwrap().len(), 2);
        crate::hooks::after_turn_ended(&core, "timur", "Settled", &first_end);
        assert_eq!(events.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn team_turn_done_codex_stop_once_and_rearms_on_prompt() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-codex-hook", events.clone());
        let _finish = Finish(core.clone());
        core.agent_states.started("timur", "codex", false);
        core.agent_states.hook("timur", Some(Status::Working), Some("session-a"));
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert_eq!(events.0.lock().unwrap().len(), 1);
        post(&core, "UserPromptSubmit", "next turn", Duration::from_secs(3)).unwrap();
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert_eq!(events.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn team_turn_done_is_team_scoped_and_waits_for_incoming_service_work() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-scope", events.clone());
        let _finish = Finish(core.clone());
        let job = with(&core.db, |c| {
            c.execute("INSERT INTO teams(id, name, kind, created_at) VALUES('other', 'Other', 'service', 0)", [])
                .map_err(|e| e.to_string())?;
            c.execute("INSERT INTO plans(team_id, leader, created_at, updated_at) VALUES('other', 'other-leader', 0, 0)", [])
                .map_err(|e| e.to_string())?;
            let plan = c.last_insert_rowid();
            c.execute("INSERT INTO plan_items(plan_id, key, title, prompt, target) VALUES(?1, 'A', 'A', 'work', 'other-worker')", [plan])
                .map_err(|e| e.to_string())?;
            c.execute("INSERT INTO delegations(from_agent, to_agent, team_id, prompt, created_at, updated_at) VALUES('other-leader', 'other-worker', 'other', 'work', 0, 0)", [])
                .map_err(|e| e.to_string())?;
            Ok(c.last_insert_rowid())
        }).unwrap();
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1, "other teams do not block this leader");
        with(&core.db, |c| {
            c.execute("UPDATE delegations SET service_team_id='default', to_agent='kaan' WHERE id=?1", [job])
                .map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1, "incoming service work keeps this team busy");
    }

    #[test]
    fn team_turn_done_interrupt_does_not_notify_the_patron() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-interrupt", events.clone());
        let _finish = Finish(core.clone());
        crate::hooks::on_interrupt(&core, "timur");
        assert_eq!(core.agent_states.get("timur").status, Status::Idle);
        assert!(events.0.lock().unwrap().is_empty());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms() + 1));
        assert!(events.0.lock().unwrap().is_empty(), "interrupted turn cannot complete later");

        core.claude_hook_received("timur", "SessionStart", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms() + 1));
        assert!(events.0.lock().unwrap().is_empty(), "same-session start is not a new prompt");

        // Tool and Stop hooks still belong to the interrupted turn.
        core.claude_hook_received("timur", "PreToolUse", Some("session-a"), next_seq());
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        observe(&core, row("turn_duration", crate::diagnostics::now_ms() + 1));
        assert!(events.0.lock().unwrap().is_empty(), "only a new prompt rearms completion");

        post(&core, "UserPromptSubmit", "next turn", Duration::from_secs(3)).unwrap();
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        observe(&core, row("turn_duration", crate::diagnostics::now_ms() + 1));
        assert_eq!(events.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn team_turn_done_codex_interrupt_blocks_late_stop_until_new_prompt() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-codex-interrupt", events.clone());
        let _finish = Finish(core.clone());
        core.agent_states.started("timur", "codex", false);
        core.agent_states.hook("timur", Some(Status::Working), Some("session-a"));
        crate::hooks::on_interrupt(&core, "timur");
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert!(events.0.lock().unwrap().is_empty(), "late Codex Stop cannot complete interrupted work");
        post(&core, "PreToolUse", "", Duration::from_secs(3)).unwrap();
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert!(events.0.lock().unwrap().is_empty(), "tool hooks cannot rearm completion");
        post(&core, "UserPromptSubmit", "next turn", Duration::from_secs(3)).unwrap();
        post(&core, "Stop", "", Duration::from_secs(3)).unwrap();
        assert_eq!(events.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn team_turn_done_codex_interrupt_rearms_on_new_rollout_turn() {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-codex-interrupt-rollout", events.clone());
        let _finish = Finish(core.clone());
        core.agent_states.started("timur", "codex", false);
        let process = core.ptys.generation("timur").unwrap();
        core.agent_states.codex_observed("timur", process, Some("A"), Some(crate::diagnostics::now_ms())).unwrap();
        crate::hooks::on_interrupt(&core, "timur");
        assert!(core.agent_states.codex_completed("timur", process, "A", Some(crate::diagnostics::now_ms() + 1)).is_none());
        assert!(events.0.lock().unwrap().is_empty());
        core.agent_states.codex_observed("timur", process, Some("B"), Some(crate::diagnostics::now_ms() + 1)).unwrap();
        let end = core.agent_states.codex_completed("timur", process, "B", Some(crate::diagnostics::now_ms() + 2)).unwrap();
        crate::hooks::after_turn_ended(&core, "timur", "Settled", &end);
        assert_eq!(events.0.lock().unwrap().len(), 1);
    }

    fn team_turn_done_waits_for_pending_leader_work(kind: &str) {
        let events = Arc::new(DoneEvents::default());
        let (core, _) = core("team-pending", events.clone());
        let _finish = Finish(core.clone());
        match kind {
            "result" => { result(&core); }
            "note" | "paste" => {
                with(&core.db, |c| crate::plan::note(c, "timur", "next work")).unwrap();
                if kind == "paste" {
                    core.agent_states.hook("timur", Some(Status::Idle), None);
                    core.leader_notes_wake("timur");
                    assert!(core.leader_pastes.lock().unwrap().contains_key("timur"));
                    with(&core.db, |c| {
                        c.execute("UPDATE leader_notes SET notified=1 WHERE agent_id='timur'", []).map_err(|e| e.to_string())?;
                        Ok(())
                    }).unwrap();
                    core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
                }
            }
            "chat" => {
                core.chat_queues.lock().unwrap().entry("timur".into()).or_default().push_back("echo next work".into());
            }
            "open" | "escalated" => {
                let id = result(&core);
                with(&core.db, |c| {
                    c.execute("UPDATE delegations SET notified=1 WHERE id=?1", [id]).map_err(|e| e.to_string())?;
                    c.execute("INSERT INTO questions(delegation_id, worker, leader, team_id, question, status, created_at, updated_at) VALUES(?1, 'kaan', 'timur', 'default', 'next work?', ?2, 0, 0)", rusqlite::params![id, kind]).map_err(|e| e.to_string())?;
                    Ok(())
                }).unwrap();
            }
            _ => unreachable!(),
        }
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert!(events.0.lock().unwrap().is_empty(), "pending {kind} must not notify the patron");
        if kind == "chat" {
            wait_for(|| core.chat_queues.lock().unwrap().get("timur").is_none_or(|q| q.is_empty())
                && core.agent_states.get("timur").status == Status::Working, Duration::from_secs(2));
        }
        if matches!(kind, "open" | "escalated") {
            with(&core.db, |c| {
                c.execute("UPDATE questions SET status='answered' WHERE leader='timur'", []).map_err(|e| e.to_string())?;
                Ok(())
            }).unwrap();
        }
        let prompt = core.leader_pastes.lock().unwrap().get("timur")
            .map(|paste| paste.text.clone()).unwrap_or_else(|| "process pending work".into());
        std::thread::sleep(Duration::from_millis(2));
        post(&core, "UserPromptSubmit", &prompt, Duration::from_secs(3)).unwrap();
        core.claude_hook_received("timur", "Stop", Some("session-a"), next_seq());
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        assert_eq!(events.0.lock().unwrap().len(), 1, "cleared {kind} permits the next completed turn");
    }

    #[test]
    fn team_turn_done_waits_for_undelivered_result() { team_turn_done_waits_for_pending_leader_work("result"); }
    #[test]
    fn team_turn_done_waits_for_leader_note() { team_turn_done_waits_for_pending_leader_work("note"); }
    #[test]
    fn team_turn_done_waits_for_leader_chat_queue() { team_turn_done_waits_for_pending_leader_work("chat"); }
    #[test]
    fn team_turn_done_waits_for_leader_paste() { team_turn_done_waits_for_pending_leader_work("paste"); }
    #[test]
    fn team_turn_done_waits_for_open_worker_question() { team_turn_done_waits_for_pending_leader_work("open"); }
    #[test]
    fn team_turn_done_waits_for_escalated_worker_question() { team_turn_done_waits_for_pending_leader_work("escalated"); }

    #[test]
    fn ended_turn_fences_late_stop_but_preserves_new_ups_and_real_continuation() {
        let (core, _) = core("fences", NullSink);
        let stop = core
            .claude_hook_received("timur", "Stop", Some("session-a"), next_seq())
            .unwrap();
        let delayed_ingress = next_seq();
        let old_end = row("turn_duration", crate::diagnostics::now_ms());
        observe(&core, old_end.clone());
        assert!(
            core.claude_hook_received("timur", "Stop", Some("session-a"), delayed_ingress)
                .is_none(),
            "a request accepted before completion cannot reopen its turn after a delayed read"
        );
        assert_eq!(
            core.claude_stop_status("timur", &stop, Status::Working)
                .status,
            Status::Idle
        );
        core.claude_hook_received("timur", "UserPromptSubmit", Some("session-a"), next_seq());
        std::thread::sleep(Duration::from_millis(2));
        observe(&core, old_end.clone());
        assert_eq!(
            core.agent_states.get("timur").status,
            Status::Working,
            "old completion cannot end UPS"
        );
        let next = core
            .claude_hook_received("timur", "Stop", Some("session-a"), next_seq())
            .unwrap();
        let older_end = row("turn_duration", crate::diagnostics::now_ms());
        std::thread::sleep(Duration::from_millis(2));
        core.claude_hook_received("timur", "PreToolUse", Some("session-a"), next_seq());
        observe(&core, older_end);
        assert_eq!(
            core.claude_stop_status("timur", &next, Status::Idle).status,
            Status::Working,
            "real continuation owns the turn"
        );
        let mut foreign = row("turn_duration", crate::diagnostics::now_ms());
        foreign["sessionId"] = json!("other-session");
        observe(&core, foreign);
        core.claude_transcript_row(
            "timur",
            &row("turn_duration", crate::diagnostics::now_ms()),
            Some(0),
        );
        assert_eq!(core.agent_states.get("timur").status, Status::Working);
        finish(&core);
    }

    #[test]
    fn stop_summary_accepts_payload_once_and_late_report_stays_pending() {
        let (core, _) = core("accept", NullSink);
        let id = result(&core);
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_none());
        let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
        summary["preventedContinuation"] = json!(true);
        observe(&core, summary.clone());
        let stamp = with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .unwrap();
        observe(&core, summary);
        assert_eq!(
            with(&core.db, |c| delegate::get(c, id))
                .unwrap()
                .delivered_at,
            Some(stamp)
        );
        let out = core.delegation_on_hook("timur", "Stop", &json!({}), false);
        assert!(!out.unwrap_or_default().contains("result-canary"));
        let newer = result(&core);
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        with(&core.db, |c| {
            c.execute(
                "UPDATE delegations SET updated_at=updated_at+1, summary='late-report' WHERE id=?1",
                [newer],
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
        core.claude_hook_received("timur", "PreToolUse", Some("session-a"), next_seq());
        assert!(
            with(&core.db, |c| delegate::get(c, newer))
                .unwrap()
                .delivered_at
                .is_none(),
            "acknowledgement cannot swallow a newer report"
        );
        finish(&core);
    }

    #[test]
    fn timeout_keeps_patron_draft_and_payload_pending() {
        let (core, input) = core("draft", NullSink);
        let id = result(&core);
        core.agent_input("timur", true);
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        observe(&core, row("turn_duration", crate::diagnostics::now_ms()));
        core.leader_notes_wake("timur");
        assert_eq!(core.agent_states.get("timur").status, Status::Idle);
        assert!(
            input.lock().unwrap().is_empty(),
            "patron draft cannot be overwritten"
        );
        assert!(core.agent_states.typing("timur"));
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_none());
        finish(&core);
    }

    #[test]
    fn new_ups_reconciles_stop_acceptance_before_the_file_watch_catches_up() {
        let (core, _) = core("unread-summary", NullSink);
        let id = result(&core);
        let transcript = core.paths.data_dir.join("transcript.jsonl");
        std::fs::write(&transcript, "").unwrap();
        core.agent_states
            .set_transcript("timur", transcript.to_str().unwrap());
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
        summary["preventedContinuation"] = json!(true);
        std::fs::write(&transcript, format!("{summary}\n")).unwrap();
        let old_end = row("turn_duration", crate::diagnostics::now_ms() + 1);
        std::thread::sleep(Duration::from_millis(2));
        let response = post(
            &core,
            "UserPromptSubmit",
            "patron next turn",
            Duration::from_secs(2),
        )
        .unwrap()
        .into_string()
        .unwrap();
        assert!(
            !response.contains("result-canary"),
            "accepted Stop context must not be repeated at UPS"
        );
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_some());
        observe(&core, old_end);
        assert_eq!(
            core.agent_states.get("timur").status,
            Status::Working,
            "reconciling acceptance cannot discard the new UPS boundary"
        );
        finish(&core);
    }

    #[test]
    fn codex_stop_keeps_its_existing_hook_delivery_contract() {
        let (core, _) = core("codex-contract", NullSink);
        core.agent_states.started("timur", "codex", false);
        core.agent_states
            .hook("timur", Some(Status::Working), Some("session-a"));
        let id = result(&core);
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_some());
        finish(&core);
    }

    #[test]
    fn real_block_continuation_after_timeout_fences_old_end_without_accepting_cancelled_payload() {
        let (core, _) = core("timeout-continuation", NullSink);
        let id = result(&core);
        core.delegation_on_hook("timur", "Stop", &json!({}), false)
            .unwrap();
        let old_end = row("turn_duration", crate::diagnostics::now_ms());
        observe(
            &core,
            json!({"type":"attachment","sessionId":"session-a","timestamp":old_end["timestamp"],
            "attachment":{"type":"hook_cancelled","hookEvent":"Stop","timedOut":true}}),
        );
        std::thread::sleep(Duration::from_millis(2));
        let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
        summary["preventedContinuation"] = json!(true);
        observe(&core, summary);
        observe(&core, old_end);
        assert_eq!(
            core.agent_states.get("timur").status,
            Status::Working,
            "CLI continuation supersedes old timeout evidence"
        );
        assert!(
            with(&core.db, |c| delegate::get(c, id))
                .unwrap()
                .delivered_at
                .is_none(),
            "a cancelled Dex payload is still retryable"
        );
        finish(&core);
    }

    #[test]
    fn unread_stop_cancellation_before_continuation_retries_payload_once() {
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure", "Stop"] {
            for (watch_rows, padding) in (0..=2).flat_map(|n| [0, 64 * 1024].map(|p| (n, p))) {
                let (core, input) = core("unread-cancellation", NullSink);
                let _finish = Finish(core.clone());
                let id = result(&core);
                let transcript = core.paths.data_dir.join("transcript.jsonl");
                std::fs::write(&transcript, "").unwrap();
                core.agent_states
                    .set_transcript("timur", transcript.to_str().unwrap());
                core.delegation_on_hook("timur", "Stop", &json!({}), false)
                    .unwrap();
                let cancelled = json!({"type":"attachment","sessionId":"session-a",
                    "timestamp":timestamp(crate::diagnostics::now_ms()),
                    "attachment":{"type":"hook_cancelled","hookEvent":"Stop","timedOut":true}});
                // A different Stop hook blocked, although Dex's response never arrived.
                std::thread::sleep(Duration::from_millis(2));
                let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
                summary["preventedContinuation"] = json!(true);
                let output = json!({"type":"assistant","message":{"content":"x".repeat(padding)}});
                std::fs::write(&transcript, format!("{cancelled}\n{output}\n{summary}\n")).unwrap();
                if watch_rows > 0 {
                    // An already observed timeout legitimately idles the leader. Keep a
                    // patron draft so its normal wake cannot contaminate this hook race.
                    core.agent_input("timur", true);
                    observe(&core, cancelled.clone());
                }
                if watch_rows > 1 {
                    observe(&core, summary.clone());
                }
                std::thread::sleep(Duration::from_millis(2));
                core.claude_hook_received("timur", event, Some("session-a"), next_seq());
                assert!(
                    with(&core.db, |c| delegate::get(c, id)).unwrap().delivered_at.is_none(),
                    "{event}, watch_rows={watch_rows}, padding={padding}: cancelled payload was accepted"
                );
                observe(&core, cancelled);
                observe(&core, summary);
                let reply = post(
                    &core,
                    "UserPromptSubmit",
                    "next turn",
                    Duration::from_secs(2),
                )
                .unwrap()
                .into_string()
                .unwrap();
                assert_eq!(reply.matches("result-canary").count(), 1);
                let stamp = with(&core.db, |c| delegate::get(c, id))
                    .unwrap()
                    .delivered_at;
                assert!(stamp.is_some());
                let again = post(
                    &core,
                    "UserPromptSubmit",
                    "another turn",
                    Duration::from_secs(2),
                )
                .unwrap()
                .into_string()
                .unwrap();
                assert!(!again.contains("result-canary"));
                assert_eq!(
                    with(&core.db, |c| delegate::get(c, id))
                        .unwrap()
                        .delivered_at,
                    stamp
                );
                assert!(
                    input.lock().unwrap().is_empty(),
                    "{event}, watch_rows={watch_rows}, padding={padding}: continuation cannot paste an idle prompt"
                );
            }
        }
    }

    #[test]
    fn large_unread_accepted_stop_survives_second_stop_and_ups_once() {
        for boundary in ["UserPromptSubmit", "PostToolUse"] {
            let (core, input) = core("large-unread-accepted", NullSink);
            let _finish = Finish(core.clone());
            let transcript = core.paths.data_dir.join("transcript.jsonl");
            std::fs::write(&transcript, "").unwrap();
            core.agent_states
                .set_transcript("timur", transcript.to_str().unwrap());
            core.claude_hook_received("timur", boundary, Some("session-a"), next_seq());
            let id = result(&core);
            let question = with(&core.db, |c| {
                let job = delegate::insert(
                    c,
                    delegate::NewDelegation {
                        from: &delegate::caller(c, "timur")?,
                        to_agent: "ayla".into(),
                        team_id: "default".into(),
                        service_team_id: None,
                        prompt: "work".into(),
                        task_id: None,
                        patron_words: None,
                        effort: None,
                    },
                )?;
                delegate::set_status(c, job.id, "running")?;
                crate::questions::ask(
                    c,
                    &delegate::caller(c, "ayla")?,
                    &json!({"delegationId":job.id,"question":"question-canary"}),
                )
            })
            .unwrap();
            let output = json!({"type":"assistant","message":{"content":"x".repeat(128 * 1024)}});
            // Large output is written AFTER the last live-turn boundary, before S1.
            std::fs::write(&transcript, format!("{output}\n")).unwrap();
            let first = post(&core, "Stop", "", Duration::from_secs(2))
                .unwrap()
                .into_string()
                .unwrap();
            assert_eq!(first.matches("result-canary").count(), 1);
            assert_eq!(first.matches("question-canary").count(), 1);
            let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
            summary["preventedContinuation"] = json!(true);
            // The acceptance is outside both a bounded whole-window read and a tail read.
            std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript)
                .unwrap()
                .write_all(format!("{summary}\n{output}\n").as_bytes())
                .unwrap();
            std::thread::sleep(Duration::from_millis(2));
            // No watcher has seen S1's acceptance when S2 and then UPS reach the hook.
            for event in ["Stop", "UserPromptSubmit"] {
                let reply = post(&core, event, "next turn", Duration::from_secs(2))
                    .unwrap()
                    .into_string()
                    .unwrap();
                assert!(
                    !reply.contains("result-canary") && !reply.contains("question-canary"),
                    "{boundary} -> {event}: accepted S1 payload was repeated"
                );
            }
            let stamp = with(&core.db, |c| {
                Ok((
                    delegate::get(c, id)?.delivered_at,
                    crate::questions::get(c, question.id)?.leader_delivered_at,
                ))
            })
            .unwrap();
            assert!(stamp.0.is_some() && stamp.1.is_some());
            observe(&core, summary);
            assert_eq!(
                with(&core.db, |c| {
                    Ok((
                        delegate::get(c, id)?.delivered_at,
                        crate::questions::get(c, question.id)?.leader_delivered_at,
                    ))
                })
                .unwrap(),
                stamp
            );
            assert_eq!(core.agent_states.get("timur").status, Status::Working);
            assert!(input.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn unread_accepted_stop_before_continuation_is_delivered_once() {
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure", "Stop"] {
            for (watch_rows, boundary) in
                (0..=2).flat_map(|n| ["UserPromptSubmit", "PostToolUse"].map(|b| (n, b)))
            {
                let (core, input) = core("unread-accepted", NullSink);
                let _finish = Finish(core.clone());
                let id = result(&core);
                let transcript = core.paths.data_dir.join("transcript.jsonl");
                // Old history must remain outside the current evidence window.
                std::fs::write(&transcript, format!("{}\n", "x".repeat(64 * 1024))).unwrap();
                core.agent_states
                    .set_transcript("timur", transcript.to_str().unwrap());
                core.claude_hook_received(
                    "timur",
                    "UserPromptSubmit",
                    Some("session-a"),
                    next_seq(),
                );
                core.claude_hook_received("timur", boundary, Some("session-a"), next_seq());
                let output =
                    json!({"type":"assistant","message":{"content":"x".repeat(128 * 1024)}});
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&transcript)
                    .unwrap()
                    .write_all(format!("{output}\n").as_bytes())
                    .unwrap();
                core.delegation_on_hook("timur", "Stop", &json!({}), false)
                    .unwrap();
                let mut summary = row("stop_hook_summary", crate::diagnostics::now_ms());
                summary["preventedContinuation"] = json!(true);
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&transcript)
                    .unwrap()
                    .write_all(format!("{summary}\n{output}\n").as_bytes())
                    .unwrap();
                if watch_rows == 1 {
                    observe(&core, summary.clone());
                }
                std::thread::sleep(Duration::from_millis(2));
                core.claude_hook_received("timur", event, Some("session-a"), next_seq());
                let stamp = with(&core.db, |c| delegate::get(c, id))
                    .unwrap()
                    .delivered_at;
                assert!(
                    stamp.is_some(),
                    "{event}, {boundary}, watch_rows={watch_rows}: unread acceptance was lost"
                );
                if watch_rows == 2 {
                    observe(&core, summary.clone());
                }
                let reply = post(
                    &core,
                    "UserPromptSubmit",
                    "next turn",
                    Duration::from_secs(2),
                )
                .unwrap()
                .into_string()
                .unwrap();
                assert!(!reply.contains("result-canary"));
                observe(&core, summary);
                assert_eq!(
                    with(&core.db, |c| delegate::get(c, id))
                        .unwrap()
                        .delivered_at,
                    stamp
                );
                assert!(input.lock().unwrap().is_empty());
                assert_eq!(core.agent_states.get("timur").status, Status::Working);
            }
        }
    }

    #[test]
    fn delayed_turn_duration_after_stop_processing_idles_leader_and_pastes_question() {
        let (core, input) = core("late-duration", NullSink);
        let _finish = Finish(core.clone());
        let id = result(&core);
        let ended = row("turn_duration", crate::diagnostics::now_ms());
        std::thread::sleep(Duration::from_millis(2));
        let reply: Value = post(&core, "Stop", "", Duration::from_secs(2))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(reply["decision"], "block");
        assert_eq!(core.agent_states.get("timur").status, Status::Working);
        let q = with(&core.db, |c| {
            let job = delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &delegate::caller(c, "timur")?,
                    to_agent: "ayla".into(),
                    team_id: "default".into(),
                    service_team_id: None,
                    prompt: "work".into(),
                    task_id: None,
                    patron_words: None,
                    effort: None,
                },
            )?;
            delegate::set_status(c, job.id, "running")?;
            crate::questions::ask(
                c,
                &delegate::caller(c, "ayla")?,
                &json!({"delegationId":job.id,"question":"question-canary"}),
            )
        })
        .unwrap();
        // The watcher reads the CLI's earlier completion only after Stop returned block.
        observe(&core, ended.clone());
        assert_eq!(core.agent_states.get("timur").status, Status::Idle);
        core.leader_notes_wake("timur");
        let prompt = core
            .leader_pastes
            .lock()
            .unwrap()
            .get("timur")
            .unwrap()
            .text
            .clone();
        assert!(prompt.contains("question-canary"));
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_none());
        assert!(with(&core.db, |c| crate::questions::get(c, q.id))
            .unwrap()
            .leader_delivered_at
            .is_none());
        let retry = post(&core, "UserPromptSubmit", &prompt, Duration::from_secs(2))
            .unwrap()
            .into_string()
            .unwrap();
        assert_eq!(retry.matches("result-canary").count(), 1);
        assert!(!retry.contains("question-canary"));
        assert!(with(&core.db, |c| crate::questions::get(c, q.id))
            .unwrap()
            .leader_delivered_at
            .is_some());
        observe(&core, ended);
        assert_eq!(core.agent_states.get("timur").status, Status::Working);
        assert_eq!(
            input
                .lock()
                .unwrap()
                .windows(6)
                .filter(|w| *w == b"\x1b[200~")
                .count(),
            1
        );
    }

    #[test]
    fn missing_stop_acceptance_retries_once_on_idle_until_matching_ups() {
        for evidence in ["no-path", "missing-file", "unreadable", "no-summary"] {
            let (core, input) = core("missing-acceptance", NullSink);
            let _finish = Finish(core.clone());
            let id = result(&core);
            let transcript = core.paths.data_dir.join("transcript.jsonl");
            match evidence {
                "no-path" => {}
                "unreadable" => std::fs::create_dir(&transcript).unwrap(),
                "no-summary" => std::fs::write(&transcript, "").unwrap(),
                _ => {}
            }
            if evidence != "no-path" {
                core.agent_states
                    .set_transcript("timur", transcript.to_str().unwrap());
            }
            let out: Value = post(&core, "Stop", "", Duration::from_secs(2))
                .unwrap()
                .into_json()
                .unwrap();
            assert_eq!(out["decision"], "block");
            core.claude_hook_received("timur", "PreToolUse", Some("session-a"), next_seq());
            assert!(
                with(&core.db, |c| delegate::get(c, id))
                    .unwrap()
                    .delivered_at
                    .is_none(),
                "{evidence}"
            );
            assert!(input.lock().unwrap().is_empty());
            crate::hooks::turn_ended(&core, "timur", "Settled");
            for _ in 0..3 {
                core.leader_notes_wake("timur");
            }
            let prompt = core
                .leader_pastes
                .lock()
                .unwrap()
                .get("timur")
                .unwrap()
                .text
                .clone();
            assert!(with(&core.db, |c| delegate::get(c, id))
                .unwrap()
                .delivered_at
                .is_none());
            let reply = post(&core, "UserPromptSubmit", &prompt, Duration::from_secs(2))
                .unwrap()
                .into_string()
                .unwrap();
            assert_eq!(reply.matches("result-canary").count(), 1, "{evidence}");
            let stamp = with(&core.db, |c| delegate::get(c, id))
                .unwrap()
                .delivered_at;
            assert!(stamp.is_some());
            crate::hooks::turn_ended(&core, "timur", "Settled");
            for _ in 0..3 {
                core.leader_notes_wake("timur");
            }
            let again = post(
                &core,
                "UserPromptSubmit",
                "next turn",
                Duration::from_secs(2),
            )
            .unwrap()
            .into_string()
            .unwrap();
            assert!(!again.contains("result-canary"));
            assert_eq!(
                with(&core.db, |c| delegate::get(c, id))
                    .unwrap()
                    .delivered_at,
                stamp
            );
            assert_eq!(
                input
                    .lock()
                    .unwrap()
                    .windows(6)
                    .filter(|w| *w == b"\x1b[200~")
                    .count(),
                1,
                "{evidence}"
            );
        }
    }

    #[test]
    fn six_second_stop_timeout_retries_result_and_question_once_at_matching_ups() {
        struct Delay {
            armed: AtomicBool,
            entered: std::sync::mpsc::Sender<()>,
            done: Arc<AtomicBool>,
        }
        impl EventSink for Delay {
            fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
                if event == "delegations-changed" && self.armed.swap(false, Ordering::SeqCst) {
                    self.entered.send(()).unwrap();
                    std::thread::sleep(Duration::from_secs(6));
                }
                if event == "dex-hook" && payload["event"] == "Stop" {
                    self.done.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }
        let (entered, wait) = std::sync::mpsc::channel();
        let stop_done = Arc::new(AtomicBool::new(false));
        let (core, input) = core(
            "six-second",
            Delay {
                armed: AtomicBool::new(true),
                entered,
                done: stop_done.clone(),
            },
        );
        let id = result(&core);
        let request = core.clone();
        let client = std::thread::spawn(move || post(&request, "Stop", "", Duration::from_secs(5)));
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            client.join().unwrap().is_err(),
            "CLI's unchanged five-second timeout"
        );
        let (q, observer) = with(&core.db,|c| {
            let leader = delegate::caller(c,"timur")?;
            let job = delegate::insert(c,delegate::NewDelegation {
                from:&leader,to_agent:"ayla".into(),team_id:"default".into(),service_team_id:None,
                prompt:"work".into(),task_id:None,patron_words:None,effort:None,
            })?;
            delegate::set_status(c,job.id,"running")?;
            let end = json!({"type":"attachment","sessionId":"session-a","timestamp":timestamp(crate::diagnostics::now_ms()),
                "attachment":{"type":"hook_cancelled","hookEvent":"Stop","timedOut":true,"timeoutMs":5000}});
            // The live transcript observer has its own thread. Keep both DB writes atomic
            // while its completion eligibility query waits for this connection lock.
            let request = core.clone();
            let observer = std::thread::spawn(move || observe(&request, end));
            let q = crate::questions::ask(c,&delegate::caller(c,"ayla")?,&json!({"delegationId":job.id,"question":"question-canary"}))?;
            Ok((q, observer))
        }).unwrap();
        observer.join().unwrap();
        let _ = core.events.emit("questions-changed", &());
        wait_for(
            || core.leader_pastes.lock().unwrap().contains_key("timur"),
            Duration::from_secs(2),
        );
        let prompt = core
            .leader_pastes
            .lock()
            .unwrap()
            .get("timur")
            .unwrap()
            .text
            .clone();
        assert!(prompt.contains("question-canary"));
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_none());
        assert!(with(&core.db, |c| crate::questions::get(c, q.id))
            .unwrap()
            .leader_delivered_at
            .is_none());
        let reply: Value = post(&core, "UserPromptSubmit", &prompt, Duration::from_secs(2))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(reply.to_string().matches("result-canary").count(), 1);
        assert!(
            !reply.to_string().contains("question-canary"),
            "question already arrived in the paste"
        );
        wait_for(|| stop_done.load(Ordering::SeqCst), Duration::from_secs(3));
        assert_eq!(
            core.agent_states.get("timur").status,
            Status::Working,
            "late Stop cannot close new UPS"
        );
        assert!(with(&core.db, |c| delegate::get(c, id))
            .unwrap()
            .delivered_at
            .is_some());
        assert!(with(&core.db, |c| crate::questions::get(c, q.id))
            .unwrap()
            .leader_delivered_at
            .is_some());
        core.leader_notes_wake("timur");
        let bytes = input.lock().unwrap().clone();
        assert_eq!(
            bytes.windows(6).filter(|w| *w == b"\x1b[200~").count(),
            1,
            "one paste, no duplicate retry"
        );
        finish(&core);
    }
}
