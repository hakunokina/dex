//! #150 (B): the desktop's core is `dexd --local`, a process of its own (crates/dexd/src/local.rs).
//! The shell finds it through its discovery file (`<app data>/dexd.local.json` + token file) and
//! checks it answers (`auth.hello`, `probe`: not a window); if not, it starts one, detached so it
//! outlives this window. The webview then talks to it like to any host (`local_host` → url +
//! token, in memory only). Closing every window leaves it running for its grace (120 s) — a window
//! that comes back finds it; after that it shuts its agents down and exits.
//!
//! Start: a copy of the bundled `dexd` (next to this exe; `DEX_DEXD` overrides) under
//! `<local app data>/bin` — Windows can't replace a running .exe, so a rebuild or an update never
//! fights the running core. `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB`; a job that forbids
//! breakaway (access denied: `tauri dev`) → started as explorer.exe's child (no job); no shell →
//! a one-shot scheduled task (`schtasks`; its own job, 72 h limit — last resort).
//!
//! The tray (tray.rs, #151) reads dexd's `core_life` and stops / restarts it through the same
//! paths as `local_shutdown` / `local_restart`.
//!
//! #203: `tauri dev`'s watcher rebuilds only this app — a merge left `target/debug/dexd.exe` a
//! build that did not know `--local`; it exited at once, unseen, and the window waited 40 s on an
//! empty stage. Now: under `cargo run` every start builds `dexd` first (`dev_build`); a dexd that
//! exits before it answers fails the start at once; each attempt and dexd's own output are
//! appended to `<logs>/dexd-stderr.log`; the last failure is the band's and the tray's reason.
//!
//! #161 (C): restart = dexd's `core_restart {exe, whenIdle}` — the core comes back as this app's
//! build, keeping its queue and reopening its leaders (`--resume`); `whenIdle` drains first (jobs
//! wait in the queue until nothing runs), `local_drain_cancel` takes that back. A core too old for
//! that is stopped and started (as before: its queue fails). An outdated core (its `exe` is not
//! this app's build's copy name, and that build is not older) found at start-up: a release shows the
//! band ("Çekirdek güncel değil"); a dev build only says so (#494: tray "güncelleme bekliyor",
//! Ayarlar → Çekirdek "Güncelleme hazır") — the patron restarts it, never the shell by itself.
//!
//! #494 known risks (#466, not fixed): a restart the patron picks — now or once drained — can still
//! lose work in three windows: a chat message accepted (`queued`) after `restart_mark` took its
//! snapshot of the chat queue lives only in the old process's memory (restart.rs / chat.rs
//! `chat_send`); `chat_on_stop` takes a message off the queue before `paste_prompt` holds the
//! admission guard, so a gate closed in between puts it back in memory only and it is lost; and
//! `resume_on_exit`'s retry spawns a leader without the admission gate, so a turn it opens after the
//! last quiet check is cut by the restart. They hold for every manual restart until #466 is fixed.

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Manager, Runtime, State};
use tokio::sync::{Mutex, OnceCell};
use tokio_tungstenite::tungstenite::Message;

// The shell shares storage helpers without linking the core process.
#[path = "../../crates/dex-core/src/private_fs.rs"]
#[allow(dead_code)]
mod private_fs;
// #2288: the local handshake's arithmetic (the same file dexd compiles; sha2 only).
#[path = "../../crates/dexd/src/local_auth.rs"]
#[allow(dead_code)]
mod local_auth;

const DISCOVERY_FILE: &str = "dexd.local.json";
/// #2288: the token's one trusted place — never a path the discovery file names.
const TOKEN_FILE: &str = "dexd.local.token";
/// Core start (DB open, hook receiver) plus the wait for a previous instance's lock.
const START_TIMEOUT: Duration = Duration::from_secs(40);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const STDERR_LOG: &str = "dexd-stderr.log";
/// Past this the log moves to `dexd-stderr.1.log` (one generation) before the next start.
const LOG_CAP: u64 = 1 << 20;

pub(crate) type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The endpoint for the webview: its url only. #2288: the token stays in the shell — the webview
/// has the core's challenge answered here (`local_proof`) and never holds the token itself.
#[derive(Clone, Serialize)]
pub struct LocalHost {
    url: String,
    #[serde(skip)]
    port: u16,
    #[serde(skip)]
    token: String,
}

/// `local_host`'s answer: the endpoint, or why there is none (the webview shows the band).
#[derive(Serialize)]
#[serde(untagged)]
pub enum LocalAnswer {
    Up(LocalHost),
    Down { down: String },
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalStatus {
    running: bool,
    pid: Option<u64>,
    url: Option<String>,
    commit: Option<String>,
    commit_time: Option<i64>,
    started_at: Option<i64>,
    /// How this window last started it: `direct` (detached, out of any job), `shell` (our job
    /// refused breakaway: started under explorer.exe) or `schtasks` (no shell). None: found running.
    launched: Option<&'static str>,
    /// #494: a dev build found this core outdated; the patron restarts it.
    update_pending: bool,
}

pub struct CoreProc {
    data_dir: PathBuf,
    config_dir: PathBuf,
    bin_dir: PathBuf,
    /// Start-up's find-or-start, once; its error is the `down` reason until a later start.
    startup: OnceCell<Result<(), String>>,
    /// One start / stop at a time (two windows → still one start each, `dex.lock` decides).
    busy: Mutex<()>,
    launched: std::sync::Mutex<Option<&'static str>>,
    /// The last start's failure (start-up, "Çekirdeği başlat", the tray's restart); None once one worked.
    last_error: std::sync::Mutex<Option<String>>,
    /// #494: the pid of the core start-up found outdated (dev); pending while that core runs.
    update_for: std::sync::Mutex<Option<u64>>,
}

impl CoreProc {
    fn discovery(&self) -> Option<Value> {
        serde_json::from_slice(&std::fs::read(self.data_dir.join(DISCOVERY_FILE)).ok()?).ok()
    }

