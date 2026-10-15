//! Dex core: everything the app does, without a UI framework. Hosts (the Tauri shell today,
//! `dexd` in S2) build one [`Core`] at startup and call into it; they only translate transport.

pub mod browser;
pub mod agents;
pub mod chain;
pub mod chat;
pub mod claude_accounts;
pub mod cost;
pub mod db;
pub mod dexmsg;
pub mod diagnostics;
pub mod effort;
pub mod job_model;
pub mod delegate;
pub mod events;
pub mod first_reply;
pub mod gpu;
pub mod hooks;
mod hook_delivery;
mod local_http;
mod claude_turn;
mod team_turn;
pub mod instance;
pub mod inbox;
pub mod inbox_reply;
pub mod layout;
pub mod leader_reset;
pub mod mcp;
pub mod memory;
pub mod model_news;
pub mod motivation;
pub mod notes;
pub mod paths;
pub mod private_fs;
pub mod desktop_storage;
pub mod permit;
pub mod plan;
pub mod proc;
pub mod progress;
pub mod prompts;
pub mod proposals;
pub mod push;
pub mod projects;
pub mod project_context;
pub mod providers;
pub mod shell;
pub mod pty;
pub mod pty_stream;
pub mod questions;
mod worker_notes;
pub mod quota;
pub mod repomap;
pub mod restart;
pub mod review;
pub mod roles;
pub mod rules;
pub mod secrets;
pub mod settings;
pub mod maintenance;
pub mod status;
pub mod step;
pub mod skills;
pub mod slug;
pub mod stack;
pub mod teams;
pub mod test_budget;
pub mod usage;
pub mod worktree;
mod merge_gate;

use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

pub use db::Db;
pub use events::{EventSink, Events};
pub use paths::Paths;
pub use pty_stream::PtySink;

use db::{tasks, with};

pub struct Core {
    pub paths: Paths,
    pub events: Events,
    pub db: Db,
    /// Hook receiver (loopback HTTP; also serves `/mcp`).
    pub hooks: hooks::Hooks,
    pub mcp: mcp::Mcp,
    /// Browser MCP config for catalog roles flagged `browser` (DEX-20).
    pub browser: browser::Browser,
    /// Per-process role-skill plugin root (DEX-23).
    pub skills: skills::Skills,
    /// #41: this process's hold on `<data_dir>/dex.lock`; [`Core::shutdown`] lets it go.
    instance: Mutex<Option<instance::Lock>>,
    pub ptys: pty::Ptys,
    pub agent_states: agents::States,
    pub(crate) observations: status::Observations,
    /// FIX-3: first-reply watchdog (turn started, no event yet).
    pub first_reply: first_reply::Watch,
    /// FIX-4: agents started with `--resume`, until a hook proves the session opened.
    resume_watch: Mutex<std::collections::HashMap<String, agents::ResumeWatch>>,
    agent_start_gate: Mutex<()>,
    /// Serializes turn admission with automatic leader/report close; never held by a live Turn.
    admission_gate: Mutex<HashMap<String, restart::Admission>>,
    /// DEX-17: zincir geçişi sürerken kuyruk sevkiyatı durur (öldürülen PTY'nin `pty_exited`'i
    /// aynı ajanı promptsuz başlatmasın diye).
    chain_guard: Mutex<HashSet<String>>,
    /// Sağlayıcı reset zamanlayıcısı nesilleri: aynı sağlayıcı yeniden işaretlenirse eski timer
    /// olay yayınlamaz. Event-driven; polling yok.
    chain_resets: Mutex<HashMap<String, i64>>,
    /// Görev #250 / #2019: Claude hesabı başına son kullanım ölçümü ve kaynağı (testte sahtesi;
    /// `quota_set_source`).
    pub(crate) quota: quota::Accounts,
    /// #1861: kullanım şeridinin sağlayıcı ölçümleri (Claude quota'dan, GLM/Muse kendi uçlarından).
    pub(crate) usage: usage::Meter,
    /// S5-B: live transcript watches (one per agent) + queued chat texts per agent.
    pub(crate) chat_watches: chat::Watches,
    pub(crate) chat_queues: Mutex<HashMap<String, VecDeque<String>>>,
    /// Per-agent paste transaction: readiness through the final Enter, shared by chat/inbox.
    delivery_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    inbox_gate: Mutex<()>,
    /// #1718: share turns awaiting their final answer; FCM token cache.
    inbox_marks: inbox_reply::Marks,
    pub(crate) push: push::Push,
    /// Görev #12: workers with a pending queue retry — at most one live retry thread per worker.
    /// #18: keyed to the thread's generation; a newer retry makes an older sleeping thread stale.
    queue_retry: Mutex<HashMap<String, u64>>,
    /// Görev #19: whose turn each worker is in + the one-time unreported-Stop refusals.
    turns: delegate::Turns,
    /// Görev #23: workers' permit requests the policy sent to their leader, and the grants.
    pub(crate) permits: permit::Permits,
    pub(crate) answers: questions::Answers,
    /// Görev #20: workers whose session is being replaced for their next job (PTY killed, exit
    /// pending; since when); the exit starts the job in a fresh session. A mark older than
    /// [`agents::RESET_STUCK_MS`] is stale (the exit never came) and is dropped.
    resetting: Mutex<HashMap<String, std::time::Instant>>,
    /// Görev #51: leaders asked for a hand-over note before their clean session.
    pub(crate) leader_resets: leader_reset::Resets,
    /// #126: idle watch of each running worker session; a newer session's generation makes the
    /// older watcher stale.
    idle_close: Mutex<HashMap<String, u64>>,
    /// Successful reports in the current session, consumed after its final Stop.
    reported_workers: Mutex<HashSet<String>>,
    team_idle: Mutex<HashMap<String, restart::TeamIdle>>,
    /// Only leaders suspended by Dex may be resumed by an incoming event.
    sleeping_leaders: Mutex<HashSet<String>>,
    /// Leader prompts awaiting UserPromptSubmit; DB delivery stamps remain pending.
    leader_pastes: Mutex<HashMap<String, plan::LeaderPaste>>,
    /// #2055: workers' Claude task lists → their running job's progress.
    progress: progress::Tracker,
    /// #2055: agent → its last hook time (`States::hook_at`) when its leader got the stall note;
    /// the same silence is told once, whichever client's watchdog asks.
    stall_noted: Mutex<HashMap<String, u64>>,
    claude_turns: claude_turn::Turns,
    worker_note_receipts: worker_notes::Receipts,
    /// #161: jobs wait in the queue (restart.rs); the host restarts once nothing runs.
    draining: std::sync::atomic::AtomicBool,
    /// #427/#447: turns past the gate (`Core::admit`), not shown yet (restart.rs: a drain's blocker
    /// until they are — the job is not `running`, the agent not Working yet).
    admitted: std::sync::atomic::AtomicUsize,
    /// #1875: the live snapshot (`dex.live.json`) a crash leaves for the next core.
    live: Mutex<restart::Live>,
    /// #427 (tests): run once inside the next dispatch, right after its drain check.
    #[cfg(test)]
    dispatch_pause: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// #447 (tests): run once inside the next paste, its text written, before its Enter.
    #[cfg(test)]
    paste_pause: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests: interleave patron input at the final Enter submission boundary.
    #[cfg(test)]
    paste_enter_pause: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    close_pause: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests: close an idle leader immediately before the next turn is admitted.
    #[cfg(test)]
    admit_pause: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Görev #289: GPU broker token'ı (sır deposundan, bellekte) + adres/önbellek durumu.
    pub(crate) gpu: gpu::State,
    weak_self: Weak<Core>,
}

impl Core {
    /// Opens (and migrates) `<data_dir>/dex.db`, starts the hook/MCP receiver and writes their
    /// per-process settings files into `config_dir`. Call [`Core::shutdown`] on exit.
    pub fn start(paths: Paths, events: Events) -> Result<Arc<Self>, String> {
        Self::start_with(paths, events, true)
    }

    /// FIX-5: `dexd` girişi — varsayılan takım tohumlanmaz, boş başlar.
    pub fn start_bare(paths: Paths, events: Events) -> Result<Arc<Self>, String> {
        Self::start_with(paths, events, false)
    }

