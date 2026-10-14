//! Process liveness for per-process files (`claude-hooks-<pid>.json`, `dex-mcp-<pid>.json`).
//! A reused pid only keeps a stale file around until a later sweep; it never deletes a live one.

#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        !ok || code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    (unsafe { libc::kill(pid as libc::pid_t, 0) == 0 })
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Agent resource policy. DEX-27: each agent pane's tree runs below-normal priority in its own
/// kill-on-close Job Object. DEX-31: all those jobs are nested in ONE job with a hard CPU cap on
/// the total, so agents slow down together when they reach it (no queue, no kill) and speed up
/// again when the load drops. The per-agent cap stays as an option.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// Hard CPU cap on all agents together, % of total machine CPU. 0 = unlimited.
    pub total_cpu_percent: u16,
    /// Hard CPU cap per agent tree, % of total machine CPU. 0 = off.
    pub agent_cpu_percent: u16,
    /// Per-agent committed-memory ceiling in bytes. 0 = off.
    pub mem_bytes: u64,
}

/// Default: all agents together ≤ 80% CPU, no per-agent cap, no memory cap. A percent (not a
/// core count) scales to any machine; the remaining 20% keeps the desktop responsive. Memory
/// stays off: over the limit the OS fails further commits and the agent typically crashes —
/// a guard that kills work is worse than none.
pub fn default_limits() -> Limits {
    Limits { total_cpu_percent: 80, agent_cpu_percent: 0, mem_bytes: 0 }
}

/// Env overrides the stored settings (for a one-off run; the Settings panel is the normal way):
/// `DEX_AGENTS_CPU_PERCENT` (total), `DEX_AGENT_CPU_PERCENT` (per agent) — 0 = unlimited,
/// 1–100, above 100 → 100, garbage ignored; `DEX_AGENT_MEM_BYTES` (0 = off, bytes).
/// Returns the effective limits and the names of the variables that took effect.
pub fn with_env(mut l: Limits) -> (Limits, Vec<&'static str>) {
    let mut used = Vec::new();
    let pct = |name: &'static str| std::env::var(name).ok().and_then(|v| v.trim().parse::<u16>().ok()).map(|n| n.min(100));
    if let Some(n) = pct("DEX_AGENTS_CPU_PERCENT") {
        l.total_cpu_percent = n;
        used.push("DEX_AGENTS_CPU_PERCENT");
    }
    if let Some(n) = pct("DEX_AGENT_CPU_PERCENT") {
        l.agent_cpu_percent = n;
        used.push("DEX_AGENT_CPU_PERCENT");
    }
    if let Some(n) = std::env::var("DEX_AGENT_MEM_BYTES").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        l.mem_bytes = n;
        used.push("DEX_AGENT_MEM_BYTES");
    }
    (l, used)
}

/// Defaults + env (no stored settings): for callers without a DB.
pub fn limits_from_env() -> Limits {
    with_env(default_limits()).0
}

/// DEX-31: the one job every agent tree of this Dex process is nested in. Created on first use
/// and never closed — it lives as long as Dex (the OS closes it on exit; the pane jobs inside
/// are what kill trees). No kill-on-close here: closing it must never take all agents down.
#[cfg(windows)]
fn agents_job() -> Option<windows_sys::Win32::Foundation::HANDLE> {
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let h = *JOB.get_or_init(|| unsafe {
        windows_sys::Win32::System::JobObjects::CreateJobObjectW(std::ptr::null(), std::ptr::null())
    } as usize);
    (h != 0).then_some(h as _)
}

/// Re-caps the all-agents job now: running agents slow down / speed up without a restart.
pub fn set_total_cap(percent: u16) {
    #[cfg(windows)]
    if let Some(job) = agents_job() {
        if !unsafe { set_cpu_cap(job, percent) } {
            eprintln!("dex: toplam CPU tavanı güncellenemedi");
        }
    }
    #[cfg(unix)]
    let _ = percent;
}

/// Hard CPU cap on a job (percent of the machine); 0 turns rate control off.
#[cfg(windows)]
unsafe fn set_cpu_cap(job: windows_sys::Win32::Foundation::HANDLE, percent: u16) -> bool {
    use windows_sys::Win32::System::JobObjects::*;
    let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = std::mem::zeroed();
    if percent > 0 {
        cpu.ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        cpu.Anonymous.CpuRate = percent.min(100) as u32 * 100; // percent → 1/100ths
    }
    let size = std::mem::size_of_val(&cpu) as u32;
    SetInformationJobObject(job, JobObjectCpuRateControlInformation, &cpu as *const _ as _, size) != 0
}
/// A spawned process and everything it starts later (DEX-20: claude → npx → MCP server → browser).
/// Dropping it kills what is left of the tree, so a pane leaves no orphan behind.
/// Windows: a kill-on-close Job Object; if Dex itself dies the OS closes the handle, same effect.
/// Children started before [`Tree::adopt`] returns stay outside (claude starts MCP servers well after).
pub struct Tree {
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
    /// The adopted pid: the key in [`JOBS`] (the process panel finds an agent's job by it).
    #[cfg(windows)]
    root: u32,
    #[cfg(unix)]
    pgid: u32,
}

// The job handle is only closed, never shared.
#[cfg(windows)]
unsafe impl Send for Tree {}

impl Tree {
    /// Defaults + env ([`limits_from_env`]), no stored settings. `pty::spawn` (claude)
    /// uses [`Tree::adopt_with`] with the Settings panel's limits.
    #[cfg(windows)]
    pub fn adopt(pid: u32) -> Option<Tree> {
        Self::adopt_with(pid, &limits_from_env())
    }

    #[cfg(windows)]
    pub fn adopt_with(pid: u32, limits: &Limits) -> Option<Tree> {
        Self::adopt_in(pid, limits, agents_job())
    }

