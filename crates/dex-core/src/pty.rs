use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::providers;
use crate::pty_stream::{PtySink, PtyStream, Snapshot, SubscriberId};
use crate::Core;

/// Input for one generation; terminal replies remain valid after registry retirement.
#[derive(Clone)]
pub struct PtyInput(Arc<Mutex<Box<dyn Write + Send>>>);

impl PtyInput {
    pub(crate) fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut writer = self.0.lock().map_err(|_| "PTY input kilidi zehirli")?;
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }
}

struct Session {
    gen: u64,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Serialize size ownership and its I/O within this generation, never across PTYs.
    resize_gate: Arc<Mutex<()>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Output fan-out; outlives any one subscriber (S2: several clients, reconnects).
    stream: Arc<PtyStream>,
    /// Signalled after reader EOF and the stream's single exit notification.
    reader_done: Option<std::sync::mpsc::Receiver<()>>,
    /// Only registered agents retain output after exit; transient shell ids do not accumulate.
    replay: bool,
    kind: String,
    cols: u16,
    rows: u16,
    started_at: u64,
    sizes: Sizes,
}

/// #62: one PTY has one size, several clients (desktop, phone) show it. Like tmux's
/// `window-size latest`: the client the patron last typed into or focused owns the size; the
/// others only report theirs (kept for when they take over) and draw the PTY's size as it is.
/// Without this, a phone's resize narrowed the PTY under a wide desktop xterm and the TUI's
/// redraw landed on the wrong columns.
#[derive(Default)]
struct Sizes {
    owner: Option<String>,
    /// Client → its view's size, with the report order (the newest one takes over a released size).
    wanted: HashMap<String, (u16, u16, u64)>,
    seq: u64,
}

impl Sizes {
    /// The size to apply now, if any.
    fn report(&mut self, client: &str, cols: u16, rows: u16) -> Option<(u16, u16)> {
        self.seq += 1;
        self.wanted.insert(client.to_string(), (cols, rows, self.seq));
        let owner = self.owner.get_or_insert_with(|| client.to_string());
        (owner == client).then_some((cols, rows))
    }

    fn claim(&mut self, client: &str, cols: u16, rows: u16) -> Option<(u16, u16)> {
        self.owner = Some(client.to_string());
        self.report(client, cols, rows)
    }