    fn start_with(paths: Paths, events: Events, seed_default_team: bool) -> Result<Arc<Self>, String> {
        private_fs::create_dir(&paths.data_dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        private_fs::create_dir(&paths.config_dir).map_err(|e| e.to_string())?;
        // #41: before anything reads dex.db — a second Core would reconcile the first's work away.
        let instance = instance::acquire(&paths.data_dir)?;
        let conn = if seed_default_team {
            db::open(&paths.data_dir.join("dex.db"))
        } else {
            db::open_bare(&paths.data_dir.join("dex.db"))
        }
        .map_err(|e| e.to_string())?;
        // #415/#463: once, before the Core exists — no plan watch, resume or session holds a path yet.
        if let Err(err) = slug::migrate_memory(&conn, &paths.data_dir) {
            eprintln!("dex: hafıza taşıma: {err}");
        }
        // #1728: GLM/Muse'nin son başarılı model listesi — ağ beklemeden seçici ve doğrulama görsün.
        providers::set_live(model_news::catalogs(&conn));
        let (hooks, listener) = hooks::start(&paths.config_dir)?;
        let mcp = mcp::start(&paths.config_dir, hooks.port)?;
        // DEX-22: every `delegations-changed` also advances the leaders' plans.
        let (events, kicks) = plan::tap(events);
        let browser = browser::start(&paths.config_dir)?;
        let skills = skills::start(&paths.config_dir)?;
        let db = Db::new(conn);
        let quota = quota::Accounts::new(&paths.data_dir);
        let core = Arc::new_cyclic(|weak| Self {
            paths,
            events,
            db,
            hooks,
            mcp,
            browser,
            skills,
            instance: Mutex::new(Some(instance)),
            ptys: pty::Ptys::bound(weak.clone()),
            agent_states: agents::States::default(),
            observations: status::Observations::default(),
            first_reply: first_reply::Watch::default(),
            resume_watch: Mutex::default(),
            agent_start_gate: Mutex::new(()),
            admission_gate: Mutex::default(),
            chain_guard: Mutex::default(),
            chain_resets: Mutex::default(),
            quota,
            usage: usage::Meter::default(),
            chat_watches: chat::Watches::default(),
            chat_queues: Mutex::default(),
            inbox_gate: Mutex::default(),
            inbox_marks: inbox_reply::Marks::default(),
            push: push::Push::default(),
            delivery_gates: Mutex::default(),
            queue_retry: Mutex::default(),
            turns: delegate::Turns::default(),
            permits: permit::Permits::default(),
            answers: questions::Answers::default(),
            resetting: Mutex::default(),
            leader_resets: leader_reset::Resets::default(),
            idle_close: Mutex::default(),
            reported_workers: Mutex::default(),
            team_idle: Mutex::default(),
            sleeping_leaders: Mutex::default(),
            leader_pastes: Mutex::default(),
            progress: progress::Tracker::default(),
            stall_noted: Mutex::default(),
            claude_turns: claude_turn::Turns::default(),
            worker_note_receipts: worker_notes::Receipts::default(),
            draining: Default::default(),
            admitted: Default::default(),
            live: Mutex::default(),
            #[cfg(test)]
            dispatch_pause: Mutex::default(),
            #[cfg(test)]
            paste_pause: Mutex::default(),
            #[cfg(test)]
            paste_enter_pause: Mutex::default(),
            #[cfg(test)]
            close_pause: Mutex::default(),
            #[cfg(test)]
            admit_pause: Mutex::default(),
            gpu: gpu::State::default(),
            weak_self: weak.clone(),
        });
        // #161: a restart (drained, an update) left its marker: the queue stays, its leaders come back.
        // #1875: no marker, but a fresh live snapshot — the last core went down unplanned (a crash,
        // an installer's kill): the same. A clean exit (shutdown, no window) leaves neither.
        // Both are taken: a live file left beside a marker is no later crash's.
        let live = restart::take_live(&core.paths.data_dir);
        let restarted = restart::take(&core.paths.data_dir).or(live);
        let keep_queued = restarted.is_some();
        // #1875: the running jobs of workers cut mid-turn stay running; their sessions come back.
        let kept = match &restarted {
            Some(m) => with(&core.db, |c| restart::kept_jobs(c, &m.busy))?,
            None => Vec::new(),
        };
        let kept_ids: Vec<i64> = kept.iter().map(|(id, _)| *id).collect();
        // Before reconcile closes them: plan items in flight get delegated again (on `plan_resume`).
        with(&core.db, |c| plan::requeue_interrupted_keeping(c, keep_queued, &kept_ids))?;
        plan::watch(kicks, core.db.clone(), core.events.clone(), core.ptys.clone());
        // DEX-30: no session survives a restart, so bulk model changes left waiting land now.
        with(&core.db, teams::apply_all_pending)?;
        let n = with(&core.db, |c| delegate::reconcile_on_start_keeping(c, keep_queued, &kept_ids))?;
        if n > 0 {
            eprintln!("dex: {n} açık devretme kapatıldı (önceki oturumdan)");
        }
        // Backend orchestration is ready; active plans resume exactly once per Core start.
        core.plan_resume()?;
        core.chain_schedule_existing();
        // #1987: saklanan son Muse okuması belleğe (ölçüm iş parçacıkları başlamadan).
        core.usage_restore_muse();
        core.quota_watch();
        core.usage_watch();
        #[cfg(not(test))]
        core.maintenance_watch();
        #[cfg(not(test))]
        core.live_watch();
        core.gpu_refresh_later();
        if let Some(m) = restarted {
            let weak = core.weak_self.clone();
            std::thread::spawn(move || {
                if let Some(core) = weak.upgrade() {
                    core.restart_resume(m, kept);
                }
            });
        }
        // #195: git and the disk off the start path.
        let weak = core.weak_self.clone();
        std::thread::spawn(move || {
            if let Some(core) = weak.upgrade() {
                core.worktree_sweep();
            }
        });
        core.inbox_resume()?;
        core.inbox_reply_resume()?;
        core.push_status_line();
        hooks::accept(listener, core.clone());
        Ok(core)
    }

    /// Removes the per-process files (claude sessions of this process die with it anyway).
    pub fn shutdown(&self) {
        self.team_idle.lock().unwrap().clear();
        // #1875: a clean exit: the next start is not a crash's.
        self.live_close();
        diagnostics::event("shutdown", serde_json::json!({}));
        self.hooks.remove_settings();
        self.mcp.remove_config();
        self.browser.remove_config();
        self.skills.remove_all();
        self.instance.lock().unwrap().take();
    }

    // Mutations emit the event the frontend refreshes on; an emit failure never fails the call.
    fn changed(&self, event: &str) {
        let _ = self.events.emit(event, &());
    }

    // ---- layout ----

    pub fn layout_load(&self) -> Result<Option<Value>, String> {
        layout::read_file(&layout::path_of(&self.paths.config_dir))
    }

    pub fn layout_save(&self, layout: &Value) -> Result<(), String> {
        layout::write_file(&layout::path_of(&self.paths.config_dir), layout)
    }

    // ---- tasks (every mutation emits `tasks-changed`) ----

    pub fn task_list(&self, team_id: Option<&str>) -> Result<Vec<tasks::Task>, String> {
        with(&self.db, |c| tasks::list(c, team_id))
    }

    pub fn task_create(&self, task: tasks::NewTask) -> Result<tasks::Task, String> {
        let t = with(&self.db, |c| tasks::create(c, task))?;
        self.changed("tasks-changed");
        Ok(t)
    }

    pub fn task_update(&self, id: i64, patch: tasks::TaskPatch) -> Result<tasks::Task, String> {
        let t = with(&self.db, |c| tasks::update(c, id, patch))?;
        self.changed("tasks-changed");
        Ok(t)
    }

    pub fn task_delete(&self, id: i64) -> Result<(), String> {
        with(&self.db, |c| tasks::delete(c, id))?;
        self.changed("tasks-changed");
        Ok(())
    }

    // ---- roles ----

    pub fn role_list(&self) -> Result<Vec<roles::Role>, String> {
        with(&self.db, roles::list)
    }

    // ---- delegations (every change emits `delegations-changed`) ----

    pub fn delegation_list(&self) -> Result<Vec<delegate::Delegation>, String> {
        with(&self.db, delegate::list_recent)
    }

    pub fn delegation_ack(&self, id: i64, error: Option<String>, queued: bool) -> Result<(), String> {
        delegate::ack(&self.db, &self.events, id, error, queued)
    }

    pub fn delegation_session_exited(&self, agent_id: &str) -> Result<(), String> {
        if let Some(next) = self.delegation_session_exit_next(agent_id)? {
            self.delegation_dispatch(next)?;
        }
        Ok(())
    }

    /// Settle the exited session before a new admission; its queued dispatch can run afterward.
    fn delegation_session_exit_next(&self, agent_id: &str) -> Result<Option<delegate::Delegation>, String> {
        // DEX-17: a chain transition owns this exit. Do not let a pending team-wide model
        // change clear its transient target before the replacement PTY is started.
        if self.chain_guard.lock().unwrap().contains(agent_id) {
            return Ok(None);
        }
        // Görev #94: sıfırlanma olurken çalışan ajanın ertelenen zincir dönüşü oturum kapanınca
        // uygulanır. Asıl sağlayıcı hâlâ tükenmişse (yeniden limit yediyse) bayrak düşer, ajan kalır.
        if with(&self.db, |c| teams::take_chain_return_pending(c, agent_id)).unwrap_or(false) {
            let usable = with(&self.db, |c| {
                let a = teams::get_agent(c, agent_id)?;
                let on_temp = a.active_provider.is_some() || a.active_engine.is_some();
                Ok(!on_temp
                    || !chain::exhausted(c)?.iter().any(|e| e.provider == a.provider))
            })
            .unwrap_or(false);
            if usable {
                // Yeni oturum chain_guard'sız açılır; aşağıdaki normal çıkış akışı (bekleyen
                // toplu değişiklik + devretme defteri) aynen sürer.
                if let Err(err) = self.chain_back(agent_id) {
                    eprintln!("dex: {agent_id}: ertelenen zincir dönüşü başarısız: {err}");
                }
            }
        }
        // DEX-30 first: a queued job restarting the worker must start it on the new engine/model.
        if let Err(err) = self.agent_apply_pending(agent_id) {
            eprintln!("dex: {agent_id}: {err}");
        }
        self.worktree_retry(agent_id);
        delegate::session_exited(&self.db, &self.events, agent_id)
    }

    /// DEX-19: the patron opened the worker's pane; its finished results drop the dock mark.
    pub fn delegation_seen(&self, agent_id: &str) -> Result<(), String> {
        delegate::seen(&self.db, &self.events, agent_id)
    }

    /// Hook receiver side: Stop / StopFailure / UserPromptSubmit / PreToolUse → optional JSON answer.
    /// `preserve_failure` (DEX-17): StopFailure zincir geçişine gidiyor — devretme `failed`
    /// OLMAZ, aynı iş sürer (running satırlara dokunulmaz, kuyruk teslim edilmez).
    pub fn delegation_on_hook(&self, agent: &str, event: &str, body: &Value, preserve_failure: bool) -> Option<String> {
        let stop = self.claude_hook_received(agent, event, body["session_id"].as_str(), agents::next_seq());
        self.delegation_hook_prepared(agent, event, body, preserve_failure, stop.as_ref())
    }

    pub(crate) fn delegation_hook_prepared(&self, agent: &str, event: &str, body: &Value, preserve_failure: bool, stop: Option<&claude_turn::Stop>) -> Option<String> {
        if event == "PostToolUse" { self.progress_on_hook(agent, body); }
        if matches!(event, "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "Stop") {
            self.worker_notes_reconcile(agent);
            if event == "UserPromptSubmit" { self.worker_notes_prompt_received(agent, body); }
        }
        if event == "Stop" && stop.is_none() && body["session_id"].is_string() {
            return None; // Its ingress was fenced by a completed or newer turn.
        }
        let pending = (event == "Stop").then(hook_delivery::Pending::default);
        if event == "UserPromptSubmit" {
            self.inbox_prompt_received(agent, body.get("prompt").and_then(Value::as_str).unwrap_or(""));
            self.reported_workers.lock().unwrap().remove(agent);
            if !self.leader_paste_accepted(agent, body.get("prompt").and_then(Value::as_str).unwrap_or("")) {
                // A failed acknowledgement leaves the payload pending for a later hook.
                return None;
            }
        }
        // Görev #23: a worker's permission request is answered by policy, never the patron.
        if event == "PermissionRequest" {
            return self.permit_on_request(agent, body);
        }
        // Görev #26: the same policy before the tool runs — whatever the session's permission mode
        // (auto / acceptEdits / bypass never raise a PermissionRequest). A deny ends it here.
        let pre = (event == "PreToolUse").then(|| self.permit_answer(agent, body, permit::GRAY_AT_PRE_TOOL)).flatten();
        if let Some(Err(why)) = &pre {
            return Some(pre_tool_answer("deny", Some(why)));
        }
        // DEX-33: the whole e2e suite is QA's (test budget); others give a spec or a grep.
        if event == "PreToolUse" {
            if let Some(out) = with(&self.db, |c| Ok(test_budget::on_pre_tool(c, agent, body))).ok().flatten() {
                diagnostics::permit_hook_reply(agent, body, Some(&out));
                return Some(out);
            }
        }
        // DEX-24: a worker waiting for its leader's answer keeps its job; questions/answers ride along.
        // DEX-17: zincir geçişinde devretme satırlarına dokunulmaz (failed OLMAZ), soru-cevap akar.
        let out = questions::on_hook_pending(&self.db, &self.events, agent, event, body, pending.as_ref(), || {
            if preserve_failure && event == "StopFailure" {
                return None;
            }
            let out = delegate::on_hook_pending(&self.db, &self.events, &self.turns, agent, event, body, pending.as_ref());
            // DEX-22: plan / member-proposal notes for the leader ride along.
            plan::with_notes_pending(&self.db, agent, event, out, pending.as_ref())
        });
        // Plan #114: the patron's per-message append rides the leader's own prompts.
        let out = if event == "UserPromptSubmit" { prompts::with_message_append(&self.db, agent, body, out) } else { out };
        if event == "PreToolUse" { diagnostics::permit_hook_reply(agent, body, out.as_deref()); }
        if let Some(pending) = pending.filter(|_| is_blocked(&out)) {
            if let (Some(stop), Some(reply)) = (stop, out.as_deref().and_then(|o| serde_json::from_str::<Value>(o).ok())) {
                if let Some(reason) = reply["reason"].as_str() {
                    self.claude_stage_stop(agent, stop, pending.0.into_inner().unwrap(), reason);
                }
            }
        }
        // Görev #26: an allowed call skips the patron's dialog — unless a Dex rule above answered
        // (service-worker limits, AskUserQuestion).
        if let (None, Some(Ok(()))) = (&out, &pre) {
            return Some(pre_tool_answer("allow", None));
        }
        // S5-B: a Stop that continued nothing ends the turn — the chat queue goes in.
        if event == "Stop" && !is_blocked(&out) {
            self.inbox_reply_on_stop(agent, body["last_assistant_message"].as_str());
            self.chat_on_stop(agent);
            self.delegation_idle(agent, true);
        }
        out
    }

    /// Görev #12: `on_stop` read the queue before the worker went idle (hooks.rs sets the status
    /// after this hook answers); a job queued in that window saw "working" and would wait for a
    /// Stop that never comes. Once idle, the oldest queued job goes in. A pending chat queue goes
    /// first (`chat_on_stop`): its paste starts a turn whose Stop delivers the job.
    fn delegation_after_stop(&self, agent: &str) {
        self.inbox_wake(agent);
        self.delegation_idle(agent, false);
    }

    /// Interrupt/Settled can deliver the queue, but only a real final Stop may close a report.
    fn delegation_idle(&self, agent: &str, reported_stop: bool) {
        if self.chat_queues.lock().unwrap().get(agent).is_some_and(|q| !q.is_empty()) {
            return;
        }
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(700));
            if let Some(core) = weak.upgrade() {
                core.delegation_dispatch_next(&agent, true);
                // Görev #51: a leader's pending hand-over prompt goes in at the same idle moment.
                core.leader_reset_deliver(&agent);
                if reported_stop { core.worker_close_delivered(&agent); }
                core.wake_sleeping_leaders();
            }
        });
    }

    /// The worker's oldest queued job, unless it has a running one (its Stop delivers then).
    fn delegation_dispatch_next(&self, agent: &str, emit: bool) {
        if self.chain_guard.lock().unwrap().contains(agent) {
            // DEX-17: the transition restarts the worker; try again once it is through.
            self.delegation_retry(agent);
            return;
        }
        let next = with(&self.db, |conn| delegate::next_for_worker(conn, agent));
        let result = match next {
            Ok(Some(d)) => self.dispatch(d, emit).map(|_| ()),
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            eprintln!("dex: {agent}: queued delegation: {err}");
            self.delegation_retry(agent);
        }
    }

    /// A job queued by the worker's own state (busy, patron typing) — not behind other work —
    /// is tried again later. One pending retry per worker; it ends when nothing is queued any more
    /// or the worker gets a running job (whose Stop takes over).
    fn delegation_retry(&self, agent: &str) {
        self.queue_retry_spawn(agent, false);
    }

    /// #18: the patron emptied the worker's prompt — retry once its quiet window is over, replacing
    /// a pending retry (that one would wake inside the window and wait another round).
    pub(crate) fn delegation_retry_fresh(&self, agent: &str) {
        self.queue_retry_spawn(agent, true);
    }

    fn queue_retry_spawn(&self, agent: &str, replace: bool) {
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut pending = self.queue_retry.lock().unwrap();
            if !replace && pending.contains_key(agent) {
                return;
            }
            pending.insert(agent.to_string(), gen);
        }
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5_100));
            let Some(core) = weak.upgrade() else { return };
            {
                let mut pending = core.queue_retry.lock().unwrap();
                if pending.get(&agent) != Some(&gen) {
                    return; // stale: a newer retry owns this worker
                }
                pending.remove(&agent);
            }
            core.delegation_dispatch_next(&agent, false);
        });
    }

    /// #125: status says working, yet no hook for [`agents::STALE_WORKING_MS`] and the transcript's
    /// last turn is over (Waiting is left alone: a permission dialog really waits).
    fn stale_working(&self, agent: &str) -> bool {
        let state = self.agent_states.get(agent);
        state.status == agents::Status::Working
            && self.agent_states.silent_for(agent) >= agents::STALE_WORKING_MS
            && state.transcript.is_some_and(|p| chat::claude_turn_ended(std::path::Path::new(&p)))
    }

    /// #126: watches a started worker session; once it has been silent for [`agents::IDLE_CLOSE_MS`]
    /// (no hook, and none since this start) [`Core::idle_close_now`] decides. One sleeping thread per
    /// session; it wakes at the earliest moment the silence can be long enough, and ends with the PTY.
    pub(crate) fn idle_close_watch(&self, agent: &str) {
        let worker = with(&self.db, |c| Ok(teams::get_agent(c, agent).is_ok_and(|a| a.kind == "worker"))).unwrap_or(false);
        if !worker {
            return;
        }
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.idle_close.lock().unwrap().insert(agent.to_string(), gen);
        let armed = std::time::Instant::now();
        let weak = self.weak_self.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            let mut wait = agents::IDLE_CLOSE_MS;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(wait));
                let Some(core) = weak.upgrade() else { return };
                {
                    let mut watches = core.idle_close.lock().unwrap();
                    if watches.get(&agent) != Some(&gen) {
                        return; // a newer session has its own watcher
                    }
                    if !core.ptys.is_running(&agent) {
                        watches.remove(&agent);
                        return;
                    }
                }
                let silent = core.agent_states.silent_for(&agent).min(armed.elapsed().as_millis() as u64);
                if silent < agents::IDLE_CLOSE_MS {
                    wait = agents::IDLE_CLOSE_MS - silent;
                    continue;
                }
                if core.idle_close_now(&agent) {
                    return;
                }
                wait = agents::IDLE_CLOSE_MS;
            }
        });
    }

    /// #126: closes a worker session nobody needs — silent past [`agents::IDLE_CLOSE_MS`], no queued
    /// or running job (an open question or background task rides on a running one), Dex's last turn
    /// (the patron's conversation stays), turn over, nobody typing. Takes the #20 reset mark, so a
    /// job dispatched meanwhile waits for the exit, which then starts it in a fresh session.
    /// `true`: the PTY was closed.
    fn idle_close_now(&self, agent: &str) -> bool {
        self.agent_states.silent_for(agent) >= agents::IDLE_CLOSE_MS && self.close_unneeded(agent, "idle_close")
    }

    pub(crate) fn worker_delivered(&self, d: &delegate::Delegation) {
        self.reported_workers.lock().unwrap().insert(d.to_agent.clone());
    }

    fn worker_close_delivered(&self, agent: &str) -> bool {
        #[cfg(test)]
        if let Some(pause) = self.close_pause.lock().unwrap().take() { pause(); }
        let mut admission = self.admission_gate.lock().unwrap();
        if !self.reported_workers.lock().unwrap().contains(agent) { return false; }
        if let Some(turn) = admission.get_mut(agent) {
            turn.close_pending = true;
            return false;
        }
        if self.agent_states.get(agent).in_turn() { return false; }
        let closed = self.close_unneeded(agent, "reported");
        let next = if closed { self.delegation_session_exit_next(agent) } else { Ok(None) };
        drop(admission);
        // The synchronous PTY exit defers dispatch while admission is locked.
        let dispatched = next.and_then(|next| match next {
            Some(d) => self.delegation_dispatch(d).map(|_| ()),
            None => Ok(()),
        });
        if let Err(err) = dispatched {
            eprintln!("dex: {agent}: session exit: {err}");
            self.delegation_retry(agent);
        }
        closed
    }

    /// The #126 conditions, silence aside: a worker session no job, question, conversation or
    /// queue needs is closed (`reason` for the log and the note). `true`: the PTY was closed.
    fn close_unneeded(&self, agent: &str, reason: &str) -> bool {
        let has_work = |core: &Self| {
            with(&core.db, |c| {
                let worker = teams::get_agent(c, agent).is_ok_and(|a| a.kind == "worker");
                let open: bool = c
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM delegations WHERE to_agent = ?1 AND status IN ('queued','running'))",
                        [agent],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(!worker || open)
            })
            .unwrap_or(true)
        };
        let state = self.agent_states.get(agent);
        let settled = match state.status {
            agents::Status::Idle | agents::Status::Offline => true,
            // #125: a "working" no hook will ever end (claude only: the transcript is the witness).
            agents::Status::Working => self.stale_working(agent),
            agents::Status::Waiting => false,
        };
        if !settled
            || self.turns.dex_turn(agent) == Some(false)
            || self.agent_states.typing(agent)
            || self.chain_guard.lock().unwrap().contains(agent)
            || self.chat_queues.lock().unwrap().get(agent).is_some_and(|q| !q.is_empty())
            || has_work(self)
        {
            return false;
        }
        // One session replacement at a time; read the jobs again under the mark.
        if self.resetting.lock().unwrap().insert(agent.to_string(), std::time::Instant::now()).is_some() {
            return false;
        }
        if has_work(self) {
            self.resetting.lock().unwrap().remove(agent);
            return false;
        }
        if reason == "idle_close" {
            eprintln!("dex: {agent}: idle for {} min with no job; session closed", self.agent_states.silent_for(agent) / 60_000);
            self.ptys.note(agent, &format!("
[90m— boşta {} dk, iş yok: Dex oturumu kapattı; sıradaki iş temiz oturumla açılır —[0m", agents::IDLE_CLOSE_MS / 60_000));
        } else if reason == "reported" {
            self.ptys.note(agent, "\r\n\x1b[90m— iş teslim edildi: Dex oturumu kapattı —\x1b[0m");
        } else {
            eprintln!("dex: {agent}: no job, its worktree merged; session closed");
            self.ptys.note(agent, "
[90m— iş birleştirildi: Dex oturumu kapattı, worktree temizleniyor; sıradaki iş temiz oturumla açılır —[0m");
        }
        // The exit (pty_exited) drops the mark; no session means no exit, so drop it here.
        // Report close holds admission: its exit must not dispatch under that same lock.
        let guarded = reason == "reported";
        if guarded && !self.chain_guard.lock().unwrap().insert(agent.into()) {
            self.resetting.lock().unwrap().remove(agent);
            return false;
        }
        diagnostics::event("pty_kill", serde_json::json!({"agent": agent, "reason": reason}));
        let closed = self.ptys.kill(agent);
        if guarded { self.chain_guard.lock().unwrap().remove(agent); }
        if !closed {
            self.resetting.lock().unwrap().remove(agent);
            return false;
        }
        true
    }

    /// `delegation_merge` (#144). #195: Windows keeps a folder that is a live process's current
    /// directory, and the worker's session started in its job's tree — so a session sitting there
    /// with nothing to do is closed first. What still cannot go waits for that session's exit and
    /// Dex's next start.
    /// #286: a tip with no review and none running gets one now.
    pub fn delegation_merge(&self, c: &delegate::Caller, id: i64, force_policy: bool, force_review: bool) -> Result<Value, String> {
        // Repo haritası (#99): birleştirme sonrası tazeleme bu köke gider; merge'den önce alınır
        // (başarılı merge worktree satırını temizleyebilir).
        let map_root = with(&self.db, |conn| worktree::get(conn, id)).ok().flatten().map(|w| w.root);
        let mut out = worktree::merge(&self.db, c, id, force_policy, force_review, &|w| self.worktree_release(w))?;
        if out["review"]["state"] == "yok" {
            out["note"] = match self.review_open(id) {
                Ok(Some(review::Opened::Started(r))) => {
                    out["review"]["id"] = r.id.into();
                    format!("dal ucu incelenmedi; inceleme #{} açıldı. Karar sonuç olarak sana gelir; onay gelmeden birleştirme yok.", r.id).into()
                }
                Ok(Some(review::Opened::Running(r))) => format!("inceleme #{r} sürüyor; karar gelmeden birleştirme yok.").into(),
                Ok(other) => format!("inceleme açılamadı ({other:?}); tekrar dene ya da patron onayıyla forceReview: true.").into(),
                Err(err) => format!("inceleme açılamadı: {err}").into(),
            };
        }
        if out["merged"] == true {
            self.changed("teams-changed");
            // Repo haritası (#99): birleştirmeyi yavaşlatmadan arka planda tazele.
            if let Some(root) = map_root {
                std::thread::spawn(move || {
                    if let Err(err) = repomap::refresh(&root) {
                        eprintln!("dex: repo haritası tazelenemedi: {err}");
                    }
                });
            }
        }
        Ok(out)
    }

    /// #286: opens the review of job `id`'s branch tip and dispatches it. `None`: nothing to review
    /// (no own branch, merged, no commits beyond the team branch, or itself a review).
    pub(crate) fn review_open(&self, id: i64) -> Result<Option<review::Opened>, String> {
        let (d, w, is_review) = with(&self.db, |c| Ok((delegate::get(c, id)?, worktree::get(c, id)?, review::is_review(c, id)?)))?;
        let Some(w) = w.filter(|w| w.merged_at.is_none() && !is_review) else { return Ok(None) };
        let Some((sha, team)) = worktree::tip(&w) else { return Ok(None) };
        // #1980: no change of its own (only other branches merged in) → nothing to review here;
        // the merged-in work is reviewed on its own branch.
        if matches!(worktree::own_changes(&w), worktree::OwnChanges::None { .. }) {
            return Ok(None);
        }
        let opened = with(&self.db, |c| review::open(c, &d, &w, &sha, &team))?;
        if let review::Opened::Started(r) = &opened {
            let _ = self.events.emit("tasks-changed", &());
            self.delegation_dispatch(r.clone())?;
        }
        Ok(Some(opened))
    }

    /// #286: a job reported — its branch's review opens off the caller's thread (the dispatch waits
    /// for the reviewer's start). #287: a team with no code-review member has no gate, nothing opens.
    pub(crate) fn review_after_report(&self, d: &delegate::Delegation) {
        if d.status != "done" {
            return;
        }
        let weak = self.weak_self.clone();
        let d = d.clone();
        std::thread::spawn(move || {
            let Some(core) = weak.upgrade() else { return };
            match core.review_open(d.id) {
                // #287: no code-review member, no gate — nothing to tell. Only the author is one: patron.
                Ok(Some(review::Opened::NoReviewer)) if with(&core.db, |c| review::gate_on(c, &d.team_id)).unwrap_or(false) => core.note_leader_of(
                    &d,
                    &format!("#{} inceleyici yok: takımın tek code-review üyesi işin yazarı; kod incelemesi açılmadı, birleştirme patron onayına bağlı (onaydan sonra delegation_merge forceReview: true).", d.id),
                ),
                Ok(_) => {}
                Err(err) => {
                    eprintln!("dex: #{} review: {err}", d.id);
                    core.note_leader_of(&d, &format!("#{} kod incelemesi açılamadı: {err} — delegation_merge denemesi yeniden açmayı dener.", d.id));
                }
            }
        });
    }

    /// The worker's session still works in `w` (#838 G: its bound job is that one; an unbound,
    /// pre-project session: its latest started job) and nothing needs it: closed. `true` when it was.
    fn worktree_release(&self, w: &worktree::Wt) -> bool {
        let agent = &w.agent_id;
        if !self.ptys.is_running(agent) {
            return false;
        }
        let here = with(&self.db, |c| match launch_job(c, agent)? {
            Some(job) => Ok(job == w.delegation_id),
            None => Ok(worktree::active(c, agent)?.is_some_and(|a| a.delegation_id == w.delegation_id)),
        });
        here.unwrap_or(false) && self.close_unneeded(agent, "worktree_merged")
    }

    /// #838 G (#906): job `d`, pasted into its agent's live bound session of the same project,
    /// becomes the session's job — the `agent_sessions` row and the spawn's MCP key together; an
    /// unbound row or key: neither. The prior jobs (row, key) when they changed. DB → MCP lock
    /// order serializes takes and restores; neither half can interleave with another paste.
    fn session_take_job(&self, d: &delegate::Delegation) -> Result<Option<(i64, i64)>, String> {
        with(&self.db, |c| {
            let Some(row) = agents::rebind_session_job(c, &d.to_agent, d.id)? else { return Ok(None) };
            match self.mcp.rebind_job(&d.to_agent, row, d.id) {
                Some(key) => Ok(Some((row, key))),
                None => {
                    // An unbound or changed key is not this binding's spawn: the row goes back.
                    agents::restore_session_job(c, &d.to_agent, d.id, row)?;
                    Ok(None)
                }
            }
        })
    }

    /// The paste failed: the session keeps the job it had.
    fn session_return_job(&self, d: &delegate::Delegation, (row, key): (i64, i64)) {
        if let Err(err) = with(&self.db, |c| {
            self.mcp.rebind_job(&d.to_agent, d.id, key);
            agents::restore_session_job(c, &d.to_agent, d.id, row)
        }) {
            log::error!("session job restore: {err}");
        }
    }

    /// #195: a worker session ended — its merged jobs' worktrees it held can go now. The process
    /// lets go of its folder a moment after the exit, so off this thread and with a few tries.
    fn worktree_retry(&self, agent: &str) {
        if with(&self.db, |c| worktree::pending(c, Some(agent))).map_or(true, |p| p.is_empty()) {
            return;
        }
        let db = self.db.clone();
        let events = self.events.clone();
        let agent = agent.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            for w in with(&db, |c| worktree::pending(c, Some(&agent))).unwrap_or_default() {
                if let Err(err) = worktree::clean(&db, w.delegation_id, 8) {
                    eprintln!("dex: #{} worktree still not removed: {err}", w.delegation_id);
                }
            }
            let _ = events.emit("teams-changed", &());
        });
    }

    /// Pending cleanup is a maintenance warning, never a blocking leader note.
    fn worktree_sweep(&self) {
        let leftovers = match worktree::sweep(&self.db) {
            Ok(l) => l,
            Err(err) => return eprintln!("dex: worktree sweep: {err}"),
        };
        for l in leftovers {
            eprintln!("dex: {}", l.text);
        }
        self.changed("teams-changed");
    }

    /// At startup no worker session survives: open rows are closed so leaders hear about them.
    pub fn delegation_reconcile_on_start(&self) -> Result<usize, String> {
        with(&self.db, |c| delegate::reconcile_on_start(c, false))
    }

    pub fn delegation_dispatch(&self, d: delegate::Delegation) -> Result<delegate::Delegation, String> {
        self.dispatch(d, true)
    }

    /// A line for the job's leader, woken if idle.
    fn note_leader_of(&self, d: &delegate::Delegation, text: &str) {
        if with(&self.db, |c| plan::note(c, &d.from_agent, text)).is_ok() {
            self.leader_notes_wake(&d.from_agent);
        }
    }

    /// `emit: false` — a retry that queues again changes nothing the frontend shows.
    fn dispatch(&self, d: delegate::Delegation, emit: bool) -> Result<delegate::Delegation, String> {
        let d = if d.target_role.is_some() { with(&self.db, |c| delegate::get(c, d.id))? } else { d };
        if d.to_agent.is_empty() || (d.target_role.is_some() && d.status != "queued") || !with(&self.db, |c| job_model::can_start(c, d.id))? {
            self.changed("proposals-changed");
            if emit { delegate::emit_changed(&self.events, Some(&d)); }
            return Ok(d);
        }
        // #2044: the pool counts a reported job as a free worker; one still running to its Stop
        // hands the job to an idle colleague of its role, if any.
        let d = if d.target_role.is_some() && self.agent_states.get(&d.to_agent).in_turn() {
            with(&self.db, |c| delegate::rebalance_role_job(c, d.id, &|a| self.agent_states.get(a).in_turn()))?.unwrap_or(d)
        } else {
            d
        };
        // #161: draining — the job waits in the queue; the restarted core (or the drain's end)
        // delivers it. #427/#447: past the gate, this dispatch holds the restart until it ends.
        let Some(turn) = self.admit(&d.to_agent) else {
            if emit {
                delegate::emit_changed(&self.events, Some(&d));
            }
            return Ok(d);
        };
        #[cfg(test)]
        if let Some(pause) = self.dispatch_pause.lock().unwrap().take() {
            pause();
        }
        // Görev #20: its session is being replaced; the exit starts this job fresh. #45: the mark
        // is the only thing holding the job, so a retry watches it, and it expires — an exit that
        // never arrives must not strand the queue.
        let marked = self.resetting.lock().unwrap().get(&d.to_agent).copied();
        if let Some(since) = marked {
            let stuck = since.elapsed() >= std::time::Duration::from_millis(agents::RESET_STUCK_MS);
            if self.ptys.is_running(&d.to_agent) && !stuck {
                self.delegation_retry(&d.to_agent);
                return Ok(d);
            }
            if stuck {
                eprintln!("dex: {}: session reset hung for {} s; mark dropped, job #{} retried", d.to_agent, since.elapsed().as_secs(), d.id);
            }
            self.resetting.lock().unwrap().remove(&d.to_agent);
        }
        let (mut request, has_work, engine, worker, worker_name) = with(&self.db, |conn| {
            let a = teams::get_agent(conn, &d.to_agent)?;
            let worker = a.kind == "worker";
            Ok((delegate::start_request(conn, &d)?, delegate::has_other_work(conn, &d)?, a.active_engine.unwrap_or(a.engine), worker, a.name))
        })?;
        // Görev #19: claude gets only a short trigger in its pane (argument or paste); the job comes
        // as that prompt's UserPromptSubmit context.
        request.prompt = delegate::trigger_line(d.id, &worker_name);
        let mut status = self.agent_states.get(&d.to_agent).status;
        let running = self.ptys.is_running(&d.to_agent);
        if running && status == agents::Status::Offline {
            status = agents::Status::Idle;
        }
        // #45: no PTY means no session, whatever a stale status says (it would Paste into nothing).
        if !running {
            status = agents::Status::Offline;
        }
        let typing = self.agent_states.typing(&d.to_agent);
        match agents::delivery(status, typing, has_work) {
            agents::Delivery::Queue => {
                if emit {
                    delegate::emit_changed(&self.events, Some(&d));
                }
                if !has_work {
                    // #125: "working" that no hook will ever end (a late hook turned the Stop back) —
                    // the transcript is the witness; idle now, the after-Stop path delivers.
                    if running && engine == "claude" && !typing && self.stale_working(&d.to_agent) {
                        eprintln!("dex: {}: working with no hook for {} s, transcript turn over; settled idle for job #{}", d.to_agent, self.agent_states.silent_for(&d.to_agent) / 1000, d.id);
                        hooks::turn_ended(self, &d.to_agent, "Settled");
                    }
                    self.delegation_retry(&d.to_agent);
                }
                Ok(d)
            }
            agents::Delivery::Start => {
                // Görev #20: a worker's job starts in a fresh session (no --resume).
                let options = if worker {
                    // Görev #144: the job's own worktree. #838 G: none (none planned, or it failed) —
                    // the job's own project root (agents.rs launch_plan), never another project's
                    // or the team folder.
                    let wt = match worktree::prepare(&self.db, &d) {
                        Ok(wt) => wt,
                        Err(worktree::Error::Conflict(msg)) => return delegate::start_failed(&self.db, &self.events, d.id, &msg),
                        Err(worktree::Error::Other(msg)) => {
                            self.note_leader_of(&d, &format!("#{} worktree açılamadı, iş proje kökünde başlıyor: {msg}", d.id));
                            None
                        }
                    };
                    let mut options = agents::job_start(request.prompt, request.add_dirs);
                    options.effort = d.effort;
                    // #838 G/D: the session, its MCP key, folder, memory and system text are this job's.
                    options.delegation_id = Some(d.id);
                    if let Some(wt) = wt {
                        options.env = wt.env();
                        options.cwd = Some(wt.path.to_string_lossy().into_owned());
                    }
                    options
                } else {
                    agents::StartOptions { prompt: Some(request.prompt), add_dirs: request.add_dirs, ..Default::default() }
                };
                let result = self.agent_spawn(&request.agent_id, options);
                match result {
                    Ok(_) => delegate::started(&self.db, &self.events, d.id),
                    Err(error) => delegate::start_failed(&self.db, &self.events, d.id, &error),
                }
            }
            // Görev #20: an idle worker whose session is Dex's (or has no turn) gets a fresh one for
            // this job: after the quiet window its PTY is closed, and the exit starts the job (Start
            // path above). The patron's conversation (last turn theirs) and leaders are never reset.
            agents::Delivery::Paste if worker && self.turns.dex_turn(&d.to_agent) != Some(false) => {
                if emit {
                    delegate::emit_changed(&self.events, Some(&d));
                }
                if !self.agent_states.quiet(&d.to_agent) {
                    self.delegation_retry(&d.to_agent);
                    return Ok(d);
                }
                // One reset at a time; a job the other dispatch already started is left alone.
                if self.resetting.lock().unwrap().insert(d.to_agent.clone(), std::time::Instant::now()).is_some()
                    || with(&self.db, |c| delegate::get(c, d.id)).map_or(true, |now| now.status != "queued")
                {
                    return Ok(d);
                }
                // No session to close means no exit to wait for: drop the mark, try again.
                diagnostics::event("pty_kill", serde_json::json!({"agent": d.to_agent, "reason": "reset"}));
                if !self.ptys.kill(&d.to_agent) {
                    self.resetting.lock().unwrap().remove(&d.to_agent);
                    self.delegation_retry(&d.to_agent);
                }
                Ok(d)
            }
            // #838 D/G: a session of another project never takes this job; it waits (the patron's
            // conversation is kept). No retry/poll: the session's next Stop (after-Stop dispatch)
            // or its exit (fresh Start) runs it again.
            agents::Delivery::Paste if !self.session_fits(&d) => {
                if emit {
                    delegate::emit_changed(&self.events, Some(&d));
                }
                Ok(d)
            }
            agents::Delivery::Paste => {
                // #1779: patron oturumuna yapışan iş de kendi dalına bağlanır — yoksa kayıt
                // "kendi worktree'sinde çalışmadı" sayılır, inceleme/birleştirme açılmaz.
                // Oturum korunur (öldürülmez); canlı PTY'nin cwd'si değişmez, worker
                // UserPromptSubmit bağlamındaki worktree satırıyla hazır ağaca geçer.
                match worktree::prepare(&self.db, &d) {
                    Ok(_) => {}
                    Err(worktree::Error::Conflict(msg)) => return delegate::start_failed(&self.db, &self.events, d.id, &msg),
                    Err(worktree::Error::Other(msg)) => {
                        self.note_leader_of(&d, &format!("#{} worktree açılamadı, iş proje kökünde başlıyor: {msg}", d.id));
                    }
                }
                // #838 G (#906): the bound session works on this job from now on — row and MCP key
                // together, or neither; a failed paste puts both back.
                let rebound = match self.session_take_job(&d) {
                    Ok(rebound) => rebound,
                    Err(error) => return delegate::start_failed(&self.db, &self.events, d.id, &error),
                };
                // #671: the patron's own session keeps its effort (`/effort` would rewrite their
                // settings); the result says so.
                match self.paste_admitted(&request.agent_id, &request.prompt, &turn) {
                    Ok(()) => {
                        if d.effort.is_some() {
                            let _ = with(&self.db, |c| delegate::effort_skipped(c, d.id));
                        }
                        delegate::started(&self.db, &self.events, d.id)
                    }
                    Err(error) => {
                        if let Some(prior) = rebound {
                            self.session_return_job(&d, prior);
                        }
                        delegate::start_failed(&self.db, &self.events, d.id, &error)
                    }
                }
            }
        }
    }

    // ---- teams + agents (every mutation emits `teams-changed`) ----

    pub fn team_list(&self) -> Result<Vec<teams::Team>, String> {
        let list = with(&self.db, |c| teams::list_teams(c, &self.paths.data_dir))?;
        // Service workspaces are created lazily so a Claude session can always start in them.
        for t in list.iter().filter(|t| t.kind == "service") {
            if let Some(w) = &t.workdir {
                std::fs::create_dir_all(w).map_err(|e| e.to_string())?;
            }
        }
        Ok(list)
    }

    pub fn team_create(&self, team: teams::NewTeam) -> Result<teams::Team, String> {
        let folders = teams::prepare_team_folders(&self.db, &team, None)?;
        let t = with(&self.db, |c| teams::create_team_with_folders(c, &self.paths.data_dir, team, folders.as_ref()))?;
        if let Some(w) = t.workdir.as_ref().filter(|_| t.kind == "service") {
            std::fs::create_dir_all(w).map_err(|e| e.to_string())?;
        }
        self.changed("teams-changed");
        Ok(t)
    }

    pub fn team_update(&self, id: &str, patch: teams::TeamPatch) -> Result<teams::Team, String> {
        let t = with(&self.db, |c| teams::update_team(c, &self.paths.data_dir, id, patch))?;
        self.changed("teams-changed");
        Ok(t)
    }

    /// Görev #43: what `team_delete` would stop (running members, open jobs, active plans).
    pub fn team_delete_info(&self, id: &str) -> Result<teams::DeleteInfo, String> {
        with(&self.db, |c| teams::delete_info(c, id, |a| self.ptys.is_running(a)))
    }

    /// A service team's data folder is left on disk (it may hold memory worth keeping).
    /// Görev #43: `stop_agents` closes the running members' sessions first (leader included; their
    /// open jobs close through the usual session-exit path). Deleted only once all have stopped.
    pub fn team_delete(&self, id: &str, stop_agents: bool) -> Result<(), String> {
        let running: Vec<String> = with(&self.db, |c| {
            Ok(teams::list_agents(c)?.into_iter().filter(|a| a.team_id == id && self.ptys.is_running(&a.id)).map(|a| a.id).collect())
        })?;
        if !running.is_empty() {
            if !stop_agents {
                return Err("çalışan ajanı olan takım silinemez".into());
            }
            for d in with(&self.db, |c| teams::freeze_for_delete(c, id))? {
                delegate::emit_changed(&self.events, with(&self.db, |c| delegate::get(c, d)).ok().as_ref());
            }
            for a in &running {
                self.ptys.kill(a);
            }
            let left: Vec<&str> = running.iter().filter(|a| self.ptys.is_running(a)).map(String::as_str).collect();
            if !left.is_empty() {
                self.changed("teams-changed");
                return Err(format!("durmayan ajan var ({}); takım silinmedi", left.join(", ")));
            }
        }
        let deleted = with(&self.db, |c| {
            let agents: Vec<_> = teams::list_agents(c)?.into_iter().filter(|a| a.team_id == id).map(|a| a.id).collect();
            teams::delete_team(c, id)?;
            Ok(agents)
        })?;
        for agent in deleted {
            self.ptys.forget_replay(&agent);
        }
        self.changed("teams-changed");
        self.changed("tasks-changed");
        self.changed("plans-changed");
        Ok(())
    }

    pub fn agent_list(&self) -> Result<Vec<teams::Agent>, String> {
        with(&self.db, teams::list_agents)
    }

    pub fn agent_create(&self, agent: teams::NewAgent) -> Result<teams::Agent, String> {
        let a = with(&self.db, |c| teams::create_agent(c, &self.paths.data_dir, agent))?;
        self.changed("teams-changed");
        Ok(a)
    }

    pub fn agent_update(&self, id: &str, patch: teams::AgentPatch) -> Result<teams::Agent, String> {
        if self.ptys.is_running(id) && (patch.engine.is_some() || patch.provider.is_some() || patch.model.is_some()) {
            return Err("çalışan ajanın motoru, sağlayıcısı veya modeli değiştirilemez".into());
        }
        let a = with(&self.db, |c| teams::update_agent(c, id, patch))?;
        self.changed("teams-changed");
        Ok(a)
    }

    /// DEX-30: one engine/provider/model for all of a team's workers (never its leaders). Running
    /// ones (live PTY — the backend's word, not the UI's) get it as pending. One `teams-changed`.
    pub fn team_set_workers_model(&self, team_id: &str, target: teams::PendingModel) -> Result<teams::WorkersOutcome, String> {
        let out = with(&self.db, |c| teams::set_workers_model(c, team_id, &target, |id| self.ptys.is_running(id)))?;
        self.changed("teams-changed");
        Ok(out)
    }

    /// DEX-30: lands the agent's pending change if its session is closed. `Ok(true)` = changed.
    pub fn agent_apply_pending(&self, id: &str) -> Result<bool, String> {
        if self.ptys.is_running(id) {
            return Ok(false);
        }
        let r = with(&self.db, |c| teams::apply_pending(c, id));
        // A dropped (invalid) pending change is a change too.
        if !matches!(r, Ok(false)) {
            self.changed("teams-changed");
        }
        r
    }

    /// The UI refuses while the agent's PTY runs; tasks assigned to it keep a dangling `agent_id`.
    pub fn agent_delete(&self, id: &str) -> Result<(), String> {
        if self.ptys.is_running(id) {
            return Err("çalışan ajan silinemez".into());
        }
        with(&self.db, |c| teams::delete_agent(c, id))?;
        self.ptys.forget_replay(id);
        self.changed("teams-changed");
        Ok(())
    }

    // ---- providers ----

    /// GLM/Muse kataloğu önbellekten; saati dolmuşsa arka planda tazelenir, değişince
    /// `providers-changed` gider ve seçici yeniden çeker (liste açılışı ağı beklemez).
    pub fn provider_list(&self) -> Result<Vec<providers::ProviderInfo>, String> {
        if with(&self.db, |c| Ok(model_news::catalog_due(c, model_news::now_ms())))? {
            let (db, events) = (self.db.clone(), self.events.clone());
            std::thread::spawn(move || {
                if let Err(e) = model_news::refresh(&db, &events) {
                    eprintln!("dex: model listesi tazelenemedi: {e}");
                }
            });
        }
        let mut list = providers::list()?;
        // #2019: ek Claude hesabı — "jeton" CLI'ın kimlik dosyası; durum ve e-posta karta gider.
        for p in &mut list {
            if let Some(dir) = claude_accounts::config_dir(&self.paths.data_dir, &p.id) {
                let account = claude_accounts::account(&dir);
                p.has_token = account.connected;
                p.account = Some(account);
            }
        }
        Ok(list)
    }

    /// #2019: ek Claude hesabının bağlantısını kaldırır (kimlik + hesap durumu silinir). Süren
    /// oturumlar bellekteki jetonla sürer; zincir artık bu hesaba geçmez.
    pub fn claude_account_remove(&self, provider: &str) -> Result<(), String> {
        let dir = claude_accounts::config_dir(&self.paths.data_dir, provider)
            .ok_or_else(|| format!("bilinmeyen Claude hesabı: {provider}"))?;
        claude_accounts::remove(&dir)?;
        if let Some(q) = self.quota.of(provider) {
            *q.lock().unwrap() = quota::State::default();
        }
        self.usage_forget(provider);
        self.changed("providers-changed");
        Ok(())
    }

    pub fn provider_import_routes(&self) -> Result<Vec<providers::ImportRow>, String> {
        providers::import_routes()
    }

    pub fn provider_set_token(&self, id: &str, token: &str) -> Result<(), String> {
        providers::set_token(id, token)
    }

    // DEX-25: Yerel LLM — ağ çağrısı yapar (en çok ~20 sn); host bunu UI iş parçacığında koşmamalı.
    pub fn provider_local_set_base_url(&self, url: &str) -> Result<(), String> {
        providers::set_base_url(url)
    }

    pub fn provider_local_status(&self) -> Result<providers::LocalStatus, String> {
        providers::local_status()
    }

    pub fn provider_local_add(&self, name: &str) -> Result<providers::LocalStatus, String> {
        providers::local_add(name)
    }

    pub fn provider_local_delete(&self, name: &str) -> Result<providers::LocalStatus, String> {
        providers::local_delete(name)
    }

    // ---- motor zinciri (DEX-17) ----

    /// Zincir sırası + kapalı sağlayıcılar + tükenen sağlayıcılar (Ayarlar paneli).
    pub fn chain_get(&self) -> Result<chain::ChainView, String> {
        with(&self.db, |c| {
            Ok(chain::ChainView { order: chain::order(c), disabled: chain::disabled(c), exhausted: chain::exhausted(c)? })
        })
    }

    pub fn chain_set_order(&self, order: Vec<String>) -> Result<chain::ChainView, String> {
        with(&self.db, |c| {
            chain::set_order(c, &order)?;
            Ok(chain::ChainView { order: chain::order(c), disabled: chain::disabled(c), exhausted: chain::exhausted(c)? })
        })?;
        self.changed("teams-changed");
        self.chain_get()
    }

    /// Görev #94: zincirden sağlayıcı kapat/aç. Kapalı sağlayıcı atlanır; ajanın asıl
    /// sağlayıcısı olarak seçilmesi engellenmez.
    pub fn chain_set_disabled(&self, ids: Vec<String>) -> Result<chain::ChainView, String> {
        with(&self.db, |c| {
            chain::set_disabled(c, &ids)?;
            Ok(chain::ChainView { order: chain::order(c), disabled: chain::disabled(c), exhausted: chain::exhausted(c)? })
        })?;
        self.changed("teams-changed");
        self.chain_get()
    }

    /// Ajanın zincirini aç/kapat (ajan başına). Kapatmak tükenen kaydını silmez.
    pub fn chain_set_agent(&self, id: &str, off: bool) -> Result<teams::Agent, String> {
        let a = with(&self.db, |c| {
            teams::update_agent(c, id, teams::AgentPatch { chain_off: Some(off), ..Default::default() })
        })?;
        self.changed("teams-changed");
        Ok(a)
    }

    /// Görev #120/#1728: vadesi gelen sağlayıcıları sorar (GLM/Muse saatte, Claude günde bir; ağ en
    /// çok ~13 sn — host bunu UI iş parçacığında koşmamalı), sonra son listelerde olup sabit
    /// katalogda olmayan, kapatılmamış güncel modelleri döner.
    pub fn model_news(&self) -> Result<Vec<model_news::NewModel>, String> {
        model_news::refresh(&self.db, &self.events)?;
        with(&self.db, |c| Ok(model_news::news(&model_news::cached(c), &model_news::seen(c))))
    }

    pub fn model_news_dismiss(&self, keys: Vec<String>) -> Result<(), String> {
        with(&self.db, |c| model_news::mark_seen(c, &keys))
    }

    // ---- settings (DEX-31) ----

    /// Agent CPU/memory limits. The total cap applies at once (running agents included); the
    /// per-agent cap and memory ceiling apply to panes started from now on.
    pub fn limits_get(&self) -> Result<settings::LimitsView, String> {
        with(&self.db, |c| Ok(settings::limits_view(c)))
    }

    pub fn limits_set(&self, limits: proc::Limits) -> Result<settings::LimitsView, String> {
        let view = with(&self.db, |c| settings::set_limits(c, limits))?;
        proc::set_total_cap(view.effective.total_cpu_percent);
        Ok(view)
    }

    // ---- notes (DEX-41; every write emits `notes-changed` with the note) ----

    pub fn note_get(&self, scope: &str) -> Result<notes::Note, String> {
        with(&self.db, |c| notes::get(c, scope))
    }

    pub fn note_set(&self, scope: &str, body: &str) -> Result<notes::Note, String> {
        let n = with(&self.db, |c| notes::set(c, scope, body))?;
        let _ = self.events.emit("notes-changed", &n);
        Ok(n)
    }

    // ---- leader prompts (Plan #114; `leader-prompt-changed` carries a team's view) ----

    pub fn leader_prompt_get(&self, team: &str) -> Result<prompts::LeaderPrompt, String> {
        with(&self.db, |c| prompts::get(c, team))
    }

    /// The patron's appends; `None` keeps a field.
    pub fn leader_prompt_set(&self, team: &str, system: Option<&str>, message: Option<&str>) -> Result<prompts::LeaderPrompt, String> {
        let p = with(&self.db, |c| prompts::set(c, team, system, message))?;
        let _ = self.events.emit("leader-prompt-changed", &p);
        Ok(p)
    }

    pub fn prompt_parts(&self) -> Result<prompts::Parts, String> {
        with(&self.db, prompts::parts)
    }

    pub fn prompt_part_set(&self, id: &str, text: &str) -> Result<prompts::PromptPart, String> {
        let p = with(&self.db, |c| prompts::part_set(c, id, text))?;
        self.prompt_part_changed(&p);
        Ok(p)
    }

    pub fn prompt_part_reset(&self, id: &str) -> Result<prompts::PromptPart, String> {
        let p = with(&self.db, |c| prompts::part_reset(c, id))?;
        self.prompt_part_changed(&p);
        Ok(p)
    }

    /// `prompt-parts-changed`, then every leader's view: an override may reach it.
    fn prompt_part_changed(&self, p: &prompts::PromptPart) {
        let _ = self.events.emit("prompt-parts-changed", p);
        let views = with(&self.db, |c| prompts::leader_teams(c)?.iter().map(|t| prompts::get(c, t)).collect::<Result<Vec<_>, _>>());
        for v in views.unwrap_or_default() {
            let _ = self.events.emit("leader-prompt-changed", &v);
        }
    }

    /// A leader session started with the system prompt built from `print`.
    pub(crate) fn leader_prompt_applied(&self, agent: &str, print: &str) {
        match with(&self.db, |c| prompts::get(c, &prompts::record_applied(c, agent, print)?)) {
            Ok(v) => {
                let _ = self.events.emit("leader-prompt-changed", &v);
            }
            Err(err) => log::warn!("leader prompt applied: {err}"),
        }
    }

    // ---- PTYs ----

    pub fn pty_spawn(&self, req: pty::Spawn, sink: impl PtySink) -> Result<(), String> {
        pty::spawn(self, req, sink)
    }

    pub(crate) fn pty_exited(&self, id: &str) {
        // #2019: a Claude account's sign-in ended — its card and ring show the new state now.
        if let Some(provider) = claude_accounts::login_provider(id) {
            self.changed("providers-changed");
            let weak = self.weak_self.clone();
            std::thread::spawn(move || {
                if let Some(core) = weak.upgrade() {
                    core.quota_tick_for(provider);
                }
            });
            return;
        }
        self.reported_workers.lock().unwrap().remove(id);
        if let Err(error) = with(&self.db, |c| cost::end_agent(c,id)) {
            log::error!("usage session end: {error}");
        }
        self.permits.forget(id);
        // İş #1787 (auv deseni): ölen turun bekleyen ask_leader/wait_answer'ı hemen kapansın;
        // soru `cancelled` olur, lidere iptal notu gider, iş raporsuz kapanabilir.
        if let Err(err) = self.answers.cancel_agent(&self.db, &self.events, id) {
            eprintln!("dex: {id}: bekleyen soru iptali: {err}");
        }
        self.resetting.lock().unwrap().remove(id);
        if with(&self.db, |conn| teams::get_agent(conn, id)).is_err() {
            return;
        }
        let state = self.agent_states.exited(id);
        let _ = self.events.emit("agent-status", &state);
        self.first_reply_exited(id);
        // FIX-4: a resume the CLI could not find restarts fresh with the same start (prompt kept).
        if self.resume_on_exit(id) {
            return;
        }
        if let Err(err) = self.delegation_session_exited(id) {
            // #45: the queued job's start failed on the way; nothing else would try it again.
            eprintln!("dex: {id}: session exit: {err}");
            self.delegation_retry(id);
        }
    }
}

impl Core {
    /// Görev #23: allow / deny a worker's PermissionRequest.
    /// `None` (the patron's prompt, as before) for anything Dex cannot place and for a leader's
    /// call that is not a hard no (#164 P7).
    fn permit_on_request(&self, agent: &str, body: &Value) -> Option<String> {
        let answer = self.permit_answer(agent, body, true)?;
        let decision = match answer {
            Ok(()) => serde_json::json!({ "behavior": "allow" }),
            Err(message) => serde_json::json!({ "behavior": "deny", "message": message }),
        };
        Some(serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string())
    }

    /// The policy's answer for a worker's tool call (`Ok` allow, `Err` deny with the worker's
    /// message), audited once. `gray`: whether an out-of-policy call is answered (a numbered
    /// request for the leader) or left alone (`None`, so the session decides).
    /// `None` for anything Dex cannot place. #164 P7: a leader gets the hard noes only — its allowed
    /// and out-of-policy calls are `None` (the session decides, as before).
    /// #164 P3: a worker spawned fail-closed whose call Dex cannot place (DB error, row gone) is
    /// denied rather than left to bypass mode.
    fn permit_answer(&self, agent: &str, body: &Value, gray: bool) -> Option<Result<(), String>> {
        let tool = body.get("tool_name").and_then(Value::as_str)?;
        let input = body.get("tool_input").cloned().unwrap_or(Value::Null);
        let placed = with(&self.db, |c| Ok((self.permit_ctx(c, agent, body)?, running_job(c, agent)?)));
        let (ctx, worker, running) = match placed {
            Ok((Some((ctx, worker)), running)) => (ctx, worker, running),
            _ if self.hooks.is_fail_closed(agent) => {
                diagnostics::event("permit", diagnostics::permit_fields(agent, "deny", "policy", "policy_unavailable", tool, &input, None, None));
                self.permit_audit(agent, "deny", "policy_unavailable", None, input.to_string().len());
                return Some(Err("Dex izin denetimi şu an karar veremiyor; araç engellendi. Biraz sonra aynı adımı yeniden dene; sürerse raporunda belirt.".into()));
            }
            _ => return None,
        };
        let answer = match permit::decide(tool, &input, &ctx) {
            permit::Verdict::Deny(why) if !worker => {
                diagnostics::event("permit", diagnostics::permit_fields(agent, "deny", "deny", diagnostics::denial_rule(&why), tool, &input, Some(&ctx), None));
                self.permit_audit(agent, "deny", "deny", None, input.to_string().len());
                Err(format!("Dex politikası izin vermiyor: {why}. Bu patron işi: bu adımı atla; gerekiyorsa patrona bildir."))
            }
            _ if !worker => {
                diagnostics::event("permit", diagnostics::permit_fields(agent, "allow", "leader_bypass", "leader_bypass", tool, &input, Some(&ctx), None));
                return None;
            }
            permit::Verdict::Allow => Ok(()),
            permit::Verdict::Deny(why) => {
                diagnostics::event("permit", diagnostics::permit_fields(agent, "deny", "deny", diagnostics::denial_rule(&why), tool, &input, Some(&ctx), None));
                self.permit_audit(agent, "deny", "deny", None, input.to_string().len());
                Err(format!(
                    "Dex politikası izin vermiyor: {why}. Bu patron işi: ask_leader'a gerek yok, lider de açamaz. \
                     Bu adımı atla; gerekiyorsa raporunda belirt."
                ))
            }
            permit::Verdict::Leader(_) if !gray => {
                diagnostics::event("permit", diagnostics::permit_fields(agent, "leader", "policy", "deferred", tool, &input, Some(&ctx), None));
                return None;
            }
            permit::Verdict::Leader(what) => {
                let what = format!("{tool}: {}", if what.is_empty() { tool } else { &what });
                if let Some(id) = self.permits.take(agent, &what, running) {
                    diagnostics::event("permit", diagnostics::permit_fields(agent, "allow", "grant", "permit_grant", tool, &input, Some(&ctx), Some(id)));
                    self.permit_audit(agent, "allow", "grant", Some(id), input.to_string().len());
                    Ok(())
                } else {
                    let id = self.permits.ask(agent, &what);
                    diagnostics::event("permit", diagnostics::permit_fields(agent, "leader", "policy", "permit_required", tool, &input, Some(&ctx), Some(id)));
                    self.permit_audit(agent, "leader", "policy", Some(id), input.to_string().len());
                    Err(format!(
                        "Dex politikası dışı (izin #{id}): `{what}`. ask_leader ile liderinden iste, soruda izin #{id}'i an; \
                         onaylarsa aynı isteği aynen yeniden dene. Patrona sorma."
                    ))
                }
            }
        };
        diagnostics::event("permit", diagnostics::permit_fields(agent, if answer.is_ok() { "allow" } else { "deny" }, "policy",
            answer.as_ref().err().map_or("allow_policy", |why| diagnostics::denial_rule(why)), tool, &input, Some(&ctx), None));
        Some(answer)
    }

    /// Persist metadata only; commands stay in the transient request and approval question.
    /// Only leader permission
    /// requests/grants enter the notification queue; hard denials need no leader decision.
    fn permit_audit(&self, agent: &str, decision: &str, rule: &str, id: Option<i64>, bytes: usize) {
        let prefix = match id {
            Some(id) if decision == "allow" => format!("izin #{id} tüketildi"),
            Some(id) => format!("izin #{id} istenebilir (politika dışı)"),
            None => "ret".into(),
        };
        let text = format!("{prefix}: decision={decision} rule={rule} bytes={bytes}");
        eprintln!("dex permit {agent}: {text}");
        if id.is_none() {
            return;
        }
        let _ = with(&self.db, |c| {
            let (name, team): (String, String) = c
                .query_row("SELECT name, team_id FROM agents WHERE id = ?1", [agent], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            let leaders: Vec<String> = c
                .prepare("SELECT id FROM agents WHERE team_id = ?1 AND kind = 'leader'")
                .map_err(|e| e.to_string())?
                .query_map([team], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            for l in leaders {
                plan::note(c, &l, &format!("Dex izin: {name} — {text}"))?;
            }
            Ok(())
        });
    }

    /// Workers and leaders (the bool: worker): their team folder (a service team's workdir)
    /// and team memory to write, a service job's `docs/research` too, its requester project to
    /// read; other teams' folders are off limits. A leader with no team folder is not placed.
    fn permit_ctx(&self, c: &rusqlite::Connection, agent: &str, body: &Value) -> Result<Option<(permit::Ctx, bool)>, String> {
        let a = teams::get_agent(c, agent)?;
        let worker = a.kind == "worker";
        if !worker && a.kind != "leader" {
            return Ok(None);
        }
        let all = teams::list_teams(c, &self.paths.data_dir)?;
        let Some(team) = all.iter().find(|t| t.id == a.team_id) else { return Ok(None) };
        let home = team.cwd.clone().or_else(|| team.workdir.clone()).map(std::path::PathBuf::from);
        if !worker && home.is_none() {
            return Ok(None);
        }
        let mut ctx = permit::Ctx { leader: a.kind == "leader", team_root: home.clone(), qa: a.role_slug.as_deref() == Some("qa"), read_only: worker && a.role_slug.as_deref() == Some(review::ROLE), ..Default::default() };
        if a.role_slug.as_deref() == Some("assistant") {
            ctx.assistant = true;
            ctx.read_only = true;
            ctx.leader = false;
            ctx.read.extend(home.map(|p| p.join(".dex").join("inbox")));
            let pc = hooks::session_context(c, &self.paths.data_dir, agent, true).map_err(|e| e.to_string())?;
            ctx.read.push(pc.team_memory_root.join("shared"));
            ctx.read.push(slug::memory_dir(c, &pc.agent_memory_root, agent)?);
            ctx.cwd = pc.project_root.or_else(|| ctx.team_root.clone());
            return Ok(Some((ctx, true)));
        }
        // #222: izinli push hedef desenleri — settings satırı (ajan yazamaz), boş = kapalı.
        ctx.push_allow = settings::push_allow(c);
        // The debug build's source root is trusted build metadata, never hook input.
        // Release binaries grant no extra push root.
        #[cfg(debug_assertions)]
        if let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(std::path::Path::parent) {
            ctx.extra_push_roots.push(root.to_path_buf());
        }
        // #838 G: a worker session bound to a job (agents.rs records it before the CLI starts; a
        // same-project paste moves it) is placed by that binding's context alone — its project P,
        // worktree W and report; a queued job of another project changes nothing, and a binding
        // whose project is gone places nothing (fail-closed). Unbound (pre-project) sessions: as before.
        let (bound_project, bound_job) = session_binding(c, agent)?;
        let bound = worker && (bound_project.is_some() || bound_job.is_some());
        let (wt, place) = if bound {
            let pc = hooks::session_context(c, &self.paths.data_dir, agent, true).map_err(|e| e.to_string())?;
            let memory = hooks::memory_scope(c, &pc)?;
            let service = team.kind == "service";
            let p = pc.project_root.clone();
            let wt = pc.delegation_id.map(|id| worktree::get(c, id)).transpose()?.flatten().filter(|w| w.removed_at.is_none() && w.agent_id == agent);
            match (&wt, &p) {
                // Its worktree, its report folder and the project's scratch; P itself read-only.
                (Some(w), _) => {
                    let base = p.clone().unwrap_or_else(|| w.root.clone());
                    ctx.write.extend([w.path.clone(), base.join(".dex").join("results"), base.join(".dex").join("tmp")]);
                }
                // A service job: its own workspace and its one research file; the requester's P read-only.
                _ if service => ctx.write.extend(home.clone().into_iter().chain(pc.result_path.clone())),
                // No worktree (none planned, not git, worktrees off): P itself, never another root.
                (None, Some(p)) => ctx.write.push(p.clone()),
                (None, None) => {}
            }
            // Memory: team shared + own private + P shared to write; the anchors' roots stay readable.
            ctx.write.extend(memory.write);
            ctx.read = ctx.write.clone();
            ctx.read.extend(memory.read);
            ctx.read.extend(p.clone());
            // A prerequisite of the plan: only its report file, read-only (B: never its folder).
            if let Some(job) = pc.delegation_id {
                ctx.read.extend(plan::dependency_access(c, job)?.into_iter().filter_map(|d| d.report_file));
            }
            // The reviewer's read root and report folder are its job's P (the reviewed job's project).
            if !service {
                ctx.team_root = p.clone().or(home.clone());
            }
            if ctx.read_only {
                ctx.job = pc.delegation_id;
            }
            let place = if service { home.clone() } else { p.or(home.clone()) };
            (wt, place)
        } else {
            // Görev #144: a job in its own worktree writes there (+ its report and the team scratch
            // `.dex/tmp`, #200), the team folder is read-only.
            let wt = if worker { worktree::active(c, agent)? } else { None };
            match &wt {
                Some(w) => ctx.write.extend([w.path.clone(), w.root.join(".dex").join("results"), w.root.join(".dex").join("tmp")]),
                None => ctx.write.extend(home.clone()),
            }
            // A service team's memory sits beside its workdir, not in it. #838 G: plus the folders
            // the spawn made from the session's context anchors (the same ones on an old team).
            ctx.write.extend(team.memory_dir.clone().map(std::path::PathBuf::from));
            if let Ok(pc) = hooks::session_context(c, &self.paths.data_dir, agent, false) {
                ctx.write.extend(hooks::memory_scope(c, &pc)?.write);
            }
            let requesters: Vec<String> = c
                .prepare("SELECT DISTINCT team_id FROM delegations WHERE to_agent = ?1 AND service_team_id IS NOT NULL")
                .map_err(|e| e.to_string())?
                .query_map([agent], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            ctx.read = ctx.write.clone();
            ctx.read.extend(wt.as_ref().and(home.clone()));
            for t in requesters {
                if let Ok(research) = delegate::research_dir(c, &t) {
                    ctx.read.extend(research.parent().and_then(std::path::Path::parent).map(std::path::Path::to_path_buf));
                    ctx.write.push(research.clone());
                    ctx.read.push(research);
                }
            }
            if ctx.read_only {
                ctx.job = running_job(c, agent)?;
            }
            (wt, home.clone())
        };
        ctx.others = all.iter().filter(|t| t.id != team.id).filter_map(|t| t.cwd.clone()).map(Into::into).collect();
        // #293: the reviewer reads Dex's role skills and writes only its own report (permit.rs).
        if ctx.read_only {
            ctx.read.push(self.skills.root.clone());
            use rusqlite::OptionalExtension;
            let state = self.agent_states.get(agent);
            let saved: Option<(String, Option<String>)> = c.query_row(
                "SELECT session_id, transcript FROM agent_sessions WHERE agent_id = ?1 AND engine = ?2",
                rusqlite::params![agent, a.active_engine.as_deref().unwrap_or(&a.engine)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().map_err(|e| e.to_string())?;
            let session = state.session_id.as_deref().or_else(|| saved.as_ref().map(|s| s.0.as_str()));
            let transcript = state.transcript.as_deref().or_else(|| saved.as_ref().and_then(|s| s.1.as_deref()));
            ctx.tool_results = session.zip(transcript).and_then(|(s, t)| permit::claude_tool_results(t, s));
        }
        // #35: Claude Code's own scratch space of this session (common hook field), nothing wider.
        if let Some(sp) = body.get("scratchpad_dir").and_then(Value::as_str).and_then(permit::scratchpad) {
            ctx.write.push(sp.clone());
            ctx.read.push(sp);
        }
        ctx.cwd = body.get("cwd").and_then(Value::as_str).map(Into::into).or(wt.map(|w| w.path)).or(place);
        Ok(Some((ctx, worker)))
    }
}

/// #838 G: the agent session's launch binding (project, job) as agents.rs recorded it — the CLI
/// may not have named its session yet (`session_id` ''). Both none: an unbound session.
fn session_binding(c: &rusqlite::Connection, agent: &str) -> Result<(Option<String>, Option<i64>), String> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT project_id, delegation_id FROM agent_sessions WHERE agent_id = ?1", [agent], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map(Option::unwrap_or_default)
        .map_err(|e| e.to_string())
}

/// #838 G: the job the agent's session is bound to; none for an unbound session.
fn launch_job(c: &rusqlite::Connection, agent: &str) -> Result<Option<i64>, String> {
    Ok(session_binding(c, agent)?.1)
}

fn pre_tool_answer(decision: &str, reason: Option<&str>) -> String {
    let mut out = serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": decision } });
    if let Some(r) = reason {
        out["hookSpecificOutput"]["permissionDecisionReason"] = r.into();
    }
    out.to_string()
}

fn running_job(c: &rusqlite::Connection, agent: &str) -> Result<Option<i64>, String> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT id FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id LIMIT 1", [agent], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}

/// S5-B: a Stop answer with `decision: block` continues the turn — not a real turn end.
fn is_blocked(out: &Option<String>) -> bool {
    out.as_deref()
        .and_then(|o| serde_json::from_str::<Value>(o).ok())
        .is_some_and(|v| v["decision"] == "block")
}

#[cfg(test)]
mod security_tests {
    #[cfg(unix)]
    #[test]
    fn core_repairs_existing_data_and_config_directory_modes() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("private-core-{}-{}", std::process::id(), crate::agents::next_seq()));
        let paths = Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        for dir in [&paths.data_dir, &paths.config_dir] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let core = Core::start(paths.clone(), Events::new(events::NullSink)).unwrap();
        for dir in [&paths.data_dir, &paths.config_dir] {
            assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        core.shutdown();
    }
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    fn hook(port: u16, token: &str, agent: &str, key: &str) -> String {
        let body = r#"{"hook_event_name":"Stop"}"#;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {key}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out.lines().next().unwrap_or("").to_string()
    }

    #[test]
    fn running_agent_cannot_be_deleted_or_reconfigured_with_its_team() {
        let root = std::env::temp_dir().join(format!("dex-sec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink),
        )
        .unwrap();
        let team = core
            .team_create(teams::NewTeam { profile: None,
                name: "Security test".into(), kind: "service".into(), cwd: None, worker_roles: None,
                members: None,
            })
            .unwrap();
        let agent = core.agent_list().unwrap().into_iter().find(|a| a.team_id == team.id).unwrap();
        let key = core.mcp.key_for(&agent.id);
        assert!(hook(core.hooks.port, &core.hooks.token, &agent.id, "wrong").contains("401 Unauthorized"));
        assert!(hook(core.hooks.port, &core.hooks.token, &agent.id, &key).contains("204 No Content"));
        core.pty_spawn(
            pty::Spawn {
                id: agent.id.clone(), cmd: shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default()
            },
            agents::shell_sink_for_test(&core, &agent.id, Sink),
        )
        .unwrap();
        assert!(core.ptys.is_running(&agent.id));
        assert!(core.agent_delete(&agent.id).unwrap_err().contains("çalışan ajan"));
        assert!(core.team_delete(&team.id, false).unwrap_err().contains("çalışan ajan"));
        assert!(core
            .agent_update(&agent.id, teams::AgentPatch { model: Some("haiku".into()), ..Default::default() })
            .unwrap_err()
            .contains("modeli değiştirilemez"));
        core.ptys.kill(&agent.id);
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Görev #43: `stop_agents` closes the sessions, the open jobs close, then the team goes.
    #[test]
    fn team_with_running_agent_is_deleted_after_stopping_it() {
        let root = std::env::temp_dir().join(format!("dex-sec-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink),
        )
        .unwrap();
        let team = core
            .team_create(teams::NewTeam { profile: None, name: "Stop test".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None })
            .unwrap();
        let agent = core.agent_list().unwrap().into_iter().find(|a| a.team_id == team.id).unwrap();
        // Jobs from the default team's leader (rows outlive the deleted team) + a plan of the team.
        let (running, queued) = with(&core.db, |c| {
            let job = |status: &str| -> Result<i64, String> {
                c.execute(
                    "INSERT INTO delegations (from_agent, to_agent, team_id, prompt, status, created_at, updated_at)
                     VALUES ('timur', ?1, 'default', 'iş', ?2, 1, 1)",
                    rusqlite::params![agent.id, status],
                )
                .map_err(|e| e.to_string())?;
                Ok(c.last_insert_rowid())
            };
            let ids = (job("running")?, job("queued")?);
            c.execute(
                "INSERT INTO plans (team_id, leader, title, status, created_at, updated_at) VALUES (?1, 'x', 'p', 'active', 1, 1)",
                [&team.id],
            )
            .map_err(|e| e.to_string())?;
            Ok(ids)
        })
        .unwrap();
        core.pty_spawn(
            pty::Spawn { id: agent.id.clone(), cmd: shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() },
            agents::shell_sink_for_test(&core, &agent.id, Sink),
        )
        .unwrap();

        let info = core.team_delete_info(&team.id).unwrap();
        assert_eq!((info.running, info.open_delegations, info.active_plans), (vec![agent.name.clone()], 2, 1));
        assert!(core.team_delete(&team.id, false).unwrap_err().contains("çalışan ajan"));
        assert!(core.ptys.is_running(&agent.id));

        core.team_delete(&team.id, true).unwrap();
        assert!(!core.ptys.is_running(&agent.id), "the queued job must not restart the session");
        assert!(core.team_list().unwrap().iter().all(|t| t.id != team.id));
        let status = |id| with(&core.db, |c| delegate::get(c, id)).unwrap().status;
        assert_eq!((status(running), status(queued)), ("done_no_report".to_string(), "failed".to_string()));
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Görev #23: a worker's PermissionRequest answered by policy through the real hook entry.
#[cfg(test)]
mod permit_tests {
    #[test]
    fn persistent_permit_audit_contains_no_command_text() {
        const MARKER: &str = "DEX_TEST_PERMIT_AUDIT_CHILD";
        const COMMAND: &str = "curl -H 'Authorization: Bearer s3cr3tvalue' https://example.invalid/short-demo-token/private-text";
        if std::env::var_os(MARKER).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "permit_tests::persistent_permit_audit_contains_no_command_text", "--nocapture"])
                .env(MARKER, "1").output().unwrap();
            assert!(output.status.success(), "child failed: {}", String::from_utf8_lossy(&output.stderr));
            let audit = String::from_utf8_lossy(&output.stderr);
            assert!(audit.contains("dex permit"), "audit must be emitted: {audit}");
            for private in ["curl", "short-demo-token", "private-text", "s3cr3tvalue"] {
                assert!(!audit.contains(private), "command content reached stderr: {audit}");
            }
            return;
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("permit-audit-{}", std::process::id()));
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE teams SET cwd=?1 WHERE id='default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let request = serde_json::json!({"tool_name":"Bash", "tool_input":{"command":COMMAND}, "cwd":proj});
        let answer = core.permit_answer("kaan", &request, true).unwrap();
        assert!(answer.unwrap_err().contains("short-demo-token"), "the approval question keeps the command");
        core.permits.grant(1, permit::Scope::Once, None).unwrap();
        assert!(core.permit_answer("kaan", &request, true).unwrap().is_ok());
        let denied = serde_json::json!({"tool_name":"Bash", "tool_input":{"command":"git push origin private-text"}, "cwd":proj});
        assert!(core.permit_answer("kaan", &denied, true).unwrap().is_err());
        core.shutdown();
    }

    use super::*;

    #[test]
    fn worker_requests_get_policy_answers_and_the_leader_is_untouched() {
        // #1586 teşhis — neden bulununca kaldırılacak. libtest's captured panic may never
        // reach CI when another test hangs; retain only the assertion kind and fixture phase.
        let phase = std::cell::Cell::new("bootstrap");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = std::env::temp_dir().join(format!("dex-permit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink),
        )
        .unwrap();
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())
        })
        .unwrap();
        let ask = |agent: &str, tool: &str, input: Value| -> Option<Value> {
            let body = serde_json::json!({ "hook_event_name": "PermissionRequest", "tool_name": tool, "tool_input": input, "cwd": proj.to_string_lossy() });
            core.delegation_on_hook(agent, "PermissionRequest", &body, false).map(|o| serde_json::from_str(&o).unwrap())
        };
        let decision = |v: Option<Value>| v.unwrap()["hookSpecificOutput"]["decision"].clone();
        phase.set("safe tool decision");
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "cargo test -p x permit" })))["behavior"], "allow");
        phase.set("hard denial decision");
        let d = decision(ask("kaan", "Bash", serde_json::json!({ "command": "cargo test && git push" })));
        assert_eq!(d["behavior"], "deny");
        assert!(d["message"].as_str().unwrap().contains("git push"));
        let edit = |p: &std::path::Path| decision(ask("kaan", "Edit", serde_json::json!({ "file_path": p.to_string_lossy() })));
        phase.set("own file decision");
        assert_eq!(edit(&proj.join("src/a.rs"))["behavior"], "allow");
        phase.set("outside file decision");
        assert_eq!(edit(&root.join("elsewhere.rs"))["behavior"], "deny");
        // Expected policy denials stay local, including a reviewer's read-only boundary.
        phase.set("reviewer decision");
        with(&core.db, |c| {
            c.execute("UPDATE agents SET role_slug = ?1 WHERE id = 'ayla'", [review::ROLE]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        for event in ["PermissionRequest", "PreToolUse"] {
            let body = serde_json::json!({ "tool_name": "Edit", "tool_input": { "file_path": proj.join("src/a.rs") } });
            let out: Value = serde_json::from_str(&core.delegation_on_hook("ayla", event, &body, false).unwrap()).unwrap();
            assert!(out.to_string().contains("salt okuma"), "{out}");
        }
        with(&core.db, |c| {
            let notes: i64 = c.query_row("SELECT COUNT(*) FROM leader_notes WHERE agent_id = 'timur'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            phase.set("hard denial note count");
            assert_eq!(notes, 0, "hard denials must not queue a leader wakeup");
            c.execute("UPDATE agents SET role_slug = NULL WHERE id = 'ayla'", []).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        for event in ["Stop", "UserPromptSubmit"] {
            phase.set("hard denial context");
            assert!(plan::with_notes(&core.db, "timur", event, None).is_none(), "expected denials must not reopen Stop or add prompt context");
        }
        // Outside the policy: a numbered request for the leader; granted once, it passes once.
        phase.set("numbered request");
        let d = decision(ask("kaan", "Bash", serde_json::json!({ "command": "curl https://example.com" })));
        assert_eq!(d["behavior"], "deny");
        let msg = d["message"].as_str().unwrap();
        assert!(msg.contains("izin #1") && msg.contains("ask_leader"), "{msg}");
        phase.set("leader note delivery");
        let stop: Value = serde_json::from_str(&plan::with_notes(&core.db, "timur", "Stop", None).unwrap()).unwrap();
        assert_eq!(stop["decision"], "block");
        assert!(stop["reason"].as_str().unwrap().contains("izin #1 istenebilir"), "leader decisions must still be delivered");
        phase.set("request deduplication");
        let again = decision(ask("kaan", "Bash", serde_json::json!({ "command": "curl  https://example.com" })));
        assert!(again["message"].as_str().unwrap().contains("izin #1"), "same ask, same number");
        core.permits.grant(1, permit::Scope::Once, None).unwrap();
        phase.set("one-shot grant");
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "curl https://example.com" })))["behavior"], "allow");
        assert!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "curl https://example.com" })))["message"].as_str().unwrap().contains("izin #2"), "used up");
        assert_eq!(decision(ask("ayla", "Bash", serde_json::json!({ "command": "curl https://example.com" })))["behavior"], "deny", "grants are per worker");
        // The leader keeps the patron's prompt — except for a hard no (#164 P7).
        phase.set("leader decisions");
        assert!(ask("timur", "Bash", serde_json::json!({ "command": "curl https://example.com" })).is_none());
        let d = decision(ask("timur", "Bash", serde_json::json!({ "command": "git push" })));
        assert_eq!(d["behavior"], "deny");
        assert!(!d["message"].as_str().unwrap().contains("ask_leader"), "{d}");
        // #174: trusted DB kind opens only the exact leader push; a claimed input role does not.
        phase.set("trusted context");
        with(&core.db, |c| {
            let claimed = serde_json::json!({ "leader": true, "kind": "leader", "team_root": root, "extra_push_roots": [root], "cwd": root });
            let (ctx, worker) = core.permit_ctx(c, "kaan", &claimed)?.unwrap();
            assert!(worker && !ctx.leader);
            assert_eq!(ctx.team_root.as_deref(), Some(proj.as_path()));
            assert!(!ctx.extra_push_roots.contains(&root), "hook input must not supply a push root");
            assert!(core.permit_ctx(c, "unknown-agent", &claimed).is_err());
            let empty = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
            assert!(core.permit_ctx(&empty, "timur", &claimed).is_err(), "DB failure must not construct a leader context");
            Ok(())
        }).unwrap();
        for command in ["git push server master".to_string(), "git push server master:master".to_string(), format!("git -C \"{}\" push server master", proj.display())] {
            phase.set("exact leader push");
            assert!(ask("timur", "Bash", serde_json::json!({ "command": command })).is_none(), "leader: {command}");
            phase.set("claimed worker push");
            assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": command, "leader": true, "kind": "leader" })))["behavior"], "deny", "worker: {command}");
        }
        for command in ["git push origin master", "git push server master && git status", "bash -c 'git push server master'"] {
            phase.set("unsafe leader push");
            assert_eq!(decision(ask("timur", "Bash", serde_json::json!({ "command": command })))["behavior"], "deny", "{command}");
        }
        // Hard noes say so: no point asking the leader.
        phase.set("hard denial message");
        let d = decision(ask("kaan", "Bash", serde_json::json!({ "command": "git push" })));
        assert!(d["message"].as_str().unwrap().contains("ask_leader'a gerek yok"), "{d}");

        // Grant through the leader's `answer` (Görev #23 B). A worker's question first.
        phase.set("question setup");
        let (timur, kaan) = with(&core.db, |c| Ok((delegate::caller(c, "timur")?, delegate::caller(c, "kaan")?))).unwrap();
        let job = with(&core.db, |c| {
            let d = delegate::insert(c, delegate::NewDelegation { from: &timur, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })?;
            delegate::set_status(c, d.id, "running")?;
            Ok(d.id)
        })
        .unwrap();
        let call = |who: &delegate::Caller, name: &str, args: Value| {
            questions::mcp_call(&core.db, &core.events, &core.permits, &core.answers, who, name, &args).unwrap()
        };
        let d = decision(ask("kaan", "Bash", serde_json::json!({ "command": "make all" })));
        let p = d["message"].as_str().unwrap().split("izin #").nth(1).unwrap().split(')').next().unwrap().parse::<i64>().unwrap();
        let q = with(&core.db, |c| questions::ask(c, &kaan, &serde_json::json!({ "question": format!("izin #{p}: make all?") }))).unwrap().id;
        // A worker cannot answer, nor grant.
        phase.set("worker cannot grant");
        assert!(call(&kaan, "answer", serde_json::json!({ "questionId": q, "text": "ok", "permit": p })).is_err());
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "make all" })))["behavior"], "deny", "still not granted");
        // Another worker's request cannot ride on this question.
        phase.set("grant ownership");
        let other = decision(ask("ayla", "Bash", serde_json::json!({ "command": "make other" })));
        let po = other["message"].as_str().unwrap().split("izin #").nth(1).unwrap().split(')').next().unwrap().parse::<i64>().unwrap();
        assert!(call(&timur, "answer", serde_json::json!({ "questionId": q, "text": "ok", "permit": po })).unwrap_err().contains("ait değil"));
        assert!(call(&timur, "answer", serde_json::json!({ "questionId": q, "text": "ok", "permit": p, "scope": "forever" })).is_err());
        phase.set("session grant");
        let ok = call(&timur, "answer", serde_json::json!({ "questionId": q, "text": "tamam", "permit": p, "scope": "session" })).unwrap();
        assert!(ok["note"].as_str().unwrap().contains(&format!("İzin #{p} verildi")), "{ok}");
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "make all" })))["behavior"], "allow");
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "make all" })))["behavior"], "allow", "session");
        // The job ends → the session grant goes with it.
        phase.set("job-end grant expiry");
        with(&core.db, |c| delegate::set_status(c, job, "done")).unwrap();
        assert_eq!(decision(ask("kaan", "Bash", serde_json::json!({ "command": "make all" })))["behavior"], "deny");
        // Audit: the leader's next context contains metadata only.
        phase.set("audit metadata");
        let _ = decision(ask("kaan", "Bash", serde_json::json!({ "command": "curl -H 'Authorization: Bearer s3cr3tvalue' x" })));
        let notes: Vec<String> = with(&core.db, |c| {
            c.prepare("SELECT text FROM leader_notes WHERE agent_id = 'timur'").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())
        })
        .unwrap();
        let all = notes.join("\n");
        assert!(all.contains("tüketildi") && all.contains("istenebilir"), "{all}");
        assert!(!all.contains("ret:"), "hard denials belong in the log, not leader context: {all}");
        assert!(!all.contains("s3cr3tvalue"), "{all}");
        assert!(!all.contains("curl") && !all.contains("make all"), "{all}");
        phase.set("shutdown");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
        }));
        if let Err(panic) = result {
            let message = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("non-string panic");
            // Messages/values can contain fixture commands or paths; never echo those to public CI.
            let kind = if message.starts_with("assertion `") {
                message.split(" failed").next().unwrap_or("assertion")
            } else if message.starts_with("called `Option::unwrap()`") {
                "called `Option::unwrap()` on a `None` value"
            } else if message.starts_with("called `Result::unwrap()`") {
                "called `Result::unwrap()` on an `Err` value"
            } else { "assertion or panic" };
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "permit policy test failed: phase={}, {kind}", phase.get());
            std::panic::resume_unwind(panic);
        }
    }

    /// Görev #144: a job in its own worktree writes there and to its report; the team folder is
    /// read-only to it. Without a worktree job the team folder stays writable.
    #[test]
    fn worktree_job_writes_only_in_its_tree() {
        let proj = worktree::tests::repo("permit");
        let data = std::env::temp_dir().join(format!("dex-wt-permit-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let core = Core::start(Paths { data_dir: data.join("data"), config_dir: data.join("config") }, Events::new(events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let edit = |cwd: &std::path::Path, p: &std::path::Path| -> Value {
            let body = serde_json::json!({ "hook_event_name": "PermissionRequest", "tool_name": "Edit", "tool_input": { "file_path": p.to_string_lossy() }, "cwd": cwd.to_string_lossy() });
            let out: Value = serde_json::from_str(&core.delegation_on_hook("kaan", "PermissionRequest", &body, false).unwrap()).unwrap();
            out["hookSpecificOutput"]["decision"]["behavior"].clone()
        };
        assert_eq!(edit(&proj, &proj.join("src/a.rs")), "allow", "no job: the team folder");
        let d = worktree::tests::job(&core.db, "kaan");
        let wt = worktree::prepare(&core.db, &d).unwrap().unwrap();
        assert_eq!(edit(&wt.path, &wt.path.join("src/a.rs")), "allow");
        assert_eq!(edit(&wt.path, &proj.join(".dex/results/1.md")), "allow");
        assert_eq!(edit(&wt.path, &proj.join(".dex/memory/shared/x.md")), "allow");
        assert_eq!(edit(&wt.path, &proj.join(".dex/tmp/build.log")), "allow", "#200: the team scratch");
        assert_eq!(edit(&wt.path, &proj.join(".dex/tmpx/a.md")), "deny", "only .dex/tmp itself");
        assert_eq!(edit(&wt.path, &proj.join("src/a.rs")), "deny", "the main checkout is not the job's");
        assert_eq!(edit(&wt.path, &wt.path.join(".git")), "deny", "protected as before");
        with(&core.db, |c| {
            let (ctx, _) = core.permit_ctx(c, "kaan", &serde_json::json!({}))?.unwrap();
            assert!(ctx.read.contains(&proj), "the team folder stays readable");
            assert!(ctx.read.contains(&proj.join(".dex").join("tmp")), "#200: .dex/tmp readable too");
            assert_eq!(ctx.cwd.as_deref(), Some(wt.path.as_path()));
            let prompt = delegate::worker_prompt(c, &d)?;
            assert!(prompt.contains(&wt.path.display().to_string()) && prompt.contains(&wt.branch), "{prompt}");
            Ok(())
        })
        .unwrap();
        // The other workers keep the team folder.
        let body = serde_json::json!({ "hook_event_name": "PermissionRequest", "tool_name": "Edit", "tool_input": { "file_path": proj.join("src/a.rs").to_string_lossy() }, "cwd": proj.to_string_lossy() });
        let out: Value = serde_json::from_str(&core.delegation_on_hook("ayla", "PermissionRequest", &body, false).unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&data);
    }
}