    /// `parent` = the all-agents job ([`agents_job`]; tests pass their own). Its cap is set to
    /// the current total on every spawn, so a changed setting reaches running agents too.
    #[cfg(windows)]
    fn adopt_in(pid: u32, limits: &Limits, parent: Option<windows_sys::Win32::Foundation::HANDLE>) -> Option<Tree> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::*;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION,
            PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };
        unsafe {
            if let Some(p) = parent {
                if !set_cpu_cap(p, limits.total_cpu_percent) {
                    eprintln!("dex: toplam CPU tavanı kurulamadı (ajan başı job yine kurulur)");
                }
            }
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if limits.mem_bytes > 0 {
                // Whole-job commit ceiling (opt-in): past it the OS fails further commits,
                // which usually crashes the agent — hence off by default.
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.JobMemoryLimit = limits.mem_bytes as usize;
            }
            let size = std::mem::size_of_val(&info) as u32;
            if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as _, size) == 0 {
                CloseHandle(job);
                return None;
            }
            if limits.agent_cpu_percent > 0 && !set_cpu_cap(job, limits.agent_cpu_percent) {
                CloseHandle(job);
                return None;
            }
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_SET_INFORMATION, 0, pid);
            if proc.is_null() {
                CloseHandle(job);
                return None;
            }
            // Niceness for the desktop; the hard caps are the enforcement (a child may
            // raise its own class, it cannot escape the caps). Inherited by later children.
            SetPriorityClass(proc, BELOW_NORMAL_PRIORITY_CLASS);
            // Parent first, then the pane's own job: the pane job becomes a child of the parent
            // (nested jobs, Windows 8+). The reverse order is refused with ACCESS_DENIED
            // (measured). A failed parent assignment only loses the total cap, never the pane job.
            if let Some(p) = parent {
                if AssignProcessToJobObject(p, proc) == 0 {
                    eprintln!("dex: ajan toplam CPU job'una alınamadı (pid {pid}); yalnız pane job'u");
                }
            }
            let ok = AssignProcessToJobObject(job, proc) != 0;
            CloseHandle(proc);
            if !ok {
                CloseHandle(job);
                return None;
            }
            jobs().insert(pid, job as usize);
            Some(Tree { job, root: pid })
        }
    }

    /// portable-pty makes the child a session (and process group) leader.
    #[cfg(unix)]
    pub fn adopt(pid: u32) -> Option<Tree> {
        Self::adopt_with(pid, &limits_from_env())
    }

    /// Unix keeps DEX-20 behavior; DEX-27 Linux policy is `nice` + cgroup v2 (report only).
    #[cfg(unix)]
    pub fn adopt_with(pid: u32, _limits: &Limits) -> Option<Tree> {
        Some(Tree { pgid: pid })
    }
}

impl Drop for Tree {
    /// Out of [`JOBS`] and closed under its lock: a panel query never holds a closed handle.
    #[cfg(windows)]
    fn drop(&mut self) {
        let mut jobs = jobs();
        if jobs.get(&self.root) == Some(&(self.job as usize)) {
            jobs.remove(&self.root);
        }
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job) };
    }

    // Playwright launches the browser in its own group but closes it when its server gets SIGTERM.
    #[cfg(unix)]
    fn drop(&mut self) {
        unsafe { libc::kill(-(self.pgid as libc::pid_t), libc::SIGTERM) };
    }
}

/// #911: every live agent job by its adopted (root) pid — the process panel's owner map. Only
/// what Dex itself adopted is here: a pid outside these jobs belongs to no agent.
#[cfg(windows)]
static JOBS: std::sync::Mutex<std::collections::BTreeMap<u32, usize>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(windows)]
fn jobs() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<u32, usize>> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner())
}

/// #911: one process of an agent's tree, as the process panel shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProcess {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// Shortened, secrets masked ([`short_cmd`]); the raw line never leaves this module.
    pub cmd: String,
    /// Share of the whole machine over the sample window (like the CPU limits).
    pub cpu_pct: f64,
    /// Working set.
    pub mem_mb: f64,
    pub kind: ProcKind,
    /// The agent's own CLI (claude): shown, never killed.
    pub root: bool,
}

/// #925: one agent's panel entry. `error` = its tree could not be read this time (the others
/// still come back); `processes` is then empty, not a silent "nothing runs".
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProcs {
    pub processes: Vec<AgentProcess>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    #[serde(rename = "browser-tool")]
    BrowserTool,
    Browser,
    Node,
    Cargo,
    Godot,
    Shell,
    Other,
}

/// CPU% is the difference of two samples this far apart, taken only when asked (no poll).
#[cfg(windows)]
const SAMPLE_MS: u64 = 300;
const CMD_MAX: usize = 200;

/// `name` = image file name, `cmd` = the raw command line (classification only).
pub fn classify(name: &str, cmd: &str) -> ProcKind {
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    let cmd = cmd.to_ascii_lowercase().replace('\\', "/");
    match stem {
        "chrome" | "msedge" | "firefox" | "chromium" | "brave" | "headless_shell" | "chrome-headless-shell" => ProcKind::Browser,
        "cmd" | "sh" | "node" | "npx" | "npm" if cmd.contains("@playwright/mcp") || cmd.contains("playwright-mcp") => ProcKind::BrowserTool,
        "cargo" | "rustc" | "rustdoc" | "clippy-driver" | "rustup" => ProcKind::Cargo,
        s if s.starts_with("cargo-") || s.starts_with("build-script-") => ProcKind::Cargo,
        s if s.starts_with("godot") => ProcKind::Godot,
        "node" | "npm" | "npx" | "bun" | "deno" | "pnpm" | "yarn" => ProcKind::Node,
        "cmd" | "powershell" | "pwsh" | "bash" | "sh" | "zsh" | "fish" => ProcKind::Shell,
        _ => ProcKind::Other,
    }
}

fn sensitive(word: &str) -> bool {
    let w = word.to_ascii_lowercase();
    ["token", "key", "pass", "pwd", "secret", "auth", "credential", "cookie"].iter().any(|k| w.contains(k))
}