    /// The client stopped showing the PTY; its size goes to the latest remaining reporter.
    fn release(&mut self, client: &str) -> Option<(u16, u16)> {
        self.wanted.remove(client);
        if self.owner.as_deref() != Some(client) {
            return None;
        }
        let next = self.wanted.iter().max_by_key(|(_, w)| w.2).map(|(c, &(cols, rows, _))| (c.clone(), cols, rows));
        self.owner = next.as_ref().map(|n| n.0.clone());
        next.map(|(_, cols, rows)| (cols, rows))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyInfo {
    pub id: String,
    pub kind: String,
    pub cols: u16,
    pub rows: u16,
    pub alive: bool,
    pub started_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub subscriber: SubscriberId,
    pub snapshot: Snapshot,
}

/// Live sessions by PTY id (= agent id). Clones share the map.
#[derive(Clone)]
pub struct Ptys {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// At most one finished stream per registered agent, replaced on spawn and cleared on deletion.
    finished: Arc<Mutex<HashMap<String, Arc<PtyStream>>>>,
    /// Retired jobs remain visible here until resource release, including deadline overruns.
    teardowns: Arc<Teardowns>,
    owner: std::sync::Weak<Core>,
}

#[derive(Default)]
struct Teardowns {
    /// true = the deadline was exceeded; retained until the job actually completes.
    jobs: Mutex<HashMap<u64, bool>>,
    changed: Condvar,
}

impl Default for Ptys {
    fn default() -> Self {
        Self { sessions: Arc::default(), finished: Arc::default(), teardowns: Arc::default(), owner: std::sync::Weak::new() }
    }
}

impl Ptys {
    pub fn bound(owner: std::sync::Weak<Core>) -> Self {
        Self { sessions: Arc::default(), finished: Arc::default(), teardowns: Arc::default(), owner }
    }

    pub(crate) fn core(&self) -> Option<Arc<Core>> {
        self.owner.upgrade()
    }
}

static GEN: AtomicU64 = AtomicU64::new(0);

/// #164 P2 (Core's call, B): user + project settings, never `local`. The project's CLAUDE.md and
/// committed `.claude/settings.json` (its hooks run beside Dex's) keep reaching agents; the
/// gitignored `.claude/settings.local.json` — where a planted `disableAllHooks`/hook would hide —
/// does not. Agents cannot write any of them (permit.rs `protected`). Verified on Claude Code
/// 2.1.289: CLAUDE.md loaded, settings.json hook ran, settings.local.json hook did not.
const SETTING_SOURCES: &str = "user,project";

/// What the frontend asks for; one PTY per `id`, a new spawn replaces (kills) the old one.
#[derive(Debug, Default)]
pub struct Spawn {
    pub id: String,
    pub cmd: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub memory_dir: Option<String>,
    pub provider: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// DEX-26: for `cmd = "codex"` the job comes structured and `codex::argv` builds the argv
    /// (`args` is ignored).
    pub codex: Option<crate::codex::Launch>,
    /// Görev #144: on top of the inherited environment (a job worktree's `CARGO_TARGET_DIR`).
    pub env: Vec<(String, String)>,
}

/// #838 G: what an agent session (agents.rs) brings from its resolved launch context instead of the
/// IPC's `memory_dir`: the job its MCP key and Codex trust follow (#838 D), its memory folders and
/// its system text. Dex's own, never from IPC or env.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentLaunch {
    pub job: Option<i64>,
    pub memory: crate::hooks::MemoryScope,
    pub prompt: String,
}

#[cfg(test)]
thread_local! {
    /// Capture the fully wired argv at the process boundary, without running either CLI.
    static CAPTURE: std::cell::RefCell<Option<Vec<Vec<String>>>> = const { std::cell::RefCell::new(None) };
}

/// `sink` is subscribed before the child starts, so it sees every byte.
pub fn spawn(core: &Core, req: Spawn, sink: impl PtySink) -> Result<(), String> {
    spawn_bound(core, req, None, sink)
}

/// [`spawn`] of an agent session with its [`AgentLaunch`]; `None` is the IPC's spawn (unchanged).
pub(crate) fn spawn_bound(core: &Core, req: Spawn, launch: Option<AgentLaunch>, mut sink: impl PtySink) -> Result<(), String> {
    let Spawn { id, cmd, args, cwd, memory_dir, provider, cols, rows, codex, env } = req;
    let delegation_id = launch.as_ref().and_then(|l| l.job);
    validate_size(cols, rows)?;
    // DEX-29: the patron's shell — none of the agent wiring below applies (no job, hooks, memory).
    let is_shell = cmd == crate::shell::SHELL_CMD;
    let (cmd, args) = if is_shell { crate::shell::resolve() } else { (cmd, args) };
    let pair = native_pty_system()
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;

    // DEX-13: PATH'teki `claude` bir claude-route SHİM'ine çözülüyor olabilir; Dex gerçek
    // ikiliyi çağırır — env'i Dex basarken shim üzerinden ikinci bir yönlendirme doğar.
    let cmd = providers::resolve_claude(&cmd);
    // DEX-26: same shim problem for `codex` (claude-route `[slot-binaries]`).
    let cmd = crate::codex::resolve(&cmd);
    let is_claude = std::path::Path::new(&cmd)
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("claude"));
    // DEX-27: Codex motoru (DEX-26) aynı job/limit yolundan geçer; spawn tek noktada.
    let is_codex = std::path::Path::new(&cmd)
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("codex"));
    // DEX-26/17: motor DB'den gelir; zincir geçişinde geçici motor kalıcı ayarı ezmez.
    let engine: Option<String> = {
        let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
        effective_engine(&conn, &id)
    };
    if (is_claude || is_codex) && engine.as_deref().is_some_and(|e| e != if is_codex { "codex" } else { "claude" }) {
        return Err(format!("motor uyuşmuyor: ajan {} motoruyla kayıtlı", engine.unwrap_or_default()));
    }
    let check_ready = true;
    #[cfg(test)]
    let check_ready = check_ready && CAPTURE.with(|c| c.borrow().is_none());
    if is_codex && check_ready {
        crate::codex::check_ready(&cmd)?;
    }
    // #838 G: an agent session registers its context's private anchor root; the IPC's own `memory_dir` as before.
    let memory_dir = match &launch {
        Some(l) => Some(l.memory.root.to_string_lossy().into_owned()),
        None => memory_dir,
    }
    .filter(|_| is_claude || is_codex);
    let id_ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    // A codex profile file is named after the id.
    if (memory_dir.is_some() || is_codex) && !id_ok {
        return Err(format!("hafıza için geçersiz ajan id: {id:?}"));
    }
    let mut builder = CommandBuilder::new(cmd);
    if is_claude {
        let h = &core.hooks;
        // #164 P3: a worker's PreToolUse fails closed (hooks::settings_json); leaders and the
        // patron's own panes (no worker row) keep the HTTP hook.
        let worker = {
            let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
            crate::teams::get_agent(&conn, &id).is_ok_and(|a| a.kind == "worker")
        };
        {
            let mut fail_closed = h.fail_closed.lock().unwrap();
            if worker { fail_closed.insert(id.clone()) } else { fail_closed.remove(&id) };
        }
        builder.arg("--settings");
        builder.arg(if worker { &h.worker_settings } else { &h.settings });
        builder.arg("--setting-sources");
        builder.arg(SETTING_SOURCES);
        // Every Claude pane (leader + workers, resume too) runs without prompts.
        // Dex's PreToolUse policy (permit.rs) still denies in this mode.
        builder.arg("--permission-mode");
        builder.arg("bypassPermissions");
        builder.env("DEX_AGENT_ID", &id);
        builder.env("DEX_HOOK_PORT", h.port.to_string());
        builder.env("DEX_HOOK_TOKEN", &h.token);
        // Dex MCP (DEX-4). Both flags are variadic: a flag must follow them, never a positional.
        let m = &core.mcp;
        mcp_args(&mut builder, &m.config);
        // DEX-20: browser MCP only for roles flagged in the catalog; others never see the tools.
        let browser = crate::roles::browser_for_agent(&*core.db.0.lock().map_err(|_| "db kilidi zehirli")?, &id);
        if browser {
            builder.arg(&core.browser.config);
        }
        builder.arg("--allowedTools");
        builder.arg("mcp__dex");
        if browser {
            builder.arg("mcp__browser");
        }
        builder.env("DEX_MCP_KEY", m.key_for_job(&id, delegation_id));
        // DEX-23: only this agent's role skills load, via a per-process plugin dir.
        // Roles without skills (e.g. marketing) get no flag. Never ~/.claude/skills.
        let role_dir = {
            let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
            core.skills.plugin_dir_for_agent(&conn, &id)?
        };
        if let Some(dir) = role_dir {
            builder.arg("--plugin-dir");
            builder.arg(&dir);
        }
        let mut memory = h.memory.lock().unwrap();
        match &memory_dir {
            Some(root) => memory.insert(id.clone(), root.into()),
            None => memory.remove(&id),
        };
    }
    // What the engine may read without asking: the IPC's whole root, or (#838 G) the context's
    // memory folders (team shared, own private, project shared) — the ones made here.
    let mut grant: Vec<std::path::PathBuf> = Vec::new();
    if let Some(root) = memory_dir.as_deref() {
        let root = std::path::Path::new(root);
        let make = match &launch {
            Some(l) => {
                grant.clone_from(&l.memory.write);
                l.memory.write.clone()
            }
            None => {
                grant.push(root.to_path_buf());
                vec![root.join("shared"), crate::slug::memory_dir(&*core.db.0.lock().map_err(|_| "db kilidi zehirli")?, root, &id)?]
            }
        };
        for dir in make {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        builder.env("DEX_MEMORY_DIR", root);
        // Sub-team memory lives in app data, outside the workdir: without this every read prompts.
        // codex: via `codex::argv` (its `resume` subcommand must come first).
        if is_claude {
            for dir in &grant {
                builder.arg("--add-dir");
                builder.arg(dir);
            }
        }
    }
    // DEX-35: role text + Dex rules, one text for `--append-system-prompt` (claude takes the flag
    // once; a second one would silently replace the first) and codex's `developer_instructions`.
    let mut append: Vec<String> = Vec::new();
    if let Some(l) = launch.as_ref().filter(|_| is_claude || is_codex) {
        // #838 G: built from the session's context (rules::system_prompt_for_context, agents.rs).
        append.push(l.prompt.clone());
    } else if is_claude || is_codex {
        // PTY ids are agent ids; the role comes from the DB, not from the webview.
        let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
        let memory = memory_dir.as_deref().map(std::path::Path::new).map(|r| Ok::<_, String>((r.join("shared"), crate::slug::memory_dir(&conn, r, &id)?))).transpose()?;
        append.push(crate::rules::system_prompt(&conn, &id, memory.as_ref().map(|(s, o)| (s.as_path(), o.as_path()))));
    }
    let args = if is_codex {
        codex_setup(core, &id, &mut builder, append.split_off(0), memory_dir.as_deref(), &grant, cwd.as_deref(), delegation_id, codex.unwrap_or_default())?
    } else {
        args
    };
    if is_claude && !append.is_empty() {
        builder.arg("--append-system-prompt");
        builder.arg(append.join("\n\n"));
    }
    // Dex launched from inside a Claude session inherits its markers; the child claude then
    // thinks it is nested (transcript off → --resume breaks). Only strip when that happened,
    // so a user's own CLAUDE_CODE_* config survives a normal launch.
    if (is_claude || is_shell) && std::env::var_os("CLAUDECODE").is_some() {
        for (k, _) in std::env::vars_os() {
            let inherited = |k: &str| {
                matches!(k, "CLAUDECODE" | "CLAUDE_EFFORT" | "CLAUDE_PID") || k.starts_with("CLAUDE_CODE_")
            };
            if k.to_str().is_some_and(inherited) {
                builder.env_remove(k);
            }
        }
    }
    // DEX-13: glm/muse sağlayıcısı gerçek uç env'ini basar (jeton kaseden); anthropic/None
    // env'e dokunmaz. Jeton yoksa burada başarısız olur → pane kullanıcıya ne yapacağını yazar.
    // DEX-26: codex runs on its own login; provider routing is claude-only.
    let provider = provider.filter(|_| !is_codex && !is_shell);
    let provider_env = if provider.as_deref()==Some("gateway") {
        for name in ["OPENAI_API_KEY","OPENAI_ACCESS_TOKEN","OPENAI_REFRESH_TOKEN","CODEX_API_KEY"] {builder.env_remove(name);}
        Some(core.gateway_spawn_env(&id,providers::model_flag(&args).as_deref().ok_or("gateway_model_required")?)?)
    } else {
        providers::spawn_env(provider.as_deref(),providers::model_flag(&args).as_deref())?
    };
    if let Some(env) = provider_env {
        for (k, v) in env {
            builder.env(k, v);
        }
        builder.env_remove("ANTHROPIC_API_KEY"); // CLI API_KEY'i tercih edebilir; tek jeton kaynağı kalır
    }
    builder.args(args);
    if let Some(dir) = cwd.or_else(|| std::env::var("USERPROFILE").ok()) {
        builder.cwd(dir);
    }
    for (k, v) in env {
        builder.env(k, v);
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");

    #[cfg(test)]
    if CAPTURE.with(|c| {
        let mut capture = c.borrow_mut();
        let Some(commands) = capture.as_mut() else { return false };
        commands.push(builder.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect());
        true
    }) {
        return Ok(());
    }

    // DEX-31: read before the spawn — nothing may widen the gap between spawn and adopt.
    let limits = {
        let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
        crate::proc::with_env(crate::settings::limits(&conn)).0
    };
    let mut child = pair.slave.spawn_command(builder).map_err(|e| e.to_string())?;
    // DEX-20: claude's MCP servers (and their browsers) die with the pane, not just claude.
    // DEX-27/31: claude + codex ağaçları BELOW_NORMAL, kendi job'unda; o job tüm ajanların
    // toplam CPU tavanını taşıyan tek üst job'un içinde (Ayarlar → Kaynak sınırları).
    let tree = child
        .process_id()
        .filter(|_| is_claude || is_codex)
        .and_then(|pid| crate::proc::Tree::adopt_with(pid, &limits));
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let writer = Arc::new(Mutex::new(writer));
    sink.bind_input(PtyInput(writer.clone()));
    let killer = child.clone_killer();
    let gen = GEN.fetch_add(1, Ordering::Relaxed);

    let stream = Arc::new(PtyStream::new(rows, cols, scrollback_lines()));
    stream.subscribe(sink)?;
    let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel();

    if is_claude {
        core.agent_states.spawned(&id);
        crate::hooks::watch_pre_session(core.ptys.owner.clone(), id.clone(), stream.clone());
    }

    // Publish the session before output/wait threads can observe the child. Besides making a very
    // short-lived process visible, this lets the backend terminal sink answer startup queries.
    let old = core.ptys.insert(id.clone(), Session {
        gen,
        master: Arc::new(Mutex::new(Some(pair.master))),
        writer,
        resize_gate: Arc::default(),
        killer,
        stream: stream.clone(),
        reader_done: Some(reader_done_rx),
        replay: !is_shell && engine.is_some(),
        kind: if is_shell { "shell" } else { "agent" }.into(),
        cols,
        rows,
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        sizes: Sizes::default(),
    });
    if let Some(old) = old {
        crate::diagnostics::event("pty_kill", serde_json::json!({"agent": id, "reason": "replacement", "generation": old.gen}));
        core.ptys.teardown(&id, old, true);
    }
    core.observations.spawn(&id, child.process_id(), gen);
    // #126: an idle worker session is closed after a while (leaders and shells are not watched).
    if is_claude || is_codex {
        core.idle_close_watch(&id);
    }

    // Keeps draining with no subscriber left, so the child never blocks on a full pipe.
    let out = stream.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.publish(&buf[..n]),
            }
        }
        out.close();
        let _ = reader_done_tx.send(());
    });

    // ConPTY does not EOF the reader when the child exits; dropping the master does.
    let waiter_id = id.clone();
    let ptys = core.ptys.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        crate::diagnostics::event("pty_exit", serde_json::json!({"agent": waiter_id, "generation": gen, "reason": "exit"}));
        drop(tree);
        if let Some(session) = ptys.retire(&waiter_id, Some(gen)) {
            ptys.teardown(&waiter_id, session, false);
            if let Some(core) = ptys.owner.upgrade() {
                core.pty_exited(&waiter_id);
            }
        }
    });

    Ok(())
}