/// Görev #26: the policy at PreToolUse — it runs in every permission mode (auto / acceptEdits /
/// bypass never raise a PermissionRequest).
#[cfg(test)]
mod pre_tool_tests {
    use super::*;

    #[test]
    fn pre_tool_use_denies_and_allows_by_policy_and_leaves_gray_to_the_session() {
        let root = std::env::temp_dir().join(format!("dex-pretool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink),
        )
        .unwrap();
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())
        })
        .unwrap();
        let hook = |agent: &str, event: &str, tool: &str, input: Value| -> Option<Value> {
            let body = serde_json::json!({ "hook_event_name": event, "tool_name": tool, "tool_input": input, "cwd": proj.to_string_lossy(), "permission_mode": "auto" });
            core.delegation_on_hook(agent, event, &body, false).map(|o| serde_json::from_str(&o).unwrap())
        };
        let pre = |agent: &str, cmd: &str| hook(agent, "PreToolUse", "Bash", serde_json::json!({ "command": cmd }));
        let decision = |v: &Option<Value>| v.as_ref().map(|v| v["hookSpecificOutput"]["permissionDecision"].as_str().unwrap().to_string());

        // Hard no: denied before it runs, whatever the mode; the patron-work text comes along.
        let out = pre("kaan", "cargo test && git push server HEAD");
        assert_eq!(decision(&out).as_deref(), Some("deny"));
        let why = out.unwrap()["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_string();
        assert!(why.contains("git push") && why.contains("ask_leader'a gerek yok"), "{why}");
        assert_eq!(decision(&pre("kaan", "git push --dry-run dex-test-yok HEAD")).as_deref(), Some("deny"));
        let edit = hook("kaan", "PreToolUse", "Write", serde_json::json!({ "file_path": root.join("x.rs").to_string_lossy() }));
        assert_eq!(decision(&edit).as_deref(), Some("deny"));
        assert_eq!(decision(&pre("kaan", "cargo test -p dex-core --lib")).as_deref(), Some("deny"));
        // Safe: allowed (no dialog, no wait).
        assert_eq!(decision(&pre("kaan", "cargo test -p dex-core --lib chat")).as_deref(), Some("allow"));
        // Gray: no answer — the session's mode decides; a dialog would come back as PermissionRequest.
        assert!(pre("kaan", "curl -sI https://example.com").is_none());
        let pr = hook("kaan", "PermissionRequest", "Bash", serde_json::json!({ "command": "curl -sI https://example.com" })).unwrap();
        assert!(pr["hookSpecificOutput"]["decision"]["message"].as_str().unwrap().contains("izin #1"));
        // #164 P7: the leader gets the hard noes only; safe and gray calls stay with its session.
        assert_eq!(decision(&pre("timur", "git push")).as_deref(), Some("deny"));
        assert!(pre("timur", "cargo test -p dex-core --lib chat").is_none());
        assert!(pre("timur", "curl -sI https://example.com").is_none());
        let leader_write = |p: &std::path::Path| decision(&hook("timur", "PreToolUse", "Write", serde_json::json!({ "file_path": p.to_string_lossy() })));
        assert_eq!(leader_write(&proj.join(".claude").join("settings.local.json")).as_deref(), Some("deny"));
        assert_eq!(leader_write(&root.join("x.rs")).as_deref(), Some("deny"));
        assert!(leader_write(&proj.join("src").join("a.rs")).is_none());
        // #164 P3: a fail-closed worker Dex cannot place is denied; anyone else is left alone.
        assert!(pre("ghost", "cargo test -p dex-core --lib chat").is_none());
        core.hooks.fail_closed.lock().unwrap().insert("ghost".into());
        let out = pre("ghost", "cargo test -p dex-core --lib chat");
        assert_eq!(decision(&out).as_deref(), Some("deny"));
        assert!(out.unwrap()["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("karar veremiyor"));
        // Only the curl request needs the leader; hard denials remain in the audit log.
        let notes: Vec<String> = with(&core.db, |c| {
            c.prepare("SELECT text FROM leader_notes WHERE agent_id = 'timur' ORDER BY id").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(notes.iter().filter(|n| n.contains("izin #1")).count(), 1, "{notes:?}");
        assert_eq!(notes.len(), 1, "only the leader decision should be queued: {notes:?}");
        assert!(!notes.iter().any(|n| n.contains("ret:")), "{notes:?}");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Session output reads are scoped by host metadata, not the requested tool's claims.
    #[test]
    fn reviewer_tool_results_use_own_stored_session_not_tool_input() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().join(".dex/tmp/permit-744-context");
        std::fs::create_dir_all(&root).unwrap();
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap();
        let project = std::path::PathBuf::from(home).join(".claude/projects/test-744");
        let transcript = project.join("s1.jsonl");
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [root.to_string_lossy()]).map_err(|e| e.to_string())?;
            c.execute("UPDATE agents SET role_slug = ?1, engine = 'claude', active_engine = NULL WHERE id = 'kaan'", [review::ROLE]).map_err(|e| e.to_string())?;
            agents::record_session(c, "kaan", "s1", "claude", Some(&transcript.to_string_lossy()))?;
            let claimed = serde_json::json!({"transcript_path": project.join("s2.jsonl"), "session_id": "s2", "tool_results": project.join("s2/tool-results")});
            let (ctx, _) = core.permit_ctx(c, "kaan", &claimed)?.unwrap();
            assert_eq!(ctx.tool_results, Some(project.join("s1/tool-results")));
            assert!(!ctx.read.contains(&project), "do not grant the projects tree");
            c.execute("UPDATE agents SET role_slug = 'backend' WHERE id = 'kaan'", []).map_err(|e| e.to_string())?;
            assert!(core.permit_ctx(c, "kaan", &claimed)?.unwrap().0.tool_results.is_none());
            Ok(())
        }).unwrap();
        core.shutdown();
        drop(core);
        // Core's background DB handles may still be closing on Windows.
        let _ = std::fs::remove_dir_all(root);
    }

    /// #35: the hook's `scratchpad_dir` (this session's) is writable; without it, it is outside.
    #[test]
    fn the_sessions_scratchpad_comes_from_the_hook_body() {
        let root = std::env::temp_dir().join(format!("dex-scratch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let sp = root.join("claude").join("C--proj").join("s1").join("scratchpad");
        let write = |scratch: Option<&std::path::Path>, file: &std::path::Path| {
            let mut body = serde_json::json!({ "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_input": { "file_path": file.to_string_lossy() }, "cwd": proj.to_string_lossy() });
            if let Some(s) = scratch {
                body["scratchpad_dir"] = s.to_string_lossy().into();
            }
            let out: Value = serde_json::from_str(&core.delegation_on_hook("kaan", "PreToolUse", &body, false).unwrap()).unwrap();
            out["hookSpecificOutput"]["permissionDecision"].as_str().unwrap().to_string()
        };
        assert_eq!(write(Some(&sp), &sp.join("a.py")), "allow");
        assert_eq!(write(None, &sp.join("a.py")), "deny", "no field, no scratchpad root");
        assert_eq!(write(Some(&sp), &root.join("claude").join("C--proj").join("s2").join("scratchpad").join("a.py")), "deny", "sibling session");
        assert_eq!(write(Some(&root.join("claude")), &root.join("claude").join("a.py")), "deny", "not a scratchpad path");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gray_at_pre_tool_stays_with_the_session() {
        assert!(!permit::GRAY_AT_PRE_TOOL, "patron's call: flip only on purpose");
    }
}

/// Görev #20: every job in a fresh session. A shell PTY stands in for the worker; `chain_guard`
/// swallows the exit's restart so no real claude is started here.
#[cfg(test)]
mod fresh_tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    fn core(tag: &str, ids: &[&str]) -> (Arc<Core>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dex-fresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        for id in ids {
            let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
            core.pty_spawn(pty::Spawn { id: (*id).into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, agents::shell_sink_for_test(&core, id, Sink)).unwrap();
            core.chain_guard.lock().unwrap().insert((*id).into());
        }
        (core, root)
    }

    fn turn(core: &Core, id: &str, prompt: &str) {
        core.delegation_on_hook(id, "UserPromptSubmit", &serde_json::json!({ "prompt": prompt }), false);
        core.agent_states.hook(id, Some(agents::Status::Idle), None); // its Stop: idle, hook just now
    }

    fn job(core: &Core, to: &str) -> delegate::Delegation {
        with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(c, delegate::NewDelegation { from: &from, to_agent: to.into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })
        })
        .unwrap()
    }

    fn status(core: &Core, id: i64) -> String {
        with(&core.db, |c| delegate::get(c, id)).unwrap().status
    }

    fn gone(core: &Core, id: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while core.ptys.is_running(id) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        !core.ptys.is_running(id)
    }

    fn done(core: Arc<Core>, root: std::path::PathBuf, ids: &[&str]) {
        for id in ids {
            core.ptys.kill(id);
        }
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_jobs_start_never_resumes() {
        let o = agents::job_start("[Dex] yeni iş #1".into(), vec!["d".into()]);
        assert!(o.fresh && o.resume.is_none(), "no --resume, not even the saved session");
        assert_eq!(o.prompt.as_deref(), Some("[Dex] yeni iş #1"));
    }

    #[test]
    fn a_dex_session_is_replaced_for_the_next_job_only_after_the_quiet_window() {
        let (core, root) = core("dex", &["kaan"]);
        turn(&core, "kaan", "[Dex] yeni iş #0 — …");
        let d = job(&core, "kaan");
        // A hook just came (the report / memory write / Stop): no reset yet.
        assert_eq!(core.delegation_dispatch(d.clone()).unwrap().status, "queued");
        std::thread::sleep(Duration::from_millis(300));
        assert!(core.ptys.is_running("kaan"), "inside the quiet window the session stays");
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS));
        core.delegation_dispatch(d.clone()).unwrap();
        assert!(gone(&core, "kaan"), "after the quiet window the session is closed");
        assert_eq!(status(&core, d.id), "queued", "the exit starts it fresh");
        assert!(!core.resetting.lock().unwrap().contains_key("kaan"), "exit clears the mark");
        done(core, root, &[]);
    }

    /// #45: a reset mark with no exit behind it must not hold the queue. Fresh mark: the job
    /// waits and a retry watches it. Stale mark (the exit never came): dropped, reset again.
    #[test]
    fn a_reset_that_never_exits_is_retried_after_the_stuck_window() {
        let (core, root) = core("stuck", &["kaan"]);
        turn(&core, "kaan", "[Dex] yeni iş #0 — …");
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        let d = job(&core, "kaan");
        core.resetting.lock().unwrap().insert("kaan".into(), Instant::now());
        core.delegation_dispatch(d.clone()).unwrap();
        assert!(core.ptys.is_running("kaan"), "fresh mark: the exit is awaited, no second reset");
        assert!(core.queue_retry.lock().unwrap().contains_key("kaan"), "…and a retry keeps watching");
        let stale = Instant::now() - Duration::from_millis(agents::RESET_STUCK_MS + 1_000);
        core.resetting.lock().unwrap().insert("kaan".into(), stale);
        core.delegation_dispatch(d.clone()).unwrap();
        assert!(gone(&core, "kaan"), "stale mark: dropped, the session is reset again");
        assert!(!core.resetting.lock().unwrap().contains_key("kaan"));
        assert_eq!(status(&core, d.id), "queued", "the exit starts it fresh");
        done(core, root, &[]);
    }

    #[test]
    fn killing_a_missing_session_reports_it() {
        let (core, root) = core("nokill", &["kaan"]);
        assert!(!core.ptys.kill("nobody"), "no session: no exit to wait for");
        assert!(core.ptys.kill("kaan"));
        assert!(!core.ptys.kill("kaan"), "already gone");
        done(core, root, &[]);
    }

    #[test]
    fn the_patrons_session_a_leader_and_a_running_job_are_never_reset() {
        let (core, root) = core("keep", &["kaan", "ayla", "timur"]);
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        // Patron talking in kaan's session: the job goes in, the session stays.
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron soruyor" }), false);
        core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        let k = job(&core, "kaan");
        assert_eq!(core.delegation_dispatch(k).unwrap().status, "running");
        // A leader is never reset, whatever its last turn.
        turn(&core, "timur", "[Dex] Devretme sonucu geldi");
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        let t = job(&core, "timur");
        assert_eq!(core.delegation_dispatch(t).unwrap().status, "running");
        // A worker on a running job (Esc kept it open, or it waits for an answer): the next job queues.
        turn(&core, "ayla", "[Dex] yeni iş #1");
        let busy = job(&core, "ayla");
        with(&core.db, |c| delegate::set_status(c, busy.id, "running")).unwrap();
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        let next = job(&core, "ayla");
        assert_eq!(core.delegation_dispatch(next).unwrap().status, "queued");
        std::thread::sleep(Duration::from_millis(300));
        for id in ["kaan", "ayla", "timur"] {
            assert!(core.ptys.is_running(id), "{id} kept its session");
        }
        done(core, root, &["kaan", "ayla", "timur"]);
    }

    /// #671: a job pasted into the patron's own session keeps that session's effort (no
    /// `/effort`, it would rewrite their settings); the row says so for the leader's result line.
    #[test]
    fn effort_is_not_applied_in_the_patrons_session() {
        let (core, root) = core("effort", &["kaan"]);
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron soruyor" }), false);
        core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        std::thread::sleep(Duration::from_millis(agents::RESET_QUIET_MS + 100));
        let k = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(c, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: Some(effort::Effort::High) })
        })
        .unwrap();
        assert_eq!(core.delegation_dispatch(k.clone()).unwrap().status, "running");
        assert!(core.ptys.is_running("kaan"), "the patron's session stays");
        assert!(with(&core.db, |c| delegate::get(c, k.id)).unwrap().effort_skipped);
        done(core, root, &["kaan"]);
    }

    /// #1779: patron oturumundaki worker'a iş verilince de iş kendi dalında yürür —
    /// kayıt `dex/<agent>/<id>` dalına bağlanır, inceleme/birleştirme normal akar.
    /// Patron oturumu korunur (öldürülmez), effort yine uygulanmaz.
    #[test]
    fn patron_session_job_still_gets_its_worktree() {
        let proj = worktree::tests::repo("patron-wt");
        let (core, root) = core("patron-wt", &["kaan"]);
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())?;
            c.execute("UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", rusqlite::params![proj.to_string_lossy(), crate::projects::path_key(&proj.to_string_lossy())]).map_err(|e| e.to_string())?;
            let ready = crate::projects::prepare_root(&proj.to_string_lossy(), None).map_err(|e| e.to_string())?;
            crate::projects::verify(c, "default", "legacy-default", &ready).map_err(|e| e.to_string())
        })
        .unwrap();
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron soruyor" }), false);
        core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let k = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(c, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: Some(effort::Effort::High) })
        })
        .unwrap();
        assert_eq!(core.delegation_dispatch(k.clone()).unwrap().status, "running");
        assert!(core.ptys.is_running("kaan"), "the patron's session stays");
        let w = with(&core.db, |c| worktree::get(c, k.id)).unwrap().expect("patron oturumunda da iş kaydı dala bağlanır");
        assert!(w.path.is_dir() && w.removed_at.is_none(), "{w:?}");
        let prompt = with(&core.db, |c| delegate::worker_prompt(c, &delegate::get(c, k.id).unwrap())).unwrap();
        assert!(prompt.contains(&w.branch) && prompt.contains(&w.path.display().to_string()), "{prompt}");
        assert!(with(&core.db, |c| delegate::get(c, k.id)).unwrap().effort_skipped, "effort yine uygulanmaz");
        done(core, root, &["kaan"]);
    }

    /// #671: `delegate`'s effort is one of three; anything else is refused before any row is written.
    #[test]
    fn delegate_refuses_an_effort_outside_the_three() {
        let (core, root) = core("effort-bad", &[]);
        let schema = mcp::tools_list_for_test(&core, "timur").into_iter().find(|t| t["name"] == "delegate").unwrap();
        assert_eq!(schema["inputSchema"]["properties"]["effort"]["enum"], serde_json::json!(["medium", "high", "xhigh"]));
        for bad in [serde_json::json!("low"), serde_json::json!("max"), serde_json::json!(2)] {
            let r = mcp::call_with_args_for_test(&core, "timur", "delegate", serde_json::json!({ "agentId": "kaan", "prompt": "iş", "effort": bad }));
            assert_eq!(r["isError"], true, "{r}");
            assert!(r["content"][0]["text"].as_str().unwrap().contains("medium | high | xhigh"), "{r}");
        }
        let rows: i64 = with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap();
        assert_eq!(rows, 0);
        done(core, root, &[]);
    }
}

