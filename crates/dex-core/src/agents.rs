use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Offline,
    Idle,
    Working,
    Waiting,
}

impl Status {
    pub fn from_hook(value: &str) -> Option<Self> {
        match value {
            "offline" => Some(Self::Offline),
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "waiting" => Some(Self::Waiting),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub agent_id: String,
    pub status: Status,
    pub session_id: Option<String>,
    pub engine: Option<String>,
    /// S5-B: SessionStart/UserPromptSubmit hook'undaki `transcript_path` — sohbet bu dosyayı okur.
    #[serde(skip)]
    pub transcript: Option<String>,
    #[serde(skip)]
    input_at: u64,
    #[serde(skip)]
    draft: bool,
    /// FIX-1: spawned, no hook heard yet — a dialog shown now (folder trust) blocks with no hook.
    #[serde(skip)]
    pre_session: bool,
    /// #447: started with a prompt whose turn has not shown yet (its first hook past SessionStart,
    /// or the exit, ends it) — a restart waits for it as for a Working agent.
    #[serde(skip)]
    prompt_pending: bool,
    /// Görev #20: when the last hook of this agent came (ms) — the quiet window before a reset.
    #[serde(skip)]
    hook_at: u64,
    /// #125: [`next_seq`] of the change that set `status` — an older hook applied late loses.
    #[serde(skip)]
    status_seq: u64,
    /// Reconciled team completion is evaluated once until this agent works again.
    #[serde(skip)]
    team_turn_checked: bool,
    /// Interrupt remains ineligible even if delayed tool hooks mark this turn busy again.
    #[serde(skip)]
    team_turn_interrupted: bool,
    /// Rollout fallback completion must belong to this PTY generation and active turn.
    #[serde(skip)]
    codex_turn: Option<(u64, String)>,
    /// A settled turn's delayed rollout rows cannot cross this boundary into the next turn.
    #[serde(skip)]
    codex_after: u64,
    /// #1033: Dex parked it for quota (zincir bekletmesi) — any later status ends the wait.
    #[serde(skip)]
    pub(crate) chain_wait: bool,
}

/// #125: arrival order of status changes. Hooks take theirs when the connection is accepted
/// (`hooks::accept`), before any lock: each runs on its own thread, and one that waits longer (a
/// PostToolUse behind the db lock) must not turn the Stop that came after it back into "working".
pub fn next_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl State {
    fn set_status(&mut self, status: Status) {
        if matches!(status, Status::Working | Status::Waiting) {
            self.team_turn_checked = self.team_turn_interrupted;
        }
        if !matches!(status, Status::Working | Status::Waiting) {
            if matches!(self.status, Status::Working | Status::Waiting) {
                self.codex_after = now();
            }
            self.codex_turn = None;
        }
        self.status = status;
        self.status_seq = next_seq();
        self.chain_wait = false;
    }

    /// #447: mid-turn, or about to be (a restart's blocker).
    pub(crate) fn in_turn(&self) -> bool {
        matches!(self.status, Status::Working | Status::Waiting) || self.prompt_pending
    }
}

#[derive(Clone, Default)]
pub struct States(Arc<Mutex<HashMap<String, State>>>);

impl States {
    pub fn get(&self, id: &str) -> State {
        self.0.lock().unwrap().get(id).cloned().unwrap_or_else(|| State { agent_id: id.into(), ..Default::default() })
    }

    pub fn list(&self) -> Vec<State> {
        let mut states: Vec<_> = self.0.lock().unwrap().values().cloned().collect();
        states.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        states
    }

    /// `prompted`: the CLI was given a prompt — its turn is pending until it shows (#447).
    pub fn started(&self, id: &str, engine: &str, prompted: bool) -> State {
        self.change(id, |state| {
            state.set_status(Status::Idle);
            state.codex_after = 0; // A new process has its own rollout boundary.
            state.team_turn_interrupted = false;
            state.engine = Some(engine.into());
            state.prompt_pending = prompted;
        })
    }

    /// #447: a hook past SessionStart — a prompt the start was given is the turn now (or over).
    pub fn prompt_seen(&self, id: &str) {
        self.change(id, |state| state.prompt_pending = false);
    }

    /// A claude process started; until its first hook, [`States::blocked`] may flag it waiting.
    pub fn spawned(&self, id: &str) {
        self.change(id, |state| state.pre_session = true);
    }

    pub fn pre_session(&self, id: &str) -> bool {
        self.get(id).pre_session
    }

    /// FIX-1: the pre-session screen settled — the process waits on the patron (trust dialog).
    /// `Some` only on the change, so the caller emits (and notifies) once.
    pub fn blocked(&self, id: &str) -> Option<State> {
        let mut states = self.0.lock().unwrap();
        let state = states.get_mut(id).filter(|s| s.pre_session && s.status != Status::Waiting)?;
        state.set_status(Status::Waiting);
        Some(state.clone())
    }

    /// #1033: parked for quota — waiting, and marked so the motor's reset continues it.
    pub fn chain_wait(&self, id: &str) -> State {
        self.change(id, |state| {
            state.pre_session = false;
            state.set_status(Status::Waiting);
            state.chain_wait = true;
        })
    }

    /// A status set now (paste, interrupt, Dex's own decision): newer than every hook already accepted.
    pub fn hook(&self, id: &str, status: Option<Status>, session_id: Option<&str>) -> State {
        self.hook_seq(id, status, session_id, next_seq())
    }

    /// #125: a hook's status applies only if no newer change (`seq`, see [`next_seq`]) set it already.
    pub fn hook_seq(&self, id: &str, status: Option<Status>, session_id: Option<&str>, seq: u64) -> State {
        self.hook_turn_seq(id, status, session_id, seq, false)
    }

    pub(crate) fn prompt_seq(&self, id: &str, session_id: Option<&str>, seq: u64) -> State {
        self.hook_turn_seq(id, Some(Status::Working), session_id, seq, true)
    }

    fn hook_turn_seq(&self, id: &str, status: Option<Status>, session_id: Option<&str>, seq: u64, new_turn: bool) -> State {
        self.change(id, |state| {
            state.pre_session = false;
            state.hook_at = now();
            if let Some(status) = status.filter(|_| seq > state.status_seq) {
                if new_turn {
                    state.team_turn_interrupted = false;
                }
                if matches!(status, Status::Working | Status::Waiting) {
                    state.team_turn_checked = state.team_turn_interrupted;
                }
                if !matches!(status, Status::Working | Status::Waiting) {
                    // Idle with an initial prompt pending is SessionStart, not that turn's end.
                    if !state.prompt_pending || matches!(state.status, Status::Working | Status::Waiting) {
                        state.codex_after = now();
                    }
                    state.codex_turn = None;
                }
                state.status = status;
                state.status_seq = seq;
                state.chain_wait = false;
            }
            if let Some(session_id) = session_id {
                state.session_id = Some(session_id.into());
            }
        })
    }

    pub(crate) fn interrupt_team_turn(&self, id: &str) {
        self.change(id, |state| {
            state.team_turn_interrupted = true;
            state.team_turn_checked = true;
        });
    }

    /// Consume a reconciled end only while its exact Idle transition is still current.
    pub(crate) fn claim_team_turn_end(&self, ended: &State) -> bool {
        let mut states = self.0.lock().unwrap();
        let Some(state) = states.get_mut(&ended.agent_id) else { return false };
        if state.status != Status::Idle || state.status_seq != ended.status_seq
            || state.team_turn_checked
        {
            return false;
        }
        state.team_turn_checked = true;
        true
    }

    /// Gate both start and prompt under the status lock. Use the preceding end rather than
    /// the Working hook clock: Codex writes the new start before UserPromptSubmit reaches us.
    pub(crate) fn codex_observed(&self, id: &str, process: u64, turn: Option<&str>, timestamp: Option<u64>) -> Option<State> {
        let mut states = self.0.lock().unwrap();
        let state = states.entry(id.into()).or_insert_with(|| State { agent_id: id.into(), ..Default::default() });
        if state.codex_after != 0 && timestamp.is_none_or(|ts| ts < state.codex_after) {
            return None;
        }
        state.pre_session = false;
        state.hook_at = now();
        state.team_turn_interrupted = false;
        state.set_status(Status::Working);
        state.prompt_pending = false;
        if let Some(turn) = turn {
            state.codex_turn = Some((process, turn.into()));
        }
        Some(state.clone())
    }

    /// Check and end under the same lock: a Stop/new prompt between them cannot end a newer turn.
    pub(crate) fn codex_completed(&self, id: &str, process: u64, turn: &str, timestamp: Option<u64>) -> Option<State> {
        let mut states = self.0.lock().unwrap();
        let state = states.get_mut(id)?;
        if !matches!(state.status, Status::Working | Status::Waiting)
            || !state.codex_turn.as_ref().is_some_and(|(p, t)| *p == process && t == turn)
        {
            return None;
        }
        state.pre_session = false;
        state.hook_at = now();
        // A single read may also contain the next turn, written before this read's clock.
        // Preserve a newer hook boundary; missing rollout clocks retain the conservative gate.
        let after = state.codex_after.max(timestamp.unwrap_or_else(now));
        state.set_status(Status::Idle);
        state.codex_after = after;
        Some(state.clone())
    }

    /// `true` when this ends a draft (the patron sent or emptied the prompt).
    pub fn input(&self, id: &str, draft: bool) -> bool {
        let mut ended = false;
        self.change(id, |state| {
            ended = state.draft && !draft;
            state.input_at = now();
            state.draft = draft;
        });
        ended
    }

    /// S5-B: latest known transcript file of the agent's session (hook's `transcript_path`).
    pub fn set_transcript(&self, id: &str, path: &str) {
        self.change(id, |state| {
            state.transcript = Some(path.into());
        });
    }

    /// Görev #20: no hook for [`RESET_QUIET_MS`] — the turn's last writes (report, memory) are done.
    pub fn quiet(&self, id: &str) -> bool {
        self.silent_for(id) >= RESET_QUIET_MS
    }

    /// ms since the agent's last hook.
    pub fn silent_for(&self, id: &str) -> u64 {
        now().saturating_sub(self.get(id).hook_at)
    }

    pub fn typing(&self, id: &str) -> bool {
        let state = self.get(id);
        state.draft || now().saturating_sub(state.input_at) < 5_000
    }

    pub fn exited(&self, id: &str) -> State {
        self.change(id, |state| {
            state.set_status(Status::Offline);
            state.draft = false;
            state.pre_session = false;
            state.prompt_pending = false;
        })
    }

    fn change(&self, id: &str, f: impl FnOnce(&mut State)) -> State {
        let mut states = self.0.lock().unwrap();
        let state = states.entry(id.into()).or_insert_with(|| State { agent_id: id.into(), ..Default::default() });
        f(state);
        state.clone()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Start,
    Paste,
    Queue,
}

/// Görev #20: silence after a worker's last hook before its session may be replaced (fixed).
pub const RESET_QUIET_MS: u64 = 2_500;

/// #125 watchdog: a "working" claude worker this long without a hook, whose transcript says the turn
/// ended, is idle — its queued job goes in. Above any think-pause between a turn's hooks, and well
/// under the patron noticing a stuck queue.
pub const STALE_WORKING_MS: u64 = 30_000;

/// #45: a reset whose PTY exit has not come after this long is stuck; its mark is dropped and the
/// queued job tried again (the exit normally comes within a second).
pub const RESET_STUCK_MS: u64 = 15_000;

/// #126: a worker this long without a hook, with no job of its own and Dex's last turn, has its
/// session closed (fixed). Each job opens a fresh session anyway (#20); the cost is a cold start.
pub const IDLE_CLOSE_MS: u64 = 60 * 1000;

/// Görev #20: options of a job's start — always a fresh session (no `--resume`, not even the
/// saved one): the job and its continuity come from the trigger, the memory and the report files.
pub fn job_start(prompt: String, add_dirs: Vec<String>) -> StartOptions {
    StartOptions { prompt: Some(prompt), add_dirs, fresh: true, ..Default::default() }
}

pub fn delivery(status: Status, typing: bool, has_work: bool) -> Delivery {
    if has_work || typing {
        return Delivery::Queue;
    }
    match status {
        Status::Offline => Delivery::Start,
        Status::Idle => Delivery::Paste,
        Status::Working | Status::Waiting => Delivery::Queue,
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    pub prompt: Option<String>,
    #[serde(default)]
    pub add_dirs: Vec<String>,
    pub resume: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    /// FIX-4: start without any resume (not even the saved one) — the one retry after a lost session.
    /// Görev #32: also the patron's "temiz oturum" for a leader (`options.fresh` over IPC).
    #[serde(default)]
    pub fresh: bool,
    /// DEX-17: provider resume açılamazsa yeni oturum bu transcript devir özetiyle başlar.
    #[serde(skip)]
    pub fallback_prompt: Option<String>,
    /// Görev #144: the job's worktree instead of the team folder, and what its session adds to
    /// the environment. Dex's own, never from IPC.
    #[serde(skip)]
    pub cwd: Option<String>,
    #[serde(skip)]
    pub env: Vec<(String, String)>,
    /// #671: the job's effort (none: the CLI's default, as before). Dex's own, never from IPC;
    /// left none, a worker's respawn mid-job takes its running job's (`agent_spawn`).
    #[serde(skip)]
    pub effort: Option<crate::effort::Effort>,
    #[serde(skip)]
    pub model_route_reason: Option<String>,
    /// #838 D: the job this start launches (the dispatch's own `d.id`) — the session, its MCP key
    /// and its folder are bound to that job's project. Dex's own, never from IPC or env.
    #[serde(skip)]
    pub delegation_id: Option<i64>,
    /// #838 D: a respawn of the same session (chain, chain back, lost resume) keeps its binding.
    /// Dex's own, never from IPC.
    #[serde(skip)]
    pub launch: Option<Binding>,
}

/// #838 D: what a session is launched for — project, job and folder. Written to `agent_sessions`
/// before the CLI starts, so its first hook (SessionStart) and its MCP key see it already; until
/// the CLI names its session the row's `session_id` is empty ([`NO_SESSION`]). All none: an
/// unbound (pre-project or team) session — the resolver's legacy context, as before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    pub project_id: Option<String>,
    pub delegation_id: Option<i64>,
    pub cwd: Option<String>,
}

/// #838 D: `agent_sessions.session_id` of a launch whose CLI has not named its session yet.
const NO_SESSION: &str = "";

/// #838 D: where a start runs and what it resumes ([`launch_plan`]).
struct LaunchPlan {
    binding: Binding,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    resume: Option<String>,
    /// The saved session this start abandons (session, engine).
    stale: Option<(String, String)>,
}

/// #838 D (tests): a captured agent start — its spawn request and launch job.
#[cfg(test)]
pub(crate) type Captured = (crate::pty::Spawn, Option<i64>);

#[cfg(test)]
thread_local! {
    /// #838 D (tests): spawn requests of this thread's agent starts, kept instead of run.
    pub(crate) static CAPTURE: std::cell::RefCell<Option<Vec<Captured>>> = const { std::cell::RefCell::new(None) };
    /// #838 G (tests): the captured starts' launch contexts (memory, system text), in order.
    pub(crate) static LAUNCHES: std::cell::RefCell<Vec<crate::pty::AgentLaunch>> = const { std::cell::RefCell::new(Vec::new()) };
    /// #1018 (tests): Codex rollout klasörü; boşsa Codex kotası bilinmiyor (gerçek ~/.codex okunmaz).
    pub(crate) static CODEX_SESSIONS: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Codex'in rollout klasörü: worker'lar Dex'in kendi `CODEX_HOME`'unu (yoksa ~/.codex) görür.
fn codex_sessions() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    return CODEX_SESSIONS.with(|s| s.borrow().clone());
    #[cfg(not(test))]
    Some(crate::codex::home().join("sessions"))
}

/// #838 D: the agent's `agent_sessions` row, launch binding included.
#[derive(Clone, Debug)]
struct SessionRow {
    session: String,
    engine: String,
    transcript: Option<String>,
    updated_at: i64,
    binding: Binding,
}

fn session_row(conn: &Connection, agent: &str) -> Result<Option<SessionRow>, String> {
    conn.query_row(
        "SELECT session_id, engine, transcript, updated_at, project_id, delegation_id, cwd FROM agent_sessions WHERE agent_id = ?1",
        [agent],
        |r| {
            Ok(SessionRow {
                session: r.get(0)?,
                engine: r.get(1)?,
                transcript: r.get(2)?,
                updated_at: r.get(3)?,
                binding: Binding { project_id: r.get(4)?, delegation_id: r.get(5)?, cwd: r.get(6)? },
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// #838 D: the start's binding, folder, env and resume. A job start (`options.delegation_id`)
/// binds to that job; a respawn (`options.launch`) keeps its own; any other start continues the
/// saved session's binding. A new leader session takes the team's default project with a root;
/// folderless legacy sessions and new workers without a job remain unbound. Nothing comes from the queue.
fn launch_plan(
    conn: &Connection,
    data_dir: &std::path::Path,
    agent: &crate::teams::Agent,
    team: &crate::teams::Team,
    engine: &str,
    options: &StartOptions,
    renew: bool,
) -> Result<LaunchPlan, String> {
    let saved = session_row(conn, &agent.id)?;
    let job = options.delegation_id.map(|id| crate::delegate::get(conn, id)).transpose()?;
    if job.as_ref().is_some_and(|d| d.to_agent != agent.id) {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    let mut binding = match (&job, &options.launch) {
        (Some(d), _) => Binding { project_id: d.project_id.clone(), delegation_id: Some(d.id), cwd: None },
        (None, Some(b)) => b.clone(),
        (None, None) if !renew && saved.is_some() => saved.as_ref().unwrap().binding.clone(),
        (None, None) if agent.kind == "leader" => {
            let ctx = crate::project_context::resolve(conn, data_dir, crate::project_context::ResolveRequest {
                team_id: &team.id, agent_id: Some(&agent.id), delegation_id: None,
                project_id: team.default_project_id.as_deref(), mode: crate::project_context::ContextMode::Team,
            }).map_err(|e| e.to_string())?;
            Binding { project_id: ctx.project_id.filter(|_| ctx.project_root.is_some()), ..Default::default() }
        }
        (None, None) => Binding::default(),
    };
    let bound = binding.project_id.is_some() || binding.delegation_id.is_some();
    // The folder stays in the binding's project: its job's worktree, else its root. A recorded
    // folder that is gone (a merged worktree) falls back inside the project, never to another.
    let wt = binding.delegation_id.map(|id| crate::worktree::get(conn, id)).transpose()?.flatten().filter(|w| w.removed_at.is_none());
    let root = if team.kind == "service" {
        None
    } else if let Some(id) = binding.delegation_id {
        job.clone().map_or_else(|| crate::delegate::get(conn, id), Ok)?.project_root
    } else if let Some(p) = &binding.project_id {
        crate::projects::get(conn, &team.id, p).map_err(|e| e.to_string())?.root_path
    } else {
        None
    };
    let is_dir = |p: &String| std::path::Path::new(p).is_dir();
    let cwd = options
        .cwd
        .clone()
        .or_else(|| binding.cwd.clone().filter(is_dir))
        .or_else(|| wt.as_ref().map(|w| w.path.to_string_lossy().into_owned()).filter(is_dir))
        .or(root)
        .or_else(|| team.workdir.clone());
    if bound {
        binding.cwd = cwd.clone();
    }
    let env = match wt.filter(|w| options.env.is_empty() && cwd.as_deref().is_some_and(|c| std::path::Path::new(c) == w.path)) {
        Some(w) => w.env(),
        None => options.env.clone(),
    };
    let saved_resumable = saved.as_ref().filter(|s| s.engine == engine && s.session != NO_SESSION);
    let mut resume = if options.fresh { None } else { options.resume.clone().or_else(|| saved_resumable.map(|s| s.session.clone())) };
    // Never the saved session of another project: a caller-bound start opens a fresh one.
    let mut dropped = false;
    if let (Some(s), None, true) = (saved_resumable, &options.resume, job.is_some() || options.launch.is_some()) {
        if resume.is_some() && s.binding != binding && !binding_fits(conn, data_dir, &agent.id, &agent.team_id, &s.binding, binding.project_id.as_deref())? {
            resume = None;
            dropped = true;
        }
    }
    let stale = saved
        .filter(|s| resume.is_none() && (options.fresh || dropped) && s.session != NO_SESSION)
        .map(|s| (s.session, s.engine));
    Ok(LaunchPlan { binding, cwd, env, resume, stale })
}

/// #838 D: whether a session bound by `b` may work in `project` — a session of another project
/// never. An unbound session (pre-project, team) keeps the old single-project behavior; with
/// several projects the resolver's legacy rule for saved sessions names its project.
fn binding_fits(conn: &Connection, data_dir: &std::path::Path, agent: &str, team: &str, b: &Binding, project: Option<&str>) -> Result<bool, String> {
    let Some(project) = project else { return Ok(true) };
    let job_project = match b.delegation_id {
        Some(id) => conn
            .query_row("SELECT project_id FROM delegations WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten(),
        None => None,
    };
    if let Some(own) = b.project_id.clone().or(job_project) {
        return Ok(own == project);
    }
    let active = crate::projects::list(conn, team, false).map_err(|e| e.to_string())?;
    if active.iter().all(|p| p.id == project) {
        return Ok(true);
    }
    let request = crate::project_context::ResolveRequest {
        team_id: team,
        agent_id: Some(agent),
        delegation_id: None,
        project_id: None,
        mode: crate::project_context::ContextMode::Session,
    };
    Ok(crate::project_context::resolve(conn, data_dir, request).ok().and_then(|c| c.project_id).as_deref() == Some(project))
}

/// #838 D: records the launch binding before the spawn; `drop_session` also abandons the saved
/// session (an empty id until the CLI names its own). An unbound launch keeps the old rows: none
/// until the CLI names its session. The prior row, for [`restore_session_row`].
fn bind_launch(conn: &Connection, agent: &str, engine: &str, b: &Binding, drop_session: bool) -> Result<Option<SessionRow>, String> {
    let prior = session_row(conn, agent)?;
    let unbound = *b == Binding::default();
    if unbound && (drop_session || prior.as_ref().is_none_or(|p| p.session == NO_SESSION)) {
        conn.execute("DELETE FROM agent_sessions WHERE agent_id = ?1", [agent])
    } else if prior.as_ref().is_some_and(|p| !drop_session && p.binding == *b) {
        Ok(0)
    } else if drop_session || prior.is_none() {
        conn.execute(
            "INSERT INTO agent_sessions (agent_id, session_id, engine, transcript, updated_at, project_id, delegation_id, cwd)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET session_id=excluded.session_id, engine=excluded.engine, transcript=NULL,
                 updated_at=excluded.updated_at, project_id=excluded.project_id, delegation_id=excluded.delegation_id, cwd=excluded.cwd",
            params![agent, NO_SESSION, engine, now() as i64, b.project_id, b.delegation_id, b.cwd],
        )
    } else {
        conn.execute(
            "UPDATE agent_sessions SET project_id = ?2, delegation_id = ?3, cwd = ?4 WHERE agent_id = ?1",
            params![agent, b.project_id, b.delegation_id, b.cwd],
        )
    }
    .map_err(|e| e.to_string())?;
    Ok(prior)
}

/// #838 G: what the start brings from the binding just recorded — the context its SessionStart
/// resolves too ([`crate::hooks::session_context`]): memory folders and system text. A bound
/// session takes its binding's context or does not start; an unbound one the legacy/team context.
/// Unbound without a project in a single-project team: the old automatic text (its stack) stays.
fn agent_launch(conn: &Connection, data_dir: &std::path::Path, agent: &crate::teams::Agent, b: &Binding) -> Result<crate::pty::AgentLaunch, String> {
    let bound = *b != Binding::default();
    let ctx = crate::hooks::session_context(conn, data_dir, &agent.id, bound).map_err(|e| e.to_string())?;
    let memory = crate::hooks::memory_scope(conn, &ctx)?;
    let single = || crate::projects::list(conn, &agent.team_id, false).map(|l| l.iter().filter(|p| p.root_path.is_some()).count() <= 1);
    let prompt = if !bound && ctx.project_id.is_none() && single().map_err(|e| e.to_string())? {
        let shared = ctx.team_memory_root.join("shared");
        let own = crate::slug::memory_dir(conn, &ctx.agent_memory_root, &agent.id)?;
        crate::rules::system_prompt(conn, &agent.id, Some((&shared, &own)))
    } else {
        crate::rules::system_prompt_for_context(conn, &agent.id, &ctx)
    };
    Ok(crate::pty::AgentLaunch { job: b.delegation_id, memory, prompt })
}

/// #838 G (#906): a job of the session's own project pasted into its live bound session becomes
/// the session's job — what its permit, restart and MCP read go by from now on. An unbound
/// (pre-project) row, another agent's job or another project's: nothing changes. The prior job,
/// when it changed ([`restore_session_job`] puts it back).
pub(crate) fn rebind_session_job(conn: &Connection, agent: &str, job: i64) -> Result<Option<i64>, String> {
    let Some(prior) = session_row(conn, agent)?.and_then(|s| s.binding.delegation_id) else { return Ok(None) };
    let changed = conn
        .execute(
            "UPDATE agent_sessions SET delegation_id = ?2,
               project_id = COALESCE(project_id, (SELECT project_id FROM delegations WHERE id = ?3))
               WHERE agent_id = ?1 AND delegation_id = ?3
               AND EXISTS (SELECT 1 FROM delegations d WHERE d.id = ?2 AND d.to_agent = ?1
                 AND d.project_id IS COALESCE(agent_sessions.project_id, (SELECT project_id FROM delegations WHERE id = ?3)))",
            params![agent, job, prior],
        )
        .map_err(|e| e.to_string())?;
    Ok((changed == 1).then_some(prior))
}

pub(crate) fn restore_session_job(conn: &Connection, agent: &str, job: i64, prior: i64) -> Result<(), String> {
    conn.execute("UPDATE agent_sessions SET delegation_id = ?3 WHERE agent_id = ?1 AND delegation_id = ?2", params![agent, job, prior])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// #838 D: a failed spawn leaves the session row as it was before [`bind_launch`].
fn restore_session_row(conn: &Connection, agent: &str, prior: Option<&SessionRow>) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM agent_sessions WHERE agent_id = ?1", [agent]).map_err(|e| e.to_string())?;
    if let Some(p) = prior {
        tx.execute(
            "INSERT INTO agent_sessions (agent_id, session_id, engine, transcript, updated_at, project_id, delegation_id, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![agent, p.session, p.engine, p.transcript, p.updated_at, p.binding.project_id, p.binding.delegation_id, p.binding.cwd],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// #671: where the effort goes. Claude Code (anthropic, glm, muse): `CLAUDE_CODE_EFFORT_LEVEL`
/// in the spawn env, which pty sets after its strip and the provider env, so it wins over the
/// patron's own and over `--effort`. Codex: its argv (`codex::Launch::effort`). None: nothing.
/// #671: the effort a spawn uses — the given one, else (a worker's respawn mid-job: provider chain,
/// quota route back, restart, lost resume) its running job's. Leaders and job-less spawns: none.
pub fn spawn_effort(conn: &rusqlite::Connection, agent: &crate::teams::Agent, given: Option<crate::effort::Effort>) -> Result<Option<crate::effort::Effort>, String> {
    match given {
        Some(e) => Ok(Some(e)),
        None if agent.kind == "worker" => crate::delegate::running_effort(conn, &agent.id),
        None => Ok(None),
    }
}

pub fn effort_env(engine: &str, effort: Option<crate::effort::Effort>, env: &mut Vec<(String, String)>) {
    if let Some(e) = effort.filter(|_| engine != "codex") {
        env.retain(|(k, _)| k != crate::effort::CLAUDE_ENV);
        env.push((crate::effort::CLAUDE_ENV.into(), e.as_str().into()));
    }
}

fn leader_colors_env(
    skills: &crate::skills::Skills,
    engine: &str,
    kind: &str,
    inherited: Option<&str>,
    env: &mut Vec<(String, String)>,
) -> Result<(), String> {
    if kind != "leader" || !std::path::Path::new(engine).file_stem().is_some_and(|s| s.eq_ignore_ascii_case("claude")) {
        return Ok(());
    }
    const KEY: &str = "CLAUDE_CODE_PLUGIN_DIRS";
    let dir = skills.leader_colors_dir()?;
    let dir = dir.to_str().ok_or("lider renk modu yolu UTF-8 olmalı")?;
    let existing = env.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(KEY)).map(|(_, v)| v.as_str()).or(inherited).unwrap_or("");
    // Claude's platform path-list separator; keep the patron's value verbatim.
    let sep = if cfg!(windows) { ';' } else { ':' };
    let value = if existing.split(sep).any(|p| p == dir) {
        existing.to_string()
    } else if existing.is_empty() {
        dir.to_string()
    } else {
        format!("{existing}{sep}{dir}")
    };
    env.retain(|(k, _)| !k.eq_ignore_ascii_case(KEY));
    env.push((KEY.into(), value));
    Ok(())
}

/// FIX-4: what the CLI prints when `--resume <id>` names no conversation, then exits.
const RESUME_LOST: &[&str] = &["No conversation found", "No saved session found"];

/// FIX-4: a start that resumed a session, until its first hook proves the session opened.
pub(crate) struct ResumeWatch {
    session: String,
    engine: String,
    retry: StartOptions,
    lost: bool,
}

struct AgentSink {
    core: std::sync::Weak<crate::Core>,
    id: String,
    /// FIX-4: recent output while a resume is unconfirmed (the marker may span two reads).
    tail: String,
}

impl crate::PtySink for AgentSink {
    fn data(&mut self, bytes: &[u8]) -> bool {
        let text = String::from_utf8_lossy(bytes);
        if let Some(core) = self.core.upgrade() {
            core.resume_output(&self.id, &mut self.tail, &text);
        }
        let mut reply = String::new();
        for (query, answer) in [
            ("\x1b[6n", "\x1b[1;1R"),
            ("\x1b[c", "\x1b[?1;2c"),
            ("\x1b]10;?", "\x1b]10;rgb:c9c9/cdcd/d2d2\x1b\\"),
            ("\x1b]11;?", "\x1b]11;rgb:1313/1414/1616\x1b\\"),
        ] {
            reply.push_str(&answer.repeat(text.matches(query).count()));
        }
        if !reply.is_empty() {
            if let Some(core) = self.core.upgrade() {
                let _ = core.ptys.write(&self.id, reply.as_bytes());
            }
        }
        true
    }
    fn exit(&mut self) {}
}

impl crate::Core {
    /// Starts one backend-owned agent PTY. Concurrent calls are idempotent. #447: one with a prompt
    /// starts a turn — past the gate only ([`crate::restart::HELD`] while a drain holds it).
    pub fn agent_start(&self, id: &str, options: StartOptions) -> Result<crate::pty::PtyInfo, String> {
        let _turn = match options.prompt {
            Some(_) => Some(self.admit(id).ok_or(crate::restart::HELD)?),
            None => None,
        };
        self.agent_spawn(id, options)
    }

    /// [`Core::agent_start`] for a path that holds its turn already (a dispatch, a chat send) or
    /// goes on one already started (a lost resume, a provider change — `chain_guard` holds those).
    pub(crate) fn agent_spawn(&self, id: &str, options: StartOptions) -> Result<crate::pty::PtyInfo, String> {
        let _disk = crate::maintenance::DISK_GATE.read().map_err(|_| "maintenance gate poisoned")?;
        let _gate = self.agent_start_gate.lock().map_err(|_| "agent start kilidi zehirli")?;
        self.agent_spawn_locked(id, options)
    }

    pub(crate) fn agent_resume_sleeping(&self, id: &str) -> Result<(), String> {
        let _turn = self.admit(id).ok_or(crate::restart::HELD)?;
        let _disk = crate::maintenance::DISK_GATE.read().map_err(|_| "maintenance gate poisoned")?;
        let _gate = self.agent_start_gate.lock().map_err(|_| "agent start kilidi zehirli")?;
        if !self.sleeping_leaders.lock().unwrap().contains(id) || self.ptys.is_running(id) || !self.leader_has_events(id) {
            return Ok(());
        }
        self.agent_spawn_locked(id, StartOptions {
            prompt: Some("[Dex] Bekleyen soru, sonuç ve plan bildirimlerini değerlendir; kaldığın yerden devam et.".into()),
            ..Default::default()
        }).map(|_| ())
    }

    /// Caller owns `agent_start_gate`; manual start/stop and automatic wake serialize here.
    fn agent_spawn_locked(&self, id: &str, mut options: StartOptions) -> Result<crate::pty::PtyInfo, String> {
        if let Some(info) = self.ptys.list().into_iter().find(|pty| pty.id == id) {
            return Ok(info);
        }
        // #838 D: a caller's fresh start is a new session; a quota route to Codex below makes a
        // continuing one fresh too, yet it keeps its binding.
        let renew = options.fresh;
        self.quota_route(id, &mut options);
        let (agent, engine, plan) = crate::db::with(&self.db, |conn| {
            let mut agent = crate::teams::get_agent(conn, id)?;
            crate::job_model::apply(conn, &mut agent, options.delegation_id)?;
            let team = crate::teams::get_team(conn, &self.paths.data_dir, &agent.team_id)?;
            let engine = agent.active_engine.clone().unwrap_or_else(|| agent.engine.clone());
            options.effort = spawn_effort(conn, &agent, options.effort)?;
            let plan = launch_plan(conn, &self.paths.data_dir, &agent, &team, &engine, &options, renew)?;
            Ok((agent, engine, plan))
        })?;
        // The lost-resume retry below is a clone: it runs on the same binding, folder and env.
        options.launch = Some(plan.binding.clone());
        options.cwd = plan.cwd.clone();
        options.env = plan.env.clone();
        let mut retry = StartOptions { resume: None, fresh: true, ..options.clone() };
        if let Some(prompt) = retry.fallback_prompt.take() {
            retry.prompt = Some(prompt);
        }
        let resume = plan.resume.clone();
        if resume.is_none() {
            crate::db::with(&self.db, |c| crate::cost::reset_context(c, id))?;
            let _ = self.agent_context(id);
        }
        let watch = resume.clone().map(|session| ResumeWatch { session, engine: engine.clone(), retry, lost: false });
        // DEX-17: zincirle geçici motor/sağlayıcı/modeldeyse onlar kullanılır; asıl ayar değişmez.
        let eff_provider = agent.active_provider.clone().unwrap_or_else(|| agent.provider.clone());
        let eff_model = agent.active_model.clone().unwrap_or_else(|| agent.model.clone());
        let codex = (engine == "codex").then(|| crate::codex::Launch {
            model: Some(eff_model.clone()),
            add_dirs: options.add_dirs.clone(),
            resume: resume.clone(),
            prompt: options.prompt.clone(),
            effort: options.effort,
        });
        let args = if codex.is_some() {
            Vec::new()
        } else {
            options
                .add_dirs
                .iter()
                .flat_map(|dir| ["--add-dir".into(), dir.clone()])
                .chain((eff_model != crate::providers::CLAUDE_DEFAULT).then(|| ["--model".into(), eff_model.clone()]).into_iter().flatten())
                .chain(resume.into_iter().flat_map(|session| ["--resume".into(), session]))
                .chain(options.prompt.clone())
                .collect()
        };
        // Before the spawn: the CLI may print its "not found" line and exit at once.
        let mut env = {
            let mut env = options.env.clone();
            effort_env(&engine, options.effort, &mut env);
            leader_colors_env(&self.skills, &engine, &agent.kind, std::env::var("CLAUDE_CODE_PLUGIN_DIRS").ok().as_deref(), &mut env)?;
            env
        };
        // #838 D: the binding is on record before the CLI's first hook. #284: a start that resumes
        // nothing abandons the saved session here too — else the next plain start (restart marker,
        // a pane reopened) resumes it. A failed spawn puts the old row back.
        // #838 G: memory and system text come from that recorded binding's context — the one its
        // SessionStart resolves too. A bound session whose project is gone does not start.
        let (prior, launch) = crate::db::with(&self.db, |c| {
            let prior = bind_launch(c, id, &engine, &plan.binding, plan.stale.is_some())?;
            match agent_launch(c, &self.paths.data_dir, &agent, &plan.binding) {
                Ok(launch) => Ok((prior, launch)),
                Err(err) => restore_session_row(c, id, prior.as_ref()).and(Err(err)),
            }
        })?;
        if engine=="claude" && eff_provider=="gateway" {
            match self.gateway_spawn_env(id,&eff_model) {
                Ok(gateway_env) => env.extend(gateway_env),
                Err(error) => {
                    let _=crate::db::with(&self.db,|c|restore_session_row(c,id,prior.as_ref()));
                    return Err(error);
                }
            }
        }
        match watch {
            Some(w) => self.resume_watch.lock().unwrap().insert(id.into(), w),
            None => self.resume_watch.lock().unwrap().remove(id),
        };
        self.agent_pty_spawn(
            crate::pty::Spawn {
                id: id.into(),
                cmd: engine.clone(),
                args,
                cwd: options.cwd.clone(),
                memory_dir: Some(launch.memory.root.to_string_lossy().into_owned()),
                provider: Some(eff_provider.clone()),
                cols: options.cols.unwrap_or(120),
                rows: options.rows.unwrap_or(36),
                codex,
                env,
            },
            launch,
            AgentSink { core: self.weak_self.clone(), id: id.into(), tail: String::new() },
        )
        .inspect_err(|_| {
            self.gateway.revoke_agent(id);
            self.resume_watch.lock().unwrap().remove(id);
            if let Err(err) = crate::db::with(&self.db, |c| restore_session_row(c, id, prior.as_ref())) {
                log::error!("session row restore: {err}");
            }
        })?;
        let observed = crate::db::with(&self.db, |c| crate::job_model::record_spawn(c, id, plan.binding.delegation_id, &engine, &eff_provider, &eff_model, options.model_route_reason.as_deref()));
        match observed {
            Ok(Some(leader)) => {
                self.changed("delegations-changed");
                let weak = self.weak_self.clone();
                std::thread::spawn(move || { if let Some(core) = weak.upgrade() { core.leader_notes_wake(&leader); } });
            }
            Ok(None) => {}
            Err(error) => { self.ptys.kill(id); return Err(error); }
        }
        if let Some((_, old_engine)) = &plan.stale {
            self.session_changed(id, None, old_engine);
        }
        let state = self.agent_states.started(id, &engine, options.prompt.is_some());
        self.reported_workers.lock().unwrap().remove(id);
        self.sleeping_leaders.lock().unwrap().remove(id);
        if agent.kind == "leader" { self.team_idle_reset(&agent.team_id); }
        let _ = self.events.emit("agent-status", &state);
        self.ptys.list().into_iter().find(|pty| pty.id == id).ok_or_else(|| "agent PTY başlatılamadı".into())
    }

    /// The CLI's PTY. #838 D (tests): with [`CAPTURE`] set the request is kept and a shell stands
    /// in for the CLI — core tests never start a real claude or codex.
    fn agent_pty_spawn(&self, req: crate::pty::Spawn, launch: crate::pty::AgentLaunch, sink: AgentSink) -> Result<(), String> {
        #[cfg(test)]
        if CAPTURE.with(|c| c.borrow().is_some()) {
            let shell = crate::pty::Spawn { id: req.id.clone(), cmd: crate::shell::SHELL_CMD.into(), cols: req.cols, rows: req.rows, ..Default::default() };
            CAPTURE.with(|c| c.borrow_mut().as_mut().map(|v| v.push((req, launch.job))));
            LAUNCHES.with(|l| l.borrow_mut().push(launch));
            return self.pty_spawn(shell, sink);
        }
        crate::pty::spawn_bound(self, req, Some(launch), sink)
    }

    /// #838 D — G's dispatch asks this before pasting job `d` into the live session of its agent:
    /// only a session of `d`'s project takes it. A session of another project leaves the job
    /// queued (the patron's conversation is not reset for it; a Dex session is replaced anyway and
    /// the job starts fresh). Unknown: no (the job waits).
    pub fn session_fits(&self, d: &crate::delegate::Delegation) -> bool {
        if d.model.is_some() { return false; }
        crate::db::with(&self.db, |c| {
            let a = crate::teams::get_agent(c, &d.to_agent)?;
            let b = session_row(c, &a.id)?.map(|s| s.binding).unwrap_or_default();
            if a.active_provider.as_deref().unwrap_or(&a.provider)=="gateway" && b.delegation_id!=Some(d.id) {return Ok(false);}
            binding_fits(c, &self.paths.data_dir, &a.id, &a.team_id, &b, d.project_id.as_deref())
        })
        .unwrap_or(false)
    }

    pub fn agent_status_list(&self) -> Vec<State> {
        self.agent_states.list()
    }

    /// #18: an ended draft frees the worker — its queued job goes right after the quiet window,
    /// not a poll round later.
    pub fn agent_input(&self, id: &str, draft: bool) {
        let ended = self.agent_states.input(id, draft);
        self.leader_paste_input(id);
        if ended {
            self.delegation_retry_fresh(id);
            self.leader_reset_deliver_later(id);
        }
    }

    /// FIX-4: the hook side. A session is kept for `--resume` only once it can be resumed: a turn
    /// finished (Stop — claude flushes the transcript just after it) or the transcript already holds
    /// a conversation ([`resumable_transcript`]; a resumed session, a turn mid-way). A session that
    /// ends before that (folder trust refused, quit before a reply) leaves no record.
    pub(crate) fn session_on_hook(
        &self,
        agent: &str,
        event: &str,
        session: &str,
        transcript: Option<&str>,
        engine_hint: Option<&str>,
    ) {
        if event=="SessionEnd" && self.agent_states.get(agent).session_id.as_deref()==Some(session) {
            self.gateway.revoke_agent(agent);
        }
        // A hook means the CLI got past the resume — except SessionEnd: claude sends one, with the
        // missing id, right after "No conversation found".
        if event != "SessionEnd" {
            self.resume_watch.lock().unwrap().remove(agent);
        }
        let Ok((engine, saved)) = crate::db::with(&self.db, |c| {
            let a = crate::teams::get_agent(c, agent)?;
            let engine = engine_hint.map(str::to_string).unwrap_or_else(|| a.active_engine.unwrap_or(a.engine));
            let saved = resumable_session(c, agent, &engine)?;
            Ok((engine, saved))
        }) else {
            return;
        };
        // S5-B: the transcript path is live state even when the session row is unchanged.
        if let Err(error) = crate::db::with(&self.db, |c| crate::cost::observe(c, agent, session, &engine, transcript, event)) {
            log::error!("usage attribution: {error}");
        }
        if let Err(error) = self.agent_context(agent) {
            log::error!("context refresh: {error}");
        }
        if let Some(p) = transcript {
            self.agent_states.set_transcript(agent, p);
        }
        if saved.as_deref() == Some(session) {
            if transcript.is_some() {
                let _ = crate::db::with(&self.db, |c| record_transcript(c, agent, &engine, transcript));
            }
            self.chat_rescan(agent);
            return;
        }
        let usable = event == "Stop" || transcript.is_some_and(|p| resumable_transcript(std::path::Path::new(p)));
        if usable
            && crate::db::with(&self.db, |c| record_session(c, agent, session, &engine, transcript)).unwrap_or(false)
        {
            self.session_changed(agent, Some(session), &engine);
        }
        self.chat_rescan(agent);
    }

    fn session_changed(&self, agent: &str, session: Option<&str>, engine: &str) {
        let _ = self.events.emit("agent-session", &serde_json::json!({ "agentId": agent, "sessionId": session, "engine": engine }));
    }

    /// FIX-4: output of an agent whose resume is unconfirmed — look for the CLI's "not found" line.
    fn resume_output(&self, id: &str, tail: &mut String, text: &str) {
        let mut watch = self.resume_watch.lock().unwrap();
        let Some(w) = watch.get_mut(id).filter(|w| !w.lost) else { return };
        tail.push_str(text);
        if RESUME_LOST.iter().any(|m| tail.contains(m)) {
            w.lost = true;
        }
        let keep = tail.len().saturating_sub(256);
        let cut = (keep..=tail.len()).find(|&i| tail.is_char_boundary(i)).unwrap_or(0);
        tail.drain(..cut);
    }

    /// FIX-4: the patron stopped the agent; whatever it printed is not a lost resume.
    pub(crate) fn resume_forget(&self, id: &str) {
        self.resume_watch.lock().unwrap().remove(id);
    }

    /// FIX-4: the agent exited. If it was a resume the CLI could not find, the saved session goes
    /// and the same start runs once more without one (`fresh`: no saved id, no second retry).
    /// `true` = restarted; the exit is not a session end for delegations.
    pub(crate) fn resume_on_exit(&self, id: &str) -> bool {
        // The last output may reach the sink after the exit (the reader drains after the waiter).
        let end = std::time::Instant::now() + std::time::Duration::from_millis(1_500);
        let watch = loop {
            let mut map = self.resume_watch.lock().unwrap();
            match map.get(id) {
                None => return false,
                Some(w) if w.lost => break map.remove(id).unwrap(),
                Some(_) if std::time::Instant::now() >= end => {
                    map.remove(id);
                    return false;
                }
                Some(_) => {}
            }
            drop(map);
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        if crate::db::with(&self.db, |c| forget_session(c, id, &watch.engine, &watch.session)).unwrap_or(false) {
            self.session_changed(id, None, &watch.engine);
        }
        let text = "Önceki oturum bulunamadı, yeni oturum açıldı.";
        eprintln!("dex: {id}: resume {} bulunamadı, yeni oturum", watch.session);
        match self.agent_spawn(id, watch.retry) {
            Ok(_) => {
                let _ = self.events.emit("dex-resume-lost", &serde_json::json!({ "agentId": id, "sessionId": watch.session, "text": text }));
                true
            }
            Err(err) => {
                eprintln!("dex: {id}: resume'suz yeniden başlatma: {err}");
                false
            }
        }
    }
}

impl crate::Core {
    // ── DEX-17: motor zinciri ──

    /// Sağlayıcıda kullanılabilir jeton var mı (zincir atlama kuralı). `anthropic` yerleşik,
    /// jeton istemez; `local` adres + jeton ister.
    pub(crate) fn chain_has_token(&self, id: &str) -> bool {
        if id=="gateway" {return self.gateway_auth_status().is_ok_and(|s|s.active_account_id.is_some());}
        if id == "anthropic" || id == "codex" {
            return true;
        }
        let secret = |acct: &str| crate::secrets::get(crate::providers::KEYRING_SERVICE, acct).ok().flatten();
        if id == "local" {
            return secret("local.base_url").is_some_and(|v| !v.is_empty())
                && secret("local").is_some_and(|v| !v.is_empty());
        }
        secret(id).is_some_and(|v| !v.is_empty())
    }

    /// StopFailure'ta zincir kararı (hook thread'inde koşar, spawn YOK). `Some` = zincir bu ajan
    /// için açık → çağıran devretmeyi `failed` saymamalı, sonra [`Self::chain_execute`] ile
    /// geçişi başlatmalı. İki tetikleyici: asıl sağlayıcıda kalıcı kota/kredi bitimi, ya da
    /// ZİNCİR GEÇİCİ sağlayıcıdayken ağ/timeout sinyali (o sağlayıcı kısa süreyle unreachable).
    pub(crate) fn chain_plan(&self, agent: &str, body: &serde_json::Value) -> Option<ChainPlan> {
        let field = |k: &str| body.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
        let a = crate::db::with(&self.db, |c| crate::teams::get_agent(c, agent)).ok()?;
        if a.engine != "claude" || a.chain_off {
            return None;
        }
        let on_temp = a.active_provider.is_some() || a.active_engine.is_some();
        let current = a.active_provider.as_deref().unwrap_or(&a.provider).to_string();
        let msg = field("last_assistant_message");
        // Geçici sağlayıcıda ağ/timeout: unreachable say + ilerle (asıl sağlayıcıda değişmez).
        let (kind, reason) = if crate::chain::classify(field("error"), msg) == crate::chain::Kind::Permanent {
            if a.active_engine.is_some() {
                return None;
            }
            (true, "kota")
        } else if on_temp && crate::chain::unreachable_signal(field("error"), msg) {
            (true, "unreachable")
        } else {
            (false, "")
        };
        if !kind {
            return None;
        }
        let reset = if reason == "unreachable" {
            Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
                    + crate::chain::UNREACHABLE_EXHAUST_MS,
            )
        } else {
            crate::chain::parse_reset(&current, msg)
        };
        // `next: None` da bir plan (zincir bitti → bekleme).
        self.chain_measure();
        let next = crate::db::with(&self.db, |c| crate::chain::pick_next(c, &current, &|p| self.chain_has_token(p))).ok()?;
        Some(ChainPlan {
            current,
            next,
            reset,
            reason: reason.to_string(),
            session: body.get("session_id").and_then(serde_json::Value::as_str).map(str::to_string),
            transcript: body.get("transcript_path").and_then(serde_json::Value::as_str).map(str::to_string),
            threshold: None,
            threshold_reset: None,
        })
    }

    /// Zincir geçişini yürütür (arka plan thread'inde): tükenen kaydı düş, sıradakine geç
    /// (`--resume` + devam turu) ya da zincir bittiyse bekleme bildir. Devretmedeki worker'ın
    /// işi SÜRER (delegation'a dokunulmaz); lider tek satırla bilgilenir.
    pub(crate) fn chain_execute(&self, agent: &str, plan: ChainPlan) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let reset = plan.reset.unwrap_or(now_ms + crate::chain::DEFAULT_EXHAUST_MS);
        let from_label = chain_target_label(&plan.current);
        // Görev #250: eşik geçişi sağlayıcıyı tükenmiş saymaz (liderler Claude'da sürer).
        if plan.threshold.is_none() {
            if crate::db::with(&self.db, |c| crate::chain::mark_exhausted(c, &plan.current, Some(reset), &plan.reason)).is_err() {
                return;
            }
            self.chain_schedule_reset(&plan.current, reset);
        }
        let a = match crate::db::with(&self.db, |c| crate::teams::get_agent(c, agent)) {
            Ok(a) => a,
            Err(_) => return,
        };
        let source_model = a.active_model.as_deref().unwrap_or(&a.model);
        let (picked, skipped) = self.chain_pick(plan.next.clone(), source_model);
        let Some((next, active_engine, active_provider, model)) = picked else {
            let suffix = if skipped.is_empty() { String::new() } else { format!(" ({})", skipped.join("; ")) };
            if let Some(why) = &plan.threshold {
                // Eşikte iş durmaz: worker Claude'da kalır.
                eprintln!("dex: {agent}: {why} ama zincirde kullanılabilir hedef yok{suffix} — Claude'da kalıyor");
                return;
            }
            // #1018: kotası bilinen-bitmiş motorlar varsa en erken sıfırlanması da söylenir.
            let earliest = crate::db::with(&self.db, crate::chain::earliest_reset).ok().flatten();
            let none = self.chain_none_reason(&plan.current, &a.provider, &skipped);
            let text = if plan.reason == "unreachable" {
                format!("{from_label} yanıt vermiyor, {none}{suffix} — ajan bekliyor.")
            } else {
                format!("{from_label} kotası doldu, {none}{suffix} — ajan bekliyor.")
            };
            self.chain_wait(agent, &text, earliest);
            self.chain_note_lead(agent, &format!("{agent}: {none}{} — iş bekliyor.", reset_clause(earliest)));
            eprintln!("dex: {agent}: zincir bitti, bekleniyor{suffix}");
            return;
        };
        let to_label = chain_target_label(&next);
        // #1027: Codex'ten çıkışta devir bağlamı Codex'in kendi rollout'undan; sürdürülecek Claude oturumu yok.
        let from_codex = plan.current == "codex";
        let handoff = self
            .chain_handoff_summary_from(if from_codex { "codex" } else { "claude" }, plan.session.as_deref(), plan.transcript.as_deref(), 3_600)
            .unwrap_or_else(|_| {
            "Önceki oturumun transcript'i okunamadı. Son kullanıcı işini kaldığın yerden sürdür; bitmiş adımları tekrar etme.".into()
        });
        let prior = (a.active_engine.clone(), a.active_provider.clone(), a.active_model.clone());
        // Guard precedes the transient DB write: a synchronous PTY exit cannot apply pending
        // team settings and clear the target between this write and replacement spawn.
        self.chain_guard.lock().unwrap().insert(agent.to_string());
        // #838 D: the replacement runs on the running session's binding — its job's project,
        // worktree and env — not in the team folder.
        let launch = match crate::db::with(&self.db, |c| {
            let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
            if next == "codex" {
                save_chain_origin(&tx, agent, plan.session.as_deref(), plan.transcript.as_deref())?;
            }
            tx.execute(
                "UPDATE agents SET active_engine = ?2, active_provider = ?3, active_model = ?4 WHERE id = ?1",
                rusqlite::params![agent, active_engine, active_provider, model],
            )
            .map_err(|e| e.to_string())?;
            let launch = session_row(&tx, agent)?.map(|s| s.binding).unwrap_or_default();
            tx.commit().map_err(|e| e.to_string())?;
            Ok(launch)
        }) {
            Ok(launch) => Some(launch),
            Err(_) => {
                self.chain_guard.lock().unwrap().remove(agent);
                return;
            }
        };
        self.ptys.kill(agent);
        let continuation = match &plan.threshold {
            Some(why) => crate::chain::continue_prompt_threshold(why, &to_label),
            None => crate::chain::continue_prompt(&from_label, &to_label),
        };
        let mut options = if next == "codex" || from_codex {
            StartOptions { prompt: Some(format!("{continuation}\n\n{handoff}")), fresh: true, launch, ..Default::default() }
        } else {
            StartOptions {
                prompt: Some(continuation),
                resume: plan.session.clone(),
                fallback_prompt: Some(handoff),
                launch,
                ..Default::default()
            }
        };
        options.model_route_reason = Some(plan.threshold.clone().unwrap_or_else(|| plan.reason.clone()));
        let r = self.agent_spawn(agent, options);
        self.chain_guard.lock().unwrap().remove(agent);
        self.changed("teams-changed");
        match r {
            Ok(_) => {
                let text = if let Some(why) = &plan.threshold {
                    format!("{why} → {to_label} ile devam ediliyor.")
                } else if plan.reason == "unreachable" {
                    format!("{from_label} yanıt vermiyor → {to_label} ile devam ediliyor.")
                } else {
                    format!("{from_label} limiti doldu → {to_label} ile devam ediliyor.")
                };
                let _ = self.events.emit("dex-chain", &serde_json::json!({
                    "agentId": agent, "from": plan.current, "to": next,
                    "activeEngine": active_engine.as_deref().unwrap_or("claude"),
                    "activeProvider": active_provider,
                    "resetAt": plan.threshold_reset.clone().unwrap_or_else(|| ms_to_iso(reset)), "text": text,
                }));
                eprintln!("dex: {agent}: zincir {from_label} → {to_label}");
                // Lider tek satırla bilgilenir (bir sonraki hook bağlamında).
                let from = plan.threshold.as_deref().unwrap_or(&from_label);
                self.chain_note_lead(agent, &format!("{agent}: {from} → {to_label} ile sürüyor — iş bölünmedi."));
            }
            Err(err) => {
                let _ = crate::db::with(&self.db, |c| restore_chain_state(c, agent, &prior, next == "codex"));
                self.chain_spawn_failed(agent, &format!("{to_label} ile devam edilemedi: {err}"));
                eprintln!("dex: {agent}: zincir geçişi başarısız: {err}");
            }
        }
    }

    /// Zincirde `first`'ten başlayarak gerçekten kullanılabilir ilk hedef: (hedef, active_engine,
    /// active_provider, model). Jetonsuz sağlayıcı / hazır olmayan Codex atlanır, nedeni döner.
    #[allow(clippy::type_complexity)]
    fn chain_pick(
        &self,
        first: Option<String>,
        source_model: &str,
    ) -> (Option<(String, Option<String>, Option<String>, String)>, Vec<String>) {
        let mut candidate = first;
        let mut skipped = Vec::new();
        while let Some(next) = candidate {
            if next == "gateway" {
                match self.gateway_models() {
                    Ok(rows) => {
                        let catalog=rows.into_iter().map(|m|m.id).collect::<Vec<_>>();
                        if let Some(model)=crate::chain::map_codex_model(source_model,&catalog) {
                            return (Some((next,None,Some("gateway".into()),model)),skipped);
                        }
                        skipped.push("ChatGPT hesabında kullanılabilir model yok".into());
                    }
                    Err(error) => skipped.push(format!("ChatGPT kullanılamıyor: {error}")),
                }
            } else if next == "codex" {
                let bin = crate::codex::resolve("codex");
                match crate::codex::check_ready(&bin).and_then(|_| crate::codex::models()) {
                    Ok(rows) => {
                        let catalog = rows.into_iter().map(|m| m.id).collect::<Vec<_>>();
                        if let Some(model) = crate::chain::map_codex_model(source_model, &catalog) {
                            return (Some((next, Some("codex".to_string()), None, model)), skipped);
                        }
                        skipped.push("Codex hesabında kullanılabilir model yok".to_string());
                    }
                    Err(err) => skipped.push(format!("Codex kullanılamıyor: {err}")),
                }
            } else if next == "anthropic" || self.chain_has_token(&next) {
                let model = crate::chain::map_model(&next, source_model);
                return (Some((next.clone(), None, Some(next), model)), skipped);
            } else {
                skipped.push(format!("{} jetonu yok", chain_target_label(&next)));
            }
            candidate = crate::db::with(&self.db, |c| {
                crate::chain::pick_next(c, &next, &|p| self.chain_has_token(p))
            })
            .ok()
            .flatten();
        }
        (None, skipped)
    }

    /// Ayarlar panelindeki "geri dön": tükenen asıl sağlayıcı kullanılabilirse geçici
    /// sağlayıcıyı kaldırır, asıl sağlayıcıyla resume'ler.
    pub fn chain_back(&self, agent: &str) -> Result<(), String> {
        let a = crate::db::with(&self.db, |c| crate::teams::get_agent(c, agent))?;
        if a.active_provider.is_none() && a.active_engine.is_none() {
            return Err("ajan zaten asıl sağlayıcısında".into());
        }
        self.chain_measure();
        let usable = crate::db::with(&self.db, |c| {
            Ok(!crate::chain::exhausted(c)?.iter().any(|e| e.provider == a.provider))
        })?;
        if !usable {
            return Err(format!("{} hâlâ tükenmiş — sıfırlanma saati dolmadı", a.provider));
        }
        let handoff = self.chain_handoff_summary(agent, 3_600).ok();
        let prior = (a.active_engine.clone(), a.active_provider.clone(), a.active_model.clone());
        self.chain_guard.lock().unwrap().insert(agent.to_string());
        // #838 D: back on the current session's binding; the original Claude session is resumed
        // only if it ran on that same binding (another job's or project's is never resumed).
        let (origin, launch) = match crate::db::with(&self.db, |c| {
            let origin = c
                .query_row(
                    "SELECT session_id, project_id, delegation_id, cwd FROM agent_chain_origins WHERE agent_id = ?1 AND engine = 'claude'",
                    [agent],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            Binding { project_id: row.get(1)?, delegation_id: row.get(2)?, cwd: row.get(3)? },
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let launch = session_row(c, agent)?.map(|s| s.binding).unwrap_or_default();
            c.execute(
                "UPDATE agents SET active_engine = NULL, active_provider = NULL, active_model = NULL WHERE id = ?1",
                [agent],
            )
            .map_err(|e| e.to_string())?;
            Ok((origin.filter(|(_, b)| *b == launch).and_then(|(s, _)| s), launch))
        }) {
            Ok(found) => found,
            Err(err) => {
                self.chain_guard.lock().unwrap().remove(agent);
                return Err(err);
            }
        };
        self.ptys.kill(agent);
        let r = self.agent_spawn(agent, StartOptions {
            prompt: Some(handoff.unwrap_or_else(|| "Kaldığın yerden devam et: asıl sağlayıcı yeniden kullanılabilir durumda.".into())),
            resume: origin,
            launch: Some(launch),
            model_route_reason: Some("zincirin kayıtlı sağlayıcıya dönüşü".into()),
            ..Default::default()
        });
        self.chain_guard.lock().unwrap().remove(agent);
        if r.is_ok() {
            let _ = crate::db::with(&self.db, |c| {
                c.execute("DELETE FROM agent_chain_origins WHERE agent_id = ?1", [agent])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        } else {
            // Keep the retry/back affordance and the original-session row if spawn failed.
            let _ = crate::db::with(&self.db, |c| {
                c.execute(
                    "UPDATE agents SET active_engine = ?2, active_provider = ?3, active_model = ?4 WHERE id = ?1",
                    rusqlite::params![agent, prior.0, prior.1, prior.2],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            });
        }
        self.changed("teams-changed");
        r.map(|_| ())
    }

    pub(crate) fn chain_schedule_existing(&self) {
        let rows = crate::db::with(&self.db, crate::chain::exhausted).unwrap_or_default();
        for row in rows {
            self.chain_schedule_reset(&row.provider, row.reset_at);
        }
    }

    fn chain_schedule_reset(&self, provider: &str, reset_at: i64) {
        self.chain_resets.lock().unwrap().insert(provider.to_string(), reset_at);
        let weak = self.weak_self.clone();
        let provider = provider.to_string();
        std::thread::spawn(move || {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
            std::thread::sleep(std::time::Duration::from_millis(reset_at.saturating_sub(now).max(0) as u64));
            let Some(core) = weak.upgrade() else { return };
            let mut timers = core.chain_resets.lock().unwrap();
            if timers.get(&provider).copied() != Some(reset_at) {
                return;
            }
            timers.remove(&provider);
            drop(timers);
            let _ = crate::db::with(&core.db, crate::chain::exhausted);
            core.chain_return(&provider);
            core.changed("teams-changed");
        });
    }

    /// Görev #94: sıfırlanan sağlayıcıya otomatik geri dönüş. O sağlayıcı aslı olup şu an
    /// geçicide olan her ajan: boşsa hemen [`Self::chain_back`], çalışıyorsa oturum kapanışına
    /// ertelenir (`chain_return_pending`, [`Core::delegation_session_exited`] uygular).
    pub(crate) fn chain_return(&self, provider: &str) {
        let label = chain_target_label(provider);
        self.chain_resume_waiting(provider);
        let agents = crate::db::with(&self.db, |c| crate::teams::chain_displaced(c, provider)).unwrap_or_default();
        if agents.is_empty() {
            let text = format!("{label} yeniden kullanılabilir — istersen geri dön.");
            let _ = self.events.emit("dex-chain-available", &serde_json::json!({ "provider": provider, "text": text }));
            return;
        }
        let gone = crate::db::with(&self.db, crate::chain::exhausted).unwrap_or_default();
        for a in agents {
            // #1027: geçici motoru da tükenmiş (zincir bekletmesi) oturum ilerleyemez — kapanışı beklenmez.
            // #1033: yalnız kota tükenmesi ya da Dex'in bekletmesi; başkasının "unreachable" işareti
            // sağlıklı oturumu tur ortasında öldürmez.
            let stuck = self.agent_states.get(&a.id).chain_wait
                || gone.iter().any(|e| e.provider == current_motor(&a) && e.reason == "kota");
            if self.ptys.is_running(&a.id) && !stuck {
                let _ = crate::db::with(&self.db, |c| crate::teams::set_chain_return_pending(c, &a.id, true));
                let text = format!("{label} sıfırlandı — {} oturumu kapanınca {label}'e dönecek.", a.name);
                let _ = self.events.emit("dex-chain-available", &serde_json::json!({
                    "provider": provider, "agentId": a.id, "pending": true, "text": text,
                }));
                eprintln!("dex: {}: {label} sıfırlandı, dönüş oturum kapanışına ertelendi", a.id);
                continue;
            }
            match self.chain_back(&a.id) {
                Ok(()) => {
                    let text = format!("{label} sıfırlandı — {} {label}'e döndü.", a.name);
                    let _ = self.events.emit("dex-chain-available", &serde_json::json!({
                        "provider": provider, "agentId": a.id, "text": text,
                    }));
                    eprintln!("dex: {}: {label} sıfırlandı, geri döndü", a.id);
                }
                Err(err) => {
                    let text = format!("{label} sıfırlandı ama {} geri dönemedi: {err}", a.name);
                    let _ = self.events.emit("dex-chain-available", &serde_json::json!({
                        "provider": provider, "agentId": a.id, "text": text,
                    }));
                    eprintln!("dex: {}: otomatik geri dönüş başarısız: {err}", a.id);
                }
            }
        }
    }

    /// #1033: `provider` sıfırlandı — o motorda kota için bekletilen canlı oturum (asıl motoru
    /// Codex olan dahil) aynı oturumda `[Dex]` devam turuyla sürer. Dönülecek asıl sağlayıcı
    /// burada değil ([`Self::chain_back`]); yazmakta olan patronun girdisine karışılmaz.
    fn chain_resume_waiting(&self, provider: &str) {
        let label = chain_target_label(provider);
        let agents = crate::db::with(&self.db, crate::teams::list_agents).unwrap_or_default();
        for a in agents.iter().filter(|a| current_motor(a) == provider && self.agent_states.get(&a.id).chain_wait) {
            if !self.ptys.is_running(&a.id) {
                continue;
            }
            let text = if self.agent_states.typing(&a.id) {
                format!("{label} sıfırlandı — {} bekliyor, devam etmesini yaz.", a.name)
            } else {
                let prompt = format!("[Dex] {label} kotası sıfırlandı. Kaldığın yerden devam et; bitmiş işi baştan yapma.");
                match self.paste_prompt(&a.id, &prompt) {
                    Ok(()) => {
                        eprintln!("dex: {}: {label} sıfırlandı, bekleyen oturum sürüyor", a.id);
                        format!("{label} sıfırlandı — {} devam ediyor.", a.name)
                    }
                    Err(err) => {
                        eprintln!("dex: {}: {label} sıfırlandı ama devam turu verilemedi: {err}", a.id);
                        format!("{label} sıfırlandı ama {} devam ettirilemedi: {err}", a.name)
                    }
                }
            };
            let _ = self.events.emit("dex-chain-available", &serde_json::json!({
                "provider": provider, "agentId": a.id, "text": text,
            }));
        }
    }

    fn chain_wait(&self, agent: &str, text: &str, reset: Option<i64>) {
        let state = self.agent_states.chain_wait(agent);
        let _ = self.events.emit("agent-status", &state);
        let _ = self.events.emit("dex-chain-wait", &serde_json::json!({
            "agentId": agent, "text": text, "resetAt": reset.map(ms_to_iso),
        }));
        self.changed("teams-changed");
    }

    /// Ajanın süren devretmesinin liderine tek satır (bir sonraki hook bağlamında). İş yoksa sessiz.
    fn chain_note_lead(&self, agent: &str, text: &str) {
        let lead = crate::db::with(&self.db, |c| {
            c.query_row(
                "SELECT from_agent FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
                [agent],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())
        });
        if let Ok(Some(lead)) = lead {
            let _ = crate::db::with(&self.db, |c| crate::plan::note(c, &lead, text));
        }
    }

    /// #1018: seçimden hemen önce, kotası ölçülüp BİLİNEN bitmiş motoru tükenmiş kaydına işler
    /// (Claude: OAuth ölçümü, Codex: kendi rollout kaydı) — atlama, bildirim ve sıfırlanınca geri
    /// dönüş StopFailure kaydıyla aynı yoldan. Ölçüm yok/bayat/okunamıyor = dokunulmaz (aday kalır).
    /// Poll yok: yalnız seçim anında okunur. GLM/Muse/local'de ölçüm yok, yalnız StopFailure kaydı.
    pub(crate) fn chain_measure(&self) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let claude = self.quota.lock().unwrap().exhausted_until(now);
        let codex = codex_sessions().and_then(|dir| crate::chain::codex_exhausted_until(&dir, now));
        for (provider, reset) in [("anthropic", claude), ("codex", codex)] {
            let Some(reset) = reset else { continue };
            let marked = crate::db::with(&self.db, |c| {
                if crate::chain::exhausted(c)?.iter().any(|e| e.provider == provider && e.reset_at >= reset) {
                    return Ok(false);
                }
                crate::chain::mark_exhausted(c, provider, Some(reset), "kota")?;
                Ok(true)
            })
            .unwrap_or(false);
            if marked {
                self.chain_schedule_reset(provider, reset);
                eprintln!("dex: {} kotası dolu ölçüldü, sıfırlanma {}", chain_target_label(provider), ms_to_iso(reset));
                self.changed("teams-changed");
            }
        }
    }

    /// #1027: zincirde geçilecek motor yokken nedeni ([`crate::chain::none_reason`]); `home` = ajanın
    /// asıl sağlayıcısı (Codex'ten dönüş adayı).
    fn chain_none_reason(&self, current: &str, home: &str, skipped: &[String]) -> &'static str {
        crate::db::with(&self.db, |c| Ok(crate::chain::none_reason(c, current, home, !skipped.is_empty(), &|p| self.chain_has_token(p))))
            .unwrap_or("zincirde kullanılabilir hedef yok")
    }

    /// #1027: Codex'te koşan ajanın turu kota hatasıyla bitti. Codex'te StopFailure hook'u yok;
    /// sinyal rollout'un kendisi ([`crate::chain::codex_turn_limit`]), canlı sohbet izleyicisi
    /// (dosya olayı, poll yok) çağırır. StopFailure eşdeğeri: Codex tükenmiş yazılır (sıfırlanma
    /// aynı rollout'un `rate_limits`'inden, yoksa varsayılan) ve
    /// - Claude'dan taşınmış zincirli worker sıradaki kotası olan motora geçer; sırada yoksa ve asıl
    ///   sağlayıcısı tükenmemişse ona döner ([`Self::chain_back`]); hiçbiri yoksa bekler + tek not;
    /// - asıl motoru Codex olan (zinciri olmayan) ajan bekler + tek not.
    ///
    /// `false` = işlenmedi (geçiş sürüyor / ajan Codex'te değil): izleyici sonraki kota turunu yine verir.
    pub(crate) fn chain_codex_limit(&self, agent: &str, transcript: &std::path::Path) -> bool {
        if self.chain_guard.lock().unwrap().contains(agent) {
            return false;
        }
        let Ok(a) = crate::db::with(&self.db, |c| crate::teams::get_agent(c, agent)) else { return false };
        if a.active_engine.as_deref().unwrap_or(&a.engine) != "codex" {
            return false;
        }
        // #1033: aynı kuruyan tur ikinci kez görüldü (izleyici yeniden kuruldu): zaten bekliyor.
        if self.agent_states.get(agent).chain_wait {
            return true;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let reset = crate::chain::codex_file_exhausted_until(transcript, now);
        eprintln!("dex: {agent}: Codex turu kota hatasıyla bitti (usage_limit_exceeded)");
        let mut plan = ChainPlan {
            current: "codex".into(),
            next: None,
            reset,
            reason: "kota".into(),
            session: None,
            transcript: Some(transcript.to_string_lossy().into_owned()),
            threshold: None,
            threshold_reset: None,
        };
        if a.engine != "claude" || a.chain_off {
            let reset = reset.unwrap_or(now + crate::chain::DEFAULT_EXHAUST_MS);
            if crate::db::with(&self.db, |c| crate::chain::mark_exhausted(c, "codex", Some(reset), "kota")).is_ok() {
                self.chain_schedule_reset("codex", reset);
            }
            self.chain_wait(agent, "Codex kotası doldu — ajan bekliyor.", Some(reset));
            self.chain_note_lead(agent, &format!("{agent}: Codex kotası doldu{} — iş bekliyor.", reset_clause(Some(reset))));
            return true;
        }
        self.chain_measure();
        plan.next = crate::db::with(&self.db, |c| crate::chain::pick_next(c, "codex", &|p| self.chain_has_token(p))).ok().flatten();
        let home_free = crate::db::with(&self.db, |c| Ok(!crate::chain::exhausted(c)?.iter().any(|e| e.provider == a.provider)))
            .unwrap_or(false);
        if plan.next.is_none() && home_free {
            let reset = reset.unwrap_or(now + crate::chain::DEFAULT_EXHAUST_MS);
            if crate::db::with(&self.db, |c| crate::chain::mark_exhausted(c, "codex", Some(reset), "kota")).is_ok() {
                self.chain_schedule_reset("codex", reset);
            }
            let to_label = chain_target_label(&a.provider);
            match self.chain_back(agent) {
                Ok(()) => {
                    let text = format!("Codex limiti doldu → {to_label} ile devam ediliyor.");
                    let _ = self.events.emit("dex-chain", &serde_json::json!({
                        "agentId": agent, "from": "codex", "to": a.provider,
                        "activeEngine": "claude", "activeProvider": serde_json::Value::Null,
                        "resetAt": ms_to_iso(reset), "text": text,
                    }));
                    eprintln!("dex: {agent}: zincir Codex → {to_label} (asıl)");
                    self.chain_note_lead(agent, &format!("{agent}: Codex → {to_label} ile sürüyor — iş bölünmedi."));
                    return true;
                }
                Err(err) => eprintln!("dex: {agent}: Codex'ten {to_label}'e dönülemedi: {err}"),
            }
        }
        self.chain_execute(agent, plan);
        true
    }

    /// A replacement process could not start. Unlike chain exhaustion there is no live PTY that
    /// can wait for input, so publish the error notice while keeping backend/UI state offline.
    fn chain_spawn_failed(&self, agent: &str, text: &str) {
        let state = self.agent_states.hook(agent, Some(Status::Offline), None);
        let _ = self.events.emit("agent-status", &state);
        let _ = self.events.emit("dex-chain-wait", &serde_json::json!({ "agentId": agent, "text": text }));
        self.changed("teams-changed");
    }
}

impl crate::Core {
    // ── Görev #250: proaktif Claude kota eşiği ──

    /// Eşik geçince hemen taşınacak worker'lar: Claude'da (asıl ve etkin sağlayıcı anthropic),
    /// zinciri açık, oturumu canlı ve elinde süren bir devretme olan. Liderler hiç girmez.
    pub(crate) fn quota_targets(&self) -> Vec<String> {
        let guard = self.chain_guard.lock().unwrap().clone();
        crate::db::with(&self.db, |c| {
            let mut st = c
                .prepare("SELECT DISTINCT to_agent FROM delegations WHERE status = 'running'")
                .map_err(|e| e.to_string())?;
            let busy = st
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(crate::teams::list_agents(c)?
                .into_iter()
                .filter(|a| quota_bound(a) && busy.contains(&a.id))
                .map(|a| a.id)
                .collect::<Vec<_>>())
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !guard.contains(id) && self.ptys.is_running(id))
        .collect()
    }

    /// Eşik aşıldı: hedef worker'ları mevcut zincir devriyle (resume + `[Dex]` devam turu)
    /// sıradaki sağlayıcıya geçir. Hedef yoksa worker Claude'da kalır, bir kez uyarı loglanır.
    pub(crate) fn quota_switch(&self, breach: &crate::quota::Breach) {
        self.quota_switch_by(breach, &|model| self.quota_pick(model));
    }

    /// [`Self::quota_switch`]; `pick` = zincir seçimi (testler jeton kasasız verir). #1033: seçim
    /// gerçekten kullanılabilir hedefe bakar — atlananlar (Codex hazır değil, jeton yok) nottaki
    /// nedene girer.
    #[allow(clippy::type_complexity)]
    fn quota_switch_by(
        &self,
        breach: &crate::quota::Breach,
        pick: &dyn Fn(&str) -> (Option<(String, Option<String>, Option<String>, String)>, Vec<String>),
    ) {
        let targets = self.quota_targets();
        if targets.is_empty() {
            return;
        }
        self.chain_measure();
        let model = crate::db::with(&self.db, |c| crate::teams::get_agent(c, &targets[0])).map(|a| a.model).unwrap_or_default();
        let (picked, skipped) = pick(&model);
        let Some((next, ..)) = picked else {
            self.quota_no_target(breach, &targets, &skipped);
            return;
        };
        let next = Some(next);
        for id in targets {
            let saved = crate::db::with(&self.db, |c| {
                c.query_row(
                    "SELECT session_id, transcript FROM agent_sessions WHERE agent_id = ?1 AND engine = 'claude' AND session_id <> ''",
                    [&id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|e| e.to_string())
            })
            .ok()
            .flatten();
            let (session, transcript) = saved.map_or((None, None), |(s, t)| (Some(s), t));
            self.chain_execute(&id, ChainPlan {
                current: "anthropic".into(),
                next: next.clone(),
                reset: None,
                reason: "esik".into(),
                session,
                transcript,
                threshold: Some(breach.label()),
                threshold_reset: breach.resets_at.clone(),
            });
        }
    }

    /// #1018: eşik aşıldı ama zincirde geçilecek motor yok → worker'lar Claude'da kalır. Eşik
    /// bölümü başına TEK bildirim: log + ilgili liderlere birer not (sonraki ölçüm/başlatma susar).
    fn quota_no_target(&self, breach: &crate::quota::Breach, agents: &[String], skipped: &[String]) {
        if !self.quota.lock().unwrap().warn_once(breach) {
            return;
        }
        let earliest = crate::db::with(&self.db, crate::chain::earliest_reset).ok().flatten();
        let none = self.chain_none_reason("anthropic", "anthropic", skipped);
        let suffix = if skipped.is_empty() { String::new() } else { format!(" ({})", skipped.join("; ")) };
        let text = format!("{} ama {none}{suffix}{} — Claude'da sürüyor.", breach.label(), reset_clause(earliest));
        eprintln!("dex: {text}");
        for a in agents {
            self.chain_note_lead(a, &format!("{a}: {text}"));
        }
    }

    /// Başlatmadan hemen önce (agent_start kapısında): eşik üstündeyse Claude worker'ı doğrudan
    /// zincirde sıradakinde açılır; eşik altındayken YENİ (fresh) oturumu açılan, Claude'dan
    /// taşınmış worker Claude'a döner (Claude tükenmiş kayıtlıysa dönmez). Süren/resume edilen
    /// oturum geri taşınmaz. Zincir geçişinin kendi başlatması (chain_guard) dokunulmaz.
    fn quota_route(&self, id: &str, options: &mut StartOptions) {
        self.quota_route_by(id, options, &|model| self.quota_pick(model));
    }

    /// Claude'dan sonraki gerçekten kullanılabilir hedef ([`Self::chain_pick`]) ve atlananlar.
    #[allow(clippy::type_complexity)]
    fn quota_pick(&self, model: &str) -> (Option<(String, Option<String>, Option<String>, String)>, Vec<String>) {
        let first = crate::db::with(&self.db, |c| crate::chain::pick_next(c, "anthropic", &|p| self.chain_has_token(p)))
            .ok()
            .flatten();
        self.chain_pick(first, model)
    }

    /// [`Self::quota_route`]; `pick` = zincir seçimi (testler jeton kasasız verir).
    #[allow(clippy::type_complexity)]
    fn quota_route_by(
        &self,
        id: &str,
        options: &mut StartOptions,
        pick: &dyn Fn(&str) -> (Option<(String, Option<String>, Option<String>, String)>, Vec<String>),
    ) {
        if self.chain_guard.lock().unwrap().contains(id) {
            return;
        }
        let Ok(a) = crate::db::with(&self.db, |c| crate::teams::get_agent(c, id)) else { return };
        if a.kind != "worker" || a.engine != "claude" || a.chain_off || a.provider != "anthropic" {
            return;
        }
        let displaced = a.active_provider.is_some() || a.active_engine.is_some();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        // #1027: 5 sa eşiğinin yanında ölçülmüş gerçek %100 tükenme (haftalık dahil) de yönlendirir —
        // bilinen-bitmiş Claude'da boşa tur yok. Haftalık pencere ölçülür, geçiş eşiği değildir.
        let breach = {
            let q = self.quota.lock().unwrap();
            q.exhausted_window(now_ms).map(|(b, _)| b).or_else(|| q.breach_at(now_ms))
        };
        // Ölçüm (Codex rollout taraması) yalnız seçim ya da dönüş kararı olacaksa.
        if breach.is_some() || displaced {
            self.chain_measure();
        }
        match breach {
            Some(b) if !displaced => {
                let (picked, skipped) = pick(&a.model);
                let Some((to, engine, provider, model)) = picked else {
                    self.quota_no_target(&b, &[id.to_string()], &skipped);
                    return;
                };
                let codex = to == "codex";
                if crate::db::with(&self.db, |c| {
                    let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
                    if codex {
                        save_chain_origin(&tx, id, None, None)?;
                    }
                    tx.execute(
                        "UPDATE agents SET active_engine = ?2, active_provider = ?3, active_model = ?4 WHERE id = ?1",
                        params![id, engine, provider, model],
                    )
                    .map_err(|e| e.to_string())?;
                    tx.commit().map_err(|e| e.to_string())
                })
                .is_err()
                {
                    return;
                }
                if codex {
                    // Claude oturumu Codex'te açılamaz: yeni oturum.
                    options.resume = None;
                    options.fresh = true;
                }
                options.model_route_reason = Some(b.label());
                let text = format!("{} → {} ile açıldı.", b.label(), chain_target_label(&to));
                let _ = self.events.emit("dex-chain", &serde_json::json!({
                    "agentId": id, "from": "anthropic", "to": to,
                    "activeEngine": engine.as_deref().unwrap_or("claude"),
                    "activeProvider": provider,
                    "resetAt": b.resets_at, "text": text,
                }));
                eprintln!("dex: {id}: {} — {} ile açılıyor", b.label(), chain_target_label(&to));
                self.changed("teams-changed");
            }
            None if displaced && options.fresh => {
                let back = crate::db::with(&self.db, |c| {
                    if crate::chain::exhausted(c)?.iter().any(|e| e.provider == "anthropic") {
                        return Ok(false);
                    }
                    let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
                    tx.execute(
                        "UPDATE agents SET active_engine = NULL, active_provider = NULL, active_model = NULL, chain_return_pending = 0 WHERE id = ?1",
                        [id],
                    )
                    .map_err(|e| e.to_string())?;
                    tx.execute("DELETE FROM agent_chain_origins WHERE agent_id = ?1", [id]).map_err(|e| e.to_string())?;
                    tx.commit().map_err(|e| e.to_string())?;
                    Ok(true)
                })
                .unwrap_or(false);
                if back {
                    eprintln!("dex: {id}: Claude kullanımı eşiğin altında — yeni oturum Claude'da");
                    self.changed("teams-changed");
                }
            }
            _ => {}
        }
    }
}

/// #1033: ajanın şu an koştuğu motor (zincir kimliği): geçici Codex, geçici sağlayıcı, asıl motor.
fn current_motor(a: &crate::teams::Agent) -> &str {
    if a.active_engine.as_deref() == Some("codex") || (a.active_engine.is_none() && a.engine == "codex") {
        return "codex";
    }
    a.active_provider.as_deref().unwrap_or(&a.provider)
}

/// Görev #250: eşiğe tabi ajan — Claude'da (asıl ve etkin) duran, zinciri açık worker.
fn quota_bound(a: &crate::teams::Agent) -> bool {
    a.kind == "worker"
        && a.engine == "claude"
        && !a.chain_off
        && a.provider == "anthropic"
        && a.active_provider.is_none()
        && a.active_engine.is_none()
}

/// [`Core::chain_plan`] kararı.
pub(crate) struct ChainPlan {
    current: String,
    next: Option<String>,
    reset: Option<i64>,
    /// `mark_exhausted` nedeni: "kota" ya da "unreachable".
    reason: String,
    session: Option<String>,
    transcript: Option<String>,
    /// Görev #250: proaktif eşik geçişi ("Claude %85 (5 sa)"); sağlayıcı tükenmiş sayılmaz.
    threshold: Option<String>,
    threshold_reset: Option<String>,
}

fn chain_target_label(id: &str) -> String {
    if id == "codex" {
        "Codex".into()
    } else {
        crate::providers::get(id).map(|p| p.label).unwrap_or(id).to_string()
    }
}

fn save_chain_origin(conn: &Connection, agent: &str, session: Option<&str>, transcript: Option<&str>) -> Result<(), String> {
    let saved = conn
        .query_row(
            "SELECT session_id, transcript FROM agent_sessions WHERE agent_id = ?1 AND engine = 'claude' AND session_id <> ''",
            [agent],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let session = session.map(str::to_string).or_else(|| saved.as_ref().map(|x| x.0.clone()));
    let transcript = transcript.map(str::to_string).or_else(|| saved.and_then(|x| x.1));
    // #838 D: the origin keeps the binding it ran on; `chain_back` resumes it only on that one.
    let b = session_row(conn, agent)?.map(|s| s.binding).unwrap_or_default();
    conn.execute(
        "INSERT INTO agent_chain_origins (agent_id, engine, session_id, transcript, project_id, delegation_id, cwd)
         VALUES (?1, 'claude', ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(agent_id) DO UPDATE SET engine = excluded.engine,
             session_id = excluded.session_id, transcript = excluded.transcript,
             project_id = excluded.project_id, delegation_id = excluded.delegation_id, cwd = excluded.cwd",
        rusqlite::params![agent, session, transcript, b.project_id, b.delegation_id, b.cwd],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn restore_chain_state(
    conn: &Connection,
    agent: &str,
    prior: &(Option<String>, Option<String>, Option<String>),
    clear_origin: bool,
) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE agents SET active_engine = ?2, active_provider = ?3, active_model = ?4 WHERE id = ?1",
        rusqlite::params![agent, prior.0, prior.1, prior.2],
    )
    .map_err(|e| e.to_string())?;
    if clear_origin {
        tx.execute("DELETE FROM agent_chain_origins WHERE agent_id = ?1", [agent])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Lider notundaki ", en erken sıfırlanma <saat>" (UTC, RFC 3339); bilinmiyorsa boş.
fn reset_clause(reset: Option<i64>) -> String {
    reset.map(|r| format!(", en erken sıfırlanma {}", ms_to_iso(r))).unwrap_or_default()
}

/// epoch ms → RFC 3339 UTC (sentetik örnek: `2002-01-01T12:00:00+00:00`); UI yerel saate çevirir.
fn ms_to_iso(ms: i64) -> String {
    let mut days = ms.div_euclid(86_400_000);
    let t = ms.rem_euclid(86_400_000);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    days = y + (m <= 2) as i64;
    format!("{days:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00", m, d, t / 3_600_000, (t % 3_600_000) / 60_000, (t % 60_000) / 1000)
}

/// FIX-4: whether `--resume` can open this transcript. claude writes its file at the first prompt/// but only with metadata lines (last-prompt, mode…); `--resume` needs a conversation line
/// (`user`/`assistant`), which lands once the turn gets going. A codex rollout is resumable once
/// it exists (`session_meta` first). Only the head is read: those lines come early.
pub fn resumable_transcript(path: &std::path::Path) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else { return false };
    std::io::BufReader::new(file).lines().take(64).map_while(Result::ok).any(|line| {
        serde_json::from_str::<serde_json::Value>(&line)
            .is_ok_and(|v| matches!(v["type"].as_str(), Some("user" | "assistant" | "session_meta")))
    })
}

/// `true` when the stored row changed. S5-B: `transcript` is the session's JSONL file
/// (hook's `transcript_path`); nullable so pre-S5-B rows keep working.
pub fn record_session(conn: &Connection, agent: &str, session: &str, engine: &str, transcript: Option<&str>) -> Result<bool, String> {
    let same = conn
        .query_row("SELECT 1 FROM agent_sessions WHERE agent_id = ?1 AND session_id = ?2 AND engine = ?3", params![agent, session, engine], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    conn.execute(
        "INSERT INTO agent_sessions (agent_id, session_id, engine, transcript, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(agent_id) DO UPDATE SET session_id=excluded.session_id, engine=excluded.engine, transcript=excluded.transcript, updated_at=excluded.updated_at",
        params![agent, session, engine, transcript, now() as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(!same)
}

/// S5-B: refresh the saved transcript path without touching the session row's identity.
pub fn record_transcript(conn: &Connection, agent: &str, engine: &str, transcript: Option<&str>) -> Result<bool, String> {
    Ok(conn
        .execute(
            "UPDATE agent_sessions SET transcript = ?3, updated_at = ?4 WHERE agent_id = ?1 AND engine = ?2",
            params![agent, engine, transcript, now() as i64],
        )
        .map_err(|e| e.to_string())?
        > 0)
}

/// FIX-4: drops the agent's saved `session` of `engine` (a newer one stays). `true` = removed.
pub fn forget_session(conn: &Connection, agent: &str, engine: &str, session: &str) -> Result<bool, String> {
    conn.execute(
        "DELETE FROM agent_sessions WHERE agent_id = ?1 AND engine = ?2 AND session_id = ?3",
        params![agent, engine, session],
    )
    .map(|n| n > 0)
    .map_err(|e| e.to_string())
}

/// #838 D: a launch whose CLI has not named its session ([`NO_SESSION`]) has nothing to resume.
pub fn resumable_session(conn: &Connection, agent: &str, engine: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT session_id FROM agent_sessions WHERE agent_id = ?1 AND engine = ?2 AND session_id <> ''",
        params![agent, engine],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_colors_load_only_for_claude_leaders_and_preserve_plugin_env() {
        let root = std::env::current_dir().unwrap().join("../../.dex/tmp/leader-colors-env-test");
        let skills = crate::skills::Skills { root: root.clone() };
        let mut env = vec![("DEX_WT".into(), "x".into())];
        for (engine, kind) in [("claude", "worker"), ("codex", "leader"), ("shell", "leader")] {
            leader_colors_env(&skills, engine, kind, Some("existing"), &mut env).unwrap();
            assert_eq!(env.len(), 1);
            assert!(!root.exists(), "non-leader must not materialize the mod");
        }
        let sep = if cfg!(windows) { ';' } else { ':' };
        leader_colors_env(&skills, "claude", "leader", Some("existing"), &mut env).unwrap();
        let dir = root.join("dex-leader-colors").to_string_lossy().into_owned();
        assert_eq!(env[1], ("CLAUDE_CODE_PLUGIN_DIRS".into(), format!("existing{sep}{dir}")));
        assert!(root.join("dex-leader-colors/hooks/register.tsx").is_file());
        leader_colors_env(&skills, "claude", "leader", Some("ignored"), &mut env).unwrap();
        assert_eq!(env.len(), 2, "retry must not add a second env entry");
        assert_eq!(env[1].1, format!("existing{sep}{dir}"), "retry must not add a second path");
        env[1].1 = "override".into();
        leader_colors_env(&skills, "claude", "leader", Some("inherited"), &mut env).unwrap();
        assert_eq!(env[1].1, format!("override{sep}{dir}"), "explicit spawn env wins over inherited env");
        env[1].1.clear();
        leader_colors_env(&skills, "claude", "leader", None, &mut env).unwrap();
        assert_eq!(env[1].1, dir, "empty env must not inject a current-directory path");
        skills.remove_all();
    }

    #[test]
    fn start_options_take_fresh_over_ipc() {
        let o: StartOptions = serde_json::from_value(serde_json::json!({ "cols": 80, "fresh": true })).unwrap();
        assert!(o.fresh);
        let o: StartOptions = serde_json::from_value(serde_json::json!({ "cols": 80 })).unwrap();
        assert!(!o.fresh, "a plain start still resumes");
    }

    #[test]
    fn idle_delivery_respects_patron_typing_but_live_is_not_busy() {
        assert_eq!(delivery(Status::Offline, false, false), Delivery::Start);
        assert_eq!(delivery(Status::Idle, false, false), Delivery::Paste);
        assert_eq!(delivery(Status::Idle, true, false), Delivery::Queue);
        assert_eq!(delivery(Status::Working, false, false), Delivery::Queue);
        assert_eq!(delivery(Status::Waiting, false, false), Delivery::Queue);
        assert_eq!(delivery(Status::Idle, false, true), Delivery::Queue);
    }

    /// #671: effort as the CLI's env (claude engines) — none adds nothing, a given one wins over
    /// an earlier entry; codex gets it in argv instead (codex.rs). A respawn with no effort of its
    /// own (the chain/quota paths build fresh `StartOptions`) takes the running job's.
    #[test]
    fn effort_reaches_the_spawn_and_survives_a_respawn_mid_job() {
        use crate::effort::{Effort, CLAUDE_ENV};
        let mut env = vec![("DEX_WT".to_string(), "x".to_string())];
        effort_env("claude", None, &mut env);
        assert_eq!(env.len(), 1, "no effort: no env");
        effort_env("codex", Some(Effort::High), &mut env);
        assert_eq!(env.len(), 1, "codex: argv, not env");
        env.push((CLAUDE_ENV.into(), "low".into()));
        effort_env("claude", Some(Effort::Medium), &mut env);
        assert_eq!(env.iter().filter(|(k, _)| k == CLAUDE_ENV).collect::<Vec<_>>(), [&(CLAUDE_ENV.to_string(), "medium".to_string())]);

        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let kaan = crate::teams::get_agent(&conn, "kaan").unwrap();
        let timur = crate::teams::get_agent(&conn, "timur").unwrap();
        assert_eq!(spawn_effort(&conn, &kaan, None).unwrap(), None, "no job: none, as before");
        let from = crate::delegate::caller(&conn, "timur").unwrap();
        let d = crate::delegate::insert(&conn, crate::delegate::NewDelegation {
            from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None,
            prompt: "iş".into(), task_id: None, patron_words: None, effort: Some(Effort::Xhigh),
        })
        .unwrap();
        crate::delegate::set_status(&conn, d.id, "running").unwrap();
        let chain = StartOptions { prompt: Some("devam".into()), resume: Some("s1".into()), ..Default::default() };
        assert_eq!(spawn_effort(&conn, &kaan, chain.effort).unwrap(), Some(Effort::Xhigh), "chain respawn keeps the job's effort");
        assert_eq!(spawn_effort(&conn, &kaan, Some(Effort::Medium)).unwrap(), Some(Effort::Medium), "a given one wins");
        assert_eq!(spawn_effort(&conn, &timur, None).unwrap(), None, "leaders: none");
        let o: StartOptions = serde_json::from_value(serde_json::json!({ "effort": "xhigh" })).unwrap();
        assert_eq!(o.effort, None, "never from IPC");
    }

    #[test]
    fn hook_status_and_session_are_backend_state() {
        let states = States::default();
        states.started("kaan", "claude", false);
        assert_eq!(states.get("kaan").status, Status::Idle);
        states.hook("kaan", Some(Status::Working), Some("session-1"));
        assert_eq!(states.get("kaan").session_id.as_deref(), Some("session-1"));
        assert_eq!(states.get("kaan").status, Status::Working);
        states.exited("kaan");
        assert_eq!(states.get("kaan").status, Status::Offline);
    }

    /// #447: a start given a prompt is in its turn (a restart's blocker) before any hook says so;
    /// the first hook past SessionStart, or the exit, ends that.
    #[test]
    fn a_prompted_start_is_in_turn_until_its_turn_shows() {
        let states = States::default();
        states.started("kaan", "claude", false);
        assert!(!states.get("kaan").in_turn());
        states.started("kaan", "claude", true);
        assert!(states.get("kaan").in_turn() && states.get("kaan").status == Status::Idle);
        states.hook("kaan", Some(Status::Idle), Some("s1")); // SessionStart: not its turn yet
        assert!(states.get("kaan").in_turn());
        states.hook("kaan", Some(Status::Working), None);
        states.prompt_seen("kaan"); // UserPromptSubmit
        states.hook("kaan", Some(Status::Idle), None);
        assert!(!states.get("kaan").in_turn(), "its Stop ends it");
        states.started("kaan", "claude", true);
        states.exited("kaan");
        assert!(!states.get("kaan").in_turn(), "an exit too");
    }

    #[test]
    fn codex_pending_prompt_survives_session_start() {
        let states = States::default();
        let written_at = now().saturating_sub(1);
        states.started("kaan", "codex", true);
        states.hook("kaan", Some(Status::Idle), Some("session"));
        assert!(states.codex_observed("kaan", 1, Some("A"), Some(written_at)).is_some(),
            "SessionStart must not reject the prompted process's already written start");
    }

    #[test]
    fn codex_completion_preserves_a_newer_hook_boundary() {
        let states = States::default();
        states.started("kaan", "codex", false);
        states.hook("kaan", Some(Status::Idle), None);
        let hook_boundary = states.get("kaan").codex_after;
        assert!(states.codex_observed("kaan", 1, Some("B"), Some(hook_boundary)).is_some());
        assert_eq!(states.codex_completed("kaan", 1, "B", Some(hook_boundary - 1)).unwrap().status, Status::Idle);
        assert_eq!(states.get("kaan").codex_after, hook_boundary, "a delayed completion cannot move the hook boundary backwards");
        assert!(states.codex_observed("kaan", 1, Some("A"), Some(hook_boundary - 1)).is_none());
        assert!(states.codex_observed("kaan", 1, Some("C"), Some(hook_boundary)).is_some());
    }

    #[test]
    fn codex_unobserved_turn_is_bounded_by_its_stop() {
        let states = States::default();
        states.started("kaan", "codex", false);
        let written_at = now().saturating_sub(1);
        // Even A's Working signal may still be unread when its Stop arrives.
        states.hook("kaan", Some(Status::Idle), None);
        states.hook("kaan", Some(Status::Working), None);
        assert!(states.codex_observed("kaan", 1, Some("A"), Some(written_at)).is_none());
        assert!(states.codex_observed("kaan", 1, Some("A"), None).is_none(), "unproven timestamps cannot cross the boundary");
        assert_eq!(states.get("kaan").status, Status::Working);
        let new_start = states.get("kaan").codex_after;
        assert!(states.codex_observed("kaan", 1, Some("B"), Some(new_start)).is_some(),
            "a new start written before the Working hook remains valid");
        assert!(states.codex_completed("kaan", 1, "A", None).is_none());
        assert_eq!(states.codex_completed("kaan", 1, "B", None).unwrap().status, Status::Idle);
    }

    /// Synthetic hook race: the memory Write's PostToolUse was accepted before the Stop but its thread
    /// reached the status after it — the worker stayed "working" and its queue never moved.
    #[test]
    fn a_hook_applied_late_does_not_undo_a_newer_status() {
        let states = States::default();
        states.started("ayla", "claude", false);
        let post_tool = next_seq();
        let stop = next_seq();
        states.hook_seq("ayla", Some(Status::Idle), None, stop);
        states.hook_seq("ayla", Some(Status::Working), None, post_tool);
        assert_eq!(states.get("ayla").status, Status::Idle, "the older hook loses");
        assert!(states.silent_for("ayla") < 1_000, "it still counts as a hook (quiet window)");
        // Dex's own changes are newer than every hook accepted before them, exit included.
        let late = next_seq();
        states.hook("ayla", Some(Status::Working), None);
        states.hook_seq("ayla", Some(Status::Idle), None, late);
        assert_eq!(states.get("ayla").status, Status::Working);
        states.exited("ayla");
        states.hook_seq("ayla", Some(Status::Working), None, late);
        assert_eq!(states.get("ayla").status, Status::Offline);
        states.hook_seq("ayla", Some(Status::Idle), None, next_seq());
        assert_eq!(states.get("ayla").status, Status::Idle, "a new hook applies");
    }

    #[test]
    fn settled_screen_before_the_first_hook_is_waiting_once() {
        let states = States::default();
        assert!(states.blocked("kaan").is_none(), "never spawned");
        states.started("kaan", "claude", false);
        states.spawned("kaan");
        let s = states.blocked("kaan").expect("trust dialog → waiting");
        assert_eq!(s.status, Status::Waiting);
        assert!(states.blocked("kaan").is_none(), "emitted once");
        // Dialog answered: SessionStart (idle) ends the pre-session phase for good.
        states.hook("kaan", Some(Status::Idle), Some("s1"));
        assert!(!states.pre_session("kaan"));
        assert!(states.blocked("kaan").is_none());
        assert_eq!(states.get("kaan").status, Status::Idle);
        // "No, exit": the process ends in the dialog.
        states.spawned("kaan");
        states.blocked("kaan").unwrap();
        states.exited("kaan");
        assert!(states.blocked("kaan").is_none());
        assert_eq!(states.get("kaan").status, Status::Offline);
    }

    #[test]
    fn latest_session_only_resumes_on_the_same_engine() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        record_session(&conn, "kaan", "session-1", "claude", None).unwrap();
        assert_eq!(resumable_session(&conn, "kaan", "claude").unwrap().as_deref(), Some("session-1"));
        assert_eq!(resumable_session(&conn, "kaan", "codex").unwrap(), None);
        // FIX-4: a lost session goes; another agent's or another id's row stays.
        assert!(!forget_session(&conn, "kaan", "claude", "other").unwrap());
        assert!(forget_session(&conn, "kaan", "claude", "session-1").unwrap());
        assert_eq!(resumable_session(&conn, "kaan", "claude").unwrap(), None);
    }

    // DEX-17: StopFailure → zincir kararı + devretme koruma + bekleme yolu (spawn yok).
    #[test]
    fn chain_plan_preserves_and_wait_path_emits() {
        use std::sync::{Arc, Mutex};
        struct Got(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
        impl crate::EventSink for Got {
            fn emit(&self, event: &str, data: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), data));
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("dex-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let got = Arc::new(Mutex::new(Vec::new()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Got(got.clone())),
        )
        .unwrap();
        // GLM worker'ı için sentetik kota mesajı.
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
                provider: Some("glm".into()), model: Some("sonnet".into()), ..Default::default()
            })?;
            crate::chain::set_order(c, &["glm".into(), "codex".into(), "anthropic".into(), "muse".into(), "local".into()])?;
            Ok(())
        })
        .unwrap();
        let body = serde_json::json!({
            "hook_event_name": "StopFailure", "error": "rate_limit",
            "last_assistant_message": "API Error: 429 · [1308][Usage limit reached for 5 hour. Your limit will reset at 2099-01-01 12:00:00]",
        });
        // GLM'den sonraki son halka Codex; zincir listenin başına geri sarmaz.
        let plan = core.chain_plan("kaan", &body).expect("kalıcı kota → plan");
        assert_eq!(plan.current, "glm");
        assert_eq!(plan.next.as_deref(), Some("codex"));
        assert_eq!(plan.reset, crate::chain::parse_glm_reset("reset at 2099-01-01 12:00:00"));
        // Zincir kapalıysa plan YOK (normal failed akışı).
        core.chain_set_agent("kaan", true).unwrap();
        assert!(core.chain_plan("kaan", &body).is_none());
        core.chain_set_agent("kaan", false).unwrap();
        // Claude sağlayıcıları tükenince son halka Codex'tir.
        crate::db::with(&core.db, |c| {
            for p in ["anthropic", "muse", "local"] {
                crate::chain::mark_exhausted(c, p, Some(9_999_999_999_999), "test")?;
            }
            Ok(())
        })
        .unwrap();
        let wait = core.chain_plan("kaan", &body).expect("tükenmiş zincir de plan");
        assert_eq!(wait.next.as_deref(), Some("codex"));
        // Codex'in kendisi kullanılamazsa yürütücünün ulaştığı son durum: bekleme.
        core.chain_execute("kaan", ChainPlan { next: None, ..wait });
        // GLM kayda girdi, PTY açılmadı (bekleme), olaylar yayında.
        let events = got.lock().unwrap();
        assert!(events.iter().any(|(e, _)| e == "dex-chain-wait"), "{events:?}");
        assert!(!core.ptys.is_running("kaan"), "beklemede spawn yok");
        let gone = crate::db::with(&core.db, |c| crate::chain::exhausted(c)).unwrap();
        assert!(gone.iter().any(|x| x.provider == "glm"), "{gone:?}");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chain_reset_timer_clears_expired_and_emits_available() {
        assert_chain_reset_timer(60);
    }

    #[test]
    fn chain_reset_timer_past_reset_fires_immediately() {
        assert_chain_reset_timer(-1_000);
    }

    fn assert_chain_reset_timer(reset_offset_ms: i64) {
        use std::sync::{Arc, Mutex};
        struct Got(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
        impl crate::EventSink for Got {
            fn emit(&self, event: &str, data: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), data));
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("dex-chain-reset-{}-{reset_offset_ms}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let got = Arc::new(Mutex::new(Vec::new()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Got(got.clone())),
        )
        .unwrap();
        let reset = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64 + reset_offset_ms;
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "muse", Some(reset), "test")).unwrap();
        core.chain_schedule_reset("muse", reset);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < end
            && !got.lock().unwrap().iter().any(|(event, data)| event == "dex-chain-available" && data["provider"] == "muse")
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(got.lock().unwrap().iter().any(|(event, data)| {
            event == "dex-chain-available" && data["provider"] == "muse"
        }));
        assert!(!crate::db::with(&core.db, crate::chain::exhausted)
            .unwrap()
            .iter()
            .any(|row| row.provider == "muse"));
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    // Görev #94: geçici sağlayıcıda timeout → unreachable + ilerleme; asılda değişmez.
    #[test]
    fn chain_plan_temp_timeout_goes_unreachable() {
        use std::sync::{Arc, Mutex};
        struct Got(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
        impl crate::EventSink for Got {
            fn emit(&self, event: &str, data: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), data));
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("dex-chain-unreach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Got(Arc::new(Mutex::new(Vec::new())))),
        )
        .unwrap();
        let body = serde_json::json!({
            "hook_event_name": "StopFailure", "error": "timeout",
            "last_assistant_message": "Request timed out · attempt 4/10",
        });
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
                provider: Some("glm".into()), model: Some("sonnet".into()), ..Default::default()
            })?;
            crate::chain::set_order(c, &["muse".into(), "anthropic".into(), "glm".into(), "local".into(), "codex".into()])?;
            Ok(())
        })
        .unwrap();
        // Asıl sağlayıcıda timeout → plan YOK (davranış değişmez, normal retry akışı).
        assert!(core.chain_plan("kaan", &body).is_none(), "asıl sağlayıcıda transient zinciri çalıştırmaz");
        // Geçici sağlayıcıya geç (muse; jetonu yok ama plan kararı jetona bakmaz, pick_next bakar).
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET active_provider = 'muse', active_model = 'muse-spark-1.3' WHERE id = 'kaan'", [])
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .unwrap();
        let plan = core.chain_plan("kaan", &body).expect("geçicide timeout → plan");
        assert_eq!(plan.current, "muse");
        assert_eq!(plan.reason, "unreachable");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        let reset = plan.reset.expect("unreachable planı 15 dk sonraya kurulur");
        assert!((reset - (now + crate::chain::UNREACHABLE_EXHAUST_MS)).abs() < 10_000, "{reset}");
        // muse'tan sonra jetonlular: anthropic (yerleşik) — jetonsuz local atlanır.
        assert_eq!(plan.next.as_deref(), Some("anthropic"));
        // Geçicide 5xx / hız sınırı → plan YOK (yalnız ağ/timeout/connection sınıfı).
        for (error, msg) in [
            ("overloaded", "Overloaded"),
            ("server_error", "API Error: 503 Service Unavailable"),
            ("rate_limit", "429 Too Many Requests — slow down"),
        ] {
            let b = serde_json::json!({
                "hook_event_name": "StopFailure", "error": error, "last_assistant_message": msg,
            });
            assert!(core.chain_plan("kaan", &b).is_none(), "{error} geçicide bile zinciri çalıştırmaz");
        }
        // Geçicide kalıcı kota → eski yol (reason kota).
        let quota = serde_json::json!({
            "hook_event_name": "StopFailure", "error": "rate_limit",
            "last_assistant_message": "API Error: 429 · [1308][Usage limit reached for 5 hour. Your limit will reset at 2099-01-01 12:00:00]",
        });
        let kota = core.chain_plan("kaan", &quota).expect("geçicide kota → plan");
        assert_eq!((kota.current.as_str(), kota.reason.as_str()), ("muse", "kota"));
        // Zincir kapalıysa geçicide timeout bile plan üretmez.
        core.chain_set_agent("kaan", true).unwrap();
        assert!(core.chain_plan("kaan", &body).is_none());
        core.chain_set_agent("kaan", false).unwrap();
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    // Görev #94: unreachable yürütme — kayıt nedeni + 15 dk penceresi, spawn yok (bekleme).
    #[test]
    fn chain_execute_unreachable_marks_and_waits() {
        use std::sync::{Arc, Mutex};
        struct Got(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
        impl crate::EventSink for Got {
            fn emit(&self, event: &str, data: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), data));
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("dex-chain-unreach-exec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let got = Arc::new(Mutex::new(Vec::new()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Got(got.clone())),
        )
        .unwrap();
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
                provider: Some("glm".into()), model: Some("sonnet".into()), ..Default::default()
            })?;
            c.execute("UPDATE agents SET active_provider = 'muse', active_model = 'muse-spark-1.3' WHERE id = 'kaan'", [])
                .map(|_| ())
                .map_err(|e| e.to_string())?;
            crate::chain::set_order(c, &["muse".into(), "anthropic".into(), "glm".into(), "local".into(), "codex".into()])?;
            Ok(())
        })
        .unwrap();
        let body = serde_json::json!({
            "hook_event_name": "StopFailure", "error": "timeout",
            "last_assistant_message": "Request timed out · attempt 4/10",
        });
        let plan = core.chain_plan("kaan", &body).expect("geçicide timeout → plan");
        // Zincir bitmiş gibi yürüt (spawn yok): kayıt yine unreachable düşer.
        core.chain_execute("kaan", ChainPlan { next: None, ..plan });
        let rows = crate::db::with(&core.db, crate::chain::exhausted).unwrap();
        let row = rows.iter().find(|e| e.provider == "muse").expect("muse kayda girmeli");
        assert_eq!(row.reason, "unreachable");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        assert!((row.reset_at - (now + crate::chain::UNREACHABLE_EXHAUST_MS)).abs() < 10_000, "{row:?}");
        let events = got.lock().unwrap();
        assert!(events.iter().any(|(e, d)| e == "dex-chain-wait" && d["text"].as_str().is_some_and(|t| t.contains("yanıt vermiyor"))),
            "{events:?}");
        assert!(!core.ptys.is_running("kaan"), "beklemede spawn yok");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    // Görev #94: sıfırlamada çalışan ajan pending'e düşer (oturum kapanınca döner).
    #[test]
    fn chain_return_defers_while_running() {
        use std::sync::{Arc, Mutex};
        struct Got(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
        impl crate::EventSink for Got {
            fn emit(&self, event: &str, data: serde_json::Value) -> Result<(), String> {
                self.0.lock().unwrap().push((event.into(), data));
                Ok(())
            }
        }
        struct ShellSink;
        impl crate::PtySink for ShellSink {
            fn data(&mut self, _: &[u8]) -> bool { true }
            fn exit(&mut self) {}
            fn resized(&mut self, _cols: u16, _rows: u16) {}
        }
        let root = std::env::temp_dir().join(format!("dex-chain-return-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let got = Arc::new(Mutex::new(Vec::new()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Got(got.clone())),
        )
        .unwrap();
        // Kaan aslı muse, geçicide glm.
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
                provider: Some("muse".into()), model: Some("muse-spark-1.3".into()), ..Default::default()
            })?;
            c.execute("UPDATE agents SET active_provider = 'glm', active_model = 'GLM-5.3' WHERE id = 'kaan'", [])
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .unwrap();
        // Çalışan oturum: gerçek kabuk PTY'si (CLI spawn yok).
        core.pty_spawn(crate::pty::Spawn { id: "kaan".into(), cmd: crate::shell::SHELL_CMD.into(), cols: 120, rows: 40, ..Default::default() }, ShellSink).unwrap();
        assert!(core.ptys.is_running("kaan"));
        core.chain_return("muse");
        let a = crate::db::with(&core.db, |c| crate::teams::get_agent(c, "kaan")).unwrap();
        assert_eq!(a.active_provider.as_deref(), Some("glm"), "çalışanın geçicisi korunur");
        let pending: i64 = crate::db::with(&core.db, |c| {
            c.query_row("SELECT chain_return_pending FROM agents WHERE id = 'kaan'", [], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(pending, 1, "çalışan ajan pending'e düşer");
        {
            // Kilit kill'den önce bırakılır: çıkış olayı aynı sink'e yazar.
            let events = got.lock().unwrap();
            assert!(events.iter().any(|(e, d)| e == "dex-chain-available"
                && d["agentId"] == "kaan" && d["pending"] == true
                && d["text"].as_str().is_some_and(|t| t.contains("oturumu kapanınca"))), "{events:?}");
        }
        // Temizlik: iniş tetiklenmesin diye bayrağı indir, sonra öldür (yoksa kill → chain_back → gerçek spawn).
        crate::db::with(&core.db, |c| crate::teams::set_chain_return_pending(c, "kaan", false)).unwrap();
        core.ptys.kill("kaan");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    // Görev #94: pending inişi — asıl sağlayıcı hâlâ tükenmişse bayrak düşer, ajan kalır (spawn yok).
    #[test]
    fn chain_return_pending_lands_when_still_exhausted() {
        let root = std::env::temp_dir().join(format!("dex-chain-landing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
                provider: Some("muse".into()), model: Some("muse-spark-1.3".into()), ..Default::default()
            })?;
            c.execute("UPDATE agents SET active_provider = 'glm', active_model = 'GLM-5.3' WHERE id = 'kaan'", [])
                .map(|_| ())
                .map_err(|e| e.to_string())?;
            crate::chain::mark_exhausted(c, "muse", Some(9_999_999_999_999), "kota")?;
            crate::teams::set_chain_return_pending(c, "kaan", true)?;
            Ok(())
        })
        .unwrap();
        core.delegation_session_exited("kaan").unwrap();
        let (a, pending) = crate::db::with(&core.db, |c| {
            let a = crate::teams::get_agent(c, "kaan")?;
            let p: i64 = c.query_row("SELECT chain_return_pending FROM agents WHERE id = 'kaan'", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            Ok((a, p))
        })
        .unwrap();
        assert_eq!(pending, 0, "bayrak düşer");
        assert_eq!(a.active_provider.as_deref(), Some("glm"), "tükenmişse ajan geçicide kalır");
        assert!(!core.ptys.is_running("kaan"), "dönüş yok, spawn yok");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chain_hook_pins_source_engine_and_guard_defers_pending() {
        let root = std::env::temp_dir().join(format!("dex-chain-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let transcript = root.join("source.jsonl");
        std::fs::write(&transcript, r#"{"type":"user","message":{"role":"user","content":"devam"}}"#).unwrap();
        crate::db::with(&core.db, |c| {
            c.execute(
                "UPDATE agents SET active_engine = 'codex', active_model = 'gpt-5.6-terra',
                 pending_engine = 'codex', pending_model = 'gpt-5.6-terra' WHERE id = 'kaan'",
                [],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
        core.session_on_hook("kaan", "StopFailure", "claude-source", transcript.to_str(), Some("claude"));
        let engine = crate::db::with(&core.db, |c| {
            c.query_row("SELECT engine FROM agent_sessions WHERE agent_id = 'kaan'", [], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(engine, "claude", "geç kalan kaynak hook active_engine'e yazılmamalı");

        core.chain_guard.lock().unwrap().insert("kaan".into());
        core.delegation_session_exited("kaan").unwrap();
        let guarded = crate::db::with(&core.db, |c| crate::teams::get_agent(c, "kaan")).unwrap();
        assert!(guarded.pending.is_some(), "zincir guard'ı pending hedefi uygulamamalı");
        assert_eq!(guarded.engine, "claude");
        core.chain_guard.lock().unwrap().remove("kaan");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chain_spawn_failure_rolls_back_transient_target_and_stays_offline() {
        let root = std::env::temp_dir().join(format!("dex-chain-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        crate::db::with(&core.db, |c| {
            c.execute(
                "UPDATE agents SET active_engine = 'codex', active_provider = 'glm', active_model = 'gpt-5.6-terra' WHERE id = 'kaan'",
                [],
            )
            .map_err(|e| e.to_string())?;
            save_chain_origin(c, "kaan", Some("source"), None)?;
            restore_chain_state(c, "kaan", &(None, None, None), true)
        })
        .unwrap();
        core.chain_spawn_failed("kaan", "spawn testi");
        let (agent, origins) = crate::db::with(&core.db, |c| {
            let a = crate::teams::get_agent(c, "kaan")?;
            let n = c
                .query_row("SELECT COUNT(*) FROM agent_chain_origins WHERE agent_id = 'kaan'", [], |row| row.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            Ok((a, n))
        })
        .unwrap();
        assert!(agent.active_engine.is_none() && agent.active_provider.is_none() && agent.active_model.is_none());
        assert_eq!(origins, 0);
        assert!(!core.ptys.is_running("kaan"));
        assert_eq!(core.agent_states.get("kaan").status, Status::Offline);
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    // DEX-17: zincir geçişinde devretme `failed` OLMAZ (aynı iş sürer); normal StopFailure eder.
    #[test]
    fn chain_preserved_stop_failure_keeps_delegation_running() {
        let root = std::env::temp_dir().join(format!("dex-chain-dlg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let body = serde_json::json!({"hook_event_name": "StopFailure"});
        let mk_running = || {
            crate::db::with(&core.db, |c| {
                let l = crate::delegate::caller(c, "timur")?;
                let d = crate::delegate::insert(c, crate::delegate::NewDelegation {
                    from: &l, to_agent: "kaan".into(), team_id: "default".into(),
                    service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None,
                })?;
                crate::delegate::set_status(c, d.id, "running")?;
                Ok(d.id)
            })
            .unwrap()
        };
        let id = mk_running();
        core.delegation_on_hook("kaan", "StopFailure", &body, true);
        let status = crate::db::with(&core.db, |c| crate::delegate::get(c, id)).unwrap().status;
        assert_eq!(status, "running", "zincir korunurken failed OLMAZ");
        let id = mk_running();
        core.delegation_on_hook("kaan", "StopFailure", &body, false);
        let status = crate::db::with(&core.db, |c| crate::delegate::get(c, id)).unwrap().status;
        assert_eq!(status, "failed", "korumasız StopFailure failed eder");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_transcript_with_a_conversation_is_resumable() {        let dir = std::env::temp_dir().join(format!("dex-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, lines: &[&str]| {
            let p = dir.join(name);
            std::fs::write(&p, lines.join("\n")).unwrap();
            p
        };
        // What claude 2.1 leaves after a prompt that got no reply (measured, FIX-4).
        let meta = [r#"{"type":"last-prompt","lastPrompt":"x"}"#, r#"{"type":"mode","mode":"default"}"#, r#"{"type":"permission-mode"}"#];
        assert!(!resumable_transcript(&file("meta.jsonl", &meta)));
        let mut talk = meta.to_vec();
        talk.push(r#"{"type":"user","message":{"role":"user","content":"x"}}"#);
        assert!(resumable_transcript(&file("talk.jsonl", &talk)));
        assert!(resumable_transcript(&file("codex.jsonl", &[r#"{"type":"session_meta","payload":{}}"#])));
        assert!(!resumable_transcript(&dir.join("missing.jsonl")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Shell fixtures still have to service ConPTY's cursor-inheritance query. Discarding it can
/// block ClosePseudoConsole on windows-2022; use the same terminal replies as agent sessions.
#[cfg(test)]
fn shell_sink_for_test(core: &crate::Core, id: &str, sink: impl crate::PtySink) -> impl crate::PtySink {
    struct ShellSink<S> { terminal: AgentSink, inner: S }
    impl<S: crate::PtySink> crate::PtySink for ShellSink<S> {
        fn data(&mut self, bytes: &[u8]) -> bool {
            self.terminal.data(bytes);
            self.inner.data(bytes)
        }
        fn exit(&mut self) { self.inner.exit(); }
        fn resized(&mut self, cols: u16, rows: u16) { self.inner.resized(cols, rows); }
    }
    ShellSink { terminal: AgentSink { core: core.weak_self.clone(), id: id.into(), tail: String::new() }, inner: sink }
}

/// #18: a job queued while the patron had a draft goes once the draft ends — event-driven, once.
#[cfg(test)]
mod draft_tests {
    use crate::{delegate, events, pty, shell, with, Core, Events, Paths, PtySink};
    use std::time::{Duration, Instant};

    struct Sink;
    impl PtySink for Sink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
    }

    #[test]
    fn shell_fixture_answers_conpty_cursor_query() {
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
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("dex-cursor-{}", std::process::id()));
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let mut sink = super::shell_sink_for_test(&core, "kaan", Sink);
        core.pty_spawn(pty::Spawn { id: "kaan".into(), cmd: shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() }, super::shell_sink_for_test(&core, "kaan", Sink)).unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        core.ptys.input_writer_for_test("kaan", |original| Box::new(Input { original, bytes: bytes.clone() }));
        // portable-pty requests cursor inheritance even when the child is just a shell.
        // Older Windows ConPTY waits for this reply when ClosePseudoConsole is called.
        assert!(sink.data(b"\x1b[6n"));
        let reply = bytes.lock().unwrap().clone();
        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
        assert!(reply.ends_with(b"\x1b[1;1R"), "the shell fixture must service cursor inheritance: {reply:?}");
    }

    #[test]
    fn emptied_draft_delivers_the_queued_job_after_the_quiet_window_once() {
        let root = std::env::temp_dir().join(format!("dex-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink))
            .unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
        core.pty_spawn(pty::Spawn { id: "kaan".into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, super::shell_sink_for_test(&core, "kaan", Sink))
            .unwrap();
        // The patron's session (Görev #20: a Dex session would be replaced, not pasted into).
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron" }), false);
        core.agent_states.hook("kaan", Some(super::Status::Idle), None);
        core.agent_input("kaan", true); // the patron types into the idle worker's prompt

        let d = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &from, to_agent: "kaan".into(), team_id: "default".into(),
                    service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None,
                },
            )
        })
        .unwrap();
        assert_eq!(core.delegation_dispatch(d.clone()).unwrap().status, "queued", "draft: no paste");
        let first = core.queue_retry.lock().unwrap()["kaan"];

        std::thread::sleep(Duration::from_millis(300));
        core.agent_input("kaan", false); // … and deletes it all
        let cleared = Instant::now();
        let fresh = core.queue_retry.lock().unwrap()["kaan"];
        assert_ne!(fresh, first, "the emptied draft replaces the pending retry");

        // The older retry wakes first (inside the quiet window) and leaves without touching anything.
        std::thread::sleep(Duration::from_millis(5_000));
        assert_eq!(core.queue_retry.lock().unwrap().get("kaan"), Some(&fresh), "stale retry exits silently");

        let status = |core: &Core| with(&core.db, |c| delegate::get(c, d.id)).unwrap().status;
        while status(&core) == "queued" && cleared.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(status(&core), "running");
        assert!(cleared.elapsed() < Duration::from_millis(5_800), "within ~5 s of the draft ending: {:?}", cleared.elapsed());
        assert_eq!(core.agent_states.get("kaan").status, super::Status::Working);
        assert!(core.queue_retry.lock().unwrap().is_empty(), "delivered once: no retry left to paste again");

        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #126: a worker session silent past IDLE_CLOSE_MS with no job and Dex's turn is closed (its
    /// pane is told why); a recent hook, a queued or running job, the patron's turn and leaders
    /// keep theirs. Shell PTYs stand in for claude: the close starts nothing (no job is queued).
    #[test]
    fn only_an_idle_worker_without_work_is_closed() {
        use std::sync::{Arc, Mutex};
        struct Rec(Arc<Mutex<Vec<u8>>>);
        impl PtySink for Rec {
            fn data(&mut self, b: &[u8]) -> bool { self.0.lock().unwrap().extend_from_slice(b); true }
            fn exit(&mut self) {}
        }
        let root = std::env::temp_dir().join(format!("dex-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink))
            .unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        let out = Arc::new(Mutex::new(Vec::new()));
        for id in ["kaan", "ayla", "timur"] {
            let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
            let spawn = pty::Spawn { id: id.into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() };
            if id == "kaan" { core.pty_spawn(spawn, super::shell_sink_for_test(&core, id, Rec(out.clone()))) }
            else { core.pty_spawn(spawn, super::shell_sink_for_test(&core, id, Sink)) }.unwrap();
        }
        let idle = |id: &str| {
            core.agent_states.hook(id, Some(super::Status::Idle), None);
            core.agent_states.change(id, |s| s.hook_at -= super::IDLE_CLOSE_MS + 1_000);
        };

        // Only workers are watched.
        core.idle_close_watch("timur");
        core.idle_close_watch("kaan");
        assert!(!core.idle_close.lock().unwrap().contains_key("timur"));
        assert!(core.idle_close.lock().unwrap().contains_key("kaan"));

        core.agent_states.hook("kaan", Some(super::Status::Idle), None);
        assert!(!core.idle_close_now("kaan"), "a hook just came");
        idle("timur");
        assert!(!core.idle_close_now("timur"), "leaders are never closed");

        let job = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &from, to_agent: "kaan".into(), team_id: "default".into(),
                    service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None,
                },
            )
        })
        .unwrap();
        idle("kaan");
        assert!(!core.idle_close_now("kaan"), "a queued job keeps the session");
        with(&core.db, |c| delegate::set_status(c, job.id, "running")).unwrap();
        assert!(!core.idle_close_now("kaan"), "so does a running one (open questions ride on it)");
        with(&core.db, |c| delegate::set_status(c, job.id, "done")).unwrap();

        // The patron is talking in ayla's session.
        core.delegation_on_hook("ayla", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron" }), false);
        idle("ayla");
        assert!(!core.idle_close_now("ayla"), "the patron's conversation stays");

        core.agent_states.hook("kaan", Some(super::Status::Waiting), None);
        core.agent_states.change("kaan", |s| s.hook_at -= super::IDLE_CLOSE_MS + 1_000);
        assert!(!core.idle_close_now("kaan"), "a dialog waits on the patron");

        idle("kaan");
        assert!(core.idle_close_now("kaan"));
        assert!(!core.ptys.is_running("kaan"));
        assert_eq!(core.agent_states.get("kaan").status, super::Status::Offline);
        assert!(!core.resetting.lock().unwrap().contains_key("kaan"), "the exit drops the reset mark");
        assert!(String::from_utf8_lossy(&out.lock().unwrap()).contains("Dex oturumu kapattı"), "the pane says why");
        assert!(core.ptys.is_running("ayla") && core.ptys.is_running("timur"));

        for id in ["ayla", "timur"] {
            core.ptys.kill(id);
        }
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Synthetic stale-worker scenario: the Stop came, then a late hook left "working"; no hook
    /// follows an idle session, so the job queued behind it waited forever. The queue's retry is
    /// the watchdog: silent past STALE_WORKING_MS + the transcript's turn over → idle → delivered.
    #[test]
    fn a_stale_working_worker_whose_turn_ended_gets_its_queued_job() {
        let root = std::env::temp_dir().join(format!("dex-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink))
            .unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        let cwd = Some(root.join("cwd").to_string_lossy().into_owned());
        core.pty_spawn(pty::Spawn { id: "kaan".into(), cmd: shell::SHELL_CMD.into(), cwd, cols: 80, rows: 24, ..Default::default() }, super::shell_sink_for_test(&core, "kaan", Sink))
            .unwrap();
        // The patron's session: the job is pasted, no real claude is started (core-tests-no-real-claude).
        core.delegation_on_hook("kaan", "UserPromptSubmit", &serde_json::json!({ "prompt": "patron" }), false);
        let transcript = root.join("t.jsonl");
        let write = |rows: &[&str]| std::fs::write(&transcript, rows.join("\n")).unwrap();
        core.agent_states.set_transcript("kaan", &transcript.to_string_lossy());
        let stale = |core: &Core| {
            core.agent_states.hook("kaan", Some(super::Status::Working), None);
            core.agent_states.change("kaan", |s| s.hook_at -= super::STALE_WORKING_MS + 1_000);
        };
        let job = with(&core.db, |c| {
            let from = delegate::caller(c, "timur")?;
            delegate::insert(
                c,
                delegate::NewDelegation {
                    from: &from, to_agent: "kaan".into(), team_id: "default".into(),
                    service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None,
                },
            )
        })
        .unwrap();
        let status = |core: &Core| with(&core.db, |c| delegate::get(c, job.id)).unwrap().status;

        // Mid-turn (a long tool, a long reply): silent but really working — never touched.
        write(&[r#"{"type":"user","message":{"content":"patron"}}"#, r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]}}"#]);
        stale(&core);
        assert_eq!(core.delegation_dispatch(job.clone()).unwrap().status, "queued");
        std::thread::sleep(Duration::from_millis(1_200));
        assert_eq!(status(&core), "queued");
        assert_eq!(core.agent_states.get("kaan").status, super::Status::Working);

        // The turn is over on disk: the next look settles it and the job goes in.
        write(&[
            r#"{"type":"user","message":{"content":"patron"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"bitti"}]}}"#,
            r#"{"type":"system","subtype":"stop_hook_summary"}"#,
            r#"{"type":"system","subtype":"turn_duration"}"#,
            r#"{"type":"ai-title"}"#,
        ]);
        stale(&core);
        let t = Instant::now();
        core.delegation_dispatch(job.clone()).unwrap();
        while status(&core) == "queued" && t.elapsed() < Duration::from_secs(4) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(status(&core), "running", "delivered by the after-Stop path");
        assert!(t.elapsed() < Duration::from_millis(2_500), "{:?}", t.elapsed());

        core.ptys.kill("kaan");
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }
}

// Görev #250: proaktif Claude kota eşiği. Gerçek CLI spawn'ı yok: oturumlar kabuk PTY'si, zincir
// seçimi kasasız enjekte edilir; eşik geçişinin kendisi chain_execute (yukarıdaki zincir testleri).
#[cfg(test)]
mod quota_tests {
    use super::*;
    use crate::quota::{Usage, UsageSource};

    struct Fixed(Mutex<Usage>);
    impl UsageSource for Fixed {
        fn read(&self) -> Result<Usage, String> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    struct ShellSink;
    impl crate::PtySink for ShellSink {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) {}
        fn resized(&mut self, _cols: u16, _rows: u16) {}
    }

    type Pick = (Option<(String, Option<String>, Option<String>, String)>, Vec<String>);

    fn core(tag: &str) -> (Arc<crate::Core>, std::path::PathBuf, Arc<Fixed>) {
        let root = std::env::temp_dir().join(format!("dex-quota-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let src = Arc::new(Fixed(Mutex::new(Usage::default())));
        core.quota_set_source(src.clone());
        (core, root, src)
    }

    fn set(src: &Fixed, five: f64, week: f64) {
        *src.0.lock().unwrap() = Usage { five_hour: five, seven_day: week, ..Default::default() };
    }

    fn agent(core: &crate::Core, id: &str) -> crate::teams::Agent {
        crate::db::with(&core.db, |c| crate::teams::get_agent(c, id)).unwrap()
    }

    fn muse(_: &str) -> Pick {
        (Some(("muse".into(), None, Some("muse".into()), "muse-spark-1.3".into())), Vec::new())
    }

    fn on_glm(core: &crate::Core, id: &str) {
        crate::db::with(&core.db, |c| {
            crate::teams::update_agent(c, id, crate::teams::AgentPatch {
                provider: Some("glm".into()), model: Some("GLM-5.3".into()), ..Default::default()
            })
            .map(|_| ())
        })
        .unwrap();
    }

    /// timur lider, kaan/ayla/bora worker; timur/kaan/ayla'nın elinde süren iş, hepsinin canlı
    /// oturumu var. ayla GLM'de, bora boşta.
    fn busy_team(core: &crate::Core) {
        on_glm(core, "ayla");
        crate::db::with(&core.db, |c| {
            for (n, to) in ["timur", "kaan", "ayla"].iter().enumerate() {
                c.execute(
                    "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(?1,'timur',?2,'default','x','running',0,0)",
                    params![n as i64 + 1, to],
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .unwrap();
        for id in ["timur", "kaan", "ayla", "bora"] {
            core.pty_spawn(crate::pty::Spawn { id: id.into(), cmd: crate::shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() }, shell_sink_for_test(core, id, ShellSink)).unwrap();
        }
    }

    fn finish(core: Arc<crate::Core>, root: std::path::PathBuf) {
        for id in ["timur", "kaan", "ayla", "bora"] {
            core.ptys.kill(id);
        }
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_busy_claude_workers_are_targets_never_the_leader() {
        let (core, root, _) = core("targets");
        busy_team(&core);
        assert_eq!(core.quota_targets(), vec!["kaan".to_string()]);
        crate::db::with(&core.db, |c| c.execute("UPDATE agents SET chain_off = 1 WHERE id = 'kaan'", []).map_err(|e| e.to_string())).unwrap();
        assert!(core.quota_targets().is_empty(), "zinciri kapalı worker hariç");
        finish(core, root);
    }

    #[test]
    fn below_threshold_nothing_moves_and_empty_chain_keeps_workers_on_claude() {
        let (core, root, src) = core("tick");
        busy_team(&core);
        set(&src, 84.9, 79.9);
        assert!(core.quota_tick());
        assert!(core.ptys.is_running("kaan"));
        assert!(agent(&core, "kaan").active_provider.is_none());
        assert!(!core.quota.lock().unwrap().warned);
        // Haftalık pencere artık eşik değil: %99'da da kimse taşınmaz.
        set(&src, 10.0, 99.0);
        assert!(core.quota_tick());
        assert!(agent(&core, "kaan").active_provider.is_none(), "haftalık doluluk worker'ı taşımaz");
        assert!(!core.quota.lock().unwrap().warned);
        // Eşik üstü (5 sa 85) ama zincirde Claude dışında açık hedef yok: iş durmaz.
        crate::db::with(&core.db, |c| {
            crate::chain::set_disabled(c, &["muse".into(), "glm".into(), "local".into(), "codex".into()]).map(|_| ())
        })
        .unwrap();
        set(&src, 85.0, 10.0);
        assert!(core.quota_tick());
        let kaan = agent(&core, "kaan");
        assert!(kaan.active_provider.is_none() && kaan.active_engine.is_none(), "Claude'da kalır");
        assert!(core.ptys.is_running("kaan"), "oturum öldürülmez");
        assert!(core.quota.lock().unwrap().warned, "uyarı bir kez");
        assert!(crate::db::with(&core.db, crate::chain::exhausted).unwrap().is_empty(), "eşik Claude'u tükenmiş saymaz");
        set(&src, 10.0, 10.0);
        core.quota_tick();
        assert!(!core.quota.lock().unwrap().warned, "eşik altında uyarı bayrağı sıfırlanır");
        finish(core, root);
    }

    #[test]
    fn new_worker_opens_on_next_provider_above_threshold_and_back_on_claude_below() {
        let (core, root, src) = core("route");
        set(&src, 50.0, 50.0);
        core.quota_tick();
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert!(agent(&core, "kaan").active_provider.is_none(), "eşik altı: yönlendirme yok");

        // 5 saatlik 85: yeni worker doğrudan Muse'de; lider Claude'da.
        set(&src, 85.0, 10.0);
        core.quota_tick();
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        let kaan = agent(&core, "kaan");
        assert_eq!((kaan.active_provider.as_deref(), kaan.active_model.as_deref()), (Some("muse"), Some("muse-spark-1.3")));
        core.quota_route_by("timur", &mut StartOptions::default(), &muse);
        assert!(agent(&core, "timur").active_provider.is_none(), "lider eşikten etkilenmez");
        on_glm(&core, "ayla");
        core.quota_route_by("ayla", &mut job_start("iş".into(), vec![]), &muse);
        assert!(agent(&core, "ayla").active_provider.is_none(), "Claude dışı worker etkilenmez");

        // Eşik altında resume ile yeniden açılış geri taşınmaz; yeni (fresh) oturum Claude'da.
        set(&src, 10.0, 10.0);
        core.quota_tick();
        core.quota_route_by("kaan", &mut StartOptions { resume: Some("s1".into()), ..Default::default() }, &muse);
        assert_eq!(agent(&core, "kaan").active_provider.as_deref(), Some("muse"), "süren oturum geri taşınmaz");
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        let kaan = agent(&core, "kaan");
        assert!(kaan.active_provider.is_none() && kaan.active_model.is_none(), "yeni worker Claude'da");

        // Claude gerçekten tükenmiş kayıtlıysa eşik altı da geri döndürmez.
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET active_provider = 'muse', active_model = 'muse-spark-1.3' WHERE id = 'kaan'", []).map_err(|e| e.to_string())?;
            crate::chain::mark_exhausted(c, "anthropic", Some(9_999_999_999_999), "kota")
        })
        .unwrap();
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert_eq!(agent(&core, "kaan").active_provider.as_deref(), Some("muse"));
        finish(core, root);
    }

    #[test]
    fn codex_target_drops_the_claude_resume() {
        let (core, root, src) = core("codex");
        set(&src, 95.0, 10.0);
        core.quota_tick();
        let mut o = StartOptions { resume: Some("claude-session".into()), ..Default::default() };
        core.quota_route_by("kaan", &mut o, &|_| (Some(("codex".into(), Some("codex".into()), None, "gpt-x".into())), Vec::new()));
        assert!(o.resume.is_none() && o.fresh);
        assert_eq!(agent(&core, "kaan").active_engine.as_deref(), Some("codex"));
        finish(core, root);
    }

    #[test]
    fn threshold_plan_never_marks_claude_exhausted_or_parks_the_worker() {
        let (core, root, _) = core("plan");
        core.chain_execute("kaan", ChainPlan {
            current: "anthropic".into(), next: None, reset: None, reason: "esik".into(),
            session: None, transcript: None,
            threshold: Some("Claude %85 (5 sa)".into()), threshold_reset: None,
        });
        assert!(crate::db::with(&core.db, crate::chain::exhausted).unwrap().is_empty());
        assert_ne!(core.agent_states.get("kaan").status, Status::Waiting, "eşikte iş durmaz");
        assert!(agent(&core, "kaan").active_provider.is_none());
        finish(core, root);
    }

    // ── #1018: sıradaki motorun BİLİNEN kotası ──

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
    }

    /// Sentetik Codex rollout'u: haftalık pencere `used` dolu, çağıranın `reset_s` anında sıfırlanır.
    fn codex_rollout(root: &std::path::Path, used: f64, reset_s: i64) {
        let day = root.join("codex-sessions").join("2002").join("01").join("01");
        std::fs::create_dir_all(&day).unwrap();
        let line = format!(
            r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"codex","primary":{{"used_percent":{used},"window_minutes":10080,"resets_at":{reset_s}}},"secondary":null}}}}}}"#
        );
        std::fs::write(day.join("rollout-2002-01-01T12-00-20-x.jsonl"), line).unwrap();
        CODEX_SESSIONS.with(|s| *s.borrow_mut() = Some(root.join("codex-sessions")));
    }

    /// Zincirde yalnız Codex açık (Claude'dan sonra).
    fn only_codex(core: &crate::Core) {
        crate::db::with(&core.db, |c| crate::chain::set_disabled(c, &["muse".into(), "glm".into(), "local".into()]).map(|_| ())).unwrap();
    }

    fn notes(core: &crate::Core, lead: &str) -> Vec<String> {
        crate::db::with(&core.db, |c| {
            let mut st = c.prepare("SELECT text FROM leader_notes WHERE agent_id = ?1 ORDER BY id").map_err(|e| e.to_string())?;
            let rows = st.query_map([lead], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })
        .unwrap()
    }

    #[test]
    fn unknown_codex_quota_keeps_codex_a_candidate() {
        let (core, root, _) = core("codex-unknown");
        only_codex(&core);
        let next = |core: &crate::Core| {
            core.chain_measure();
            crate::db::with(&core.db, |c| crate::chain::pick_next(c, "anthropic", &|_| true)).unwrap()
        };
        // Ölçüm kaynağı yok: eski davranış.
        assert_eq!(next(&core).as_deref(), Some("codex"));
        // Kotası var / sıfırlanması geçmiş: aday.
        codex_rollout(&root, 60.0, now() / 1000 + 3_600);
        assert_eq!(next(&core).as_deref(), Some("codex"));
        codex_rollout(&root, 100.0, now() / 1000 - 60);
        assert_eq!(next(&core).as_deref(), Some("codex"));
        assert!(crate::db::with(&core.db, crate::chain::exhausted).unwrap().is_empty());
        // Bitmiş ve sıfırlanması gelecekte: atlanır, saatiyle tükenmiş kaydına girer.
        let reset_s = now() / 1000 + 3_600;
        codex_rollout(&root, 100.0, reset_s);
        assert_eq!(next(&core), None);
        let rows = crate::db::with(&core.db, crate::chain::exhausted).unwrap();
        assert_eq!(rows.iter().map(|e| (e.provider.as_str(), e.reset_at)).collect::<Vec<_>>(), vec![("codex", reset_s * 1000)]);
        CODEX_SESSIONS.with(|s| s.borrow_mut().take());
        finish(core, root);
    }

    #[test]
    fn threshold_with_codex_dry_stays_on_claude_and_notifies_once() {
        let (core, root, src) = core("codex-dry");
        busy_team(&core);
        only_codex(&core);
        codex_rollout(&root, 100.0, now() / 1000 + 3_600);
        set(&src, 90.0, 10.0);
        assert!(core.quota_tick());
        let kaan = agent(&core, "kaan");
        assert!(kaan.active_provider.is_none() && kaan.active_engine.is_none(), "Codex'e geçmez, Claude'da kalır");
        assert!(core.ptys.is_running("kaan"), "oturum öldürülmez");
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("zincirde kotası olan motor yok, en erken sıfırlanma "), "{}", got[0]);
        // Sonraki ölçüm ve aynı bölümde açılan worker yeni bildirim üretmez.
        assert!(core.quota_tick());
        core.quota_route("kaan", &mut job_start("iş".into(), vec![]));
        assert!(agent(&core, "kaan").active_engine.is_none(), "taze spawn da Codex'te açılmaz");
        assert_eq!(notes(&core, "timur").len(), 1, "tek bildirim");
        CODEX_SESSIONS.with(|s| s.borrow_mut().take());
        finish(core, root);
    }

    #[test]
    fn stop_failure_with_every_motor_dry_waits_with_earliest_reset() {
        let (core, root, _) = core("codex-wait");
        busy_team(&core);
        only_codex(&core);
        let codex_reset = now() / 1000 + 7_200;
        codex_rollout(&root, 100.0, codex_reset);
        let claude_reset = now() + 3_600_000;
        core.chain_measure();
        core.chain_execute("kaan", ChainPlan {
            current: "anthropic".into(), next: None, reset: Some(claude_reset), reason: "kota".into(),
            session: None, transcript: None, threshold: None, threshold_reset: None,
        });
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting, "mevcut motor da bitti: iş bekler");
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0], format!("kaan: zincirde kotası olan motor yok, en erken sıfırlanma {} — iş bekliyor.", ms_to_iso(claude_reset)));
        assert!(codex_reset * 1000 > claude_reset);
        CODEX_SESSIONS.with(|s| s.borrow_mut().take());
        finish(core, root);
    }

    #[test]
    fn measured_dry_claude_blocks_return_and_chain_back() {
        let (core, root, src) = core("claude-dry");
        // kaan Claude'dan Muse'a taşınmış; Claude'un haftalık penceresi dolu (5 sa eşiği altında).
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET active_provider = 'muse', active_model = 'muse-spark-1.3' WHERE id = 'kaan'", [])
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .unwrap();
        let week = ms_to_iso(now() + 86_400_000);
        *src.0.lock().unwrap() = Usage { five_hour: 10.0, seven_day: 100.0, seven_day_resets_at: Some(week.clone()), ..Default::default() };
        assert!(core.quota_tick());
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert_eq!(agent(&core, "kaan").active_provider.as_deref(), Some("muse"), "dolu Claude'a taze spawn dönmez");
        let err = core.chain_back("kaan").unwrap_err();
        assert!(err.contains("hâlâ tükenmiş"), "{err}");
        let rows = crate::db::with(&core.db, crate::chain::exhausted).unwrap();
        assert_eq!(rows.iter().map(|e| (e.provider.as_str(), e.reset_at)).collect::<Vec<_>>(), vec![("anthropic", crate::chain::parse_rfc3339(&week).unwrap())]);
        finish(core, root);
    }

    #[test]
    fn stale_reading_does_not_route() {
        let (core, root, src) = core("stale");
        set(&src, 99.0, 99.0);
        core.quota_tick();
        core.quota.lock().unwrap().last.as_mut().unwrap().0 -= crate::quota::STALE_MS + 1;
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert!(agent(&core, "kaan").active_provider.is_none(), "bayat ölçüm = bilinmiyor");
        finish(core, root);
    }

    // ── #1027 ──

    #[test]
    fn weekly_full_claude_routes_a_fresh_worker_and_notes_once_with_the_reason() {
        let (core, root, src) = core("week-full");
        busy_team(&core);
        let week = ms_to_iso(now() + 86_400_000);
        *src.0.lock().unwrap() = Usage { five_hour: 10.0, seven_day: 100.0, seven_day_resets_at: Some(week), ..Default::default() };
        assert!(core.quota_tick());
        // Hedef yok (Codex hazır değil): Claude'da açılır, nedeni erişim — tek not.
        let none = |_: &str| -> Pick { (None, vec!["Codex kullanılamıyor: giriş yok".into()]) };
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &none);
        assert!(agent(&core, "kaan").active_provider.is_none());
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("kaan: Claude %100 (hafta) ama zincirde jetonu/erişimi olan motor yok (Codex kullanılamıyor: giriş yok)"), "{}", got[0]);
        assert!(core.quota_tick(), "haftalık dolu sürerken ölçüm bildirimi sıfırlamaz");
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &none);
        assert_eq!(notes(&core, "timur").len(), 1, "tek bildirim");
        // Hedef varsa bilinen-bitmiş Claude'da hiç açılmaz (boşa tur yok).
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert_eq!(agent(&core, "kaan").active_provider.as_deref(), Some("muse"));
        finish(core, root);
    }

    #[test]
    fn below_threshold_on_claude_skips_the_codex_scan() {
        let (core, root, src) = core("no-scan");
        codex_rollout(&root, 100.0, now() / 1000 + 3_600);
        set(&src, 10.0, 10.0);
        core.quota_tick();
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        assert!(crate::db::with(&core.db, crate::chain::exhausted).unwrap().is_empty(), "seçim yok: ölçüm yok");
        set(&src, 90.0, 10.0);
        core.quota_tick();
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &muse);
        let rows = crate::db::with(&core.db, crate::chain::exhausted).unwrap();
        assert_eq!(rows.iter().map(|e| e.provider.as_str()).collect::<Vec<_>>(), vec!["codex"], "eşik üstü seçimde ölçülür");
        CODEX_SESSIONS.with(|s| s.borrow_mut().take());
        finish(core, root);
    }

    /// Codex'in kota biten turu (codex-cli 0.154): önce %100 `token_count`, sonra hatalı `task_complete`.
    fn codex_limit_turn(file: &std::path::Path, reset_s: i64) {
        codex_limit_turn_at(file, reset_s, now());
    }

    /// [`codex_limit_turn`], tur sonu `at_ms` zaman damgalı (rollout satırları damgalı yazılır).
    fn codex_limit_turn_at(file: &std::path::Path, reset_s: i64, at_ms: i64) {
        use std::io::Write;
        let ts = ms_to_iso(at_ms);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
        writeln!(f, r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"codex","primary":{{"used_percent":100.0,"window_minutes":10080,"resets_at":{reset_s}}},"secondary":null}}}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"t","last_agent_message":null,"error":{{"message":"You’ve hit your usage limit.","codex_error_info":"usage_limit_exceeded"}}}}}}"#).unwrap();
    }

    fn on_codex(core: &crate::Core, id: &str) {
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET active_engine = 'codex', active_model = 'gpt-x' WHERE id = ?1", [id]).map(|_| ()).map_err(|e| e.to_string())
        })
        .unwrap();
    }

    fn exhausted_rows(core: &crate::Core) -> Vec<(String, i64)> {
        crate::db::with(&core.db, crate::chain::exhausted).unwrap().into_iter().map(|e| (e.provider, e.reset_at)).collect()
    }

    #[test]
    fn codex_limit_mid_job_returns_to_free_claude() {
        let (core, root, _) = core("codex-limit-back");
        busy_team(&core);
        on_codex(&core, "kaan");
        let file = root.join("rollout-kaan.jsonl");
        let reset_s = now() / 1000 + 7_200;
        codex_limit_turn(&file, reset_s);
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_codex_limit("kaan", &file);
        let starts = CAPTURE.with(|c| c.borrow_mut().take().unwrap());
        let kaan = agent(&core, "kaan");
        assert!(kaan.active_engine.is_none() && kaan.active_provider.is_none(), "asıl Claude'a döndü");
        assert_eq!(starts.len(), 1, "tek yeniden başlatma");
        assert_eq!(exhausted_rows(&core), vec![("codex".to_string(), reset_s * 1000)], "Codex rollout'taki sıfırlanmayla tükenmiş");
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("kaan: Codex → ") && got[0].ends_with("ile sürüyor — iş bölünmedi."), "{}", got[0]);
        finish(core, root);
    }

    #[test]
    fn codex_limit_with_claude_dry_waits_with_one_note() {
        let (core, root, _) = core("codex-limit-wait");
        busy_team(&core);
        on_codex(&core, "kaan");
        let claude_reset = now() + 3_600_000;
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "anthropic", Some(claude_reset), "kota")).unwrap();
        let file = root.join("rollout-kaan.jsonl");
        codex_limit_turn(&file, now() / 1000 + 7_200);
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_codex_limit("kaan", &file);
        assert!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).is_empty(), "başlatma yok");
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting);
        assert_eq!(agent(&core, "kaan").active_engine.as_deref(), Some("codex"));
        let got = notes(&core, "timur");
        assert_eq!(got, vec![format!("kaan: zincirde kotası olan motor yok, en erken sıfırlanma {} — iş bekliyor.", ms_to_iso(claude_reset))]);
        // Claude sıfırlanınca bekleyen (geçici motoru tükenmiş) oturum kapanışı beklemeden döner.
        crate::db::with(&core.db, |c| crate::chain::clear_exhausted(c, "anthropic")).unwrap();
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_return("anthropic");
        assert_eq!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).len(), 1);
        assert!(agent(&core, "kaan").active_engine.is_none(), "bekleme Claude'a dönüşle biter");
        finish(core, root);
    }

    #[test]
    fn codex_engine_agent_waits_and_others_are_ignored() {
        let (core, root, _) = core("codex-native");
        busy_team(&core);
        let file = root.join("rollout-kaan.jsonl");
        let reset_s = now() / 1000 + 7_200;
        codex_limit_turn(&file, reset_s);
        // Claude'da koşan ajan: sinyal onun değil.
        core.chain_codex_limit("kaan", &file);
        assert!(exhausted_rows(&core).is_empty());
        crate::db::with(&core.db, |c| c.execute("UPDATE agents SET engine = 'codex' WHERE id = 'kaan'", []).map(|_| ()).map_err(|e| e.to_string())).unwrap();
        core.chain_codex_limit("kaan", &file);
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting);
        assert_eq!(exhausted_rows(&core), vec![("codex".to_string(), reset_s * 1000)]);
        assert_eq!(notes(&core, "timur"), vec![format!("kaan: Codex kotası doldu, en erken sıfırlanma {} — iş bekliyor.", ms_to_iso(reset_s * 1000))]);
        finish(core, root);
    }

    #[test]
    fn leaving_codex_starts_fresh_with_the_codex_handoff() {
        let (core, root, _) = core("codex-leave");
        on_codex(&core, "kaan");
        let file = root.join("rollout-kaan.jsonl");
        std::fs::write(&file, r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"göç betiğini yaz"}]}}
"#).unwrap();
        codex_limit_turn(&file, now() / 1000 + 7_200);
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_execute("kaan", ChainPlan {
            current: "codex".into(), next: Some("anthropic".into()), reset: None, reason: "kota".into(),
            session: None, transcript: Some(file.to_string_lossy().into_owned()), threshold: None, threshold_reset: None,
        });
        let starts = CAPTURE.with(|c| c.borrow_mut().take().unwrap());
        assert_eq!(starts.len(), 1);
        assert!(!starts[0].0.args.iter().any(|a| a == "--resume"), "Codex oturumu Claude'da sürdürülemez: yeni oturum");
        assert!(starts[0].0.args.iter().any(|a| a.contains("göç betiğini yaz")), "devir Codex rollout'undan, ilk turda: {:?}", starts[0].0.args);
        assert_eq!(agent(&core, "kaan").active_engine, None);
        finish(core, root);
    }

    /// Uçtan uca: canlı sohbet izleyicisi (dosya olayı) kota hatalı turu görür → ajan bekler.
    #[test]
    fn the_live_rollout_watch_catches_the_limit() {
        let (core, root, _) = core("codex-watch");
        busy_team(&core);
        on_codex(&core, "kaan");
        // Dönüş adayı yok: yol spawn'sız bekletmede biter (izleyici thread'i CAPTURE görmez).
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "anthropic", Some(now() + 3_600_000), "kota")).unwrap();
        let file = root.join("rollout-kaan.jsonl");
        std::fs::write(&file, "").unwrap();
        // #1033: ensure izleyici dinlemeye başlayınca döner — uyku yok.
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "codex", &file);
        codex_limit_turn(&file, now() / 1000 + 7_200);
        let t = std::time::Instant::now();
        while core.agent_states.get("kaan").status != Status::Waiting && t.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting);
        assert!(exhausted_rows(&core).iter().any(|(p, _)| p == "codex"));
        core.chat_watches.drop_watch("kaan");
        finish(core, root);
    }

    // ── #1033 ──

    fn wait_status(core: &crate::Core, id: &str, want: Status) -> bool {
        let t = std::time::Instant::now();
        while core.agent_states.get(id).status != want && t.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        core.agent_states.get(id).status == want
    }

    #[test]
    fn unreachable_mark_leaves_a_healthy_session_alone() {
        let (core, root, _) = core("unreachable-alone");
        busy_team(&core);
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET active_provider = 'glm', active_model = 'GLM-5.3' WHERE id = 'kaan'", []).map_err(|e| e.to_string())?;
            // Başka bir ajan GLM'i kısa süreliğine yanıtsız işaretledi; kaan GLM'de sağlıklı çalışıyor.
            crate::chain::mark_exhausted(c, "glm", Some(now() + 600_000), "unreachable")
        })
        .unwrap();
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_return("anthropic");
        assert!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).is_empty(), "tur ortasında öldürülmez");
        assert!(core.ptys.is_running("kaan"));
        let pending = crate::db::with(&core.db, |c| crate::teams::take_chain_return_pending(c, "kaan")).unwrap();
        assert!(pending, "dönüş oturum kapanışına ertelenir");
        assert_eq!(agent(&core, "kaan").active_provider.as_deref(), Some("glm"));
        // Kota tükenmesi (ölçülmüş/StopFailure) oturumu ilerleyemez kılar: hemen döner.
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "glm", Some(now() + 600_000), "kota")).unwrap();
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_return("anthropic");
        assert_eq!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).len(), 1);
        assert!(agent(&core, "kaan").active_provider.is_none());
        finish(core, root);
    }

    #[test]
    fn codex_reset_continues_a_native_codex_agent_waiting_on_quota() {
        let (core, root, _) = core("codex-resume");
        busy_team(&core);
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET engine = 'codex' WHERE id IN ('kaan', 'bora')", []).map(|_| ()).map_err(|e| e.to_string())
        })
        .unwrap();
        let file = root.join("rollout-kaan.jsonl");
        codex_limit_turn(&file, now() / 1000 + 7_200);
        assert!(core.chain_codex_limit("kaan", &file));
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting);
        // bora başka bir nedenle bekliyor (izin penceresi): Codex sıfırlanması ona yazmaz.
        core.agent_states.hook("bora", Some(Status::Waiting), None);
        crate::db::with(&core.db, |c| crate::chain::clear_exhausted(c, "codex")).unwrap();
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.chain_return("codex");
        assert!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).is_empty(), "aynı oturumda sürer, yeniden başlatma yok");
        let kaan = core.agent_states.get("kaan");
        assert_eq!(kaan.status, Status::Working, "devam turu verildi");
        assert!(!kaan.chain_wait);
        assert_eq!(core.agent_states.get("bora").status, Status::Waiting);
        finish(core, root);
    }

    #[test]
    fn a_guarded_limit_is_not_swallowed_by_the_watch() {
        let (core, root, _) = core("codex-guarded");
        busy_team(&core);
        on_codex(&core, "kaan");
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "anthropic", Some(now() + 3_600_000), "kota")).unwrap();
        let file = root.join("rollout-kaan.jsonl");
        std::fs::write(&file, "").unwrap();
        // Eşzamanlı geçiş sürüyor: çağrı işlenmez.
        core.chain_guard.lock().unwrap().insert("kaan".into());
        assert!(!core.chain_codex_limit("kaan", &file));
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "codex", &file);
        codex_limit_turn(&file, now() / 1000 + 7_200);
        // Guard'lı çağrının koşması için pay; yük altında geç koşarsa test yalnız zayıflar, kırmızı olmaz.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_ne!(core.agent_states.get("kaan").status, Status::Waiting);
        core.chain_guard.lock().unwrap().remove("kaan");
        // Guard'lı tur işareti tutmaz: aynı oturumun sonraki kota turu zincire gider. (Eski davranışta
        // işaret kalırdı ve hiçbir tekrar tetiklemezdi; tur, işaret inene dek yeniden yazılır.)
        let t = std::time::Instant::now();
        while core.agent_states.get("kaan").status != Status::Waiting && t.elapsed() < std::time::Duration::from_secs(10) {
            codex_limit_turn(&file, now() / 1000 + 7_200);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert_eq!(core.agent_states.get("kaan").status, Status::Waiting);
        core.chat_watches.drop_watch("kaan");
        finish(core, root);
    }

    #[test]
    fn a_limit_written_before_the_watch_still_counts_if_fresh() {
        let (core, root, _) = core("codex-early");
        busy_team(&core);
        on_codex(&core, "kaan");
        crate::db::with(&core.db, |c| crate::chain::mark_exhausted(c, "anthropic", Some(now() + 3_600_000), "kota")).unwrap();
        // Eski (resume edilen) oturumun bayat kota hatası: tetiklemez.
        let old = root.join("rollout-old.jsonl");
        codex_limit_turn_at(&old, now() / 1000 + 7_200, now() - 2 * crate::chain::CODEX_RECENT_MS);
        // Karar ensure dönmeden verilir (tetik yoksa thread de yok): uyku gerekmez.
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "codex", &old);
        assert_ne!(core.agent_states.get("kaan").status, Status::Waiting, "bayat hata zinciri tetiklemez");
        // Taze oturum: ilk tur izleyici SessionStart'ta kurulmadan kota hatasıyla bitti.
        let fresh = root.join("rollout-kaan.jsonl");
        codex_limit_turn(&fresh, now() / 1000 + 7_200);
        core.chat_watches.ensure(core.weak_self.clone(), "kaan", "codex", &fresh);
        assert!(wait_status(&core, "kaan", Status::Waiting));
        assert!(exhausted_rows(&core).iter().any(|(p, _)| p == "codex"));
        core.chat_watches.drop_watch("kaan");
        finish(core, root);
    }

    #[test]
    fn threshold_note_does_not_silence_the_weekly_full_note() {
        let (core, root, src) = core("warn-split");
        busy_team(&core);
        crate::db::with(&core.db, |c| {
            crate::chain::set_disabled(c, &["muse".into(), "glm".into(), "local".into(), "codex".into()]).map(|_| ())
        })
        .unwrap();
        set(&src, 90.0, 10.0);
        assert!(core.quota_tick());
        assert_eq!(notes(&core, "timur").len(), 1, "eşik notu");
        let week = ms_to_iso(now() + 86_400_000);
        *src.0.lock().unwrap() = Usage { five_hour: 10.0, seven_day: 100.0, seven_day_resets_at: Some(week), ..Default::default() };
        assert!(core.quota_tick());
        let none = |_: &str| -> Pick { (None, Vec::new()) };
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &none);
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[1].starts_with("kaan: Claude %100 (hafta) ama "), "{}", got[1]);
        assert!(core.quota_tick());
        core.quota_route_by("kaan", &mut job_start("iş".into(), vec![]), &none);
        assert_eq!(notes(&core, "timur").len(), 2, "tükenme bölümünde tek not");
        finish(core, root);
    }

    #[test]
    fn threshold_switch_note_carries_the_real_skip_reason() {
        let (core, root, src) = core("switch-skip");
        busy_team(&core);
        drop(src);
        core.quota.lock().unwrap().last = Some((now(), Usage { five_hour: 90.0, seven_day: 10.0, ..Default::default() }));
        let breach = core.quota.lock().unwrap().breach_at(now()).unwrap();
        let none = |_: &str| -> Pick { (None, vec!["Codex kullanılamıyor: giriş yok".into()]) };
        core.quota_switch_by(&breach, &none);
        let got = notes(&core, "timur");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("kaan: Claude %90 (5 sa) ama zincirde jetonu/erişimi olan motor yok (Codex kullanılamıyor: giriş yok)"),
            "{}",
            got[0]
        );
        assert!(agent(&core, "kaan").active_engine.is_none() && core.ptys.is_running("kaan"), "Claude'da sürer");
        finish(core, root);
    }
}