    /// #2288: the discovery file is advisory — any process of this user (a worker agent) can
    /// rewrite it. Only its numeric `port` is used: the URL is always loopback, and the token is
    /// read from the shell's own data dir (`TOKEN_FILE`), never from a path the file names.
    fn endpoint(&self) -> Option<(Value, LocalHost)> {
        let d = self.discovery()?;
        let port = u16::try_from(d["port"].as_u64()?).ok().filter(|p| *p != 0)?;
        let token = std::fs::read_to_string(self.data_dir.join(TOKEN_FILE)).ok()?.trim().to_string();
        Some((d, LocalHost { url: format!("ws://127.0.0.1:{port}"), port, token }))
    }

    /// The running core, if its discovery file is current and it proves it holds the token.
    async fn find(&self) -> Option<(Value, LocalHost)> {
        let (d, h) = self.endpoint()?;
        hello(&h).await.ok()?;
        Some((d, h))
    }

    /// #161: the running core is not this app's dexd build and that build is not older than it.
    /// None: no core, or this app's build can't be told (no dexd beside it, `--version` unknown).
    fn outdated(&self) -> Option<bool> {
        let d = self.discovery()?;
        let src = dev_build().and_then(|()| source_exe()).ok()?;
        let exe = d["exe"].as_str();
        if exe == Some(copy_name(&std::fs::metadata(&src).ok()?).as_str()) {
            return Some(false);
        }
        // #427: another name (a relink, a touch: a new mtime) over the same bytes is the same build.
        // The running copy is ours (`run_copy`, dexd's `stage`: one bin folder); a bare name only.
        if exe.filter(|n| Path::new(n).file_name() == Some(std::ffi::OsStr::new(n))).is_some_and(|n| same_content(&self.bin_dir.join(n), &src)) {
            return Some(false);
        }
        let bundled = version_of(&src)?;
        Some(bundled["commitTime"].as_i64()? >= d["commitTime"].as_i64().unwrap_or(0))
    }

    /// #494 (patron): a dev build that finds an outdated core only marks it ("güncelleme bekliyor");
    /// the patron restarts it. A quiet wait an earlier shell left (#400 `whenQuiet`) is taken back,
    /// or that core would still restart by itself.
    async fn dev_update(&self) {
        if !cfg!(debug_assertions) || self.launched.lock().unwrap().is_some() {
            return;
        }
        let log = StartLog::open(&self.data_dir.join("logs"));
        if let Err(e) = self.cancel_quiet_wait().await {
            log.line(&format!("dex: eski otomatik güncelleme beklemesi geri alınamadı: {e}"));
        }
        if self.outdated() == Some(true) {
            *self.update_for.lock().unwrap() = self.discovery().and_then(|d| d["pid"].as_u64());
            log.line("dex: çekirdek bu derlemeden eski; güncelleme bekliyor, yeniden başlatmayı patron seçer (dev)");
        }
    }