/// #126: only Dex's own MCP servers (`--mcp-config`) — the user's global ones (~/.claude.json,
/// project .mcp.json, plugins) stay out of every Dex session. The caller may add more configs.
fn mcp_args(builder: &mut CommandBuilder, config: impl AsRef<std::ffi::OsStr>) {
    builder.arg("--strict-mcp-config");
    builder.arg("--mcp-config");
    builder.arg(config);
}

fn effective_engine(conn: &rusqlite::Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT COALESCE(active_engine, engine) FROM agents WHERE id = ?1", [id], |r| r.get(0)).ok()
}

/// DEX-26: env + per-process profile for a codex pane; returns its argv. Same Dex wiring as
/// claude (hook receiver, Dex MCP key, memory index on SessionStart), carried by the profile.
#[allow(clippy::too_many_arguments)]
fn codex_setup(
    core: &Core,
    id: &str,
    builder: &mut CommandBuilder,
    mut instructions: Vec<String>,
    memory_dir: Option<&str>,
    grant: &[std::path::PathBuf],
    cwd: Option<&str>,
    job: Option<i64>,
    launch: crate::codex::Launch,
) -> Result<Vec<String>, String> {
    let h = &core.hooks;
    builder.env("DEX_AGENT_ID", id);
    builder.env("DEX_HOOK_PORT", h.port.to_string());
    builder.env("DEX_HOOK_TOKEN", &h.token);
    builder.env("DEX_MCP_KEY", core.mcp.key_for_job(id, job));
    {
        let mut memory = h.memory.lock().unwrap();
        match memory_dir {
            Some(root) => memory.insert(id.to_string(), root.into()),
            None => memory.remove(id),
        };
    }
    let (skills, browser, worker, trust) = {
        let conn = core.db.0.lock().map_err(|_| "db kilidi zehirli")?;
        let worker = crate::teams::get_agent(&conn, id)?.kind == "worker";
        (core.skills.plugin_dir_for_agent(&conn, id)?, crate::roles::browser_for_agent(&conn, id), worker,
         crate::codex::trust_dirs(&conn, id, worker, cwd, job)?)
    };
    {
        let mut fail_closed = h.fail_closed.lock().unwrap();
        if worker { fail_closed.insert(id.to_string()) } else { fail_closed.remove(id) };
    }
    // codex has no `--plugin-dir`: the same SKILL.md files, named with their paths.
    if let Some(dir) = skills {
        let mut names: Vec<_> = std::fs::read_dir(dir.join("skills")).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        names.sort();
        let lines: Vec<String> = names
            .iter()
            .map(|p| format!("- {}: {}", p.file_name().unwrap_or_default().to_string_lossy(), p.join("SKILL.md").display()))
            .collect();
        instructions.push(format!(
            "# Süreç skill'lerin\nBu motorda skill otomatik yüklenmez. İşin bir skill'e uyuyorsa başlamadan o SKILL.md dosyasını oku ve uygula:\n{}",
            lines.join("\n")
        ));
    }
    let home = crate::codex::home();
    #[cfg(test)]
    let home = if CAPTURE.with(|c| c.borrow().is_some()) { core.paths.config_dir.join("codex-capture") } else { home };
    let (profile, overrides) = crate::codex::write_profile(&home, id, &crate::codex::Profile {
        worker,
        instructions: &instructions.join("\n\n"),
        port: h.port,
        browser: browser.then(crate::browser::stdio),
        trust: trust.iter().map(String::as_str).collect(),
        migration: launch.model.as_deref().and_then(crate::codex::migration_for),
    })?;
    let memory: Vec<&std::path::Path> = grant.iter().map(std::path::PathBuf::as_path).collect();
    Ok(crate::codex::argv(&profile, &overrides, &launch, &memory))
}

impl Ptys {
    fn insert(&self, id: String, session: Session) -> Option<Session> {
        let mut map = self.sessions.lock().unwrap();
        self.finished.lock().unwrap().remove(&id);
        map.insert(id, session)
    }

    pub(crate) fn forget_replay(&self, id: &str) {
        // Same lock order as spawn/retire: an exit cannot reinsert replay after deletion.
        let _sessions = self.sessions.lock().unwrap();
        self.finished.lock().unwrap().remove(id);
    }

    /// Publish replay without entering the stream; teardown drains its tail outside caller gates.
    fn retire(&self, id: &str, gen: Option<u64>) -> Option<Session> {
        let mut map = self.sessions.lock().unwrap();
        if gen.is_some_and(|gen| map.get(id).is_none_or(|s| s.gen != gen)) {
            return None;
        }
        let session = map.remove(id)?;
        if session.replay {
            self.finished.lock().unwrap().insert(id.to_string(), session.stream.clone());
        }
        Some(session)
    }

    fn teardown(&self, id: &str, session: Session, kill: bool) {
        let timeout = TEARDOWN_TIMEOUT;
        #[cfg(test)]
        let timeout = TEST_TEARDOWN_TIMEOUT.with(|value| value.borrow_mut().take()).unwrap_or(timeout);
        self.teardown_with_timeout(id, session, kill, timeout);
    }