// #838 D: sessions stay in their job's project (P) and worktree (W). No real CLI starts: agent
// starts are captured ([`CAPTURE`]) and a shell stands in; projects are real fixture folders.
#[cfg(test)]
mod launch_tests {
    use super::*;
    use crate::projects::{self, tests::Fixture, NewProject, Project};
    use std::path::{Path, PathBuf};

    struct Env {
        core: Arc<crate::Core>,
        root: PathBuf,
        f: Fixture,
        p1: Project,
        p2: Project,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            for id in ["timur", "kaan", "ayla", "bora"] {
                stop(&self.core, id);
            }
            self.core.shutdown();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The default team with P1 and P2 bound (`projects: false`: the old folderless team only;
    /// p1/p2 then stand for its one unbound project).
    fn env(tag: &str, projects: bool) -> Env {
        let root = std::env::temp_dir().join(format!("dex-launch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let f = Fixture::new();
        let mut made = Vec::new();
        for name in ["p1", "p2"] {
            let dir = f.dir(name);
            let ready = projects::prepare_root(dir.to_str().unwrap(), None).unwrap();
            let p = crate::db::with(&core.db, |c| {
                if !projects {
                    return projects::get(c, "default", "legacy-default").map_err(|e| e.to_string());
                }
                projects::create(c, "default", NewProject { name: name.into(), worktrees: None }, &ready).map_err(|e| e.to_string())
            })
            .unwrap();
            made.push(p);
        }
        let p2 = made.pop().unwrap();
        let p1 = made.pop().unwrap();
        Env { core, root, f, p1, p2 }
    }

    fn sql(e: &Env, q: &str, p: &[&dyn rusqlite::ToSql]) {
        crate::db::with(&e.core.db, |c| c.execute(q, p).map(|_| ()).map_err(|e| e.to_string())).unwrap();
    }

    fn job(e: &Env, id: i64, agent: &str, p: &Project, status: &str, effort: Option<&str>) {
        sql(e, "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,project_id,project_root,merge_branch,effort,created_at,updated_at)
                VALUES(?1,'timur',?2,'default','iş',?3,?4,?5,?6,?7,1,1)",
            &[&id, &agent, &status, &p.id, &p.root_path, &p.default_branch, &effort]);
    }

    /// Job `id`'s worktree (a Cargo workspace: its env carries `CARGO_TARGET_DIR`).
    fn worktree(e: &Env, id: i64, agent: &str, p: &Project) -> String {
        let dir = e.f.0.join(format!("wt-{agent}-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let path = dir.to_string_lossy().into_owned();
        sql(e, "INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(?1,?2,?3,?4,?5,'base',1)",
            &[&id, &agent, &p.root_path, &path, &format!("dex/{agent}/{id}")]);
        path
    }

    fn target(p: &Project, agent: &str) -> (String, String) {
        ("CARGO_TARGET_DIR".into(), crate::worktree::target_dir(Path::new(p.root_path.as_deref().unwrap()), agent).to_string_lossy().into_owned())
    }

    /// A session row as its hooks would leave it.
    fn saved(e: &Env, agent: &str, session: &str, engine: &str, b: &Binding) {
        sql(e, "INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id,cwd) VALUES(?1,?2,?3,1,?4,?5,?6)
                ON CONFLICT(agent_id) DO UPDATE SET session_id=excluded.session_id, engine=excluded.engine,
                project_id=excluded.project_id, delegation_id=excluded.delegation_id, cwd=excluded.cwd",
            &[&agent, &session, &engine, &b.project_id, &b.delegation_id, &b.cwd]);
    }

    fn row(e: &Env, agent: &str) -> Option<SessionRow> {
        crate::db::with(&e.core.db, |c| session_row(c, agent)).unwrap()
    }

    fn bound(p: &Project, job: i64, cwd: &str) -> Binding {
        Binding { project_id: Some(p.id.clone()), delegation_id: Some(job), cwd: Some(cwd.into()) }
    }

    /// Runs `f` with this thread's agent starts captured.
    fn captured<T>(f: impl FnOnce() -> T) -> (T, Vec<Captured>) {
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        let out = f();
        (out, CAPTURE.with(|c| c.borrow_mut().take().unwrap()))
    }

    /// Closes a session without its exit starting queued work (`chain_guard`, see lib.rs).
    fn stop(core: &crate::Core, id: &str) {
        core.chain_guard.lock().unwrap().insert(id.into());
        core.ptys.kill(id);
        core.chain_guard.lock().unwrap().remove(id);
    }

    fn resumed(req: &crate::pty::Spawn) -> Option<String> {
        req.args.iter().position(|a| a == "--resume").map(|i| req.args[i + 1].clone())
    }

    #[test]
    fn a_fresh_leader_binds_the_default_project_and_keeps_it_on_resume() {
        let e = env("leader-default", true);
        e.core.project_set_default("default", Some(&e.p2.id)).unwrap();
        let (r, got) = captured(|| e.core.agent_spawn("timur", StartOptions::default()));
        r.unwrap();
        let expected = Binding { project_id: Some(e.p2.id.clone()), delegation_id: None, cwd: e.p2.root_path.clone() };
        assert_eq!(row(&e, "timur").unwrap().binding, expected);
        assert_eq!((got[0].0.cwd.as_deref(), got[0].1), (e.p2.root_path.as_deref(), None));
        assert_eq!(e.core.session_projects(Some("default")).unwrap().get("timur"),
            Some(&projects::SessionProject { project_id: Some(e.p2.id.clone()), delegation_id: None }));
        assert_eq!(e.core.project_archive("default", &e.p2.id).unwrap_err(), "project_busy");
        crate::db::with(&e.core.db, |c| record_session(c, "timur", "L2", "claude", None)).unwrap();
        stop(&e.core, "timur");
        e.core.project_set_default("default", Some(&e.p1.id)).unwrap();
        let (r, got) = captured(|| e.core.agent_spawn("timur", StartOptions::default()));
        r.unwrap();
        assert_eq!(row(&e, "timur").unwrap().binding, expected);
        assert_eq!(resumed(&got[0].0).as_deref(), Some("L2"));
        stop(&e.core, "timur");
        let (r, _) = captured(|| e.core.agent_spawn("timur", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        assert_eq!(row(&e, "timur").unwrap().binding.project_id.as_deref(), Some(e.p1.id.as_str()));
    }

    #[test]
    fn a_folderless_fresh_leader_keeps_the_legacy_unbound_start() {
        let e = env("leader-folderless", false);
        saved(&e, "timur", "L1", "claude", &Binding::default());
        let (r, got) = captured(|| e.core.agent_spawn("timur", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        assert!(row(&e, "timur").is_none());
        assert_eq!((resumed(&got[0].0), got[0].0.cwd.as_deref(), got[0].1), (None, None, None));
        assert!(e.core.session_projects(Some("default")).unwrap().is_empty());
    }

    fn session_context(e: &Env, agent: &str) -> crate::project_context::ProjectContext {
        crate::db::with(&e.core.db, |c| {
            crate::project_context::resolve(c, &e.core.paths.data_dir, crate::project_context::ResolveRequest {
                team_id: "default", agent_id: Some(agent), delegation_id: None, project_id: None,
                mode: crate::project_context::ContextMode::Session,
            })
            .map_err(|e| e.to_string())
        })
        .unwrap()
    }

    /// Claude and Codex job starts: the dispatch's job (G passes `d.id`) binds the session to
    /// its P and W before the first hook — row, MCP key job, folder, env; nothing resumed.
    #[test]
    fn a_job_start_binds_its_project_and_worktree_before_the_first_hook() {
        let e = env("job", true);
        job(&e, 1, "kaan", &e.p1, "done", None);
        let w1 = worktree(&e, 1, "kaan", &e.p1);
        job(&e, 2, "kaan", &e.p2, "queued", None);
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &w1));
        let mut o = job_start("[Dex] yeni iş #2".into(), vec![]);
        o.delegation_id = Some(2);
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!(*key_job, Some(2), "MCP key and codex trust bound to the launch job");
        assert_eq!(req.cwd.as_deref(), Some(w2.as_str()), "its worktree, found from the job alone");
        assert!(req.env.contains(&target(&e.p2, "kaan")), "{:?}", req.env);
        assert_eq!(resumed(req), None);
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding.clone()), (NO_SESSION, bound(&e.p2, 2, &w2)), "the P1 session is abandoned");
        let ctx = session_context(&e, "kaan");
        assert_eq!((ctx.project_id.as_deref(), ctx.delegation_id, ctx.cwd.to_str()), (Some(e.p2.id.as_str()), Some(2), Some(w2.as_str())),
            "SessionStart sees the job's project already");
        assert!(crate::db::with(&e.core.db, |c| resumable_session(c, "kaan", "claude")).unwrap().is_none());

        // Codex: same binding; its trust holds the folder it starts in (no trust prompt there).
        sql(&e, "UPDATE agents SET engine = 'codex', model = 'gpt-x' WHERE id = 'ayla'", &[]);
        job(&e, 3, "ayla", &e.p1, "queued", None);
        let w3 = worktree(&e, 3, "ayla", &e.p1);
        let mut o = job_start("iş".into(), vec![]);
        o.delegation_id = Some(3);
        o.cwd = Some(w3.clone()); // the dispatch's worktree, as today
        let (r, got) = captured(|| e.core.agent_spawn("ayla", o));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!((*key_job, req.cmd.as_str(), req.cwd.as_deref()), (Some(3), "codex", Some(w3.as_str())));
        assert!(req.codex.as_ref().is_some_and(|l| l.resume.is_none()));
        assert_eq!(row(&e, "ayla").unwrap().binding, bound(&e.p1, 3, &w3));
        let trust = crate::db::with(&e.core.db, |c| crate::codex::trust_dirs(c, "ayla", true, req.cwd.as_deref(), *key_job)).unwrap();
        assert_eq!(trust, [w3.as_str()]);

        // Never another agent's job.
        let mut o = job_start("iş".into(), vec![]);
        o.delegation_id = Some(2);
        stop(&e.core, "ayla");
        let (r, _) = captured(|| e.core.agent_spawn("ayla", o));
        assert_eq!(r.unwrap_err(), "project_not_found");
    }

    /// Queued project switch: a live P1 session never takes a P2 job by paste (G's dispatch asks
    /// [`crate::Core::session_fits`]); a job start never resumes another project's session.
    #[test]
    fn another_projects_queue_neither_pastes_nor_resumes_the_session() {
        let e = env("switch", true);
        job(&e, 1, "kaan", &e.p1, "done", None);
        let w1 = worktree(&e, 1, "kaan", &e.p1);
        job(&e, 2, "kaan", &e.p2, "queued", None);
        job(&e, 4, "kaan", &e.p1, "queued", None);
        let get = |id| crate::db::with(&e.core.db, |c| crate::delegate::get(c, id)).unwrap();
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &w1));
        assert!(!e.core.session_fits(&get(2)), "P2 job, P1 session: it waits (patron's conversation kept)");
        assert!(e.core.session_fits(&get(4)), "same project: today's paste");
        saved(&e, "kaan", "s1", "claude", &Binding::default());
        assert!(!e.core.session_fits(&get(4)), "an unbound session of a several-project team names no project");

        // A non-fresh job start of P2 opens a fresh session; of P1, it may resume P1's.
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &w1));
        let o = StartOptions { prompt: Some("iş".into()), delegation_id: Some(2), ..Default::default() };
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        assert_eq!(resumed(&got[0].0), None, "no resume across projects");
        assert_eq!(got[0].0.cwd.as_deref(), e.p2.root_path.as_deref(), "no worktree: the P2 root, not the team folder");
        assert_eq!(row(&e, "kaan").unwrap().session, NO_SESSION);
        stop(&e.core, "kaan");
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &w1));
        let o = StartOptions { prompt: Some("iş".into()), delegation_id: Some(4), ..Default::default() };
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        assert_eq!(resumed(&got[0].0).as_deref(), Some("s1"), "same project: as before");
    }