/// An opaque credential-looking word: known prefixes or a long run of token characters.
fn opaque(word: &str) -> bool {
    let w = word.trim_matches(|c| c == '"' || c == '\'');
    ["sk-", "ghp_", "gho_", "github_pat_", "xox", "eyJ", "glpat-", "AKIA"].iter().any(|p| w.starts_with(p))
        || (w.len() >= 32 && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// URL userinfo and query hidden: `https://u:p@h/x?k=v` → `https://***@h/x?***`.
fn mask_url(word: &str) -> String {
    let Some(i) = word.find("://") else { return word.to_string() };
    let (scheme, rest) = word.split_at(i + 3);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    let auth = match auth.rfind('@') {
        Some(at) => format!("***{}", &auth[at..]),
        None => auth.to_string(),
    };
    let tail = match tail.find('?') {
        Some(q) => format!("{}?***", &tail[..q]),
        None => tail.to_string(),
    };
    format!("{scheme}{auth}{tail}")
}

/// Arguments as the program sees them: split on whitespace outside `"…"` and outside `{…}`, so a
/// quoted value or an inline JSON object stays one argument (and is masked as one).
fn split_args(s: &str) -> Vec<&str> {
    let (mut out, mut start, mut quoted, mut depth) = (Vec::new(), None, false, 0usize);
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && !quoted && depth == 0 {
            out.extend(start.take().map(|b| &s[b..i]));
            continue;
        }
        match c {
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
        start.get_or_insert(i);
    }
    out.extend(start.map(|b| &s[b..]));
    out
}

/// What the panel shows of a command line: the program's file name, then the arguments with
/// anything secret-looking masked (`--api-key X`, `token=X`, `Bearer X`, URL userinfo/query,
/// opaque tokens), cut to [`CMD_MAX`] characters. Masking is per argument ([`split_args`]): a
/// value after `--token` goes whole even when quoted with spaces, and a `k=v` whose value is
/// JSON or names a secret (`--config={"apiKey":…}`) loses all of `v`. Over-masking is fine;
/// leaking is not.
pub fn short_cmd(cmd: &str) -> String {
    let cmd = cmd.trim();
    let (exe, args) = match cmd.strip_prefix('"').and_then(|r| r.split_once('"')) {
        Some((exe, args)) => (exe, args),
        None => cmd.split_once(char::is_whitespace).unwrap_or((cmd, "")),
    };
    let exe = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    let mut out = vec![mask_url(exe)];
    let scheme = |w: &str| matches!(w.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase().as_str(), "bearer" | "basic");
    let mut mask_next = false;
    for arg in split_args(args) {
        // `Authorization: Bearer X`: the scheme stays, the masking moves on to X.
        if std::mem::take(&mut mask_next) && !scheme(arg) {
            out.push("***".into());
            continue;
        }
        if let Some((k, v)) = arg.split_once('=').filter(|(k, _)| !k.contains("://") && !k.contains(char::is_whitespace)) {
            if sensitive(k) || v.contains('{') || sensitive(v) {
                out.push(format!("{k}=***"));
                continue;
            }
        }
        // A quoted argument with spaces (`"Authorization: Bearer X"`) is read word by word; once
        // a word asks for masking, the rest of that argument goes, its closing quote kept.
        let mut words = Vec::new();
        for w in arg.split_whitespace() {
            let lw = w.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
            if std::mem::take(&mut mask_next) && !scheme(w) {
                words.push(if arg.ends_with('"') { "***\"" } else { "***" }.to_string());
                break;
            }
            if scheme(w) {
                words.push(w.into());
                mask_next = true;
            } else if let Some((k, _)) = w.split_once('=').filter(|(k, _)| !k.contains("://") && sensitive(k)) {
                words.push(format!("{k}=***"));
            } else if (lw.starts_with('-') || lw.ends_with(':')) && sensitive(w) {
                words.push(w.into());
                mask_next = true;
            } else if opaque(w) {
                words.push("***".into());
            } else {
                words.push(mask_url(w));
            }
        }
        out.push(words.join(" "));
    }
    let s = out.join(" ");
    match s.char_indices().nth(CMD_MAX) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// Agent → the pid Dex spawned for it (its live PTY), from the same observation `core_status` shows.
fn agent_roots(core: &crate::Core, agent: Option<&str>) -> Result<Vec<(String, Option<u32>)>, String> {
    let status = core.core_status()?;
    let rows: Vec<(String, Option<u32>)> = status["agents"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let id = r["agentId"].as_str()?;
            let pid = r["pty"]["pid"].as_u64().map(|p| p as u32);
            (agent.is_none_or(|a| a == id) && (agent.is_some() || pid.is_some())).then(|| (id.to_string(), pid))
        })
        .collect();
    if let Some(a) = agent.filter(|_| rows.is_empty()) {
        return Err(format!("ajan yok: {a}"));
    }
    Ok(rows)
}

/// #911: each agent's process tree (its job: CLI → MCP servers → browsers, builds…) with CPU%
/// and RAM. `agent` = one agent, else every agent with a live session. A session without a
/// job of Dex's (a shell pane) lists nothing; one whose job cannot be read carries its `error`.
pub fn agent_processes(core: &crate::Core, agent: Option<&str>) -> Result<std::collections::BTreeMap<String, AgentProcs>, String> {
    let roots = agent_roots(core, agent)?;
    let pids: Vec<u32> = roots.iter().filter_map(|r| r.1).collect();
    let mut by_root = processes_of(&pids)?;
    Ok(roots.into_iter().map(|(id, pid)| (id, pid.and_then(|p| by_root.remove(&p)).unwrap_or_default())).collect())
}

/// #911: ends one process of the agent's tree. Refused when the pid is not in the agent's job
/// or is the agent's own CLI (closing that is the pane's job, `pty_kill`).
pub fn kill_agent_process(core: &crate::Core, agent: &str, pid: u32) -> Result<(), String> {
    let root = agent_roots(core, Some(agent))?.first().and_then(|r| r.1).ok_or("ajanın çalışan oturumu yok")?;
    kill_in_tree(root, pid)?;
    crate::diagnostics::event("agent_process_kill", serde_json::json!({"agent": agent, "pid": pid}));
    Ok(())
}

#[cfg(unix)]
fn processes_of(_roots: &[u32]) -> Result<std::collections::BTreeMap<u32, AgentProcs>, String> {
    Err("süreç paneli şimdilik yalnız Windows'ta".into())
}

#[cfg(unix)]
fn kill_in_tree(_root: u32, _pid: u32) -> Result<(), String> {
    Err("süreç paneli şimdilik yalnız Windows'ta".into())
}

#[cfg(windows)]
fn kill_in_tree(root: u32, pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER};
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
    if pid == root {
        return Err("ajanın kendi CLI süreci buradan kapatılmaz".into());
    }
    let jobs = jobs();
    let job = *jobs.get(&root).ok_or("ajanın süreç ağacı (job) yok")?;
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // No handle means membership cannot be checked: say why, not "outside the tree".
            return Err(match GetLastError() {
                ERROR_INVALID_PARAMETER => "süreç bulunamadı (kapanmış olabilir)".into(),
                e => format!("sürece erişilemedi (Windows hata {e}; ör. yükseltilmiş süreç)"),
            });
        }
        // Membership and termination on the same handle: a reused pid cannot slip in between.
        let mut inside = 0;
        let ok = IsProcessInJob(h, job as _, &mut inside) != 0 && inside != 0;
        let done = ok && TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        match (ok, done) {
            (false, _) => Err("süreç bu ajanın ağacında değil".into()),
            (true, false) => Err("süreç kapatılamadı".into()),
            (true, true) => Ok(()),
        }
    }
}

#[cfg(windows)]
struct Open {
    pid: u32,
    root: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
    /// When `cpu0` was read: each process's CPU share is over its own window.
    t0: std::time::Instant,
    cpu0: u64,
}