    fn teardown_with_timeout(&self, id: &str, mut session: Session, kill: bool, timeout: Duration) {
        let gen = session.gen;
        let started = Instant::now();
        self.teardowns.jobs.lock().unwrap().insert(gen, false);
        let jobs = self.teardowns.clone();
        let worker_id = id.to_string();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let records = Arc::new(Mutex::new(()));
        let worker_records = records.clone();
        // No Core or caller gate is retained. Detached jobs cannot hold up process shutdown.
        std::thread::spawn(move || {
            let killed = !kill || session.killer.kill().is_ok();
            if session.replay { session.stream.limit_scrollback(FINISHED_SCROLLBACK_LINES); }
            // Take the resource, not just an Arc: an in-flight resize must not become its last
            // owner and run ClosePseudoConsole on a start/admission caller later.
            let master = session.master.lock().unwrap().take();
            drop(master);
            // ConPTY only EOFs after master close. Drain concurrently with close, keeping input
            // and the responder alive for queries already buffered when registry retirement won.
            let drained = session.reader_done.take().is_none_or(|reader| reader.recv().is_ok());
            drop(session);
            let _record = worker_records.lock().unwrap();
            let pending = {
                let mut pending = jobs.jobs.lock().unwrap();
                pending.remove(&gen);
                jobs.changed.notify_all();
                pending.len()
            };
            let fields = serde_json::json!({"agent": worker_id, "generation": gen, "duration_ms": started.elapsed().as_millis() as u64, "pending": pending});
            if killed && drained {
                crate::diagnostics::event("pty_teardown_complete", fields);
            } else {
                log::error!(target: "dex::audit", "{}", serde_json::json!({"event": "pty_teardown_complete", "fields": fields}));
            }
            let _ = done_tx.send(());
        });
        let jobs = self.teardowns.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            if done_rx.recv_timeout(timeout.saturating_sub(started.elapsed())).is_err() {
                let _record = records.lock().unwrap();
                let pending = {
                    let mut pending = jobs.jobs.lock().unwrap();
                    let Some(timed_out) = pending.get_mut(&gen) else { return };
                    *timed_out = true;
                    pending.len()
                };
                // Serialize timeout and completion records so a recovered job cannot log
                // completion before its deadline overrun. Only this job's record gate is held.
                log::error!(target: "dex::audit", "{}", serde_json::json!({"event": "pty_teardown_timeout", "fields": {
                    "agent": id, "generation": gen, "duration_ms": started.elapsed().as_millis() as u64, "pending": pending
                }}));
                jobs.changed.notify_all();
            }
        });
    }

    pub(crate) fn generation(&self, id: &str) -> Option<u64> {
        self.sessions.lock().unwrap().get(id).map(|s| s.gen)
    }

    #[cfg(test)]
    pub(crate) fn input_writer_for_test(&self, id: &str, wrap: impl FnOnce(Box<dyn Write + Send>) -> Box<dyn Write + Send>) {
        let writer = self.sessions.lock().unwrap().get(id).unwrap().writer.clone();
        let mut writer = writer.lock().unwrap();
        let original = std::mem::replace(&mut *writer, Box::new(Vec::<u8>::new()));
        *writer = wrap(original);
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        self.input(id).ok_or("no session")?.write(data)
    }

    pub(crate) fn input(&self, id: &str) -> Option<PtyInput> {
        self.sessions.lock().unwrap().get(id).map(|s| PtyInput(s.writer.clone()))
    }

    /// #62: `client`'s view is `cols`×`rows`. `claim` (the patron typed or focused there) makes it
    /// the size owner; otherwise the size applies only while it owns the size or nobody does.
    pub fn resize(&self, id: &str, client: &str, cols: u16, rows: u16, claim: bool) -> Result<(), String> {
        validate_size(cols, rows)?;
        self.apply(id, None, |sizes| if claim { sizes.claim(client, cols, rows) } else { sizes.report(client, cols, rows) })
    }

    /// #62: `client` stopped showing `id` (detach); a size it owned passes on.
    pub fn release(&self, id: &str, client: &str) {
        let _ = self.apply(id, None, |sizes| sizes.release(client));
    }

    /// #62: `client` is gone (connection closed) from every PTY.
    pub fn release_client(&self, client: &str) {
        let sessions: Vec<_> = self.sessions.lock().unwrap().iter().map(|(id, s)| (id.clone(), s.gen)).collect();
        for (id, gen) in sessions {
            let _ = self.apply(&id, Some(gen), |sizes| sizes.release(client));
        }
    }

    fn apply(&self, id: &str, expected: Option<u64>, update: impl FnOnce(&mut Sizes) -> Option<(u16, u16)>) -> Result<(), String> {
        let (gen, gate) = {
            let map = self.sessions.lock().unwrap();
            let s = map.get(id).filter(|s| expected.is_none_or(|gen| s.gen == gen)).ok_or("no session")?;
            (s.gen, s.resize_gate.clone())
        };
        // Wait outside the registry: output can answer queries through write while this resize
        // waits for ConPTY/stream. A replacement has its own gate and must not inherit this size.
        let _resize = gate.lock().unwrap();
        let (master, stream, cols, rows) = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id).filter(|s| s.gen == gen).ok_or("no session")?;
            let Some((cols, rows)) = update(&mut s.sizes).filter(|&(c, r)| (c, r) != (s.cols, s.rows)) else { return Ok(()) };
            (s.master.clone(), s.stream.clone(), cols, rows)
        };
        #[cfg(test)]
        BEFORE_RESIZE.with(|hook| { if let Some(hook) = hook.borrow_mut().take() { hook(); } });
        master.lock().unwrap()
            .as_ref().ok_or("no session")?
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        stream.resize(rows, cols);
        if let Some(s) = self.sessions.lock().unwrap().get_mut(id).filter(|s| s.gen == gen) {
            s.cols = cols;
            s.rows = rows;
        }
        Ok(())
    }

    /// DEX-30: the agent's CLI is running (its engine/model can't change under it).
    pub fn is_running(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
    }

    /// Identity and start clock of the live process, without collecting every PTY.
    pub(crate) fn process(&self, id: &str) -> Option<(u64, u64)> {
        self.sessions.lock().unwrap().get(id).map(|s| (s.gen, s.started_at))
    }

    /// #126: a line of Dex's own in the session's output (scrollback + every viewer), not its input.
    pub(crate) fn note(&self, id: &str, text: &str) {
        // Published outside the lock: a sink answering a terminal query writes back through it.
        let stream = self.sessions.lock().unwrap().get(id).map(|s| s.stream.clone());
        if let Some(stream) = stream {
            stream.publish(text.as_bytes());
        }
    }

    /// `false`: there was no session to close (so no exit will follow either).
    pub fn kill(&self, id: &str) -> bool {
        crate::diagnostics::event("pty_kill", serde_json::json!({"agent": id}));
        let session = self.retire(id, None);
        let Some(s) = session else { return false };
        self.teardown(id, s, true);
        if let Some(core) = self.core() {
            core.resume_forget(id);
            core.pty_exited(id);
        }
        true
    }

    pub fn list(&self) -> Vec<PtyInfo> {
        let map = self.sessions.lock().unwrap();
        let mut result: Vec<_> = map
            .iter()
            .map(|(id, s)| PtyInfo {
                id: id.clone(),
                kind: s.kind.clone(),
                cols: s.cols,
                rows: s.rows,
                alive: true,
                started_at: s.started_at,
            })
            .collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }

    pub fn attach(&self, id: &str, sink: impl PtySink) -> Result<Attachment, String> {
        let (stream, finished) = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| (s.stream.clone(), false))
                .or_else(|| self.finished.lock().unwrap().get(id).cloned().map(|s| (s, true)))
                .ok_or("no session")?
        };
        if finished {
            stream.limit_scrollback(FINISHED_SCROLLBACK_LINES);
        }
        let (subscriber, snapshot) = stream.attach(sink)?;
        Ok(Attachment { subscriber, snapshot })
    }

    pub fn detach(&self, id: &str, subscriber: SubscriberId) -> bool {
        let stream = {
            let map = self.sessions.lock().unwrap();
            map.get(id).map(|s| s.stream.clone())
                .or_else(|| self.finished.lock().unwrap().get(id).cloned())
        };
        stream.is_some_and(|stream| stream.detach(subscriber))
    }

    pub fn subscribe(&self, id: &str, sink: impl PtySink) -> Result<(), String> {
        let stream = self.sessions.lock().unwrap().get(id).ok_or("no session")?.stream.clone();
        stream.subscribe(sink)?;
        Ok(())
    }

    /// S5-B: the PTY's visible screen as text — the waiting question shown in the chat card.
    pub fn screen(&self, id: &str) -> Result<String, String> {
        let stream = self.sessions.lock().unwrap().get(id).ok_or("no session")?.stream.clone();
        Ok(stream.contents())
    }
}

#[cfg(test)]
thread_local! {
    /// Pause the real resize path without depending on ConPTY scheduling.
    static BEFORE_RESIZE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_TEARDOWN_TIMEOUT: std::cell::RefCell<Option<Duration>> = const { std::cell::RefCell::new(None) };
}

const MAX_COLS: u16 = 512;
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ROWS: u16 = 256;
const MAX_SCROLLBACK_LINES: usize = 10_000;
// After output EOF: registered agent count × at most 2000 history rows + the size-bounded viewport.
const FINISHED_SCROLLBACK_LINES: usize = 2_000;

fn validate_size(cols: u16, rows: u16) -> Result<(), String> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        return Err(format!("invalid PTY size: {cols}x{rows}"));
    }
    Ok(())
}