    /// Old single-project (folderless) team: unbound sessions, same starts and paste as before.
    #[test]
    fn a_legacy_team_starts_exactly_as_before() {
        let e = env("legacy", false);
        job(&e, 1, "kaan", &e.p1, "queued", None);
        let get = |id| crate::db::with(&e.core.db, |c| crate::delegate::get(c, id)).unwrap();
        assert!(e.core.session_fits(&get(1)), "no session row");
        saved(&e, "kaan", "s1", "claude", &Binding::default());
        assert!(e.core.session_fits(&get(1)), "unbound session, the team's one project");

        saved(&e, "timur", "L1", "claude", &Binding::default());
        let (r, got) = captured(|| e.core.agent_spawn("timur", StartOptions::default()));
        r.unwrap();
        assert_eq!((resumed(&got[0].0).as_deref(), got[0].0.cwd.as_deref(), got[0].1), (Some("L1"), None, None));
        let r = row(&e, "timur").unwrap();
        assert_eq!((r.session.as_str(), r.binding), ("L1", Binding::default()), "the leader's row as it was");

        // Today's dispatch shape (no job id yet): unbound, its folder as given.
        let dir = e.f.0.join("p1").to_string_lossy().into_owned();
        let mut o = job_start("iş".into(), vec![]);
        o.cwd = Some(dir.clone());
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        assert_eq!((got[0].0.cwd.as_deref(), got[0].1), (Some(dir.as_str()), None));
        assert!(row(&e, "kaan").is_none(), "unbound: no row until the CLI names its session, as before");
    }