/// Every way out of [`processes_of`] closes the handles it opened.
#[cfg(windows)]
impl Drop for Open {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
fn cpu_time(h: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    let (mut c, mut e, mut k, mut u) = unsafe { (std::mem::zeroed(), std::mem::zeroed(), std::mem::zeroed(), std::mem::zeroed()) };
    if unsafe { windows_sys::Win32::System::Threading::GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } == 0 {
        return 0;
    }
    ft(k) + ft(u)
}

#[cfg(windows)]
fn image_name(h: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().map(str::to_string)
}

/// The raw command line (PEB via ntdll; works with a limited-query handle). Never stored.
#[cfg(windows)]
fn command_line(h: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::UNICODE_STRING;
    unsafe {
        let mut len = 0u32;
        NtQueryInformationProcess(h, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut len);
        if (len as usize) < std::mem::size_of::<UNICODE_STRING>() {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr() as _, len, &mut len) < 0 {
            return None;
        }
        let us = &*(buf.as_ptr() as *const UNICODE_STRING);
        if us.Buffer.is_null() {
            return Some(String::new());
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(us.Buffer, us.Length as usize / 2)))
    }
}

#[cfg(windows)]
fn mem_mb(h: windows_sys::Win32::Foundation::HANDLE) -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let mut m: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    if unsafe { K32GetProcessMemoryInfo(h, &mut m, m.cb) } == 0 {
        return 0.0;
    }
    (m.WorkingSetSize as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0
}

/// Pid → parent pid for every process now (one Toolhelp snapshot).
#[cfg(windows)]
fn parents() -> std::collections::HashMap<u32, (u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let mut out = std::collections::HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// Pids in a job (JOBOBJECT_BASIC_PROCESS_ID_LIST: two u32 counts, then pointer-sized pids),
/// starting with room for `cap` and growing when the job has more: never a silent short list.
#[cfg(windows)]
fn job_process_ids(job: windows_sys::Win32::Foundation::HANDLE, cap: usize) -> Result<Vec<u32>, String> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_MORE_DATA};
    use windows_sys::Win32::System::JobObjects::{JobObjectBasicProcessIdList, QueryInformationJobObject};
    // The list is read as u64 words: the header packs into one, a pid fills one only on 64-bit.
    const _: () = assert!(size_of::<usize>() == 8, "JOBOBJECT_BASIC_PROCESS_ID_LIST okuması 64-bit varsayar");
    let mut cap = cap.max(1);
    for _ in 0..4 {
        let mut buf = vec![0u64; 1 + cap];
        let ok = unsafe { QueryInformationJobObject(job, JobObjectBasicProcessIdList, buf.as_mut_ptr() as _, (buf.len() * 8) as u32, std::ptr::null_mut()) };
        let (assigned, listed) = (buf[0] as u32 as usize, (buf[0] >> 32) as usize);
        if ok != 0 {
            return Ok(buf[1..1 + listed.min(cap)].iter().map(|&p| p as u32).collect());
        }
        if unsafe { GetLastError() } != ERROR_MORE_DATA {
            return Err("ajanın süreç listesi (job) okunamadı".into());
        }
        // Grown past the count just reported, with slack for children born meanwhile.
        cap = assigned.max(cap) * 2;
    }
    Err("ajanın süreç listesi (job) sürekli büyüyor; okunamadı".into())
}

/// Root pid → its job's processes. Handles are opened (and checked against the job) under the
/// [`JOBS`] lock, then held across the sample window, so a pid reused meanwhile is never measured.
/// A root whose job cannot be read carries the error; the other roots are unaffected.
#[cfg(windows)]
fn processes_of(roots: &[u32]) -> Result<std::collections::BTreeMap<u32, AgentProcs>, String> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    processes_of_sampled(roots, cores,
        |h| (std::time::Instant::now(), cpu_time(h)),
        || std::thread::sleep(std::time::Duration::from_millis(SAMPLE_MS)))
}