fn scrollback_lines() -> usize {
    std::env::var("DEX_PTY_SCROLLBACK_LINES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000)
        .min(MAX_SCROLLBACK_LINES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_lock_test(test: &str) -> bool {
        use std::{process::{Command, Stdio}, sync::mpsc, time::Duration};
        const CHILD: &str = "DEX_PTY_LOCK_TEST_CHILD";
        if std::env::var(CHILD).as_deref() != Ok(test) {
            // A broken lock order leaves two threads blocked. Keep them in a child so neither
            // a panic nor a regression can strand the main test suite's PTY registry.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture", "--test-threads=1"])
                .env(CHILD, test).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let (tx, rx) = mpsc::channel();
            let output = std::thread::spawn(move || {
                let mut text = String::new();
                stderr.read_to_string(&mut text).unwrap();
                let _ = tx.send(text);
            });
            let text = match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(text) => text,
                Err(error) => {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    output.join().unwrap();
                    panic!("isolated PTY lock test did not exit: {error}");
                }
            };
            let status = child.wait().unwrap();
            output.join().unwrap();
            assert!(status.success(), "isolated PTY lock test failed: {text}");
            return true;
        }
        false
    }

    #[test]
    fn resize_and_cursor_reply_do_not_deadlock() {
        use std::{sync::mpsc, time::Duration};
        if isolated_lock_test("pty::tests::resize_and_cursor_reply_do_not_deadlock") { return; }
        let timeout = Duration::from_secs(2);

        struct Query<S> { sink: S, entered: mpsc::Sender<()> }
        impl<S: PtySink> PtySink for Query<S> {
            fn data(&mut self, bytes: &[u8]) -> bool {
                if bytes == b"6n" { self.entered.send(()).unwrap(); }
                self.sink.data(bytes)
            }
            fn exit(&mut self) { self.sink.exit(); }
        }
        struct Reply { _original: Box<dyn Write + Send>, tx: mpsc::Sender<Vec<u8>> }
        impl Write for Reply {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.tx.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let core = replay_core();
        let stream = Arc::new(PtyStream::default());
        core.ptys.insert("kaan".into(), replay_session(1, stream.clone()));
        let (reply_tx, reply_rx) = mpsc::channel();
        core.ptys.input_writer_for_test("kaan", |original| Box::new(Reply { _original: original, tx: reply_tx }));
        let (entered_tx, entered_rx) = mpsc::channel();
        // This adapter calls the production AgentSink, including its split-query parser/write.
        stream.subscribe(Query { sink: crate::agents::shell_sink_for_test(&core, "kaan", Exited::default()), entered: entered_tx }).unwrap();
        stream.publish(b"\x1b[");
        let (resizing_tx, resizing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (resized_tx, resized_rx) = mpsc::channel();
        let ptys = core.ptys.clone();
        let resize = std::thread::spawn(move || {
            BEFORE_RESIZE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                resizing_tx.send(()).unwrap();
                release_rx.recv_timeout(timeout).unwrap();
            })));
            let result = ptys.resize("kaan", "desktop", 100, 30, true);
            resized_tx.send(result).unwrap();
        });
        resizing_rx.recv_timeout(timeout).expect("resize reached its master call");
        let (published_tx, published_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || { stream.publish(b"6n"); published_tx.send(()).unwrap(); });
        entered_rx.recv_timeout(timeout).expect("reader holds the stream lock and enters AgentSink");
        release_tx.send(()).unwrap();
        // Before the fix, resize waits for stream while the real CPR responder waits for sessions.
        assert_eq!(reply_rx.recv_timeout(timeout).expect("CPR blocked by sessions -> stream -> sessions"), b"\x1b[1;1R");
        published_rx.recv_timeout(timeout).expect("reader completed");
        resized_rx.recv_timeout(timeout).expect("resize completed").unwrap();
        reader.join().unwrap();
        resize.join().unwrap();
        assert_eq!((core.ptys.list()[0].cols, core.ptys.list()[0].rows), (100, 30));
        core.ptys.kill("kaan");
        core.shutdown();
    }

    #[test]
    fn size_operations_are_serial_and_generation_scoped() {
        use std::{sync::mpsc, time::Duration};
        if isolated_lock_test("pty::tests::size_operations_are_serial_and_generation_scoped") { return; }
        let timeout = Duration::from_secs(2);
        let ptys = Ptys::default();
        let stream = Arc::new(PtyStream::new(24, 120, 2_000));
        ptys.insert("kaan".into(), replay_session(1, stream.clone()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let resize = {
            let ptys = ptys.clone();
            std::thread::spawn(move || {
                BEFORE_RESIZE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(timeout).unwrap();
                })));
                ptys.resize("kaan", "desktop", 100, 30, true).unwrap();
            })
        };
        entered_rx.recv_timeout(timeout).unwrap();
        let gate = {
            let map = ptys.sessions.try_lock().expect("ConPTY resize must leave the registry available");
            map["kaan"].resize_gate.clone()
        };
        assert!(gate.try_lock().is_err(), "size ownership and I/O share one session gate");
        // A second operation is queued on the same generation, but leaves the registry free.
        let (started_tx, started_rx) = mpsc::channel();
        let (updated_tx, updated_rx) = mpsc::channel();
        let next = {
            let ptys = ptys.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                ptys.apply("kaan", Some(1), |sizes| {
                    updated_tx.send(()).unwrap();
                    sizes.claim("phone", 80, 20)
                }).unwrap();
            })
        };
        started_rx.recv_timeout(timeout).unwrap();
        assert!(matches!(updated_rx.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout)));
        assert!(ptys.sessions.try_lock().is_ok(), "waiting for the gate must leave the registry available");
        release_tx.send(()).unwrap();
        updated_rx.recv_timeout(timeout).unwrap();
        resize.join().unwrap();
        next.join().unwrap();
        assert_eq!((ptys.list()[0].cols, ptys.list()[0].rows), (80, 20));
        let snapshot = ptys.attach("kaan", Exited::default()).unwrap().snapshot;
        assert_eq!((snapshot.cols, snapshot.rows), (80, 20));

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let old_resize = {
            let ptys = ptys.clone();
            std::thread::spawn(move || {
                BEFORE_RESIZE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(timeout).unwrap();
                })));
                ptys.resize("kaan", "desktop", 90, 25, true).unwrap();
            })
        };
        entered_rx.recv_timeout(timeout).unwrap();
        let replacement = Arc::new(PtyStream::new(24, 120, 2_000));
        let old = ptys.insert("kaan".into(), replay_session(2, replacement));
        // A replacement proceeds through its own gate even while the old ConPTY call is paused.
        ptys.resize("kaan", "phone", 70, 15, true).unwrap();
        release_tx.send(()).unwrap();
        old_resize.join().unwrap();
        assert_eq!((ptys.list()[0].cols, ptys.list()[0].rows), (70, 15), "old completion cannot overwrite a replacement");
        assert_eq!(ptys.apply("kaan", Some(1), |sizes| sizes.release("phone")), Err("no session".into()));
        let snapshot = ptys.attach("kaan", Exited::default()).unwrap().snapshot;
        assert_eq!((snapshot.cols, snapshot.rows), (70, 15));
        assert_eq!(ptys.sessions.lock().unwrap()["kaan"].sizes.owner.as_deref(), Some("phone"));
        drop(old);
        ptys.kill("kaan");
    }

    #[derive(Clone, Default)]
    struct Exited(Arc<std::sync::atomic::AtomicBool>);
    impl PtySink for Exited {
        fn data(&mut self, _: &[u8]) -> bool { true }
        fn exit(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    #[derive(Debug)]
    struct NoChild;
    impl ChildKiller for NoChild {
        fn kill(&mut self) -> std::io::Result<()> { Ok(()) }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> { Box::new(Self) }
    }

    fn replay_session(gen: u64, stream: Arc<PtyStream>) -> Session {
        let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 120, pixel_width: 0, pixel_height: 0 }).unwrap();
        Session { gen, writer: Arc::new(Mutex::new(pair.master.take_writer().unwrap())), master: Arc::new(Mutex::new(Some(pair.master))), resize_gate: Arc::default(),
            killer: Box::new(NoChild), stream, reader_done: None, replay: true, kind: "agent".into(),
            cols: 120, rows: 24, started_at: 0, sizes: Sizes::default() }
    }

    fn replay_core() -> Arc<Core> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/pty-replay-tests")
            .join(format!("{}-{}", std::process::id(), GEN.fetch_add(1, Ordering::Relaxed)));
        Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap()
    }

    #[test]
    fn early_teardown_answers_a_retired_generation() {
        use std::{sync::mpsc, time::Duration};
        if isolated_lock_test("pty::tests::early_teardown_answers_a_retired_generation") { return; }
        struct Reply(mpsc::Sender<Vec<u8>>);
        impl Write for Reply {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let core = replay_core();
        let stream = Arc::new(PtyStream::default());
        let (tx, rx) = mpsc::channel();
        let mut session = replay_session(1, stream.clone());
        let (drained_tx, drained_rx) = mpsc::channel();
        session.reader_done = Some(drained_rx);
        session.writer = Arc::new(Mutex::new(Box::new(Reply(tx))));
        core.ptys.insert("kaan".into(), session);
        stream.subscribe(crate::agents::shell_sink_for_test(&core, "kaan", Exited::default())).unwrap();
        stream.publish(b"\x1b[");
        assert!(core.ptys.kill("kaan"));
        assert!(!core.ptys.is_running("kaan"));
        stream.publish(b"6n");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).expect("retired generation must still answer CPR"), b"\x1b[1;1R");
        stream.close();
        drained_tx.send(()).unwrap();
        wait_teardowns(&core.ptys, HashMap::is_empty);
    }

    fn wait_teardowns(ptys: &Ptys, done: impl Fn(&HashMap<u64, bool>) -> bool) {
        let jobs = ptys.teardowns.jobs.lock().unwrap();
        let (jobs, _) = ptys.teardowns.changed.wait_timeout_while(jobs, Duration::from_secs(2), |jobs| !done(jobs)).unwrap();
        assert!(done(&jobs), "teardown state did not reach its expected boundary: {jobs:?}");
    }

    #[test]
    fn early_teardown_never_answers_into_a_replacement() {
        use std::sync::mpsc;
        if isolated_lock_test("pty::tests::early_teardown_never_answers_into_a_replacement") { return; }
        struct Reply(mpsc::Sender<Vec<u8>>);
        impl Write for Reply {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.send(bytes.to_vec()).unwrap(); Ok(bytes.len()) }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let core = replay_core();
        let old = Arc::new(PtyStream::default());
        let (old_tx, old_rx) = mpsc::channel();
        let (new_tx, new_rx) = mpsc::channel();
        let mut session = replay_session(1, old.clone());
        session.writer = Arc::new(Mutex::new(Box::new(Reply(old_tx))));
        core.ptys.insert("kaan".into(), session);
        old.subscribe(crate::agents::shell_sink_for_test(&core, "kaan", Exited::default())).unwrap();
        old.publish(b"\x1b[");
        assert!(core.ptys.kill("kaan"));
        let new = Arc::new(PtyStream::default());
        let mut session = replay_session(2, new.clone());
        session.writer = Arc::new(Mutex::new(Box::new(Reply(new_tx))));
        core.ptys.insert("kaan".into(), session);
        old.publish(b"6n");
        assert_eq!(old_rx.recv_timeout(Duration::from_secs(2)).unwrap(), b"\x1b[1;1R");
        assert_eq!(new_rx.try_recv(), Err(mpsc::TryRecvError::Empty), "late reply must not enter the replacement input");
        assert!(core.ptys.retire("kaan", Some(1)).is_none(), "old waiter cannot claim a second exit or the replacement");
        assert_eq!(core.ptys.generation("kaan"), Some(2));
        core.ptys.write("kaan", b"USER_INPUT").unwrap();
        assert_eq!(new_rx.recv_timeout(Duration::from_secs(2)).unwrap(), b"USER_INPUT");
        old.close();
        new.close();
        core.ptys.kill("kaan");
        wait_teardowns(&core.ptys, HashMap::is_empty);
    }

    #[test]
    fn early_teardown_timeout_keeps_replacement_and_shutdown_available() {
        use std::sync::mpsc;
        if isolated_lock_test("pty::tests::early_teardown_timeout_keeps_replacement_and_shutdown_available") { return; }
        struct Logs(mpsc::Sender<(String, serde_json::Value)>);
        impl log::Log for Logs {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool { true }
            fn log(&self, record: &log::Record<'_>) {
                if record.target() != "dex::audit" { return; }
                let payload: serde_json::Value = serde_json::from_str(&record.args().to_string()).unwrap();
                if matches!(payload["event"].as_str(), Some("pty_teardown_timeout" | "pty_teardown_complete")) {
                    let _ = self.0.send((record.level().to_string(), payload));
                }
            }
            fn flush(&self) {}
        }
        let (log_tx, log_rx) = mpsc::channel();
        // This logger lives only in the isolated test process; other tests keep their logger.
        log::set_logger(Box::leak(Box::new(Logs(log_tx)))).unwrap();
        log::set_max_level(log::LevelFilter::Info);
        #[derive(Clone, Default)]
        struct Exits(Arc<std::sync::atomic::AtomicUsize>);
        impl crate::events::EventSink for Exits {
            fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
                if event == "agent-status" && payload["agentId"] == "kaan" && payload["status"] == "offline" {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            }
        }
        let exits = Exits::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/pty-timeout-tests")
            .join(format!("{}-{}", std::process::id(), GEN.fetch_add(1, Ordering::Relaxed)));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(exits.clone())).unwrap();
        let old_gen = GEN.fetch_add(1, Ordering::Relaxed);
        let session = replay_session(old_gen, Arc::new(PtyStream::default()));
        let master = session.master.clone();
        let stalled = master.lock().unwrap();
        core.ptys.insert("kaan".into(), session);
        TEST_TEARDOWN_TIMEOUT.with(|value| *value.borrow_mut() = Some(Duration::ZERO));
        assert!(core.ptys.kill("kaan"));
        assert!(!core.ptys.is_running("kaan"));
        assert_eq!(exits.0.load(Ordering::SeqCst), 1, "offline callback must not wait for close");
        assert!(!core.ptys.kill("kaan"));
        wait_teardowns(&core.ptys, |jobs| jobs.get(&old_gen) == Some(&true));
        let (level, timed_out) = log_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(level, "ERROR");
        assert_eq!(timed_out["event"], "pty_teardown_timeout");
        assert_eq!(timed_out["fields"]["generation"], old_gen);
        assert_eq!(timed_out["fields"]["pending"], 1);
        assert_eq!(timed_out["fields"].as_object().unwrap().len(), 4, "only id, generation, duration and count are logged");
        assert_eq!(core.ptys.teardowns.jobs.lock().unwrap().len(), 1, "overrun remains tracked");
        // Real spawn with the same id: the old master lock and teardown cannot affect it.
        core.pty_spawn(Spawn { id: "kaan".into(), cmd: crate::shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() },
            crate::agents::shell_sink_for_test(&core, "kaan", Exited::default())).unwrap();
        assert!(core.ptys.is_running("kaan"));
        core.shutdown();
        assert_eq!(exits.0.load(Ordering::SeqCst), 1, "old close has one callback even after respawn/shutdown");
        drop(stalled);
        wait_teardowns(&core.ptys, |jobs| !jobs.contains_key(&old_gen));
        let (_, completed) = log_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(completed["event"], "pty_teardown_complete");
        assert_eq!(completed["fields"]["generation"], old_gen);
        assert_eq!(completed["fields"]["pending"], 0);
        assert_eq!(log_rx.try_recv(), Err(mpsc::TryRecvError::Empty), "one timeout and one late completion per generation");
        assert!(master.lock().unwrap().is_none(), "only the teardown worker consumes the master, despite retained Arcs");
        core.ptys.kill("kaan");
        wait_teardowns(&core.ptys, HashMap::is_empty);
    }

    #[test]
    fn early_teardown_releases_start_and_admission_gates() {
        use std::{sync::mpsc, time::Duration};
        if isolated_lock_test("pty::tests::early_teardown_releases_start_and_admission_gates") { return; }
        let timeout = Duration::from_secs(2);
        #[derive(Debug)]
        struct Killing { entered: mpsc::Sender<()>, release: Mutex<mpsc::Receiver<()>> }
        impl ChildKiller for Killing {
            fn kill(&mut self) -> std::io::Result<()> {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv_timeout(Duration::from_secs(6)).expect("test releases the stalled kill");
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> { unreachable!() }
        }
        let core = replay_core();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let mut session = replay_session(GEN.fetch_add(1, Ordering::Relaxed), Arc::new(PtyStream::default()));
        session.killer = Box::new(Killing { entered: entered_tx, release: Mutex::new(release_rx) });
        core.ptys.insert("kaan".into(), session);
        let closing = {
            let core = core.clone();
            std::thread::spawn(move || {
                let _admission = core.admit("kaan").unwrap();
                let _start = core.agent_start_gate.lock().unwrap();
                assert!(core.ptys.kill("kaan"));
                returned_tx.send(()).unwrap();
            })
        };
        entered_rx.recv_timeout(timeout).unwrap();
        returned_rx.recv_timeout(timeout).expect("kill must return while teardown is stalled");
        closing.join().unwrap();
        assert!(!core.ptys.is_running("kaan"));
        assert!(!core.ptys.kill("kaan"), "retirement and exit notification happen once");
        let _admission = core.admit("kaan").expect("admission gate was released");
        let _start = core.agent_start_gate.try_lock().expect("start gate was released");
        core.pty_spawn(Spawn { id: "independent".into(), cmd: crate::shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() },
            crate::agents::shell_sink_for_test(&core, "independent", Exited::default())).unwrap();
        release_tx.send(()).unwrap();
        core.ptys.kill("independent");
        wait_teardowns(&core.ptys, HashMap::is_empty);
    }

    #[test]
    fn early_teardown_spawn_binds_before_its_first_query() {
        use std::sync::mpsc;
        if isolated_lock_test("pty::tests::early_teardown_spawn_binds_before_its_first_query") { return; }
        struct Replies { original: Box<dyn Write + Send>, tx: mpsc::Sender<Vec<u8>> }
        impl Write for Replies {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.original.write_all(bytes)?;
                self.tx.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { self.original.flush() }
        }
        struct Paused<S> {
            sink: S,
            pause: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
            replies: mpsc::Sender<Vec<u8>>,
            exited: mpsc::Sender<()>,
        }
        impl<S: PtySink> PtySink for Paused<S> {
            fn bind_input(&mut self, input: PtyInput) {
                {
                    let mut writer = input.0.lock().unwrap();
                    let original = std::mem::replace(&mut *writer, Box::new(Vec::<u8>::new()));
                    *writer = Box::new(Replies { original, tx: self.replies.clone() });
                }
                self.sink.bind_input(input);
            }
            fn data(&mut self, bytes: &[u8]) -> bool {
                if let Some((entered, release)) = self.pause.take() {
                    // Replay an incomplete cursor query at the first real reader callback. This
                    // works even when this Windows version does not request cursor inheritance.
                    self.sink.data(b"\x1b[");
                    entered.send(()).unwrap();
                    release.recv_timeout(Duration::from_secs(2)).unwrap();
                    self.sink.data(b"6n");
                }
                self.sink.data(bytes)
            }
            fn exit(&mut self) { self.sink.exit(); self.exited.send(()).unwrap(); }
        }
        let core = replay_core();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        core.pty_spawn(Spawn { id: "kaan".into(), cmd: crate::shell::SHELL_CMD.into(), cols: 80, rows: 24, ..Default::default() },
            Paused { sink: crate::agents::shell_sink_for_test(&core, "kaan", Exited::default()), pause: Some((entered_tx, release_rx)), replies: reply_tx, exited: exit_tx }).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(core.ptys.kill("kaan"), "retire must not wait on the paused reader's stream lock");
        release_tx.send(()).unwrap();
        assert_eq!(reply_rx.recv_timeout(Duration::from_secs(2)).unwrap(), b"\x1b[1;1R");
        exit_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_teardowns(&core.ptys, HashMap::is_empty);
        assert_eq!(exit_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected), "reader emits exit exactly once");
        assert!(!core.ptys.kill("kaan"));
        core.shutdown();
    }

    #[test]
    fn early_teardown_stalled_job_does_not_keep_the_process_alive() {
        if isolated_lock_test("pty::tests::early_teardown_stalled_job_does_not_keep_the_process_alive") { return; }
        let ptys = Ptys::default();
        let session = replay_session(1, Arc::new(PtyStream::default()));
        let master = session.master.clone();
        let stalled = master.lock().unwrap();
        ptys.insert("kaan".into(), session);
        TEST_TEARDOWN_TIMEOUT.with(|value| *value.borrow_mut() = Some(Duration::ZERO));
        assert!(ptys.kill("kaan"));
        wait_teardowns(&ptys, |jobs| jobs.get(&1) == Some(&true));
        // Deliberately leave only this child's teardown blocked until process exit. The parent
        // requires bounded child exit; Rust must not join the detached closer on shutdown.
        std::mem::forget(stalled);
    }

    #[test]
    fn finished_session_retains_replay_for_its_first_attach() {
        let ptys = Ptys::default();
        let stream = Arc::new(PtyStream::default());
        stream.publish(b"RX:FAST_FINAL_OUTPUT");
        ptys.sessions.lock().unwrap().insert("kaan".into(), replay_session(1, stream.clone()));
        assert!(ptys.kill("kaan"));
        stream.close();
        assert!(ptys.list().is_empty(), "list remains live-only");
        let exited = Exited::default();
        let attachment = ptys.attach("kaan", exited.clone()).unwrap();
        assert!(String::from_utf8_lossy(&attachment.snapshot.bytes()).contains("RX:FAST_FINAL_OUTPUT"));
        assert!(exited.0.load(Ordering::SeqCst));
        assert!(ptys.write("kaan", b"input").is_err());
    }

    #[test]
    fn finished_replay_is_capped_at_two_thousand_lines() {
        let ptys = Ptys::default();
        let stream = Arc::new(PtyStream::new(2, 40, 10_000));
        let output: String = (0..3_005).map(|i| format!("line-{i:04}\r\n")).collect();
        stream.publish(output.as_bytes());
        stream.close();
        ptys.sessions.lock().unwrap().insert("kaan".into(), replay_session(1, stream));
        ptys.kill("kaan");
        let snapshot = ptys.attach("kaan", Exited::default()).unwrap().snapshot;
        let history = String::from_utf8(snapshot.scrollback).unwrap();
        // Snapshot also pads the viewport before ANSI redraw; count the numbered history rows.
        assert_eq!(history.lines().filter(|line| line.starts_with("line-")).count(), 2_000);
        assert!(!history.contains("line-0000"));
        assert!(String::from_utf8_lossy(&snapshot.ansi).contains("line-3004"));
    }

    #[test]
    fn respawn_replaces_replay_and_an_old_exit_cannot_replace_it() {
        let ptys = Ptys::default();
        let old = Arc::new(PtyStream::default());
        old.publish(b"OLD_SESSION");
        old.close();
        ptys.insert("kaan".into(), replay_session(1, old));
        drop(ptys.retire("kaan", Some(1)).unwrap());
        let new = Arc::new(PtyStream::default());
        new.publish(b"NEW_SESSION");
        ptys.insert("kaan".into(), replay_session(2, new.clone()));
        assert!(ptys.finished.lock().unwrap().is_empty());
        assert!(ptys.retire("kaan", Some(1)).is_none());
        assert!(ptys.is_running("kaan"));
        new.close();
        drop(ptys.retire("kaan", Some(2)).unwrap());
        let replay = ptys.attach("kaan", Exited::default()).unwrap().snapshot.bytes();
        let text = String::from_utf8_lossy(&replay);
        assert!(text.contains("NEW_SESSION") && !text.contains("OLD_SESSION"));
        assert_eq!(ptys.finished.lock().unwrap().len(), 1);
    }

    #[test]
    fn retiring_before_output_eof_keeps_the_tail_and_exit() {
        #[derive(Clone, Default)]
        struct Tail(Arc<Mutex<(Vec<u8>, usize)>>);
        impl PtySink for Tail {
            fn data(&mut self, bytes: &[u8]) -> bool { self.0.lock().unwrap().0.extend_from_slice(bytes); true }
            fn exit(&mut self) { self.0.lock().unwrap().1 += 1; }
        }
        let ptys = Ptys::default();
        let stream = Arc::new(PtyStream::default());
        stream.publish(b"BEFORE_EXIT");
        ptys.insert("kaan".into(), replay_session(1, stream.clone()));
        drop(ptys.retire("kaan", Some(1)).unwrap());
        let tail = Tail::default();
        let attached = ptys.attach("kaan", tail.clone()).unwrap();
        assert!(String::from_utf8_lossy(&attached.snapshot.bytes()).contains("BEFORE_EXIT"));
        stream.publish(b"FINAL_TAIL");
        stream.close();
        assert_eq!(*tail.0.lock().unwrap(), (b"FINAL_TAIL".to_vec(), 1));
        assert!(!ptys.detach("kaan", attached.subscriber));
    }

    #[test]
    fn retiring_before_output_eof_preserves_split_csi() {
        for attach_before_eof in [false, true] {
            let ptys = Ptys::default();
            let stream = Arc::new(PtyStream::new(2, 40, 10_000));
            stream.publish(b"\x1b[");
            ptys.insert("kaan".into(), replay_session(1, stream.clone()));
            drop(ptys.retire("kaan", Some(1)).unwrap());
            if attach_before_eof { ptys.attach("kaan", Exited::default()).unwrap(); }
            stream.publish(b"31mFINAL");
            stream.close();
            let snapshot = ptys.attach("kaan", Exited::default()).unwrap().snapshot;
            let mut replay = vt100::Parser::new(snapshot.rows, snapshot.cols, 2_000);
            replay.process(&snapshot.bytes());
            assert_eq!(replay.screen().contents(), "FINAL", "attach before EOF: {attach_before_eof}");
            for col in 0..5 {
                assert_eq!(replay.screen().cell(0, col).unwrap().fgcolor(), vt100::Color::Idx(1));
            }
        }
    }

    #[test]
    fn retiring_before_output_eof_preserves_split_utf8() {
        for attach_before_eof in [false, true] {
            let ptys = Ptys::default();
            let stream = Arc::new(PtyStream::new(2, 40, 10_000));
            let bytes = "ş".as_bytes();
            stream.publish(&bytes[..1]);
            ptys.insert("kaan".into(), replay_session(1, stream.clone()));
            drop(ptys.retire("kaan", Some(1)).unwrap());
            if attach_before_eof { ptys.attach("kaan", Exited::default()).unwrap(); }
            stream.publish(&bytes[1..]);
            stream.close();
            let snapshot = ptys.attach("kaan", Exited::default()).unwrap().snapshot;
            let mut replay = vt100::Parser::new(snapshot.rows, snapshot.cols, 2_000);
            replay.process(&snapshot.bytes());
            assert_eq!(replay.screen().contents(), "ş", "attach before EOF: {attach_before_eof}");
        }
    }

    #[test]
    fn transient_sessions_do_not_accumulate_finished_replay() {
        let ptys = Ptys::default();
        let mut session = replay_session(1, Arc::new(PtyStream::default()));
        session.replay = false;
        ptys.insert("sh-transient".into(), session);
        drop(ptys.retire("sh-transient", Some(1)).unwrap());
        assert!(ptys.finished.lock().unwrap().is_empty());
    }

    #[test]
    fn agent_deletion_clears_finished_replay() {
        let core = replay_core();
        let stream = Arc::new(PtyStream::default());
        stream.close();
        core.ptys.insert("kaan".into(), replay_session(1, stream));
        drop(core.ptys.retire("kaan", Some(1)).unwrap());
        core.agent_delete("kaan").unwrap();
        core.shutdown();
        assert!(core.ptys.finished.lock().unwrap().is_empty());
        assert!(core.ptys.attach("kaan", Exited::default()).is_err());
    }

    #[test]
    fn team_deletion_clears_only_its_members_finished_replay() {
        let core = replay_core();
        let team = core.team_create(crate::teams::NewTeam { name: "Replay".into(), kind: "service".into(),
            cwd: None, worker_roles: None, members: None }).unwrap();
        let ids: Vec<_> = core.agent_list().unwrap().into_iter().filter(|a| a.team_id == team.id).map(|a| a.id).collect();
        for id in ids.iter().map(String::as_str).chain(["kaan"]) {
            let stream = Arc::new(PtyStream::default());
            stream.close();
            core.ptys.insert(id.into(), replay_session(1, stream));
            drop(core.ptys.retire(id, Some(1)).unwrap());
        }
        core.team_delete(&team.id, false).unwrap();
        core.shutdown();
        for id in &ids {
            assert!(core.ptys.attach(id, Exited::default()).is_err());
        }
        assert_eq!(core.ptys.finished.lock().unwrap().len(), 1);
        assert!(core.ptys.attach("kaan", Exited::default()).is_ok());
    }

    #[test]
    fn bound_launch_creates_only_its_memory_grants_and_passes_them_to_both_engines() {
        struct Sink;
        impl PtySink for Sink {
            fn data(&mut self, _: &[u8]) -> bool { true }
            fn exit(&mut self) {}
        }
        let f = crate::projects::tests::Fixture::new();
        let root = std::env::temp_dir().join(format!("dex-pty-launch-{}", std::process::id()));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        for engine in ["claude", "codex"] {
            crate::db::with(&core.db, |c| c.execute("UPDATE agents SET active_engine=?1 WHERE id='timur'", [engine]).map(|_| ()).map_err(|e| e.to_string())).unwrap();
            let base = f.0.join(engine);
            let root = base.join("agents");
            let write = vec![base.join("team/shared"), root.join("timur"), base.join("project/shared")];
            let read = vec![base.join("team"), base.join("project")];
            let other = root.join("other-agent");
            let launch = AgentLaunch { job: None,
                memory: crate::hooks::MemoryScope { root: root.clone(), write: write.clone(), read: read.clone() },
                prompt: "BOUND-CONTEXT-PROMPT".into() };
            let req = Spawn { id: "timur".into(), cmd: f.0.join(format!("{engine}.exe")).to_string_lossy().into_owned(),
                cwd: Some(f.0.to_string_lossy().into_owned()), memory_dir: Some(base.join("ipc-memory").to_string_lossy().into_owned()),
                args: vec!["--resume".into(), "s1".into()],
                codex: Some(crate::codex::Launch { resume: Some("s1".into()), ..Default::default() }),
                cols: 80, rows: 24, ..Default::default() };
            CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
            let result = spawn_bound(&core, req, Some(launch), Sink);
            let argv = CAPTURE.with(|c| c.borrow_mut().take().unwrap()).pop().unwrap();
            result.unwrap();
            assert!(write.iter().all(|d| d.is_dir()));
            assert!(!other.exists() && !base.join("ipc-memory").exists());
            let grants: Vec<_> = argv.windows(2).filter(|w| w[0] == "--add-dir").map(|w| w[1].clone()).collect();
            assert_eq!(grants, write.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>());
            assert!(!read.iter().any(|p| grants.contains(&p.to_string_lossy().into_owned())));
            assert_eq!(core.hooks.memory.lock().unwrap().get("timur"), Some(&root));
            if engine == "claude" {
                assert!(argv.windows(2).any(|w| w == ["--append-system-prompt", "BOUND-CONTEXT-PROMPT"]));
            } else {
                let resume = argv.iter().position(|s| s == "resume").unwrap();
                assert!(resume < argv.iter().position(|s| s == "--add-dir").unwrap());
            }
            assert!(!core.ptys.is_running("timur"));
        }
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn setting_sources_load_project_but_not_local() {
        let sources: Vec<&str> = SETTING_SOURCES.split(',').collect();
        assert!(sources.contains(&"user") && sources.contains(&"project"), "CLAUDE.md + committed settings load");
        assert!(!sources.contains(&"local"), "settings.local.json must not load");
    }

    #[test]
    fn claude_loads_only_dex_mcp_servers() {
        let mut builder = CommandBuilder::new("claude");
        mcp_args(&mut builder, "dex-mcp.json");
        builder.arg("--allowedTools");
        let argv: Vec<String> = builder.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let at = |flag: &str| argv.iter().position(|a| a == flag).unwrap();
        assert!(argv.contains(&"--strict-mcp-config".to_string()), "global MCP servers must not load");
        // --mcp-config is variadic: the strict flag goes before it, never between it and a positional.
        assert!(at("--strict-mcp-config") < at("--mcp-config"));
        assert_eq!(argv[at("--mcp-config") + 1], "dex-mcp.json");
    }

    #[test]
    fn size_follows_the_active_client() {
        let mut s = Sizes::default();
        assert_eq!(s.report("desk", 200, 50), Some((200, 50)), "ilk raporlayan sahip olur");
        assert_eq!(s.report("phone", 45, 30), None, "pasif istemcinin boyutu uygulanmaz");
        assert_eq!(s.claim("phone", 45, 30), Some((45, 30)), "girdi/odak boyutu alır");
        assert_eq!(s.report("desk", 210, 50), None, "sahip olmayan masaüstü raporu yok sayılır");
        assert_eq!(s.claim("desk", 210, 50), Some((210, 50)), "masaüstü yazınca boyut geri gelir");
        assert_eq!(s.report("desk", 180, 40), Some((180, 40)), "sahibin pencere boyutu uygulanır");
    }

    #[test]
    fn a_released_size_goes_to_the_latest_reporter() {
        let mut s = Sizes::default();
        s.report("desk", 200, 50);
        s.report("tab", 100, 40);
        s.report("phone", 45, 30);
        s.claim("phone", 45, 30);
        assert_eq!(s.release("desk"), None, "sahip olmayanın gidişi boyutu değiştirmez");
        assert_eq!(s.release("phone"), Some((100, 40)));
        assert_eq!(s.owner.as_deref(), Some("tab"));
        assert_eq!(s.release("tab"), None);
        assert_eq!(s.owner, None);
        assert_eq!(s.report("desk", 200, 50), Some((200, 50)), "sahipsiz PTY'yi ilk rapor alır");
    }

    /// Two clients on one real PTY: the size and its in-order push to every view follow input.
    #[test]
    fn two_clients_on_one_pty() {
        struct Seen(Arc<Mutex<Vec<(u16, u16)>>>);
        impl PtySink for Seen {
            fn data(&mut self, _: &[u8]) -> bool { true }
            fn exit(&mut self) {}
            fn resized(&mut self, cols: u16, rows: u16) { self.0.lock().unwrap().push((cols, rows)); }
        }
        let root = std::env::temp_dir().join(format!("dex-pty-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        let core = Core::start(paths, crate::Events::new(crate::events::NullSink)).unwrap();
        let spawn = Spawn { id: "kaan".into(), cmd: crate::shell::SHELL_CMD.into(), cols: 120, rows: 40, ..Default::default() };
        let (desk, phone) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let (sink, wait_ready) = crate::agents::shell_sink_with_ready_for_test(&core, "kaan", Seen(desk.clone()));
        core.pty_spawn(spawn, sink).unwrap();
        wait_ready();
        // #1586 teşhis — neden bulununca kaldırılacak. No terminal bytes or fixture paths.
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "two_clients fixture: startup ready");
        core.ptys.attach("kaan", Seen(phone.clone())).unwrap();
        let size = |core: &Core| core.ptys.list().iter().find(|p| p.id == "kaan").map(|p| (p.cols, p.rows)).unwrap();

        let _ = writeln!(std::io::stderr(), "two_clients fixture: resizing");
        core.ptys.resize("kaan", "desk", 200, 50, false).unwrap();
        core.ptys.resize("kaan", "phone", 45, 30, false).unwrap();
        assert_eq!(size(&core), (200, 50), "pasif telefon PTY'yi daraltmaz");
        core.ptys.resize("kaan", "phone", 45, 30, true).unwrap();
        assert_eq!(size(&core), (45, 30));
        core.ptys.resize("kaan", "desk", 200, 50, true).unwrap();
        assert_eq!(size(&core), (200, 50));
        let pushed = vec![(200, 50), (45, 30), (200, 50)];
        assert_eq!(*desk.lock().unwrap(), pushed, "her görünüm boyutu öğrenir");
        assert_eq!(*phone.lock().unwrap(), pushed);
        let snap = core.ptys.attach("kaan", Seen(Arc::default())).unwrap().snapshot;
        assert_eq!((snap.cols, snap.rows), (200, 50), "snapshot PTY boyutunda");

        core.ptys.resize("kaan", "phone", 45, 30, true).unwrap();
        core.ptys.release_client("phone");
        assert_eq!(size(&core), (200, 50), "telefon kopunca masaüstü boyutu döner");
        let _ = writeln!(std::io::stderr(), "two_clients fixture: closing");
        core.ptys.kill("kaan");
        let _ = writeln!(std::io::stderr(), "two_clients fixture: closed");
        drop(core);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn chain_active_engine_is_the_spawn_gate_engine() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(effective_engine(&conn, "kaan").as_deref(), Some("claude"));
        conn.execute("UPDATE agents SET active_engine = 'codex' WHERE id = 'kaan'", []).unwrap();
        assert_eq!(effective_engine(&conn, "kaan").as_deref(), Some("codex"));
        let configured: String = conn.query_row("SELECT engine FROM agents WHERE id = 'kaan'", [], |r| r.get(0)).unwrap();
        assert_eq!(configured, "claude", "zincir kalıcı motor ayarını değiştirmez");
    }
}