/// Görev #12: a queued job reaches an idle worker without a later Stop. The worker is a plain
/// shell PTY; "delivered" = the paste went in and the row turned `running`.
#[cfg(test)]
mod queue_tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    /// PSReadLine echoes the paste with syntax colors and redraws; strip the
    /// escape sequences so the echoed text is comparable.
    fn visible(out: &str) -> String {
        let mut s = String::with_capacity(out.len());
        let mut chars = out.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                s.push(c);
                continue;
            }
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '\x07' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn core(tag: &str) -> (Arc<Core>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dex-queue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(
            Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            Events::new(events::NullSink),
        )
        .unwrap();
        // The pasted prompt runs as a shell command line: keep whatever it writes in `root`.
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        core.pty_spawn(
            pty::Spawn {
                id: "kaan".into(),
                cmd: shell::SHELL_CMD.into(),
                cwd: Some(root.join("cwd").to_string_lossy().into_owned()),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            agents::shell_sink_for_test(&core, "kaan", Sink),
        )
        .unwrap();
        patron_session(&core);
        (core, root)
    }

    /// These tests are about when a job is delivered, not how: the patron is talking in kaan's
    /// session, so delivery pastes into it (a Dex session would be replaced by a real claude).
    fn patron_session(core: &Core) {
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron konuşuyor" }), false);
    }

    fn set(core: &Core, status: agents::Status) {
        core.agent_states.hook("kaan", Some(status), None);
    }

    /// A new job for `kaan`, dispatched like the MCP `delegate` tool does.
    fn delegate(core: &Core, prompt: &str) -> delegate::Delegation {
        let d = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &from, to_agent: "kaan".into(), team_id: "default".into(),
                    service_team_id: None, prompt: prompt.into(), task_id: None, patron_words: None, effort: None,
                },
            )
        })
        .unwrap();
        core.delegation_dispatch(d).unwrap()
    }

    fn status_within(core: &Core, id: i64, limit: Duration) -> String {
        let end = Instant::now() + limit;
        loop {
            let s = with(&core.db, |c| delegate::get(c, id)).unwrap().status;
            if s != "queued" || Instant::now() >= end {
                return s;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn done(core: Arc<Core>, root: std::path::PathBuf) {
        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #2044: the pool took kaan (its job reported, the session still runs to Stop); dispatch
    /// hands the job to the idle frontend worker instead of queueing it behind kaan's turn.
    #[test]
    fn pool_job_claimed_for_a_worker_mid_turn_goes_to_an_idle_colleague() {
        let (core, root) = core("rebalance");
        with(&core.db, |c| {
            c.execute("INSERT INTO agents (id, team_id, name, kind, title, model, shirt, hair, skin, sort, role_slug)
                       VALUES ('deniz', 'default', 'Deniz', 'worker', 'Geliştirici', 'sonnet', '#000', '#000', '#000', 9, 'frontend')", [])
                .map_err(|e| e.to_string())
        })
        .unwrap();
        set(&core, agents::Status::Working);
        core.drain(true); // stop right after the choice: no session starts in a test
        let d = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
            let d = delegate::insert(&tx, delegate::NewDelegation {
                from: &from, to_agent: "kaan".into(), team_id: "default".into(),
                service_team_id: None, prompt: "havuz".into(), task_id: None, patron_words: None, effort: None,
            })?;
            delegate::queue_role(&tx, d.id, "frontend", None)?;
            let d = delegate::assign_role_job(&tx, d.id)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(d)
        })
        .unwrap();
        assert_eq!(d.to_agent, "kaan", "the DB sees kaan free: team order");
        assert_eq!(core.delegation_dispatch(d).unwrap().to_agent, "deniz");
        done(core, root);
    }

    #[test]
    fn job_queued_on_a_busy_worker_is_delivered_once_it_is_idle_without_a_stop() {
        let (core, root) = core("retry");
        set(&core, agents::Status::Working);
        let d = delegate(&core, "iş bir");
        assert_eq!(d.status, "queued", "busy worker: queued");
        // Idle by some other path (no Stop carries the queue); the retry delivers.
        set(&core, agents::Status::Idle);
        assert_eq!(status_within(&core, d.id, Duration::from_secs(8)), "running");
        done(core, root);
    }

    /// #161: draining holds new jobs in the queue (not even the retry delivers them), names what
    /// still runs, and its end delivers the queue.
    #[test]
    fn drain_holds_jobs_names_what_runs_and_its_end_delivers() {
        let (core, root) = core("drain");
        set(&core, agents::Status::Idle);
        core.drain(true);
        assert!(core.drain_blockers().unwrap().is_empty(), "nothing runs: drained");
        let d = delegate(&core, "iş beş");
        assert_eq!(d.status, "queued");
        std::thread::sleep(Duration::from_millis(1_500));
        assert_eq!(with(&core.db, |c| delegate::get(c, d.id)).unwrap().status, "queued", "draining: it waits");
        set(&core, agents::Status::Working);
        assert_eq!(core.drain_blockers().unwrap(), vec!["kaan çalışıyor".to_string()]);
        set(&core, agents::Status::Idle);
        core.drain(false);
        assert_eq!(status_within(&core, d.id, Duration::from_secs(8)), "running");
        assert!(core.drain_blockers().unwrap().contains(&format!("#{} kaan üzerinde koşuyor", d.id)));
        done(core, root);
    }

    /// #400: a dev update waits for a quiet core — a working agent and a queued job both keep it
    /// (a drain's blockers leave the queue out: a drain holds it), and nothing is held meanwhile.
    #[test]
    fn quiet_blockers_name_working_agents_and_queued_jobs() {
        let (core, root) = core("quiet");
        set(&core, agents::Status::Idle);
        assert!(core.quiet_blockers().unwrap().is_empty(), "nothing runs, nothing waits: quiet");
        set(&core, agents::Status::Working);
        let d = delegate(&core, "iş altı");
        assert_eq!(d.status, "queued", "busy worker: queued");
        let queued = format!("#{} kaan için kuyrukta", d.id);
        assert_eq!(core.quiet_blockers().unwrap(), vec!["kaan çalışıyor".to_string(), queued.clone()]);
        assert!(!core.drain_blockers().unwrap().contains(&queued));
        set(&core, agents::Status::Waiting);
        assert!(core.quiet_blockers().unwrap().contains(&"kaan çalışıyor".to_string()), "waiting counts too");
        set(&core, agents::Status::Idle);
        assert_eq!(status_within(&core, d.id, Duration::from_secs(8)), "running", "not drained: the queue is delivered");
        assert!(core.quiet_blockers().unwrap().contains(&format!("#{} kaan üzerinde koşuyor", d.id)));
        done(core, root);
    }

    /// #427: a drain that comes on while a dispatch is past its drain check (the job still queued,
    /// the worker idle) is not drained/quiet until that dispatch ends — and then its job runs.
    #[test]
    fn drain_waits_for_a_dispatch_past_its_check() {
        let (core, root) = core("inflight");
        set(&core, agents::Status::Idle);
        let (reached, at_pause) = std::sync::mpsc::channel();
        let (go, resume) = std::sync::mpsc::channel::<()>();
        *core.dispatch_pause.lock().unwrap() = Some(Box::new(move || {
            reached.send(()).unwrap();
            resume.recv().unwrap();
        }));
        let c = core.clone();
        let job = std::thread::spawn(move || delegate(&c, "iş arada"));
        at_pause.recv_timeout(Duration::from_secs(5)).unwrap();
        core.drain(true);
        let paused = "1 tur başlatılıyor".to_string();
        assert_eq!(core.drain_blockers().unwrap(), vec![paused.clone()], "the job is still queued, kaan idle: only the dispatch blocks");
        assert!(core.quiet_blockers().unwrap().contains(&paused));
        go.send(()).unwrap();
        let d = job.join().unwrap();
        assert_eq!(d.status, "running", "past its check before the drain: delivered");
        let running = format!("#{} kaan üzerinde koşuyor", d.id);
        assert!(core.drain_blockers().unwrap().contains(&running) && !core.drain_blockers().unwrap().contains(&paused));
        assert!(core.quiet_blockers().unwrap().contains(&running));
        done(core, root);
    }

    /// #447: a chat paste whose text is in the PTY but whose Enter is not sent yet (kaan still idle,
    /// nothing running or queued) holds a drain's / quiet wait's look — no restart; it shows as
    /// Working once in. The gate closed, chat waits in the queue and a start with a prompt is
    /// refused; the drain's end delivers the queue.
    #[test]
    fn a_paste_before_its_enter_holds_the_restart_and_the_gate_holds_new_turns() {
        let (core, root) = core("paste");
        set(&core, agents::Status::Idle);
        let (reached, at_pause) = std::sync::mpsc::channel();
        let (go, resume) = std::sync::mpsc::channel::<()>();
        *core.paste_pause.lock().unwrap() = Some(Box::new(move || {
            reached.send(()).unwrap();
            resume.recv().unwrap();
        }));
        let c = core.clone();
        let send = std::thread::spawn(move || c.chat_send("kaan", "echo merhaba"));
        at_pause.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(core.agent_states.get("kaan").status, agents::Status::Idle, "text in, no Enter yet");
        core.drain(true);
        let paused = "1 tur başlatılıyor".to_string();
        assert_eq!(core.drain_blockers().unwrap(), vec![paused.clone()], "only the paste holds the restart");
        assert_eq!(core.quiet_blockers().unwrap(), vec![paused]);
        go.send(()).unwrap();
        assert_eq!(send.join().unwrap(), Ok("sent"), "past the gate before the drain: delivered");
        assert_eq!(core.quiet_blockers().unwrap(), vec!["kaan çalışıyor".to_string()]);

        // The gate is closed: new turns wait.
        set(&core, agents::Status::Idle);
        assert_eq!(core.chat_send("kaan", "echo sonra"), Ok("queued"));
        let start = agents::StartOptions { prompt: Some("x".into()), ..Default::default() };
        assert_eq!(core.agent_start("ayla", start).unwrap_err(), restart::HELD);
        assert!(!core.ptys.is_running("ayla"));
        assert!(core.quiet_blockers().unwrap().is_empty(), "held, nothing started");
        assert_eq!(core.restart_mark().unwrap().chat.get("kaan"), Some(&vec!["echo sonra".to_string()]), "the next core gets it");
        let _ = std::fs::remove_file(core.paths.data_dir.join(restart::FILE));

        // Taken back: the queue goes in.
        core.drain(false);
        let end = Instant::now() + Duration::from_secs(5);
        while core.agent_states.get("kaan").status != agents::Status::Working && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(core.agent_states.get("kaan").status, agents::Status::Working);
        assert!(core.chat_queues.lock().unwrap().get("kaan").is_none_or(|q| q.is_empty()));
        done(core, root);
    }

    #[test]
    fn job_queued_in_the_stop_race_window_is_delivered_on_the_idle_turn() {
        let (core, root) = core("race");
        set(&core, agents::Status::Working);
        // Stop: `on_stop` finds no queue (nothing to block) …
        let out = core.delegation_on_hook("kaan", "Stop", &serde_json::json!({ "hook_event_name": "Stop" }), false);
        assert!(!is_blocked(&out));
        // … a job lands before hooks.rs flips the status to idle …
        let d = delegate(&core, "iş iki");
        assert_eq!(d.status, "queued");
        set(&core, agents::Status::Idle);
        // … and goes in after the idle turn — well before the 5.1 s retry.
        assert_eq!(status_within(&core, d.id, Duration::from_millis(2_500)), "running");
        done(core, root);
    }

    #[test]
    fn chat_queue_goes_first_and_the_job_waits_for_its_turn() {
        let (core, root) = core("chat");
        set(&core, agents::Status::Working);
        core.chat_queues.lock().unwrap().entry("kaan".into()).or_default().push_back("patron mesajı".into());
        let out = core.delegation_on_hook("kaan", "Stop", &serde_json::json!({ "hook_event_name": "Stop" }), false);
        assert!(!is_blocked(&out));
        let d = delegate(&core, "iş üç");
        set(&core, agents::Status::Idle);
        // The chat paste (600 ms) starts the turn; no second paste lands on top of it.
        std::thread::sleep(Duration::from_millis(1_500));
        assert_eq!(core.agent_states.get("kaan").status, agents::Status::Working);
        assert_eq!(with(&core.db, |c| delegate::get(c, d.id)).unwrap().status, "queued", "next Stop delivers it");
        done(core, root);
    }

    #[test]
    fn esc_interrupt_row_in_the_transcript_turns_the_worker_idle_and_delivers_the_queue() {
        let (core, root) = core("esc");
        let transcript = root.join("session.jsonl");
        std::fs::write(&transcript, "{\"type\":\"user\",\"message\":{\"content\":\"ilk istem\"}}\n").unwrap();
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "claude", &transcript);
        std::thread::sleep(Duration::from_millis(300)); // the watch starts at the file's end
        set(&core, agents::Status::Working);
        let d = delegate(&core, "iş dört");
        assert_eq!(d.status, "queued", "busy worker: queued");
        // Esc: no hook, one transcript row.
        let row = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"[Request interrupted by user]\"}]}}\n";
        std::fs::OpenOptions::new().append(true).open(&transcript).unwrap().write_all(row.as_bytes()).unwrap();
        // Idle → post-turn delivery (700 ms), well before the 5.1 s retry.
        assert_eq!(status_within(&core, d.id, Duration::from_millis(3_000)), "running");
        core.chat_watches.drop_watch("kaan");
        done(core, root);
    }

    /// Görev #19 B: an idle claude worker's pane gets only the trigger line, never the job text.
    #[test]
    fn idle_worker_pane_gets_only_the_trigger_line() {
        struct Tap(Arc<Mutex<Vec<u8>>>);
        impl PtySink for Tap {
            fn data(&mut self, b: &[u8]) -> bool { self.0.lock().unwrap().extend_from_slice(b); true }
            fn exit(&mut self) {}
        }
        let (core, root) = core("trigger");
        core.ptys.kill("kaan");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (sink, wait_ready) = agents::shell_sink_with_ready_for_test(&core, "kaan", Tap(seen.clone()));
        core.pty_spawn(
            pty::Spawn {
                id: "kaan".into(),
                cmd: shell::SHELL_CMD.into(),
                cwd: Some(root.join("cwd").to_string_lossy().into_owned()),
                cols: 200,
                rows: 24,
                ..Default::default()
            },
            sink,
        )
        .unwrap();
        wait_ready();
        std::thread::sleep(Duration::from_millis(1_500)); // shell prompt up
        set(&core, agents::Status::Idle);
        let d = delegate(&core, "UZUNGOREVMETNI");
        assert_eq!(d.status, "running", "pasted");
        std::thread::sleep(Duration::from_millis(800)); // echo
        let out = visible(&String::from_utf8_lossy(&seen.lock().unwrap()));
        let trigger = delegate::trigger_line(d.id, "Kaan");
        assert!(out.contains(&trigger), "trigger echoed: {out}");
        assert!(!out.contains("UZUNGOREVMETNI"), "job text stayed out of the pane");
        done(core, root);
    }

    #[test]
    fn chat_send_claims_the_patron_turn_without_waiting_for_a_hook() {
        let (core, root) = core("chat-owner");
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({"prompt":"[Dex] devam"}), false);
        set(&core, agents::Status::Idle);
        assert_eq!(core.chat_send("kaan", "echo patron"), Ok("sent"));
        let owner = core.turns.dex_turn("kaan");
        assert_eq!(core.agent_states.get("kaan").status, agents::Status::Working);
        done(core, root);
        assert_eq!(owner, Some(false), "chat already knows this came from the patron");
    }

    /// Esc running işi kapatmaz; kuyruk onun ardında, patronun sonraki
    /// turunun Stop'una kadar bekler.
    #[test]
    fn esc_keeps_the_running_job_and_the_queue_waits_for_the_next_stop() {
        let (core, root) = core("esc-running");
        let transcript = root.join("session.jsonl");
        std::fs::write(&transcript, "").unwrap();
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "claude", &transcript);
        std::thread::sleep(Duration::from_millis(300));
        set(&core, agents::Status::Working);
        let running = delegate(&core, "iş beş");
        with(&core.db, |c| delegate::set_status(c, running.id, "running")).unwrap();
        let queued = delegate(&core, "iş altı");
        assert_eq!(queued.status, "queued");
        let row = "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"[Request interrupted by user for tool use]\"}]}}\n";
        std::fs::OpenOptions::new().append(true).open(&transcript).unwrap().write_all(row.as_bytes()).unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while core.agent_states.get("kaan").status != agents::Status::Idle && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(core.agent_states.get("kaan").status, agents::Status::Idle, "Esc → idle");
        // Past the post-turn delivery (700 ms) and one retry (5.1 s): nothing moves.
        std::thread::sleep(Duration::from_millis(6_000));
        assert_eq!(with(&core.db, |c| delegate::get(c, running.id)).unwrap().status, "running");
        assert_eq!(with(&core.db, |c| delegate::get(c, queued.id)).unwrap().status, "queued");
        core.chat_watches.drop_watch("kaan");
        done(core, root);
    }

    #[test]
    fn oldest_queued_job_is_not_held_back_by_a_newer_one() {
        let (core, root) = core("fifo");
        set(&core, agents::Status::Working);
        let a = delegate(&core, "iş a");
        let b = delegate(&core, "iş b");
        with(&core.db, |c| {
            assert!(!delegate::has_other_work(c, &a)?, "nothing before the oldest");
            assert!(delegate::has_other_work(c, &b)?, "the newer one waits for the older");
            Ok(())
        })
        .unwrap();
        set(&core, agents::Status::Idle);
        core.delegation_dispatch_next("kaan", false);
        assert_eq!(status_within(&core, a.id, Duration::from_secs(1)), "running");
        assert_eq!(with(&core.db, |c| delegate::get(c, b.id)).unwrap().status, "queued", "b: after a's Stop");
        done(core, root);
    }
}