/// Pair each CPU counter with its own clock reading. Tests can supply exact samples without
/// assuming that a live process received CPU during a fixed wall-clock window.
#[cfg(windows)]
fn processes_of_sampled(
    roots: &[u32],
    cores: f64,
    mut sample: impl FnMut(windows_sys::Win32::Foundation::HANDLE) -> (std::time::Instant, u64),
    wait: impl FnOnce(),
) -> Result<std::collections::BTreeMap<u32, AgentProcs>, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ};
    let mut open: Vec<Open> = Vec::new();
    let mut out: std::collections::BTreeMap<u32, AgentProcs> = std::collections::BTreeMap::new();
    {
        let jobs = jobs();
        for &root in roots {
            let Some(&job) = jobs.get(&root) else { continue };
            let pids = match job_process_ids(job as _, 1024) {
                Ok(pids) => pids,
                Err(e) => {
                    out.insert(root, AgentProcs { processes: Vec::new(), error: Some(e) });
                    continue;
                }
            };
            for pid in pids {
                let h = unsafe {
                    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
                    if h.is_null() { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) } else { h }
                };
                if h.is_null() {
                    continue;
                }
                let mut inside = 0;
                if unsafe { IsProcessInJob(h, job as _, &mut inside) } == 0 || inside == 0 {
                    unsafe { CloseHandle(h) };
                    continue;
                }
                let (t0, cpu0) = sample(h);
                open.push(Open { pid, root, handle: h, t0, cpu0 });
            }
        }
    }
    if !open.is_empty() {
        wait();
    }
    let parents = parents();
    for o in open {
        let (now, cpu) = sample(o.handle);
        let wall = now.duration_since(o.t0).as_secs_f64() * 1e7; // 100 ns units, like FILETIME
        let busy = cpu.saturating_sub(o.cpu0) as f64;
        let (ppid, snap_name) = parents.get(&o.pid).cloned().unwrap_or_default();
        let name = image_name(o.handle).unwrap_or(snap_name);
        let raw = command_line(o.handle).unwrap_or_default();
        out.entry(o.root).or_default().processes.push(AgentProcess {
            pid: o.pid,
            ppid,
            kind: classify(&name, &raw),
            cmd: short_cmd(&raw),
            name,
            cpu_pct: (busy / wall.max(1.0) / cores * 1000.0).round() / 10.0,
            mem_mb: mem_mb(o.handle),
            root: o.pid == o.root,
        });
    }
    for list in out.values_mut() {
        list.processes.sort_by_key(|p| (!p.root, p.pid));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn process_fixture() {
        use std::io::{Read, Write};
        if std::env::var_os("DEX_PROC_FIXTURE").is_none() { return; }
        println!("DEX_PROCESS_READY {}", std::process::id());
        std::io::stdout().flush().unwrap();
        let mut go = [0];
        std::io::stdin().read_exact(&mut go).unwrap();
    }

    /// The shell cannot spawn its child until adopt has returned. The child reports its PID
    /// and holds on stdin, so its lifetime is independent of scheduler load and ping duration.
    #[cfg(windows)]
    struct TestPane {
        child: std::process::Child,
        input: std::process::ChildStdin,
        ready: std::sync::mpsc::Receiver<String>,
        tree: Option<super::Tree>,
    }

    #[cfg(windows)]
    impl TestPane {
        fn spawn(parent: windows_sys::Win32::Foundation::HANDLE, limits: &super::Limits, successive: bool) -> Self {
            use std::io::BufRead;
            use std::os::windows::process::CommandExt;
            use std::process::{Command, Stdio};
            let exe = std::env::current_exe().unwrap();
            let fixture = format!(r#""{}" --exact proc::tests::process_fixture --nocapture"#, exe.display());
            let command = format!("set /p dex_go= & {fixture}{}", if successive { format!(" & {fixture}") } else { String::new() });
            let mut child = Command::new("cmd").args(["/d", "/q", "/c"]).raw_arg(command)
                .env("DEX_PROC_FIXTURE", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            let input = child.stdin.take().unwrap();
            let output = child.stdout.take().unwrap();
            let (tx, ready) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(output).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() { break; }
                }
            });
            let mut pane = Self { child, input, ready, tree: None };
            pane.tree = Some(super::Tree::adopt_in(pane.child.id(), limits, Some(parent)).expect("adopt gated shell"));
            pane
        }

        fn start(&mut self) -> super::Open {
            use std::io::Write;
            self.input.write_all(b"go\r\n").unwrap();
            self.next_child()
        }

        fn next_child(&mut self) -> super::Open {
            use windows_sys::Win32::System::JobObjects::IsProcessInJob;
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE};
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let line = self.ready.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .unwrap_or_else(|e| panic!("child-ready root {}: {e:?}; exit {:?}", self.child.id(), self.child.try_wait()));
                let Some(pid) = line.strip_prefix("DEX_PROCESS_READY ").and_then(|p| p.parse::<u32>().ok()) else { continue };
                let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
                assert!(!handle.is_null(), "ready child {pid}");
                let open = super::Open { pid, root: self.child.id(), handle, t0: std::time::Instant::now(), cpu0: 0 };
                let mut inside = 0;
                assert_ne!(unsafe { IsProcessInJob(handle, self.tree.as_ref().unwrap().job, &mut inside) }, 0);
                assert_ne!(inside, 0, "ready child {pid} belongs to root {}", self.child.id());
                return open;
            }
        }
    }

    #[cfg(windows)]
    impl Drop for TestPane {
        fn drop(&mut self) {
            drop(self.tree.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(windows)]
    fn exit_code(process: &super::Open) -> u32 {
        let mut code = 0;
        assert_ne!(unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(process.handle, &mut code) }, 0);
        code
    }

    #[cfg(windows)]
    fn wait_exit(process: &super::Open) -> u32 {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        assert_eq!(unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(process.handle, 15_000) }, WAIT_OBJECT_0,
            "pid {} still active, exit {}", process.pid, exit_code(process));
        exit_code(process)
    }

    #[cfg(windows)]
    #[test]
    fn a_completed_child_does_not_mean_its_sibling_pane_was_killed() {
        use std::io::Write;
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        let parent = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let lim = super::Limits { total_cpu_percent: 80, agent_cpu_percent: 0, mem_bytes: 0 };
        let mut a = TestPane::spawn(parent, &lim, false);
        let mut b = TestPane::spawn(parent, &lim, true);
        let _ca = a.start();
        let transient = b.start();
        assert!(job_pids(b.tree.as_ref().unwrap().job).contains(&transient.pid));
        b.input.write_all(b"x").unwrap();
        let live = b.next_child();
        let code = wait_exit(&transient);
        drop(a.tree.take());
        assert_eq!(code, 0, "pid {} exited normally before the other job closed", transient.pid);
        assert_eq!(b.child.try_wait().unwrap(), None, "sibling root {}", b.child.id());
        assert_eq!(exit_code(&live), windows_sys::Win32::Foundation::STILL_ACTIVE as u32, "sibling child {}", live.pid);
        assert!(!super::alive(transient.pid));
        drop(a);
        drop(b);
        unsafe { windows_sys::Win32::Foundation::CloseHandle(parent) };
    }

    #[test]
    fn own_process_is_alive() {
        assert!(super::alive(std::process::id()));
    }

    /// Env policy: default 35% CPU / no mem cap; 0 = unlimited; garbage = default.
    /// One fn, sequential: env is process-global, parallel tests must not touch these keys.
    #[test]
    fn limits_from_env_cases() {
        use super::{default_limits, limits_from_env, with_env, Limits};
        let keys = ["DEX_AGENTS_CPU_PERCENT", "DEX_AGENT_CPU_PERCENT", "DEX_AGENT_MEM_BYTES"];
        keys.iter().for_each(|k| std::env::remove_var(k));
        let d = default_limits();
        assert_eq!(d, Limits { total_cpu_percent: 80, agent_cpu_percent: 0, mem_bytes: 0 });
        assert_eq!(limits_from_env(), d);
        let stored = Limits { total_cpu_percent: 60, agent_cpu_percent: 20, mem_bytes: 0 };
        assert_eq!(with_env(stored), (stored, vec![])); // env yok → kayıtlı ayar aynen

        std::env::set_var("DEX_AGENTS_CPU_PERCENT", "0");
        assert_eq!(with_env(stored).0.total_cpu_percent, 0); // sınırsız
        std::env::set_var("DEX_AGENTS_CPU_PERCENT", "999");
        assert_eq!(with_env(stored).0.total_cpu_percent, 100); // kırpılır
        std::env::set_var("DEX_AGENTS_CPU_PERCENT", "saçmalık");
        assert_eq!(with_env(stored), (stored, vec![])); // çöp yok sayılır → kayıtlı ayar
        std::env::set_var("DEX_AGENT_CPU_PERCENT", "35");
        std::env::set_var("DEX_AGENT_MEM_BYTES", "6442450944"); // 6 GiB
        let (l, used) = with_env(stored);
        assert_eq!((l.total_cpu_percent, l.agent_cpu_percent, l.mem_bytes), (60, 35, 6442450944));
        assert_eq!(used, vec!["DEX_AGENT_CPU_PERCENT", "DEX_AGENT_MEM_BYTES"]);
        keys.iter().for_each(|k| std::env::remove_var(k));
    }

    /// cmd → ping: dropping the tree kills the grandchild too, not just the process we spawned.
    #[cfg(windows)]
    #[test]
    fn dropping_tree_kills_descendants() {
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 30 127.0.0.1 >NUL"]).spawn().unwrap();
        let tree = super::Tree::adopt(child.id()).expect("job");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let pids = super::job_process_ids(tree.job, 33).unwrap();
        assert!(pids.len() >= 2, "cmd + ping in job: {pids:?}");
        drop(tree);
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(300));
        for p in pids {
            assert!(!super::alive(p), "{p} still alive");
        }
    }

    /// DEX-27: adopt puts the child BELOW_NORMAL with the requested hard cap on the job,
    /// and the opt-in memory ceiling is readable back. Later-born children ride along by OS
    /// auto-join (proven by `dropping_tree_kills_descendants`: cmd→ping are both in the job).
    /// The live-burn side is `total_cap_holds_under_burners`.
    #[cfg(windows)]
    #[test]
    fn adopted_tree_gets_priority_cap_and_mem() {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::*;
        use windows_sys::Win32::System::Threading::{
            GetPriorityClass, OpenProcess, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let lim = super::Limits { total_cpu_percent: 80, agent_cpu_percent: 5, mem_bytes: 1024 * 1024 * 1024 };
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 10 127.0.0.1 >NUL"]).spawn().unwrap();
        let tree = super::Tree::adopt_with(child.id(), &lim).expect("job");
        // Priority landed on the root (children inherit it).
        let prio = unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child.id());
            assert!(!h.is_null());
            let p = GetPriorityClass(h);
            CloseHandle(h);
            p
        };
        assert_eq!(prio, BELOW_NORMAL_PRIORITY_CLASS, "priority class {prio:#x}");
        // The cap the OS sees is exactly what we asked (percent → 1/100ths).
        let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            QueryInformationJobObject(
                tree.job,
                JobObjectCpuRateControlInformation,
                &mut cpu as *mut _ as _,
                std::mem::size_of_val(&cpu) as u32,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        let rate = unsafe { cpu.Anonymous.CpuRate };
        eprintln!("DEX-27: priority=BELOW_NORMAL, CpuRate={rate} (5%), JobMemoryLimit=1GiB");
        assert_eq!(rate, 500);
        // Opt-in memory ceiling sits next to KILL_ON_JOB_CLOSE.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            QueryInformationJobObject(
                tree.job,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as _,
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        assert_eq!(info.JobMemoryLimit, 1024 * 1024 * 1024);
        assert_ne!(info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, 0);
        assert_ne!(info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY, 0);
        drop(tree);
        let _ = child.wait();
    }

    #[cfg(windows)]
    fn job_pids(job: windows_sys::Win32::Foundation::HANDLE) -> Vec<u32> {
        super::job_process_ids(job, 65).unwrap()
    }

    #[cfg(windows)]
    fn cpu_rate(job: windows_sys::Win32::Foundation::HANDLE) -> (u32, u32) {
        use windows_sys::Win32::System::JobObjects::*;
        let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { QueryInformationJobObject(job, JobObjectCpuRateControlInformation, &mut cpu as *mut _ as _, std::mem::size_of_val(&cpu) as u32, std::ptr::null_mut()) };
        assert_ne!(ok, 0);
        (cpu.ControlFlags, unsafe { cpu.Anonymous.CpuRate })
    }

    /// DEX-31: two panes nest in one parent job that carries the total cap; closing one pane
    /// kills only its own tree. The parent's cap follows the latest setting (updated per spawn).
    #[cfg(windows)]
    #[test]
    fn panes_nest_in_total_job_and_close_alone() {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        let parent = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!parent.is_null());
        let lim = |total| super::Limits { total_cpu_percent: total, agent_cpu_percent: 0, mem_bytes: 0 };
        let mut a = TestPane::spawn(parent, &lim(80), false);
        assert_eq!(cpu_rate(parent).1, 8000);
        let mut b = TestPane::spawn(parent, &lim(7), false);
        assert_eq!(cpu_rate(parent).1, 700); // son ayar geçerli, çalışanlar dahil
        assert_eq!(cpu_rate(b.tree.as_ref().unwrap().job).0, 0, "ajan başı tavan varsayılan kapalı");
        let (ca, cb) = (a.start(), b.start());
        let (pa, pb) = (job_pids(a.tree.as_ref().unwrap().job), job_pids(b.tree.as_ref().unwrap().job));
        let all = job_pids(parent);
        assert!(pa.len() >= 2 && pb.len() >= 2, "cmd+child: {pa:?} {pb:?}");
        assert!(pa.contains(&ca.pid) && pb.contains(&cb.pid));
        assert!(!pa.iter().any(|p| pb.contains(p)), "sibling pane jobs must be disjoint: {pa:?} {pb:?}");
        assert!(pa.iter().chain(&pb).all(|p| all.contains(p)), "üst job iki ağacı da görür: {all:?}");
        drop(a.tree.take());
        let closed = a.child.wait().unwrap();
        let child_code = wait_exit(&ca);
        assert!(pa.iter().all(|&p| !super::alive(p)), "kapanan pane'in ağacı öldü");
        assert!(pb.iter().all(|&p| super::alive(p)), "diğer pane yaşıyor: {pb:?}; root {} exit {:?}, child {} exit {}; closed root {} exit {closed}, child {} exit {child_code}",
            b.child.id(), b.child.try_wait(), cb.pid, exit_code(&cb), a.child.id(), ca.pid);
        assert_eq!(b.child.try_wait().unwrap(), None);
        assert_eq!(exit_code(&cb), windows_sys::Win32::Foundation::STILL_ACTIVE as u32);
        assert!(unsafe { super::set_cpu_cap(parent, 0) });
        assert_eq!(cpu_rate(parent).0, 0, "0 = sınırsız → rate control kapalı");
        drop(a);
        drop(b);
        unsafe { CloseHandle(parent) };
    }

    /// DEX-31 live burn: three one-core busy loops, each in its own pane job, under a 5% total —
    /// together they stay at the cap (uncapped they'd take 3 cores). Upper bound only: a busy
    /// machine may give them less, never more.
    #[cfg(windows)]
    #[test]
    fn total_cap_holds_under_burners() {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::*;
        let parent = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let lim = super::Limits { total_cpu_percent: 5, agent_cpu_percent: 0, mem_bytes: 0 };
        let mut kids = Vec::new();
        for _ in 0..3 {
            let c = std::process::Command::new("cmd").args(["/d", "/c", "for /l %i in (0,0,1) do @rem"]).spawn().unwrap();
            let t = super::Tree::adopt_in(c.id(), &lim, Some(parent)).expect("job");
            kids.push((c, t));
        }
        let used = || {
            let mut acc: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe { QueryInformationJobObject(parent, JobObjectBasicAccountingInformation, &mut acc as *mut _ as _, std::mem::size_of_val(&acc) as u32, std::ptr::null_mut()) };
            assert_ne!(ok, 0);
            (acc.TotalUserTime + acc.TotalKernelTime) as f64 / 1e7 // 100 ns → s
        };
        std::thread::sleep(std::time::Duration::from_millis(800));
        let (t0, w0) = (used(), std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_secs(3));
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        let share = 100.0 * (used() - t0) / w0.elapsed().as_secs_f64() / cores;
        eprintln!("DEX-31: 3 yakıcı, toplam tavan %5 → makine payı %{share:.1} ({cores} çekirdek; tavansız ≈ %{:.0})", 100.0 * 3.0_f64.min(cores) / cores);
        for (mut c, t) in kids {
            drop(t);
            let _ = c.wait();
        }
        unsafe { CloseHandle(parent) };
        assert!(share > 0.5 && share < 6.5, "toplam pay tavanda olmalı: %{share:.1}");
    }

    #[test]
    fn panel_classifies_processes() {
        use super::{classify, ProcKind::*};
        let cases = [
            ("chrome.exe", r"C:\Users\u\AppData\Local\ms-playwright\chromium-1200\chrome-win\chrome.exe --remote-debugging-pipe", Browser),
            ("chrome.exe", "chrome.exe --type=renderer", Browser),
            ("msedge.exe", "msedge", Browser),
            ("msedge.exe", "msedge --type=gpu-process", Browser),
            ("headless_shell.exe", "headless_shell --type=renderer", Browser),
            ("node.exe", "node server.js", Node),
            ("npx.cmd", "npx something", Other),
            ("cargo.exe", "cargo test", Cargo),
            ("rustc.exe", "rustc --crate-name x", Cargo),
            ("build-script-build.exe", "", Cargo),
            ("Godot_v4.3-stable_win64.exe", "godot --headless", Godot),
            ("cmd.exe", "cmd /c ping", Shell),
            ("pwsh.exe", "pwsh -NoProfile", Shell),
            ("claude.exe", "claude --resume", Other),
        ];
        for (name, cmd, want) in cases {
            assert_eq!(classify(name, cmd), want, "{name} | {cmd}");
        }
        for (name, cmd) in [
            ("node.exe", r"node C:\x\node_modules\@playwright\mcp\cli.js"),
            ("cmd.exe", "cmd /c npx -y @playwright/mcp@0.0.79 --headless"),
            ("node.exe", "node npx-cli.js -y @playwright/mcp@0.0.79"),
            ("cmd.exe", r"cmd /c C:\x\node_modules\.bin\playwright-mcp.cmd --headless"),
        ] {
            assert_eq!(serde_json::to_value(classify(name, cmd)).unwrap(), "browser-tool", "{cmd}");
        }
        assert_eq!(classify("node.exe", "node playwright test"), Node);
        assert_eq!(classify("cmd.exe", "cmd /c npx playwright test"), Shell);
    }

    #[test]
    fn panel_masks_secrets_in_commands() {
        use super::short_cmd;
        let cases = [
            (r#""C:\Program Files\nodejs\node.exe" C:\x\cli.js --api-key sk-live-1 --token=abc"#, r"node.exe C:\x\cli.js --api-key *** --token=***"),
            (r#"curl.exe -H "Authorization: Bearer abc.def" https://user:pw@host.com/p?key=1#f"#, r#"curl.exe -H "Authorization: Bearer ***" https://***@host.com/p?***"#),
            ("git.exe push https://ghp_abcdefghijklmnop@github.com/o/r", "git.exe push https://***@github.com/o/r"),
            ("tool.exe ghp_shortbutknownprefix x", "tool.exe *** x"),
            ("tool.exe 0123456789abcdef0123456789abcdef-xyz y", "tool.exe *** y"),
            ("node.exe OPENAI_API_KEY=abc PASSWORD=p x", "node.exe OPENAI_API_KEY=*** PASSWORD=*** x"),
            (r"app.exe --secret: hush --user-data-dir=C:\tmp\p", r"app.exe --secret: *** --user-data-dir=C:\tmp\p"),
            ("ping.exe -n 30 127.0.0.1", "ping.exe -n 30 127.0.0.1"),
            // #918: per argument — a quoted value goes whole, a JSON / secret-naming `=` value too.
            (r#"tool.exe --token "a b" next"#, "tool.exe --token *** next"),
            (r#"tool.exe --config={"apiKey":"xyz"} x"#, "tool.exe --config=*** x"),
            (r#"tool.exe --config={"model": "m", "auth": "p q"} x"#, "tool.exe --config=*** x"),
            ("tool.exe --url=https://h/?token=abc x", "tool.exe --url=*** x"),
            (r#"tool.exe --name "two words" x"#, r#"tool.exe --name "two words" x"#),
            // #925: a quoted flag is still a flag.
            (r#"tool.exe "--token" "a b" x"#, r#"tool.exe "--token" *** x"#),
            ("", ""),
        ];
        for (raw, want) in cases {
            assert_eq!(short_cmd(raw), want, "{raw}");
        }
        let long = short_cmd(&format!("x.exe {}", "ğ/".repeat(400)));
        assert_eq!(long.chars().count(), super::CMD_MAX + 1);
        assert!(long.ends_with('…'));
    }

    /// #911: an agent's job lists its own tree (root flagged, parent links, CPU from two
    /// samples); kill reaches only that tree and never the root; a closed tree drops out.
    #[cfg(windows)]
    #[test]
    fn panel_lists_own_tree_and_kills_only_inside() {
        use super::{kill_in_tree, processes_of};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
        let parent = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!parent.is_null());
        let lim = super::Limits { total_cpu_percent: 80, agent_cpu_percent: 0, mem_bytes: 0 };
        let mut a = TestPane::spawn(parent, &lim, false);
        let mut b = TestPane::spawn(parent, &lim, false);
        let (ca, cb) = (a.start(), b.start());
        let (ra, rb) = (a.child.id(), b.child.id());
        // Exercise OS reads as well as exact CPU math. A live process is allowed to get zero
        // CPU under load; the formula is checked with two controlled samples on real handles.
        let actual = processes_of(&[ra, rb]).unwrap();
        assert!(actual[&ra].processes.iter().any(|p| p.pid == ca.pid));
        assert!(actual[&rb].processes.iter().any(|p| p.pid == cb.pid));
        let base = std::time::Instant::now();
        let mut sampled = std::collections::HashSet::new();
        let map = super::processes_of_sampled(&[ra, rb, 1], 4.0, |h| {
            if sampled.insert(h as usize) { (base, 123) }
            else { (base + std::time::Duration::from_secs(1), 10_000_123) }
        }, || {}).unwrap();
        assert!(!map.contains_key(&1), "Dex'in job'u olmayan kök listelenmez");
        let la = &map[&ra].processes;
        assert!(la[0].root && la[0].pid == ra && la[0].kind == super::ProcKind::Shell, "{la:?}");
        let child = la.iter().find(|p| p.pid == ca.pid).unwrap_or_else(|| panic!("ready child in a: {la:?}"));
        assert!(!child.root && child.ppid == ra && child.cmd.contains("process_fixture") && !child.name.is_empty() && child.mem_mb > 0.0, "{child:?}");
        let burner = &map[&rb].processes[0];
        assert!(burner.root && burner.cpu_pct > 0.5, "iki örnek farkı: {burner:?}");
        assert_eq!(burner.cpu_pct, 25.0, "1 CPU-second / 1 wall-second / 4 cores");

        assert!(kill_in_tree(ra, ra).is_err(), "kök korunur");
        assert!(kill_in_tree(ra, rb).is_err(), "başka ajanın süreci");
        assert!(kill_in_tree(ra, std::process::id()).is_err(), "job dışı süreç");
        assert!(kill_in_tree(1, child.pid).is_err(), "bilinmeyen kök");
        // #918: no handle is "cannot reach" / "gone", never "not in this tree".
        let denied = kill_in_tree(ra, 4).unwrap_err();
        assert!(denied.contains("erişilemedi"), "System (4): {denied}");
        let gone = kill_in_tree(ra, 0).unwrap_err();
        assert!(gone.contains("bulunamadı"), "pid 0: {gone}");
        // #918: a list larger than the first buffer grows instead of coming back empty.
        let all = super::job_process_ids(a.tree.as_ref().unwrap().job, 1).unwrap();
        assert!(all.len() >= 2 && all.contains(&ra) && all.contains(&child.pid), "{all:?}");
        assert!(super::alive(ra) && super::alive(rb) && super::alive(child.pid));
        kill_in_tree(ra, child.pid).unwrap();
        assert_eq!(wait_exit(&ca), 1, "only the selected handle was terminated");
        assert!(!super::alive(child.pid));
        let _ = a.child.wait().unwrap();
        assert_eq!(b.child.try_wait().unwrap(), None, "sibling root {rb}");
        assert_eq!(exit_code(&cb), windows_sys::Win32::Foundation::STILL_ACTIVE as u32, "sibling child {}", cb.pid);

        drop(a.tree.take());
        assert!(!processes_of(&[ra]).unwrap().contains_key(&ra), "kapanan ağaç kayıttan düşer");
        drop(a);
        drop(b);
        unsafe { CloseHandle(parent) };
    }

    /// #925: one root's unreadable job is that root's error; the other root still lists its tree.
    #[cfg(windows)]
    #[test]
    fn panel_error_stays_with_its_root() {
        use super::{jobs, processes_of, Tree};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let lim = super::Limits { total_cpu_percent: 0, agent_cpu_percent: 0, mem_bytes: 0 };
        let mut a = std::process::Command::new("cmd").args(["/c", "ping -n 30 127.0.0.1 >NUL"]).spawn().unwrap();
        let ra = a.id();
        let ta = Tree::adopt_with(ra, &lim).expect("job a");
        // An odd pid is never a real one; a process handle is not a job, so its list cannot be read.
        let (bad, not_job) = (0xFFFF_FFF1u32, unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, std::process::id()) });
        assert!(!not_job.is_null());
        jobs().insert(bad, not_job as usize);
        let map = processes_of(&[bad, ra]);
        jobs().remove(&bad);
        unsafe { CloseHandle(not_job) };
        let map = map.unwrap();
        assert!(map[&bad].processes.is_empty() && map[&bad].error.as_deref().is_some_and(|e| e.contains("okunamadı")), "{:?}", map[&bad]);
        assert!(map[&ra].error.is_none() && map[&ra].processes.iter().any(|p| p.root && p.pid == ra), "{:?}", map[&ra]);
        let json = serde_json::to_value(&map[&ra]).unwrap();
        assert!(json["processes"].is_array() && json.get("error").is_none(), "{json}");
        assert_eq!(serde_json::to_value(&map[&bad]).unwrap()["error"], serde_json::json!(map[&bad].error));
        drop(ta);
        let _ = a.kill();
        let _ = a.wait();
    }

    /// #925: a dropped `Open` closes its handle (every exit of `processes_of`, early ones too).
    #[cfg(windows)]
    #[test]
    fn panel_open_closes_its_handle() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let count = || {
            let mut n = 0u32;
            assert!(unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) } != 0);
            n as i64
        };
        const N: usize = 200;
        let open: Vec<super::Open> = (0..N)
            .map(|_| {
                let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, std::process::id()) };
                assert!(!handle.is_null());
                super::Open { pid: 0, root: 0, handle, t0: std::time::Instant::now(), cpu0: 0 }
            })
            .collect();
        let before = count();
        drop(open);
        // Other tests' threads open and close handles meanwhile: slack, but far from "none closed".
        assert!(before - count() >= N as i64 * 3 / 4, "{before} → {}", count());
    }

    /// #911: the owner map is Dex's own (a live session's pid); no session → nothing listed,
    /// nothing killable; an unknown agent is an error, not an empty answer.
    #[cfg(windows)]
    #[test]
    fn panel_owner_comes_from_live_sessions() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!("proc-panel-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        assert!(super::agent_processes(&core, None).unwrap().is_empty());
        let one = super::agent_processes(&core, Some("kaan")).unwrap();
        assert_eq!(one.get("kaan"), Some(&super::AgentProcs::default()));
        assert!(super::agent_processes(&core, Some("yok-boyle-ajan")).is_err());
        assert!(super::kill_agent_process(&core, "kaan", std::process::id()).is_err());
        assert!(super::alive(std::process::id()));
        core.shutdown();
    }
}