    /// #494: dexd's quiet wait (an update restart nobody asked for now), if one runs. #501: one
    /// `core_quiet_cancel` — a drain the patron picked in another window meanwhile stays. A core
    /// without it (older than #501): look, then take back — that gap is its known risk.
    async fn cancel_quiet_wait(&self) -> Result<(), String> {
        let Some((_, h)) = self.endpoint() else { return Ok(()) };
        let (mut ws, info) = hello(&h).await?;
        if info["methods"].as_array().is_some_and(|m| m.iter().any(|m| m == "core_quiet_cancel")) {
            let r = request(&mut ws, "core_quiet_cancel").await?;
            return match r.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("core_quiet_cancel").to_string()),
                None => Ok(()),
            };
        }
        let life = request(&mut ws, "core_life").await?;
        if life["result"]["updatePending"].as_bool() != Some(true) {
            return Ok(());
        }
        let r = request(&mut ws, "core_drain_cancel").await?;
        match r.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("core_drain_cancel").to_string()),
            None => Ok(()),
        }
    }

    /// #494: start-up found the running core outdated (dev) and it still runs.
    pub(crate) fn update_pending(&self) -> bool {
        let Some(pid) = *self.update_for.lock().unwrap() else { return false };
        self.discovery().and_then(|d| d["pid"].as_u64()) == Some(pid)
    }

    /// The tray's poll: dexd's `core_life` over one held probe connection (`conn`; opened again
    /// when the core changed). None: no core. `{}`: a core that has no `core_life` (older build).
    pub(crate) async fn life(&self, conn: &mut Option<Ws>) -> Option<Value> {
        if let Some(ws) = conn.as_mut() {
            if let Ok(r) = request(ws, "core_life").await {
                return Some(r.get("result").cloned().unwrap_or_else(|| json!({})));
            }
        }
        *conn = None;
        let (_, h) = self.endpoint()?;
        let (mut ws, _) = hello(&h).await.ok()?;
        let r = request(&mut ws, "core_life").await.ok()?;
        *conn = Some(ws);
        Some(r.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    /// #161: the core again as this app's dexd build, keeping its queue and leaders — now, or
    /// once it is drained (`when_idle`; returns at once). No core: started.
    pub(crate) async fn restart(&self, when_idle: bool) -> Result<(), String> {
        let found = {
            let _busy = self.busy.lock().await;
            match self.endpoint() {
                Some((d, h)) => hello(&h).await.ok().map(|(ws, info)| (d, ws, info)),
                None => None,
            }
        };
        let Some((d, mut ws, info)) = found else { return self.ensure().await.map(drop) };
        let knows = info["methods"].as_array().is_some_and(|m| m.iter().any(|m| m == "core_drain_cancel"));
        if !knows {
            drop(ws);
            self.stop().await?;
            return self.ensure().await.map(drop);
        }
        let src = dev_build().and_then(|()| source_exe())?;
        let r = request_with(&mut ws, "core_restart", json!({ "exe": src, "whenIdle": when_idle })).await?;
        if let Some(e) = r.get("error") {
            return Err(e["message"].as_str().unwrap_or("core_restart").to_string());
        }
        if when_idle {
            return Ok(());
        }
        // The old one lets the data dir go, the new one (`--wait-lock`) takes it.
        let until = Instant::now() + STOP_TIMEOUT + START_TIMEOUT;
        loop {
            if self.find().await.is_some_and(|(n, _)| n["pid"] != d["pid"]) {
                *self.last_error.lock().unwrap() = None;
                *self.launched.lock().unwrap() = None;
                return Ok(());
            }
            if Instant::now() > until {
                return Err("yerel çekirdek yeniden açılmadı".into());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// #161: a drain taken back — the queue is delivered again.
    pub(crate) async fn drain_cancel(&self) -> Result<(), String> {
        let Some((_, h)) = self.endpoint() else { return Ok(()) };
        let (mut ws, _) = hello(&h).await?;
        let r = request(&mut ws, "core_drain_cancel").await?;
        match r.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("core_drain_cancel").to_string()),
            None => Ok(()),
        }
    }

    async fn started(&self) {
        self.startup.get_or_init(|| async { self.ensure().await.map(drop) }).await;
    }

    /// Why there is no core: the last start's failure, if any.
    pub(crate) fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    async fn ensure(&self) -> Result<LocalHost, String> {
        let _busy = self.busy.lock().await;
        let r = self.find_or_start().await;
        *self.last_error.lock().unwrap() = r.as_ref().err().cloned();
        r
    }

    async fn find_or_start(&self) -> Result<LocalHost, String> {
        if let Some((_, h)) = self.find().await {
            return Ok(h);
        }
        let log = StartLog::open(&self.data_dir.join("logs"));
        let r = match dev_build().and_then(|()| source_exe()) {
            Ok(src) => self.start(&src, &log).await,
            Err(e) => Err(e),
        };
        if let Err(e) = &r {
            log.line(&format!("dex: {e}"));
        }
        r
    }

    async fn start(&self, src: &Path, log: &StartLog) -> Result<LocalHost, String> {
        let exe = run_copy(src, &self.bin_dir)?;
        log.line(&format!("dex: çekirdek başlatılıyor: {}", exe.display()));
        let (how, mut started) = self.launch(&exe, log)?;
        *self.launched.lock().unwrap() = Some(how);
        let until = Instant::now() + START_TIMEOUT;
        loop {
            if let Some((_, h)) = self.find().await {
                return Ok(h);
            }
            if let Some(code) = started.exited() {
                // Lost a race to another window's start (that one answers), or failed.
                if let Some((_, h)) = self.find().await {
                    return Ok(h);
                }
                let out = log.tail();
                // Started under explorer its first words reach no file; a dexd that knows
                // `--background` writes them there itself — silence says it is not this build's.
                let out = if out.is_empty() { " — çıktısı yok: bu sürümün dexd'si değil olabilir".into() } else { format!(": {out}") };
                return Err(format!("yerel çekirdek açılırken çıktı (kod {code}){out} — {}", log.path.display()));
            }
            if Instant::now() > until {
                return Err(format!("yerel çekirdek {} sn içinde açılmadı — {}", START_TIMEOUT.as_secs(), log.path.display()));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub(crate) async fn stop(&self) -> Result<(), String> {
        let _busy = self.busy.lock().await;
        let Some((d, h)) = self.find().await else { return Ok(()) };
        let (mut ws, _) = hello(&h).await?;
        let r = request(&mut ws, "core_shutdown").await?;
        if let Some(e) = r.get("error") {
            return Err(e["message"].as_str().unwrap_or("core_shutdown").to_string());
        }
        // Its discovery goes just before it exits (the lock with it; a new start waits for that).
        let until = Instant::now() + STOP_TIMEOUT;
        while self.discovery().is_some_and(|n| n["pid"] == d["pid"]) {
            if Instant::now() > until {
                return Err("yerel çekirdek kapanmadı".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    fn args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = ["--local", "--background", "--wait-lock", "--data-dir"].map(OsString::from).into();
        a.push(self.data_dir.clone().into());
        if self.config_dir != self.data_dir {
            a.extend([OsString::from("--config-dir"), self.config_dir.clone().into()]);
        }
        a
    }

    fn launch(&self, exe: &Path, log: &StartLog) -> Result<(&'static str, Started), String> {
        let mut cmd = std::process::Command::new(exe);
        cmd.args(self.args());
        // What it says before it takes the log itself (`--background`) — a dexd too old to know
        // `--local` says so only here. (Started under explorer: no handles cross; its exit code.)
        cmd.stdin(std::process::Stdio::null()).stdout(log.stdio()).stderr(log.stdio());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x8;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
            match cmd.spawn() {
                Ok(c) => Ok(("direct", Started::Child(c))),
                // ERROR_ACCESS_DENIED: we are in a job without JOB_OBJECT_LIMIT_BREAKAWAY_OK —
                // `tauri dev` (cargo run's kill-on-close job), an IDE, a Dex worker. Staying in it
                // would kill the core with this window.
                Err(e) if e.raw_os_error() == Some(5) => match under_shell(exe, &self.args()) {
                    Ok(h) => Ok(("shell", Started::Process(h))),
                    Err(why) => {
                        log.line(&format!("dex: explorer altında başlatılamadı ({why}); zamanlanmış görev"));
                        schtasks(exe, &self.args()).map(|()| ("schtasks", Started::Unwatched))
                    }
                },
                Err(e) => Err(format!("{}: {e}", exe.display())),
            }
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            cmd.spawn().map(|c| ("direct", Started::Child(c))).map_err(|e| format!("{}: {e}", exe.display()))
        }
    }
}

/// A dexd just started, watched until it answers: gone first = it failed.
enum Started {
    Child(std::process::Child),
    #[cfg(windows)]
    Process(std::os::windows::io::OwnedHandle),
    /// schtasks: Task Scheduler's process, not ours to see.
    #[cfg_attr(not(windows), allow(dead_code))]
    Unwatched,
}

impl Started {
    /// Its exit code, once it has exited.
    fn exited(&mut self) -> Option<i64> {
        match self {
            Started::Child(c) => c.try_wait().ok().flatten().map(|s| s.code().map_or(-1, i64::from)),
            #[cfg(windows)]
            Started::Process(h) => {
                use std::os::windows::io::AsRawHandle;
                const STILL_ACTIVE: u32 = 259;
                let mut code = 0u32;
                let ok = unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(h.as_raw_handle() as _, &mut code) };
                (ok != 0 && code != STILL_ACTIVE).then_some(i64::from(code))
            }
            Started::Unwatched => None,
        }
    }
}

/// `<logs>/dexd-stderr.log`, appended: a few lines per start attempt from this shell, then dexd's
/// own output (a direct start hands it this file; `--background` appends to it too). Never
/// truncated — past `LOG_CAP` the previous generation becomes `dexd-stderr.1.log`.
struct StartLog {
    path: PathBuf,
    /// The file's length when this attempt began: `tail` reads only what came after.
    from: u64,
}

impl StartLog {
    fn open(logs: &Path) -> StartLog {
        let path = logs.join(STDERR_LOG);
        let _ = private_fs::log_dir(logs);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_CAP) {
            let _ = std::fs::rename(&path, logs.join("dexd-stderr.1.log"));
        }
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut log = StartLog { path, from: 0 };
        log.line(&format!("--- dex (pid {}) unix {secs}", std::process::id()));
        log.from = std::fs::metadata(&log.path).map_or(0, |m| m.len());
        log
    }

    fn append(&self) -> Option<std::fs::File> {
        private_fs::append(&self.path).ok()
    }

    fn line(&self, s: &str) {
        use std::io::Write;
        if let Some(mut f) = self.append() {
            let _ = writeln!(f, "{s}");
        }
    }

    fn stdio(&self) -> std::process::Stdio {
        self.append().map_or_else(std::process::Stdio::null, std::process::Stdio::from)
    }

    /// The last lines dexd wrote in this attempt (bounded), for the error.
    fn tail(&self) -> String {
        let all = std::fs::read(&self.path).unwrap_or_default();
        let new = String::from_utf8_lossy(all.get(self.from as usize..).unwrap_or_default()).into_owned();
        let lines: Vec<&str> = new.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("dex: ")).collect();
        let s = lines[lines.len().saturating_sub(3)..].join(" · ");
        match s.char_indices().nth(300) {
            Some((i, _)) => format!("{}…", &s[..i]),
            None => s,
        }
    }
}

/// `cargo run`'s (`tauri dev`) `CARGO` and `CARGO_MANIFEST_DIR`. `take_cargo_env` takes them out of
/// this process's environment at start, with the build's `rustc-env`s (`DEX_COMMIT`): they say how
/// this app was run — not something every dexd, agent and shell it starts should inherit (#202).
static CARGO_RUN: std::sync::OnceLock<Option<(OsString, PathBuf)>> = std::sync::OnceLock::new();

pub fn take_cargo_env() {
    let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        let _ = CARGO_RUN.set(None);
        return;
    };
    let run = std::env::var_os("CARGO").filter(|_| cfg!(debug_assertions)).map(|c| (c, PathBuf::from(dir)));
    let ours = |k: &str| k.starts_with("CARGO_PKG_") || ["CARGO", "CARGO_MANIFEST_DIR", "CARGO_MANIFEST_PATH", "CARGO_PRIMARY_PACKAGE", "DEX_COMMIT", "DEX_COMMIT_TIME"].contains(&k);
    let keys: Vec<OsString> = std::env::vars_os().map(|(k, _)| k).filter(|k| k.to_str().is_some_and(ours)).collect();
    for k in keys {
        std::env::remove_var(k);
    }
    let _ = CARGO_RUN.set(run);
}

/// Under `cargo run` (a debug build): `cargo build -p dexd` into this app's target dir before every
/// start — `tauri dev` builds dexd only in `beforeDevCommand`; its watcher rebuilds this app alone
/// (#203). Up to date: a second or two.
fn dev_build() -> Result<(), String> {
    let Some(Some((cargo, dir))) = CARGO_RUN.get() else { return Ok(()) };
    if std::env::var_os("DEX_DEXD").is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // <target>/debug/dex.exe (not a `--target <triple>` build: <target>/<triple>/debug).
    let Some(target) = exe.parent().filter(|d| d.file_name().is_some_and(|n| n == "debug")).and_then(Path::parent) else { return Ok(()) };
    let mut cmd = std::process::Command::new(cargo);
    cmd.args(["build", "-p", "dexd", "--target-dir"]).arg(target).current_dir(dir).stdin(std::process::Stdio::null());
    // #400: `cargo run` hands this app its build script's OUT_DIR; ring's build script watches that
    // variable, so a build with it and `tauri dev`'s without rebuilt ring → dexd relinked every start.
    cmd.env_remove("OUT_DIR");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("cargo build -p dexd: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = err.lines().map(str::trim).filter(|l| l.starts_with("error")).take(3).collect();
    Err(format!("çekirdek derlenemedi (cargo build -p dexd): {}", lines.join(" · ")))
}

/// The `dexd` this build ships with: beside the app (bundle `externalBin`; `cargo build -p dexd`
/// in the same target dir for `tauri dev`), or `DEX_DEXD`.
fn source_exe() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("DEX_DEXD") {
        return Ok(p.into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let p = exe.with_file_name(format!("dexd{}", std::env::consts::EXE_SUFFIX));
    if p.exists() {
        Ok(p)
    } else {
        Err(format!("çekirdek programı yok: {} (geliştirmede: cargo build -p dexd)", p.display()))
    }
}

/// A build's copy name (size + mtime). dexd stages an update under the same name (crates/dexd
/// src/local.rs `copy_name`) and publishes its own (`exe`): keep the two in step.
fn copy_name(meta: &std::fs::Metadata) -> String {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    format!("dexd-{}-{mtime}{}", meta.len(), std::env::consts::EXE_SUFFIX)
}

/// #427: byte for byte (sizes first); unreadable → not the same. dexd links reproducibly
/// (crates/dexd/build.rs `/Brepro`), so the same build gives the same bytes. Once per app start.
fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let open = |p: &Path| std::fs::File::open(p).and_then(|f| Ok((f.metadata()?.len(), std::io::BufReader::with_capacity(1 << 16, f))));
    let (Ok((la, mut ra)), Ok((lb, mut rb))) = (open(a), open(b)) else { return false };
    if la != lb {
        return false;
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let Ok(n) = ra.read(&mut ba) else { return false };
        if n == 0 {
            return true;
        }
        if rb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

/// #161: `dexd --version` — `{commit, commitTime}`; None from a dexd that predates it.
fn version_of(exe: &Path) -> Option<Value> {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--version").stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    serde_json::from_slice(&out.stdout).ok()
}

/// `src` copied once per build (size + mtime) into `bin`; older copies go when nothing runs them.
fn run_copy(src: &Path, bin: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let name = copy_name(&meta);
    std::fs::create_dir_all(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let dst = bin.join(&name);
    if !dst.exists() {
        let tmp = bin.join(format!("{name}.tmp-{}", std::process::id()));
        std::fs::copy(src, &tmp).and_then(|_| std::fs::rename(&tmp, &dst)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", dst.display())
        })?;
    }
    for e in std::fs::read_dir(bin).into_iter().flatten().flatten() {
        let n = e.file_name();
        if n.to_string_lossy().starts_with("dexd-") && n != *name.as_str() {
            // A running copy is locked: it stays until next time.
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(dst)
}

/// Started as a child of this session's shell (explorer.exe, `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`):
/// it takes the shell's job membership (none) and token (this user, not elevated), not ours — the
/// #150 experiment: from inside a no-breakaway job, such a child is in no job at all.
#[cfg(windows)]
fn under_shell(exe: &Path, args: &[OsString]) -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
    const DETACHED_PROCESS: u32 = 0x8;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    // CommandLineToArgvW rules; our arguments are paths and flags (no quotes inside).
    let quote = |s: &std::ffi::OsStr| format!("\"{}\"", s.to_string_lossy());
    let line = std::iter::once(quote(exe.as_os_str())).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
    let mut line: Vec<u16> = std::ffi::OsStr::new(&line).encode_wide().chain([0]).collect();
    unsafe {
        let mut pid = 0u32;
        let shell = GetShellWindow();
        if shell.is_null() || GetWindowThreadProcessId(shell, &mut pid) == 0 || pid == 0 {
            return Err("masaüstü kabuğu (explorer) yok".into());
        }
        let mut parent: HANDLE = OpenProcess(PROCESS_CREATE_PROCESS, 0, pid);
        if parent.is_null() {
            return Err(format!("explorer açılamadı (hata {})", GetLastError()));
        }
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut buf = vec![0u64; size.div_ceil(8)];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let mut r = Err(String::new());
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0 {
            if UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize, (&mut parent as *mut HANDLE).cast(), std::mem::size_of::<HANDLE>(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                let mut si: STARTUPINFOEXW = std::mem::zeroed();
                si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
                si.lpAttributeList = list;
                let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
                let flags = EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
                r = if CreateProcessW(std::ptr::null(), line.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, flags, std::ptr::null(), std::ptr::null(), &si.StartupInfo, &mut pi) != 0 {
                    use std::os::windows::io::FromRawHandle;
                    CloseHandle(pi.hThread);
                    Ok(std::os::windows::io::OwnedHandle::from_raw_handle(pi.hProcess as _))
                } else {
                    Err(format!("CreateProcess (hata {})", GetLastError()))
                };
            }
            DeleteProcThreadAttributeList(list);
        }
        CloseHandle(parent);
        r.map_err(|e| if e.is_empty() { format!("öznitelik listesi (hata {})", GetLastError()) } else { e })
    }
}

/// Last resort (no shell window): a one-shot task, run now — Task Scheduler starts it in this
/// user's session, outside our job, but inside its own (the default 72 h run limit applies).
#[cfg(windows)]
fn schtasks(exe: &Path, args: &[OsString]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let quote = |s: &std::ffi::OsStr| {
        let s = s.to_string_lossy();
        if s.contains([' ', '\t']) { format!("\"{s}\"") } else { s.into_owned() }
    };
    let tr = std::iter::once(format!("\"{}\"", exe.display())).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
    if tr.len() > 261 {
        return Err(format!("zamanlanmış görev komutu çok uzun ({} > 261)", tr.len()));
    }
    let user = std::env::var("USERNAME").unwrap_or_default();
    let name: String = format!("DexCore-{user}").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("schtasks").args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| format!("schtasks: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("schtasks {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
        }
    };
    run(&["/Create", "/F", "/TN", &name, "/SC", "ONCE", "/ST", "00:00", "/TR", &tr])?;
    let r = run(&["/Run", "/TN", &name]);
    // The started process keeps running; the task itself is not left behind.
    let _ = run(&["/Delete", "/F", "/TN", &name]);
    r
}

/// A probe connection and the core's `auth.hello` answer (its build, its methods). #2288: the
/// token is proved, not sent (crates/dexd/src/local_auth.rs): `auth.hello` carries none; the
/// core answers a challenge and its own proof over it, checked first — so whatever listens on
/// the port a tampered discovery file names gets nothing — then `auth.proof`.
async fn hello(h: &LocalHost) -> Result<(Ws, Value), String> {
    let go = async {
        let (mut ws, _) = tokio_tungstenite::connect_async(&h.url).await.map_err(|e| e.to_string())?;
        let params = json!({"protocol": 1, "probe": true});
        ws.send(Message::Text(json!({"id": 1, "method": "auth.hello", "params": params}).to_string().into()))
            .await
            .map_err(|e| e.to_string())?;
        let offer = reply(&mut ws).await?;
        let (Some(challenge), Some(proof)) = (offer["result"]["challenge"].as_str(), offer["result"]["proof"].as_str()) else {
            return Err(format!("yerel çekirdek reddetti: {}", offer["error"]["message"]));
        };
        let proof = answer(h, challenge, proof)?;
        ws.send(Message::Text(json!({"id": 1, "method": "auth.proof", "params": {"proof": proof}}).to_string().into()))
            .await
            .map_err(|e| e.to_string())?;
        let r = reply(&mut ws).await?;
        if r["result"]["device"]["id"] == "local" {
            Ok((ws, r["result"].clone()))
        } else {
            Err(format!("yerel çekirdek reddetti: {}", r["error"]["message"]))
        }
    };
    tokio::time::timeout(PROBE_TIMEOUT, go).await.map_err(|_| "yerel çekirdek cevap vermedi".to_string())?
}

/// #2288: the client's proof for the core's offer (`challenge`, `proof`: hex) — only once the
/// offer verifies with this shell's token and the port it dialed; a peer that is not the core
/// (another listener, a relay from another port) gets an error and nothing else.
fn answer(h: &LocalHost, challenge: &str, proof: &str) -> Result<String, String> {
    let (Some(challenge), Some(proof)) = (local_auth::unhex32(challenge), local_auth::unhex32(proof)) else {
        return Err("yerel çekirdek bozuk teklif verdi".into());
    };
    let key = local_auth::key(&h.token);
    if !local_auth::ct_eq(&local_auth::server_proof(&key, &challenge, h.port), &proof) {
        return Err("yerel çekirdek jetonu bilmiyor".into());
    }
    Ok(local_auth::hex(&local_auth::client_proof(&key, &challenge, h.port)))
}

async fn request(ws: &mut Ws, method: &str) -> Result<Value, String> {
    request_with(ws, method, json!({})).await
}

async fn request_with(ws: &mut Ws, method: &str, params: Value) -> Result<Value, String> {
    ws.send(Message::Text(json!({"id": 2, "method": method, "params": params}).to_string().into()))
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(PROBE_TIMEOUT, reply(ws)).await.map_err(|_| format!("{method}: cevap yok"))?
}

async fn reply(ws: &mut Ws) -> Result<Value, String> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str()).map_err(|e| e.to_string())?;
                if v.get("id").is_some() {
                    return Ok(v);
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("bağlantı kapandı".into()),
        }
    }
}

/// Starts looking (and starting) right away, in parallel with the webview's load.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("dex-core-proc")
        .setup(|app, _| {
            let path = crate::app_paths::get(app);
            let proc = Arc::new(CoreProc {
                data_dir: path.data_dir,
                config_dir: path.config_dir,
                bin_dir: path.local_data_dir.join("bin"),
                startup: OnceCell::new(),
                busy: Mutex::new(()),
                launched: std::sync::Mutex::new(None),
                last_error: std::sync::Mutex::new(None),
                update_for: std::sync::Mutex::new(None),
            });
            app.manage(proc.clone());
            tauri::async_runtime::spawn(async move {
                proc.started().await;
                proc.dev_update().await;
            });
            Ok(())
        })
        .build()
}

/// The endpoint for this webview, or `down` (start-up failed / it stopped since).
#[tauri::command]
pub async fn local_host(proc: State<'_, Arc<CoreProc>>) -> Result<LocalAnswer, String> {
    proc.started().await;
    Ok(match proc.find().await {
        Some((_, h)) => LocalAnswer::Up(h),
        None => LocalAnswer::Down { down: proc.last_error().unwrap_or_else(|| "yerel çekirdek çalışmıyor".into()) },
    })
}

/// "Çekirdeği başlat": the running one, or a new one.
#[tauri::command]
pub async fn local_start(proc: State<'_, Arc<CoreProc>>) -> Result<LocalHost, String> {
    proc.ensure().await
}

/// #2288: the webview's side of the handshake, with the shell's token (the webview never holds
/// it): the core's offer in, the client's proof out — or why the offer is not the running core's.
#[tauri::command]
pub async fn local_proof(proc: State<'_, Arc<CoreProc>>, challenge: String, proof: String) -> Result<String, String> {
    let (_, h) = proc.endpoint().ok_or_else(|| "yerel çekirdek çalışmıyor".to_string())?;
    answer(&h, &challenge, &proof)
}

#[tauri::command]
pub async fn local_status(proc: State<'_, Arc<CoreProc>>) -> Result<LocalStatus, String> {
    let launched = *proc.launched.lock().unwrap();
    let Some((d, h)) = proc.find().await else {
        return Ok(LocalStatus { launched, ..Default::default() });
    };
    Ok(LocalStatus {
        running: true,
        pid: d["pid"].as_u64(),
        url: Some(h.url),
        commit: d["commit"].as_str().map(String::from),
        commit_time: d["commitTime"].as_i64(),
        started_at: d["startedAt"].as_i64(),
        launched,
        update_pending: proc.update_pending(),
    })
}

/// Stops the core now (its agents with it), whoever else is connected.
#[tauri::command]
pub async fn local_shutdown(proc: State<'_, Arc<CoreProc>>) -> Result<(), String> {
    proc.stop().await
}

/// The core again as this app's `dexd` build (picks up a newer bundled one), keeping its queue and
/// leaders; `whenIdle`: once it is drained ("Çekirdek güncel değil — boşalınca yeniden başlat").
#[tauri::command]
pub async fn local_restart(proc: State<'_, Arc<CoreProc>>, when_idle: Option<bool>) -> Result<(), String> {
    proc.restart(when_idle.unwrap_or(false)).await
}

/// #161: "Vazgeç" on a drain: jobs are delivered again, no restart.
#[tauri::command]
pub async fn local_drain_cancel(proc: State<'_, Arc<CoreProc>>) -> Result<(), String> {
    proc.drain_cancel().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn local_core_flags_binary_and_logs_follow_the_shared_storage_selection() {
        use crate::desktop_storage::{select_at, IDENTIFIER, LEGACY_IDENTIFIER};
        for state in ["legacy", "new", "incomplete"] {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dex/tmp")
                .join(format!("local-storage-{state}-{}-{}", std::process::id(),
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let data = root.join("data");
            let config = root.join("config");
            let local = root.join("local");
            if state != "new" {
                std::fs::create_dir_all(data.join(LEGACY_IDENTIFIER)).unwrap();
            }
            if state == "legacy" {
                std::fs::write(data.join(LEGACY_IDENTIFIER).join("dex.db"), b"existing database").unwrap();
            }
            let selected = select_at(IDENTIFIER, &data, &config, &local);
            let id = if state == "legacy" { LEGACY_IDENTIFIER } else { IDENTIFIER };
            let mut proc = proc_for(&root);
            proc.data_dir = selected.data_dir;
            proc.config_dir = selected.config_dir;
            proc.bin_dir = selected.local_data_dir.join("bin");
            assert_eq!(proc.data_dir, data.join(id));
            assert_eq!(proc.config_dir, config.join(id));
            assert_eq!(proc.bin_dir, local.join(id).join("bin"));
            let args = proc.args();
            assert!(args.windows(2).any(|pair| pair == [OsString::from("--data-dir"), data.join(id).into_os_string()]));
            assert!(args.windows(2).any(|pair| pair == [OsString::from("--config-dir"), config.join(id).into_os_string()]));
            let log = StartLog::open(&proc.data_dir.join("logs"));
            assert_eq!(log.path, data.join(id).join("logs").join(STDERR_LOG));
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-core-proc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A listener standing in for a planted `url`/`port`: the text frames it receives, and the
    /// offer it makes (a bogus challenge and proof, as a peer without the token would).
    fn fake_core(listener: tokio::net::TcpListener) -> tokio::task::JoinHandle<Vec<String>> {
        tokio::spawn(async move {
            let mut got = Vec::new();
            let Ok(Ok((stream, _))) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await else { return got };
            let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return got };
            while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                match m {
                    Message::Text(t) => {
                        got.push(t.to_string());
                        let offer = json!({"id": 1, "result": {"challenge": "11".repeat(32), "proof": "22".repeat(32)}});
                        let _ = ws.send(Message::Text(offer.to_string().into())).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            got
        })
    }

    /// #2288 (security audit HIGH): a discovery file a worker rewrote — `url` to its listener,
    /// `tokenFile` to any file, `port` to another of its listeners — gives it nothing: the shell
    /// dials `127.0.0.1:<port>` only, reads its own token file, sends no token in any frame, and
    /// says nothing more to a peer whose proof is not the core's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tampered_discovery_file_leaks_no_token() {
        let d = dir("tamper");
        let p = proc_for(&d);
        std::fs::create_dir_all(&p.data_dir).unwrap();
        std::fs::write(p.data_dir.join(TOKEN_FILE), "real-token-0123456789abcdef\n").unwrap();
        let planted = d.join("planted.token");
        std::fs::write(&planted, "planted-token").unwrap();
        let by_url = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let by_port = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (url_port, port) = (by_url.local_addr().unwrap().port(), by_port.local_addr().unwrap().port());
        let (by_url, by_port) = (fake_core(by_url), fake_core(by_port));
        let disc = json!({
            "pid": 1, "port": port, "url": format!("ws://127.0.0.1:{url_port}"), "tokenFile": planted,
            "commit": "x", "commitTime": 0, "startedAt": 0, "exe": "dexd.exe",
        });
        std::fs::write(p.data_dir.join(DISCOVERY_FILE), disc.to_string()).unwrap();

        let (_, h) = p.endpoint().expect("a numeric port and the shell's own token file");
        assert_eq!((h.url.as_str(), h.port), (format!("ws://127.0.0.1:{port}").as_str(), port), "url ignored");
        assert_eq!(h.token, "real-token-0123456789abcdef", "tokenFile ignored");
        assert!(!serde_json::to_string(&h).unwrap().contains("token"), "the webview gets the url only: {}", serde_json::to_string(&h).unwrap());

        assert!(p.find().await.is_none(), "a peer that cannot prove the token is not the core");
        let (by_url, by_port) = (by_url.await.unwrap(), by_port.await.unwrap());
        assert!(by_url.is_empty(), "the planted url was dialed: {by_url:?}");
        assert_eq!(by_port.len(), 1, "one frame — the hello; no proof for a bogus offer: {by_port:?}");
        let hello: Value = serde_json::from_str(&by_port[0]).unwrap();
        assert_eq!((hello["method"].as_str(), hello["params"]["probe"].as_bool()), (Some("auth.hello"), Some(true)), "{hello}");
        assert!(hello["params"].get("token").is_none(), "{hello}");
        for secret in ["real-token", "planted-token"] {
            assert!(!by_port[0].contains(secret), "{secret} crossed the socket: {}", by_port[0]);
        }
        // The webview's path: the same check, the same refusal.
        assert_eq!(answer(&h, &"11".repeat(32), &"22".repeat(32)).unwrap_err(), "yerel çekirdek jetonu bilmiyor");
        assert!(answer(&h, "zz", "22").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #2288: no port, a port that is not a number, out of range, or 0 → no endpoint; the token
    /// file missing → no endpoint (the discovery file alone names no credential).
    #[test]
    fn endpoint_needs_a_valid_port_and_the_shells_own_token_file() {
        let d = dir("port");
        let p = proc_for(&d);
        std::fs::create_dir_all(&p.data_dir).unwrap();
        let write = |disc: Value| std::fs::write(p.data_dir.join(DISCOVERY_FILE), disc.to_string()).unwrap();
        write(json!({"port": 4242}));
        assert!(p.endpoint().is_none(), "no token file");
        std::fs::write(p.data_dir.join(TOKEN_FILE), "t").unwrap();
        assert_eq!(p.endpoint().map(|(_, h)| (h.url, h.port, h.token)), Some(("ws://127.0.0.1:4242".into(), 4242, "t".into())));
        for bad in [json!({}), json!({"port": "4242", "url": "ws://127.0.0.1:4242"}), json!({"port": 0}), json!({"port": 65536}), json!({"port": -1}), json!({"port": 42.5}), json!({"url": "ws://127.0.0.1:4242"})] {
            write(bad.clone());
            assert!(p.endpoint().is_none(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copies_once_per_build_and_sweeps_old_copies() {
        let d = dir("copy");
        let src = d.join("dexd.exe");
        std::fs::write(&src, b"v1").unwrap();
        let bin = d.join("bin");
        let a = run_copy(&src, &bin).unwrap();
        assert_eq!(run_copy(&src, &bin).unwrap(), a, "same build, same copy");
        std::fs::write(bin.join("dexd-old.exe"), b"old").unwrap();
        std::fs::write(&src, b"v2-longer").unwrap();
        let b = run_copy(&src, &bin).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&b).unwrap(), b"v2-longer");
        let left: Vec<_> = std::fs::read_dir(&bin).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(left, vec![b.clone()], "older copies swept");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #427: the running copy and this build's dexd with the same bytes under another mtime (a
    /// relink that changed nothing) are not outdated; a byte apart, they are told by commitTime.
    #[test]
    fn same_bytes_under_another_name_is_not_outdated() {
        let d = dir("same");
        let p = proc_for(&d);
        let src = d.join("dexd.exe");
        std::fs::write(&src, b"build-one").unwrap();
        let running = run_copy(&src, &p.bin_dir).unwrap();
        let name = running.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::create_dir_all(&p.data_dir).unwrap();
        std::fs::write(p.data_dir.join(DISCOVERY_FILE), json!({ "exe": name, "commitTime": 0 }).to_string()).unwrap();
        // Relinked: same bytes, a later mtime → another copy name.
        let later = std::time::SystemTime::now() + Duration::from_secs(120);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(later).unwrap();
        assert_ne!(copy_name(&std::fs::metadata(&src).unwrap()), name);
        std::env::set_var("DEX_DEXD", &src);
        assert_eq!(p.outdated(), Some(false), "same bytes: no restart");
        // Only a bare name in the bin folder is read ("../dexd.exe" would be the source itself).
        std::fs::write(p.data_dir.join(DISCOVERY_FILE), json!({ "exe": "../dexd.exe", "commitTime": 0 }).to_string()).unwrap();
        assert_ne!(p.outdated(), Some(false));
        std::fs::write(&src, b"build-two").unwrap();
        assert!(!same_content(&running, &src), "same size, other bytes");
        assert!(!same_content(&running, &d.join("none.exe")));
        std::env::remove_var("DEX_DEXD");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn proc_for(d: &Path) -> CoreProc {
        CoreProc {
            data_dir: d.join("data"),
            config_dir: d.join("data"),
            bin_dir: d.join("bin"),
            startup: OnceCell::new(),
            busy: Mutex::new(()),
            launched: std::sync::Mutex::new(None),
            last_error: std::sync::Mutex::new(None),
            update_for: std::sync::Mutex::new(None),
        }
    }

    /// #203: the log is appended (never truncated), an attempt's `tail` is its own output only,
    /// and past the cap the previous generation is kept as `.1`.
    #[test]
    fn start_log_appends_and_tails_its_attempt() {
        let d = dir("log");
        let logs = d.join("logs");
        let a = StartLog::open(&logs);
        std::fs::OpenOptions::new().append(true).open(&a.path).unwrap().write_all(b"first: boom\n").unwrap();
        let b = StartLog::open(&logs);
        b.line("dex: çekirdek başlatılıyor: x");
        std::fs::OpenOptions::new().append(true).open(&b.path).unwrap().write_all(b"bilinmeyen se\xc3\xa7enek: --local\n").unwrap();
        assert_eq!(b.tail(), "bilinmeyen seçenek: --local", "its own lines, not the shell's or the earlier attempt's");
        let all = std::fs::read_to_string(&b.path).unwrap();
        assert!(all.contains("first: boom") && all.matches("--- dex (pid").count() == 2, "{all}");
        std::fs::write(&b.path, vec![b'x'; LOG_CAP as usize + 1]).unwrap();
        let c = StartLog::open(&logs);
        assert!(logs.join("dexd-stderr.1.log").exists() && std::fs::metadata(&c.path).unwrap().len() < 100);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #203's case: a "dexd" that does not know `--local` exits at once — the start fails then,
    /// with its exit code and what it said (a direct start), not after START_TIMEOUT.
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_core_that_exits_fails_the_start_at_once() {
        let d = dir("exits");
        let p = proc_for(&d);
        let log = StartLog::open(&p.data_dir.join("logs"));
        let t = Instant::now();
        let e = p.start(Path::new(r"C:\Windows\System32\where.exe"), &log).await.err().expect("must fail");
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert!(e.starts_with("yerel çekirdek açılırken çıktı (kod 2)") && e.ends_with("dexd-stderr.log"), "{e}");
        if *p.launched.lock().unwrap() == Some("direct") {
            assert!(e.contains("--local"), "its stderr: {e}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The real binary (DEX_DEXD, else the workspace's): not found → started detached → found;
    /// a second `ensure` finds the same one; `stop` ends it. Records how it was started (job
    /// breakaway or schtasks) — the B experiment.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "starts a real dexd --local: DEX_DEXD=<target>/debug/dexd.exe cargo test -p dex -- --ignored"]
    async fn finds_starts_and_stops_a_real_core() {
        let d = dir("real");
        let p = proc_for(&d);
        assert!(p.find().await.is_none());
        let h = p.ensure().await.unwrap();
        eprintln!("launched: {:?}", p.launched.lock().unwrap());
        let disc = p.discovery().unwrap();
        assert_eq!(p.ensure().await.unwrap().url, h.url, "found, not started again");
        // The token stays out of the discovery file; the url is loopback.
        assert!(h.url.starts_with("ws://127.0.0.1:") && !disc.to_string().contains(&h.token));
        // #161: started from this build's copy → current; a restart comes back (a new pid) as it.
        assert_eq!(p.outdated(), Some(false), "{disc}");
        p.restart(false).await.unwrap();
        let again = p.discovery().unwrap();
        assert!(again["pid"] != disc["pid"] && again["exe"] == disc["exe"], "{again}");
        assert!(p.find().await.is_some());
        p.stop().await.unwrap();
        assert!(p.find().await.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