    /// The patron's chat (or a restart) opening an offline worker resumes its session where it ran.
    #[test]
    fn a_plain_start_resumes_in_the_bound_worktree_and_stays_in_its_project() {
        let e = env("plain", true);
        job(&e, 2, "kaan", &e.p2, "done", None);
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        saved(&e, "kaan", "s1", "claude", &bound(&e.p2, 2, &w2));
        let chat = || StartOptions { prompt: Some("selam".into()), ..Default::default() };
        let (r, got) = captured(|| e.core.agent_spawn("kaan", chat()));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!((resumed(req).as_deref(), req.cwd.as_deref(), *key_job), (Some("s1"), Some(w2.as_str()), Some(2)));
        assert!(req.env.contains(&target(&e.p2, "kaan")));
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding), ("s1", bound(&e.p2, 2, &w2)));

        // Its worktree merged and gone: the P2 root, never another project's folder.
        stop(&e.core, "kaan");
        sql(&e, "UPDATE delegation_worktrees SET removed_at = 1 WHERE delegation_id = 2", &[]);
        std::fs::remove_dir_all(&w2).unwrap();
        let (r, got) = captured(|| e.core.agent_spawn("kaan", chat()));
        r.unwrap();
        assert_eq!(got[0].0.cwd.as_deref(), e.p2.root_path.as_deref());
        assert_eq!(row(&e, "kaan").unwrap().binding.cwd.as_deref(), e.p2.root_path.as_deref());
    }

    /// Regression: chain and quota respawns used to start with no folder (the team's) — the
    /// session lost its worktree, env and so its `--resume`. They keep the running job's P/W/effort.
    #[test]
    fn chain_and_quota_respawns_keep_the_jobs_project_worktree_and_effort() {
        let e = env("chain", true);
        crate::db::with(&e.core.db, |c| {
            crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch { provider: Some("glm".into()), model: Some("GLM-5.3".into()), ..Default::default() })
                .map(|_| ())
        })
        .unwrap();
        job(&e, 2, "kaan", &e.p2, "queued", Some("xhigh"));
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        let mut o = job_start("[Dex] yeni iş #2".into(), vec![]);
        o.delegation_id = Some(2);
        let (r, _) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        sql(&e, "UPDATE delegations SET status = 'running' WHERE id = 2", &[]);
        crate::db::with(&e.core.db, |c| record_session(c, "kaan", "s1", "claude", None)).unwrap();
        assert_eq!(row(&e, "kaan").unwrap().binding, bound(&e.p2, 2, &w2), "the session's first Stop keeps the binding");

        let plans = [
            ChainPlan { current: "glm".into(), next: Some("anthropic".into()), reset: None, reason: "kota".into(),
                session: Some("s1".into()), transcript: None, threshold: None, threshold_reset: None },
            // quota_switch's shape: a threshold move of the running worker.
            ChainPlan { current: "anthropic".into(), next: Some("anthropic".into()), reset: None, reason: "esik".into(),
                session: Some("s1".into()), transcript: None, threshold: Some("Claude %85 (5 sa)".into()), threshold_reset: None },
        ];
        for plan in plans {
            let (_, got) = captured(|| e.core.chain_execute("kaan", plan));
            let (req, key_job) = got.last().expect("respawned");
            assert_eq!((resumed(req).as_deref(), req.cwd.as_deref(), *key_job), (Some("s1"), Some(w2.as_str()), Some(2)));
            assert!(req.env.contains(&target(&e.p2, "kaan")), "{:?}", req.env);
            assert!(req.env.contains(&(crate::effort::CLAUDE_ENV.into(), "xhigh".into())), "{:?}", req.env);
            assert_eq!(row(&e, "kaan").unwrap().binding, bound(&e.p2, 2, &w2));
        }

        // To Codex (chain_execute's codex start, and a quota route at start): fresh, same binding.
        stop(&e.core, "kaan");
        sql(&e, "UPDATE agents SET active_engine = 'codex', active_model = 'gpt-x' WHERE id = 'kaan'", &[]);
        let launch = row(&e, "kaan").unwrap().binding;
        let o = StartOptions { prompt: Some("devam".into()), fresh: true, launch: Some(launch), ..Default::default() };
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!((req.cmd.as_str(), req.cwd.as_deref(), *key_job), ("codex", Some(w2.as_str()), Some(2)));
        assert!(req.codex.as_ref().is_some_and(|l| l.resume.is_none() && l.effort == Some(crate::effort::Effort::Xhigh)));
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding), (NO_SESSION, bound(&e.p2, 2, &w2)));
        crate::db::with(&e.core.db, |c| {
            let team = crate::teams::get_team(c, &e.core.paths.data_dir, "default")?;
            let a = crate::teams::get_agent(c, "kaan")?;
            // quota_route turned a continuing start fresh (Codex): the caller did not renew it.
            let o = StartOptions { fresh: true, ..Default::default() };
            let plan = launch_plan(c, &e.core.paths.data_dir, &a, &team, "codex", &o, false)?;
            assert_eq!((plan.binding, plan.cwd.as_deref(), plan.resume), (bound(&e.p2, 2, &w2), Some(w2.as_str()), None));
            Ok(())
        })
        .unwrap();
    }

    /// Back to Claude: the origin session resumes only on the binding it ran on.
    #[test]
    fn chain_back_resumes_the_origin_only_on_its_own_binding() {
        let e = env("back", true);
        job(&e, 1, "kaan", &e.p1, "done", None);
        let w1 = worktree(&e, 1, "kaan", &e.p1);
        job(&e, 2, "kaan", &e.p2, "running", None);
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        let on_codex = |origin: &Binding| {
            sql(&e, "UPDATE agents SET active_engine = 'codex', active_model = 'gpt-x' WHERE id = 'kaan'", &[]);
            saved(&e, "kaan", "x1", "codex", &bound(&e.p2, 2, &w2));
            sql(&e, "INSERT INTO agent_chain_origins(agent_id,engine,session_id,project_id,delegation_id,cwd) VALUES('kaan','claude','c1',?1,?2,?3)
                     ON CONFLICT(agent_id) DO UPDATE SET session_id='c1', project_id=excluded.project_id, delegation_id=excluded.delegation_id, cwd=excluded.cwd",
                &[&origin.project_id, &origin.delegation_id, &origin.cwd]);
        };
        on_codex(&bound(&e.p2, 2, &w2));
        let (r, got) = captured(|| e.core.chain_back("kaan"));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!((req.cmd.as_str(), resumed(req).as_deref(), req.cwd.as_deref(), *key_job), ("claude", Some("c1"), Some(w2.as_str()), Some(2)));

        stop(&e.core, "kaan");
        on_codex(&bound(&e.p1, 1, &w1));
        let (r, got) = captured(|| e.core.chain_back("kaan"));
        r.unwrap();
        let (req, key_job) = &got[0];
        assert_eq!((resumed(req), req.cwd.as_deref(), *key_job), (None, Some(w2.as_str()), Some(2)), "P1's origin is not resumed for P2");

        // The origin a Codex move saves carries the running binding.
        stop(&e.core, "kaan");
        saved(&e, "kaan", "s9", "claude", &bound(&e.p2, 2, &w2));
        crate::db::with(&e.core.db, |c| save_chain_origin(c, "kaan", None, None)).unwrap();
        let origin = crate::db::with(&e.core.db, |c| {
            c.query_row("SELECT session_id, project_id, delegation_id, cwd FROM agent_chain_origins WHERE agent_id = 'kaan'", [], |r| {
                Ok((r.get::<_, String>(0)?, Binding { project_id: r.get(1)?, delegation_id: r.get(2)?, cwd: r.get(3)? }))
            })
            .map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(origin, ("s9".into(), bound(&e.p2, 2, &w2)));
    }

    /// A lost resume retries fresh on the same binding; a failed spawn leaves the row as it was.
    #[test]
    fn lost_resume_and_failed_spawn_keep_the_binding() {
        let e = env("lost", true);
        job(&e, 1, "kaan", &e.p1, "done", None);
        let w1 = worktree(&e, 1, "kaan", &e.p1);
        job(&e, 2, "kaan", &e.p2, "running", None);
        let w2 = worktree(&e, 2, "kaan", &e.p2);
        saved(&e, "kaan", "s1", "claude", &bound(&e.p2, 2, &w2));
        let (r, _) = captured(|| e.core.agent_spawn("kaan", StartOptions { prompt: Some("selam".into()), ..Default::default() }));
        r.unwrap();
        // The CLI said "No conversation found" and exited (the exit itself stands in here).
        let mut watch = e.core.resume_watch.lock().unwrap().remove("kaan").expect("resume watched");
        watch.lost = true;
        stop(&e.core, "kaan");
        e.core.resume_watch.lock().unwrap().insert("kaan".into(), watch);
        let (again, got) = captured(|| e.core.resume_on_exit("kaan"));
        assert!(again);
        let (req, key_job) = &got[0];
        assert_eq!((resumed(req), req.cwd.as_deref(), *key_job), (None, Some(w2.as_str()), Some(2)));
        assert_eq!(req.args.last().map(String::as_str), Some("selam"), "the prompt rides along");
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding), (NO_SESSION, bound(&e.p2, 2, &w2)));

        stop(&e.core, "kaan");
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &w1));
        let o = StartOptions { prompt: Some("iş".into()), fresh: true, delegation_id: Some(2), cols: Some(0), ..Default::default() };
        let (r, _) = captured(|| e.core.agent_spawn("kaan", o));
        assert!(r.is_err());
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding), ("s1", bound(&e.p1, 1, &w1)), "a failed start abandons nothing");
    }

    /// The captured starts' launches (memory, system text), taken.
    fn launches() -> Vec<crate::pty::AgentLaunch> {
        LAUNCHES.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn stack(root: &str, text: &str) {
        let dex = Path::new(root).join(".dex");
        std::fs::create_dir_all(&dex).unwrap();
        std::fs::write(dex.join("stack.md"), format!("# {text}\n")).unwrap();
    }

    /// #838 G: a job start's memory and system text come from its binding's context — the context
    /// its SessionStart resolves too: only its project's stack, its job/report lines, and team
    /// shared + own private + project shared as the folders it may write. Another agent's private
    /// folder or a memory root is not among them.
    #[test]
    fn a_job_start_takes_memory_and_text_from_its_bound_context() {
        let e = env("g-ctx", true);
        stack(e.p1.root_path.as_deref().unwrap(), "P1-STACK");
        stack(e.p2.root_path.as_deref().unwrap(), "P2-STACK");
        job(&e, 2, "kaan", &e.p2, "queued", None);
        launches();
        let mut o = job_start("iş".into(), vec![]);
        o.delegation_id = Some(2);
        let (r, got) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        let launch = launches().pop().unwrap();
        assert_eq!((launch.job, got[0].1), (Some(2), Some(2)), "the MCP key's job");
        let ctx = session_context(&e, "kaan");
        let p2 = PathBuf::from(e.p2.root_path.as_deref().unwrap());
        let p2_memory = p2.join(".dex").join("memory");
        let own = crate::db::with(&e.core.db, |c| crate::slug::memory_dir(c, &ctx.agent_memory_root, "kaan")).unwrap();
        assert_eq!(launch.memory.write, [ctx.team_memory_root.join("shared"), own, p2_memory.join("shared")]);
        assert_eq!(launch.memory.read, [ctx.team_memory_root.clone(), p2_memory.clone()]);
        assert_eq!(launch.memory.root, ctx.agent_memory_root, "registered for SessionStart");
        assert_eq!(got[0].0.memory_dir.as_deref().map(PathBuf::from), Some(ctx.agent_memory_root.clone()));
        let same = crate::db::with(&e.core.db, |c| crate::hooks::memory_scope(c, &ctx)).unwrap();
        assert_eq!(launch.memory, same, "SessionStart's context is the spawn's");
        let text = &launch.prompt;
        assert!(text.contains("P2-STACK") && !text.contains("P1-STACK"), "only the job's project stack");
        assert!(text.contains("İş: 2") && text.contains(&format!("Proje: {}", e.p2.id)), "{text}");
        assert!(text.contains(&p2.join(".dex").join("results").join("2.md").display().to_string()), "its report");
        assert!(text.contains(&format!("Proje ortak: {}", p2_memory.join("shared").display())), "{text}");

        // An unbound (patron's fresh) start of a several-project team: no project chosen, no stack.
        stop(&e.core, "kaan");
        let (r, _) = captured(|| e.core.agent_spawn("kaan", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        let launch = launches().pop().unwrap();
        assert_eq!(launch.job, None);
        assert!(!launch.prompt.contains("STACK") && launch.prompt.contains("proje seçili değil"), "{}", launch.prompt);
        assert!(!launch.memory.write.iter().any(|d| d.starts_with(&p2)), "no project memory without a project");
    }

    /// #838 G: the dispatch starts a worker's job bound to it (`options.delegation_id`) — the row,
    /// the MCP key's job and the P root (no worktree here) before the first hook; never the team
    /// folder of a folderless team.
    #[test]
    fn a_dispatched_start_binds_the_job() {
        let e = env("g-dispatch", true);
        job(&e, 2, "kaan", &e.p2, "queued", None);
        let d = crate::db::with(&e.core.db, |c| crate::delegate::get(c, 2)).unwrap();
        launches();
        let (r, got) = captured(|| e.core.delegation_dispatch(d));
        assert_eq!(r.unwrap().status, "running");
        assert_eq!((got[0].1, got[0].0.cwd.as_deref()), (Some(2), e.p2.root_path.as_deref()));
        assert_eq!(launches().pop().unwrap().job, Some(2));
        assert_eq!(row(&e, "kaan").unwrap().binding, bound(&e.p2, 2, e.p2.root_path.as_deref().unwrap()));
    }

    /// #838 G fail-closed: a bound session whose project is archived, or whose job lost its root
    /// snapshot while the project has one, does not start — no fallback to the team folder or
    /// another project; the row stays as it was. A caller's fresh start is a new unbound session.
    #[test]
    fn a_bound_start_of_a_gone_project_does_not_start() {
        let e = env("g-gone", true);
        job(&e, 1, "kaan", &e.p1, "done", None);
        let p1 = e.p1.root_path.clone().unwrap();
        saved(&e, "kaan", "s1", "claude", &bound(&e.p1, 1, &p1));
        crate::db::with(&e.core.db, |c| projects::archive(c, "default", &e.p1.id, |_| false).map_err(|e| e.to_string())).unwrap();
        let (r, got) = captured(|| e.core.agent_spawn("kaan", StartOptions::default()));
        assert_eq!(r.unwrap_err(), "project_unavailable");
        assert!(got.is_empty() && !e.core.ptys.is_running("kaan"));
        let r = row(&e, "kaan").unwrap();
        assert_eq!((r.session.as_str(), r.binding), ("s1", bound(&e.p1, 1, &p1)), "nothing abandoned");
        let (r, _) = captured(|| e.core.agent_spawn("kaan", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        assert_eq!(launches().pop().unwrap().job, None);
        stop(&e.core, "kaan");

        // The old folderless default project bound to a folder later: a job snapshotted before
        // has no root — unavailable, not the team folder.
        let e = env("g-lost", false);
        job(&e, 3, "ayla", &e.p1, "queued", None);
        let folder = e.f.dir("bound-later").to_string_lossy().into_owned();
        sql(&e, "UPDATE teams SET cwd = ?1 WHERE id = 'default'", &[&folder]);
        sql(&e, "UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", &[&folder, &projects::path_key(&folder)]);
        let mut o = job_start("iş".into(), vec![]);
        o.delegation_id = Some(3);
        let (r, got) = captured(|| e.core.agent_spawn("ayla", o));
        assert_eq!(r.unwrap_err(), "project_unavailable");
        assert!(got.is_empty() && row(&e, "ayla").is_none());
    }

    /// #838 G: an old single-project team (cwd, default project and both memory anchors on one
    /// repo) starts as before — the registered root and an unbound start's text are the old ones;
    /// a bound job's text is the old text plus its job block. Its memory folders are team shared
    /// and its own: the old id folder while the slug move is pending.
    #[test]
    fn an_old_single_project_team_starts_as_before() {
        let e = env("g-old", false);
        let repo = e.f.dir("old");
        let repo_s = repo.to_string_lossy().into_owned();
        let mem = repo.join(".dex").join("memory");
        sql(&e, "UPDATE teams SET cwd = ?1, team_memory_root = ?2, agent_memory_root = ?2 WHERE id = 'default'", &[&repo_s, &mem.to_string_lossy().into_owned()]);
        sql(&e, "UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", &[&repo_s, &projects::path_key(&repo_s)]);
        stack(&repo_s, "OLD-STACK");
        let old_text = |agent: &str| {
            crate::db::with(&e.core.db, |c| {
                let own = crate::slug::memory_dir(c, &mem, agent)?;
                Ok(crate::rules::system_prompt(c, agent, Some((&mem.join("shared"), &own))))
            })
            .unwrap()
        };
        launches();
        let (r, got) = captured(|| e.core.agent_spawn("kaan", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        let launch = launches().pop().unwrap();
        assert_eq!(got[0].0.memory_dir.as_deref(), Some(mem.to_string_lossy().as_ref()), "the old registration");
        assert_eq!(launch.prompt, old_text("kaan"), "an unbound start's text is unchanged");
        assert!(launch.prompt.contains("OLD-STACK"));
        stop(&e.core, "kaan");

        // A job of the old project: the old text, then its job block.
        let p = crate::db::with(&e.core.db, |c| projects::get(c, "default", "legacy-default").map_err(|e| e.to_string())).unwrap();
        job(&e, 1, "kaan", &p, "queued", None);
        let mut o = job_start("iş".into(), vec![]);
        o.delegation_id = Some(1);
        let (r, _) = captured(|| e.core.agent_spawn("kaan", o));
        r.unwrap();
        let launch = launches().pop().unwrap();
        let old = old_text("kaan");
        let block = launch.prompt.strip_prefix(&old).unwrap_or_else(|| panic!("old text first:\n{}", launch.prompt));
        assert!(block.starts_with("\n\n## İş/proje bağlamı") && block.contains("İş: 1"), "{block}");
        assert_eq!(launch.memory.write, [mem.join("shared"), mem.join("agents").join("kaan")]);
        assert_eq!(launch.memory.read, std::slice::from_ref(&mem));
        stop(&e.core, "kaan");

        // Slug move pending: its old `agents/<id>` folder stays its own (and the only one).
        sql(&e, "UPDATE agents SET slug = 'kaan-yeni' WHERE id = 'kaan'", &[]);
        sql(&e, "DELETE FROM settings WHERE key = 'memory_slug_migrated'", &[]);
        std::fs::create_dir_all(mem.join("agents").join("kaan")).unwrap();
        let (r, _) = captured(|| e.core.agent_spawn("kaan", StartOptions { fresh: true, ..Default::default() }));
        r.unwrap();
        assert_eq!(launches().pop().unwrap().memory.write, [mem.join("shared"), mem.join("agents").join("kaan")]);
    }
}