/// #195: merging a job whose worker session still sits in its worktree (Windows keeps a folder
/// that is a live process's current directory). Shell PTYs stand in for the sessions; no job is
/// queued, so no exit starts a real claude.
#[cfg(test)]
mod worktree_release_tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    /// A finished job of kaan's with one commit, its session (idle) in the job's tree.
    fn setup(tag: &str) -> (Arc<Core>, std::path::PathBuf, worktree::Wt) {
        let proj = worktree::tests::repo(tag);
        let data = std::env::temp_dir().join(format!("dex-wt-{tag}-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let core = Core::start(Paths { data_dir: data.join("data"), config_dir: data.join("config") }, Events::new(events::NullSink)).unwrap();
        with(&core.db, |c| c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [proj.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let d = worktree::tests::job(&core.db, "kaan");
        let w = worktree::prepare(&core.db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        worktree::tests::commit(&w.path, "b ekle");
        let cwd = Some(w.path.to_string_lossy().into_owned());
        core.pty_spawn(pty::Spawn { id: "kaan".into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, agents::shell_sink_for_test(&core, "kaan", Sink)).unwrap();
        core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        with(&core.db, |c| delegate::set_status(c, d.id, "done")).unwrap();
        (core, data, w)
    }

    fn merge(core: &Core, w: &worktree::Wt) -> Value {
        let lead = with(&core.db, |c| delegate::caller(c, "timur")).unwrap();
        core.delegation_merge(&lead, w.delegation_id, false, false).unwrap()
    }

    fn removed(core: &Core, id: i64) -> bool {
        with(&core.db, |c| worktree::get(c, id)).unwrap().unwrap().removed_at.is_some()
    }

    fn branch_gone(w: &worktree::Wt) -> bool {
        let out = std::process::Command::new("git").arg("-C").arg(&w.root).args(["branch", "--list", &w.branch]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    fn end(core: Arc<Core>, data: std::path::PathBuf) {
        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn cleanup_failure_does_not_queue_leader_notes_on_repeated_sweeps() {
        let (core, data, w) = setup("quiet-cleanup");
        core.ptys.kill("kaan");
        std::fs::write(w.path.join("untracked.txt"), "keep").unwrap();
        let out = merge(&core, &w);
        assert_eq!(out["merged"], true, "{out}");
        assert!(out["cleanup"].as_str().is_some_and(|s| s.contains("bekleyen temizlik")), "{out}");
        let notes = || with(&core.db, |c| c.query_row("SELECT count(*) FROM leader_notes", [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())).unwrap();
        let before = notes();
        core.worktree_sweep();
        core.worktree_sweep();
        assert_eq!(notes(), before, "cleanup must not enqueue blocking leader notes");
        assert!(w.path.join("untracked.txt").is_file());
        let team = || serde_json::to_value(core.team_list().unwrap().into_iter().find(|t| t.id == "default").unwrap()).unwrap();
        assert_eq!(team()["worktreeCleanup"], serde_json::json!([w.path.to_string_lossy()]));
        std::fs::remove_file(w.path.join("untracked.txt")).unwrap();
        worktree::clean(&core.db, w.delegation_id, 8).unwrap();
        assert_eq!(team()["worktreeCleanup"], serde_json::json!([]));
        end(core, data);
    }

    #[test]
    fn merge_closes_the_idle_session_in_the_tree_then_removes_it() {
        let (core, data, w) = setup("release");
        let out = merge(&core, &w);
        assert_eq!(out["merged"], true, "{out}");
        assert!(out.get("cleanup").is_none(), "{out}");
        assert!(!core.ptys.is_running("kaan"), "the session in the tree was closed");
        assert!(!w.path.exists() && branch_gone(&w) && removed(&core, w.delegation_id));
        end(core, data);
    }

    /// The patron is talking to the worker: its session stays; the branch goes, the folder waits for
    /// that session's exit.
    #[test]
    fn a_session_still_needed_stays_and_its_exit_finishes_the_cleanup() {
        let (core, data, w) = setup("needed");
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron soruyor" }), false);
        core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
        let out = merge(&core, &w);
        assert_eq!(out["merged"], true, "{out}");
        assert!(core.ptys.is_running("kaan"), "the patron's conversation is not closed");
        assert!(branch_gone(&w), "{out}");
        if cfg!(windows) {
            assert!(out["cleanup"].as_str().is_some_and(|s| s.contains("bekleyen temizlik")), "{out}");
            assert!(w.path.is_dir() && !removed(&core, w.delegation_id));
        }
        core.ptys.kill("kaan");
        let end_at = Instant::now() + Duration::from_secs(15);
        while !removed(&core, w.delegation_id) && Instant::now() < end_at {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(removed(&core, w.delegation_id) && !w.path.exists(), "the exit's retry removed it");
        end(core, data);
    }
}

/// #838 G: a worker session bound to its job (agents.rs records the binding before the spawn) is
/// placed by that job's project alone — permit roots, reviewer/service limits, prerequisite
/// reports — and a same-project paste moves the binding and the MCP key's job together.
#[cfg(test)]
mod project_bind_tests {
    use super::*;
    use crate::projects::{self, tests::Fixture, NewProject, Project};
    use permit::Verdict;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};

    struct Env {
        core: Arc<Core>,
        root: PathBuf,
        f: Fixture,
        p1: Project,
        p2: Project,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            for p in self.core.ptys.list() {
                self.core.chain_guard.lock().unwrap().insert(p.id.clone());
                self.core.ptys.kill(&p.id);
            }
            self.core.shutdown();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The default team with P1 and P2 bound.
    fn env(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("dex-gbind-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let f = Fixture::new();
        let mut made = Vec::new();
        for name in ["p1", "p2"] {
            let ready = projects::prepare_root(f.dir(name).to_str().unwrap(), None).unwrap();
            made.push(with(&core.db, |c| projects::create(c, "default", NewProject { name: name.into(), worktrees: None }, &ready).map_err(|e| e.to_string())).unwrap());
        }
        let p2 = made.pop().unwrap();
        let p1 = made.pop().unwrap();
        Env { core, root, f, p1, p2 }
    }

    fn sql(e: &Env, q: &str, p: &[&dyn rusqlite::ToSql]) {
        with(&e.core.db, |c| c.execute(q, p).map(|_| ()).map_err(|e| e.to_string())).unwrap();
    }

    fn root(p: &Project) -> PathBuf {
        PathBuf::from(p.root_path.as_deref().unwrap())
    }

    fn job(e: &Env, id: i64, agent: &str, p: &Project, status: &str) {
        sql(e, "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,project_id,project_root,merge_branch,created_at,updated_at)
                VALUES(?1,'timur',?2,'default','iş',?3,?4,?5,?6,1,1)",
            &[&id, &agent, &status, &p.id, &p.root_path, &p.default_branch]);
    }

    fn worktree(e: &Env, id: i64, agent: &str, p: &Project) -> PathBuf {
        let dir = e.f.0.join(format!("wt-{agent}-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        sql(e, "INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(?1,?2,?3,?4,?5,'base',1)",
            &[&id, &agent, &p.root_path, &dir.to_string_lossy().into_owned(), &format!("dex/{agent}/{id}")]);
        dir
    }

    /// The row a bound spawn leaves before the CLI names its session.
    fn bind(e: &Env, agent: &str, project: &str, job: i64) {
        sql(e, "INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES(?1,'','claude',1,?2,?3)
                ON CONFLICT(agent_id) DO UPDATE SET project_id=excluded.project_id, delegation_id=excluded.delegation_id",
            &[&agent, &project, &job]);
    }

    fn ctx(e: &Env, agent: &str) -> Result<permit::Ctx, String> {
        with(&e.core.db, |c| Ok(e.core.permit_ctx(c, agent, &serde_json::json!({}))?.unwrap().0))
    }

    fn write(c: &permit::Ctx, p: &Path) -> bool {
        permit::decide("Write", &serde_json::json!({ "file_path": p.to_string_lossy() }), c) == Verdict::Allow
    }

    fn read(c: &permit::Ctx, p: &Path) -> bool {
        permit::decide("Read", &serde_json::json!({ "file_path": p.to_string_lossy() }), c) == Verdict::Allow
    }

    fn memory(e: &Env, agent: &str) -> project_context::ProjectContext {
        with(&e.core.db, |c| hooks::session_context(c, &e.core.paths.data_dir, agent, true).map_err(|e| e.to_string())).unwrap()
    }

    #[test]
    fn a_bound_worker_is_placed_by_its_jobs_project_only() {
        let e = env("place");
        let (p1, p2) = (root(&e.p1), root(&e.p2));
        job(&e, 1, "kaan", &e.p1, "running");
        let w1 = worktree(&e, 1, "kaan", &e.p1);
        job(&e, 2, "kaan", &e.p2, "queued");
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        bind(&e, "kaan", &e.p1.id, 1);
        let c = ctx(&e, "kaan").unwrap();
        assert_eq!(c.cwd.as_deref(), Some(w1.as_path()));
        assert!(write(&c, &w1.join("src").join("a.rs")));
        assert!(write(&c, &p1.join(".dex").join("results").join("1.md")) && write(&c, &p1.join(".dex").join("tmp").join("x.log")));
        for p in [p1.join("src").join("a.rs"), p2.join("src").join("a.rs"), w2.join("src").join("a.rs"), p2.join(".dex").join("results").join("2.md")] {
            assert!(!write(&c, &p), "{}", p.display());
        }
        assert!(read(&c, &p1.join("src").join("a.rs")) && !read(&c, &p2.join("src").join("a.rs")));

        // Memory: team shared + own private + P1 shared to write; the anchors' roots stay readable.
        let pc = memory(&e, "kaan");
        let own = with(&e.core.db, |c| slug::memory_dir(c, &pc.agent_memory_root, "kaan")).unwrap();
        let team = pc.team_memory_root.clone();
        for p in [team.join("shared").join("x.md"), own.join("x.md"), p1.join(".dex").join("memory").join("shared").join("x.md")] {
            assert!(write(&c, &p), "{}", p.display());
        }
        for p in [team.join("agents").join("ayla").join("x.md"), team.join("x.md"), p2.join(".dex").join("memory").join("shared").join("x.md")] {
            assert!(!write(&c, &p), "narrowed: {}", p.display());
        }
        assert!(read(&c, &team.join("agents").join("ayla").join("x.md")), "reading is not narrowed");

        // P2's job started later (the old `worktree::active` would place it in W2): the binding decides.
        sql(&e, "UPDATE delegations SET status = 'running' WHERE id = 2", &[]);
        let c = ctx(&e, "kaan").unwrap();
        assert!(write(&c, &w1.join("a.rs")) && !write(&c, &w2.join("a.rs")));

        // Its worktree gone: P1 itself, never the team folder or P2.
        sql(&e, "UPDATE delegation_worktrees SET removed_at = 1 WHERE delegation_id = 1", &[]);
        let c = ctx(&e, "kaan").unwrap();
        assert_eq!(c.cwd.as_deref(), Some(p1.as_path()));
        assert!(write(&c, &p1.join("src").join("a.rs")) && !write(&c, &p2.join("src").join("a.rs")));

        // Its project archived: nothing is placed — the worker's call is denied (fail-closed).
        sql(&e, "UPDATE delegations SET status = 'done' WHERE id IN (1, 2)", &[]);
        sql(&e, "UPDATE delegation_worktrees SET removed_at = 1", &[]);
        with(&e.core.db, |c| projects::archive(c, "default", &e.p1.id, |_| false).map_err(|e| e.to_string())).unwrap();
        assert_eq!(ctx(&e, "kaan").err().as_deref(), Some("project_unavailable"));
        e.core.hooks.fail_closed.lock().unwrap().insert("kaan".into());
        let body = serde_json::json!({ "tool_name": "Write", "tool_input": { "file_path": p1.join("a.rs") }, "cwd": p1 });
        let out = e.core.delegation_on_hook("kaan", "PreToolUse", &body, false).unwrap();
        assert!(out.contains("\"deny\"") && out.contains("karar veremiyor"), "{out}");
        // An unbound session of the same worker keeps the old placement.
        sql(&e, "DELETE FROM agent_sessions WHERE agent_id = 'kaan'", &[]);
        assert!(ctx(&e, "kaan").is_ok());
    }

    #[test]
    fn reviewer_service_and_prerequisite_reports_come_from_the_bound_job() {
        let e = env("limits");
        let (p1, p2) = (root(&e.p1), root(&e.p2));
        // The reviewer: reads its job's P, writes only `<P>/.dex/results/<job>.md`.
        sql(&e, "UPDATE agents SET role_slug = ?1 WHERE id = 'ayla'", &[&review::ROLE]);
        job(&e, 3, "ayla", &e.p2, "running");
        bind(&e, "ayla", &e.p2.id, 3);
        std::fs::create_dir_all(p2.join(".dex").join("results")).unwrap();
        std::fs::create_dir_all(p1.join(".dex").join("results")).unwrap();
        let c = ctx(&e, "ayla").unwrap();
        assert!(c.read_only && c.job == Some(3) && c.team_root.as_deref() == Some(p2.as_path()));
        assert!(write(&c, &p2.join(".dex").join("results").join("3.md")));
        assert!(!write(&c, &p1.join(".dex").join("results").join("3.md")) && !write(&c, &p2.join("src").join("a.rs")));
        assert!(read(&c, &p2.join("src").join("a.rs")) && !read(&c, &p1.join("src").join("a.rs")));

        // A service worker: its workspace and its one research file; the requester's P read-only.
        let svc = e.core.team_create(teams::NewTeam { profile: None, name: "Araştırma".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
        let worker = e.core.agent_list().unwrap().into_iter().find(|a| a.team_id == svc.id && a.kind == "worker").unwrap().id;
        let report = p1.join("docs").join("research").join("4-konu.md");
        sql(&e, "INSERT INTO delegations(id,from_agent,to_agent,team_id,service_team_id,prompt,status,project_id,project_root,result_path,created_at,updated_at)
                VALUES(4,'timur',?1,'default',?2,'konu','running',?3,?4,?5,1,1)",
            &[&worker, &svc.id, &e.p1.id, &e.p1.root_path, &report.to_string_lossy().into_owned()]);
        bind(&e, &worker, &e.p1.id, 4);
        let c = ctx(&e, &worker).unwrap();
        let workspace = PathBuf::from(svc.workdir.clone().unwrap());
        assert!(write(&c, &report) && write(&c, &workspace.join("notes.md")));
        for p in [p1.join("docs").join("research").join("5-other.md"), p1.join("src").join("a.rs"), p1.join(".dex").join("memory").join("shared").join("x.md")] {
            assert!(!write(&c, &p), "{}", p.display());
        }
        assert!(read(&c, &p1.join("src").join("a.rs")) && !read(&c, &p2.join("src").join("a.rs")));
        let pc = memory(&e, &worker);
        assert!(write(&c, &pc.team_memory_root.join("shared").join("x.md")), "its own team's memory");

        // A prerequisite in another project: only its report file, read-only.
        let done = p2.join(".dex").join("results").join("5.md");
        sql(&e, "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,project_id,project_root,merge_branch,result_path,created_at,updated_at)
                VALUES(5,'timur','bora','default','A','done',?1,?2,?3,?4,1,1)",
            &[&e.p2.id, &e.p2.root_path, &e.p2.default_branch, &done.to_string_lossy().into_owned()]);
        job(&e, 6, "kaan", &e.p1, "running");
        sql(&e, "INSERT INTO plans(id,team_id,leader,title,status,created_at,updated_at) VALUES(1,'default','timur','p','active',1,1)", &[]);
        sql(&e, "INSERT INTO plan_items(plan_id,key,title,prompt,target,depends_on,status,delegation_id,project_id,project_root) VALUES(1,'A','A','A','bora','[]','done',5,?1,?2)",
            &[&e.p2.id, &e.p2.root_path]);
        sql(&e, "INSERT INTO plan_items(plan_id,key,title,prompt,target,depends_on,status,delegation_id,project_id,project_root) VALUES(1,'B','B','B','kaan','[\"A\"]','running',6,?1,?2)",
            &[&e.p1.id, &e.p1.root_path]);
        bind(&e, "kaan", &e.p1.id, 6);
        let c = ctx(&e, "kaan").unwrap();
        assert!(read(&c, &done) && !write(&c, &done));
        assert!(!read(&c, &p2.join(".dex").join("results").join("other.md")) && !read(&c, &p2.join("src").join("a.rs")));
    }

    /// An old single-project team (cwd, default project and both memory anchors on one repo): a
    /// bound job is placed as an unbound session was — but for the approved narrowing of memory
    /// writes (another agent's private folder); its own old id folder stays writable.
    #[test]
    fn an_old_single_project_teams_bound_job_keeps_its_placement() {
        let root = std::env::temp_dir().join(format!("dex-gbind-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_s = repo.to_string_lossy().into_owned();
        let mem = repo.join(".dex").join("memory");
        let db = |q: &str, p: &[&dyn rusqlite::ToSql]| with(&core.db, |c| c.execute(q, p).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        db("UPDATE teams SET cwd = ?1, team_memory_root = ?2, agent_memory_root = ?2 WHERE id = 'default'", &[&repo_s, &mem.to_string_lossy().into_owned()]);
        db("UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", &[&repo_s, &projects::path_key(&repo_s)]);
        db("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,project_id,project_root,created_at,updated_at)
            VALUES(1,'timur','kaan','default','iş','running','legacy-default',?1,1,1)", &[&repo_s]);
        let ctx = || with(&core.db, |c| Ok(core.permit_ctx(c, "kaan", &serde_json::json!({}))?.unwrap().0)).unwrap();
        let unbind = || db("DELETE FROM agent_sessions WHERE agent_id = 'kaan'", &[]);
        let bind = || db("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES('kaan','','claude',1,'legacy-default',1)", &[]);
        let outside = root.join("elsewhere").join("a.rs");
        let samples = [repo.join("src").join("a.rs"), mem.join("shared").join("x.md"), mem.join("agents").join("kaan").join("x.md"), mem.join("agents").join("ayla").join("x.md"), repo.join(".dex").join("results").join("1.md"), outside.clone()];
        let decide = |c: &permit::Ctx| samples.iter().map(|p| (write(c, p), read(c, p))).collect::<Vec<_>>();

        // No worktree: the repo, as before.
        let old = ctx();
        bind();
        let new = ctx();
        assert_eq!(decide(&new), decide(&old));
        assert_eq!((new.cwd.clone(), new.team_root.clone()), (old.cwd.clone(), old.team_root.clone()));

        // Its worktree: the same, but another agent's private memory folder is no longer writable.
        let wt = root.join("wt-kaan-1");
        std::fs::create_dir_all(&wt).unwrap();
        db("INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(1,'kaan',?1,?2,'dex/kaan/1','base',1)",
            &[&repo_s, &wt.to_string_lossy().into_owned()]);
        let new = ctx();
        unbind();
        let old = ctx();
        let (o, n) = (decide(&old), decide(&new));
        for (i, p) in samples.iter().enumerate() {
            if p.starts_with(mem.join("agents").join("ayla")) {
                assert_eq!((o[i].0, n[i].0, n[i].1), (true, false, true), "narrowed write, read kept");
            } else {
                assert_eq!(n[i], o[i], "{}", p.display());
            }
        }
        assert!(write(&new, &wt.join("a.rs")) && new.cwd.as_deref() == Some(wt.as_path()));

        // Slug move pending: its old `agents/<id>` folder is its own.
        db("UPDATE agents SET slug = 'kaan-yeni' WHERE id = 'kaan'", &[]);
        db("DELETE FROM settings WHERE key = 'memory_slug_migrated'", &[]);
        std::fs::create_dir_all(mem.join("agents").join("kaan")).unwrap();
        bind();
        let c = ctx();
        assert!(write(&c, &mem.join("agents").join("kaan").join("x.md")) && !write(&c, &mem.join("agents").join("kaan-yeni").join("x.md")));
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The Dex MCP endpoint as the session's CLI calls it, with its spawn key.
    fn rpc(core: &Core, agent: &str, key: &str, method: &str, params: Value) -> Value {
        let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            core.hooks.token,
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        serde_json::from_str(out.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    #[test]
    fn a_stale_paste_restore_keeps_the_newer_job_and_key() {
        let e = env("paste-restore");
        for id in 1..=3 { job(&e, id, "kaan", &e.p1, "queued"); }
        bind(&e, "kaan", &e.p1.id, 1);
        let key = e.core.mcp.key_for_job("kaan", Some(1));
        let d = |id| with(&e.core.db, |c| delegate::get(c, id)).unwrap();
        let prior = e.core.session_take_job(&d(2)).unwrap().unwrap();
        let newer = e.core.session_take_job(&d(3)).unwrap().unwrap();
        e.core.session_return_job(&d(2), prior);
        assert_eq!(with(&e.core.db, |c| launch_job(c, "kaan")).unwrap(), Some(3));
        assert_eq!(rpc(&e.core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))
            ["result"]["structuredContent"]["delegationId"].as_i64(), Some(3));
        e.core.session_return_job(&d(3), newer);
        assert_eq!(with(&e.core.db, |c| launch_job(c, "kaan")).unwrap(), Some(2));
    }

    #[test]
    fn a_legacy_session_rebinds_using_its_old_jobs_project() {
        let e = env("paste-legacy");
        job(&e, 1, "kaan", &e.p1, "done");
        job(&e, 2, "kaan", &e.p1, "queued");
        job(&e, 3, "kaan", &e.p2, "queued");
        bind(&e, "kaan", &e.p1.id, 1);
        // Reproduce a historical row, then restore today's constraint before exercising rebind.
        with(&e.core.db, |c| {
            let trigger: String = c.query_row("SELECT sql FROM sqlite_master WHERE name='agent_sessions_project_update'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            c.execute_batch("DROP TRIGGER agent_sessions_project_update; UPDATE agent_sessions SET project_id=NULL WHERE agent_id='kaan';").map_err(|e| e.to_string())?;
            c.execute_batch(&trigger).map_err(|e| e.to_string())
        }).unwrap();
        let key = e.core.mcp.key_for_job("kaan", Some(1));
        let d = |id| with(&e.core.db, |c| delegate::get(c, id)).unwrap();
        assert!(e.core.session_fits(&d(2)));
        assert_eq!(e.core.session_take_job(&d(2)).unwrap(), Some((1, 1)));
        assert_eq!(e.core.session_take_job(&d(3)).unwrap(), None);
        assert_eq!(with(&e.core.db, |c| launch_job(c, "kaan")).unwrap(), Some(2));
        assert_eq!(rpc(&e.core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))
            ["result"]["structuredContent"]["delegationId"].as_i64(), Some(2));
        let project: Option<String> = with(&e.core.db, |c| c.query_row("SELECT project_id FROM agent_sessions WHERE agent_id='kaan'", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap();
        assert_eq!(project.as_deref(), Some(e.p1.id.as_str()), "the rebind satisfies today's job/project constraint");
    }

    #[test]
    fn paste_restore_serializes_with_a_concurrent_take_before_the_key_cas() {
        let e = env("paste-reverse-race");
        for id in 1..=3 { job(&e, id, "kaan", &e.p1, "queued"); }
        bind(&e, "kaan", &e.p1.id, 1);
        let key = e.core.mcp.key_for_job("kaan", Some(1));
        let d = |id| with(&e.core.db, |c| delegate::get(c, id)).unwrap();
        let prior = e.core.session_take_job(&d(2)).unwrap().unwrap();
        let advanced = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = advanced.clone();
        let core = e.core.clone();
        mcp::BEFORE_REBIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            // B's DB half can interleave only if A released the DB before its MCP half.
            if let Ok(c) = core.db.0.try_lock() {
                assert_eq!(agents::rebind_session_job(&c, "kaan", 3).unwrap(), Some(2));
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        })));
        e.core.session_return_job(&d(2), prior);
        if advanced.load(std::sync::atomic::Ordering::SeqCst) {
            // A restored the key to J1; B's CAS from J2 fails and returns its row to J2.
            assert_eq!(e.core.mcp.rebind_job("kaan", 2, 3), None);
            with(&e.core.db, |c| agents::restore_session_job(c, "kaan", 3, 2)).unwrap();
        } else {
            // With one DB critical section, B runs wholly after A and takes J3 from J1.
            assert_eq!(e.core.session_take_job(&d(3)).unwrap(), Some((1, 1)));
        }
        assert_eq!(with(&e.core.db, |c| launch_job(c, "kaan")).unwrap(), Some(3));
        assert_eq!(rpc(&e.core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))
            ["result"]["structuredContent"]["delegationId"].as_i64(), Some(3));
    }

    /// #906: a same-project job pasted into the patron's conversation becomes the session's job —
    /// the row and the MCP key together (its project read and GPU tools follow); a job of another
    /// project waits and changes neither. Unbound rows/keys, other agents' or projects' jobs: no move.
    #[test]
    fn a_same_project_paste_moves_the_sessions_job_and_key_together() {
        let e = env("paste");
        let core = &e.core;
        core.pty_spawn(pty::Spawn { id: "kaan".into(), cmd: shell::SHELL_CMD.into(), cwd: e.p1.root_path.clone(), cols: 80, rows: 24, ..Default::default() }, agents::shell_sink_for_test(core, "kaan", Sink)).unwrap();
        core.chain_guard.lock().unwrap().insert("kaan".into());
        job(&e, 1, "kaan", &e.p1, "done");
        sql(&e, "INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES('kaan','s1','claude',1,?1,1)", &[&e.p1.id]);
        let key = core.mcp.key_for_job("kaan", Some(1));
        job(&e, 2, "kaan", &e.p1, "queued");
        job(&e, 3, "kaan", &e.p2, "queued");
        // A GPU grant for job 2.
        sql(&e, "INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,status,created_at,project_id) VALUES(1,'default','timur','kaan','[\"t2i\"]','x',2,'approved',1,?1)", &[&e.p1.id]);
        sql(&e, "INSERT INTO gpu_grants(request_id,team_id,worker,types,budget,delegation_id,created_at,project_id) VALUES(1,'default','kaan','[\"t2i\"]',2,2,1,?1)", &[&e.p1.id]);
        let job_of = || rpc(core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))["result"]["structuredContent"]["delegationId"].as_i64();
        let gpu = || rpc(core, "kaan", &key, "tools/list", serde_json::json!({}))["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "gpu_recipes");
        let row = || with(&core.db, |c| launch_job(c, "kaan")).unwrap();
        assert_eq!((job_of(), gpu(), row()), (Some(1), false, Some(1)));

        // The patron talks in kaan's session (idle, their turn last); Dex delivers job 2 (P1) into it.
        let patron = || {
            core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron soruyor" }), false);
            core.agent_states.hook("kaan", Some(agents::Status::Idle), None);
            std::thread::sleep(std::time::Duration::from_millis(agents::RESET_QUIET_MS + 100));
        };
        patron();
        let d = |id| with(&core.db, |c| delegate::get(c, id)).unwrap();
        assert!(!core.session_fits(&d(3)) && core.session_fits(&d(2)));
        assert_eq!(core.delegation_dispatch(d(2)).unwrap().status, "running");
        assert!(core.ptys.is_running("kaan"), "the patron's conversation is kept");
        assert_eq!((job_of(), gpu(), row()), (Some(2), true, Some(2)), "row and key moved together; GPU follows job 2");

        // Job 2 done; the P2 job waits for the session (no retry/poll) and moves neither row nor key.
        sql(&e, "UPDATE delegations SET status = 'done' WHERE id = 2", &[]);
        assert!(!gpu(), "the finished job's GPU access ends");
        patron();
        assert_eq!(core.delegation_dispatch(d(3)).unwrap().status, "queued");
        assert!(core.ptys.is_running("kaan") && !core.queue_retry.lock().unwrap().contains_key("kaan"));
        assert_eq!((job_of(), row()), (Some(2), Some(2)));

        // No move for another project's or another agent's job, nor for an unbound row or key.
        assert_eq!(core.session_take_job(&d(3)).unwrap(), None);
        job(&e, 7, "ayla", &e.p1, "queued");
        assert_eq!(with(&core.db, |c| agents::rebind_session_job(c, "kaan", 7)).unwrap(), None);
        assert_eq!((job_of(), row()), (Some(2), Some(2)));
        job(&e, 8, "kaan", &e.p1, "queued");
        let unbound = core.mcp.key_for("kaan");
        assert_eq!(core.session_take_job(&d(8)).unwrap(), None, "an unbound key: the row goes back");
        assert_eq!(row(), Some(2));
        assert!(rpc(core, "kaan", &unbound, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))["result"]["isError"] == true);
        sql(&e, "UPDATE agent_sessions SET project_id = NULL, delegation_id = NULL WHERE agent_id = 'kaan'", &[]);
        let key = core.mcp.key_for_job("kaan", Some(2));
        assert_eq!(core.session_take_job(&d(8)).unwrap(), None, "an unbound row stays unbound");
        assert_eq!(rpc(core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))["result"]["structuredContent"]["delegationId"].as_i64(), Some(2));

        // A failed paste puts both back.
        sql(&e, "UPDATE agent_sessions SET project_id = ?1, delegation_id = 2 WHERE agent_id = 'kaan'", &[&e.p1.id]);
        let prior = core.session_take_job(&d(8)).unwrap().unwrap();
        assert_eq!(prior, (2, 2));
        core.session_return_job(&d(8), prior);
        assert_eq!(row(), Some(2));
        assert_eq!(rpc(core, "kaan", &key, "tools/call", serde_json::json!({ "name": "project_context", "arguments": {} }))["result"]["structuredContent"]["delegationId"].as_i64(), Some(2));
    }
}
