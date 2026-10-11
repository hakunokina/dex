//! #150 (B): `dexd --local` — the desktop's core in its own process (.dex/results/188.md §2).
//!
//! - Same data dir as the app (`%APPDATA%\com.hakunokina.dex`), seeded `Core::start`; `dex.lock` keeps it
//!   to one per data dir (a second one exits with the lock error).
//! - Loopback only, one random token per start (`LocalToken`, the A0 branch of `auth.hello`).
//! - Discovery: `<data_dir>/dexd.local.json` (pid, port, url, token file, commit) and the token in
//!   `<data_dir>/dexd.local.token`; both owner-only, written atomically, removed on exit.
//! - Lifetime (patron): while no window is connected for [`GRACE`] (120 s), it shuts the core and
//!   its agents down and exits; a window coming back inside that time keeps it. A `probe` hello
//!   (the shell checking it is alive) does not count as a window.
//! - `core_shutdown` / `core_restart` (the tray): exit now; restart = a fresh copy of this binary
//!   once this one let the data dir go. `core_life` (tray, Ayarlar): windows connected and how long
//!   until the grace runs out.
//! - #161 (C): `core_restart {exe?, whenIdle?}` — `exe`: the build to come back as (an update; staged
//!   as a copy next to this one, [`stage`]); `whenIdle`: drain first (`Core::drain`: jobs wait in the
//!   queue) and restart once nothing runs ([`Life::drain`]); `core_drain_cancel` takes it back. A
//!   restart leaves dex-core's marker (`Core::restart_mark`): the next core keeps the queue and
//!   reopens the leaders with `--resume`. `core_shutdown` and the no-window exit leave none.
//! - #400: `whenQuiet` (with `whenIdle`; a dev update) waits for a quiet core without holding the
//!   queue ([`Life::drain`] `quiet`); `auth.hello`'s `restartWhenQuiet` says this build knows it.
//! - #501: `core_quiet_cancel` takes back only a quiet wait ([`Life::quiet_cancel`]); a drain the
//!   patron asked for stays.

use crate::server::Server;
use crate::tailnet::TailnetState;
use dex_core::Paths;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// No window for this long → exit.
pub const GRACE: Duration = Duration::from_secs(120);
/// #161: a draining core looks this often; it restarts after two drained looks in a row (a
/// worker's report wakes its leader a moment later — one look could fall between the two).
pub const DRAIN_POLL: Duration = Duration::from_secs(1);
pub const DISCOVERY_FILE: &str = "dexd.local.json";
pub const TOKEN_FILE: &str = "dexd.local.token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// [`GRACE`] passed with no window.
    Idle,
    Shutdown,
    Restart,
    /// Ctrl+C / the listener ended.
    Signal,
}

/// Connected windows and a requested exit, watched by [`run`].
pub struct Life {
    clients: watch::Sender<usize>,
    exit: watch::Sender<Option<Exit>>,
    grace: Duration,
    /// Unix secs.
    started_at: i64,
    /// Since when no window is connected (from the start, too); None while one is.
    idle_since: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// #161: the build a restart comes back as (staged by `core_restart {exe}`); None: this one.
    next_exe: std::sync::Mutex<Option<PathBuf>>,
    /// #161: the drain watch, while one runs.
    drain: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// #400: that watch waits for a quiet core instead of draining.
    quiet: std::sync::atomic::AtomicBool,
}

/// One connected window; dropping it is the window leaving.
pub struct Client(Arc<Life>);

impl Drop for Client {
    fn drop(&mut self) {
        self.0.clients.send_modify(|n| *n -= 1);
        if self.0.clients() == 0 {
            *self.0.idle_since.lock().unwrap() = Some(tokio::time::Instant::now());
        }
    }
}

impl Life {
    /// `grace`: no window for this long → [`Exit::Idle`].
    pub fn new(grace: Duration) -> Arc<Self> {
        let idle_since = std::sync::Mutex::new(Some(tokio::time::Instant::now()));
        Arc::new(Self {
            clients: watch::channel(0).0,
            exit: watch::channel(None).0,
            grace,
            started_at: unix_now(),
            idle_since,
            next_exe: Default::default(),
            drain: Default::default(),
            quiet: Default::default(),
        })
    }

    pub fn enter(self: &Arc<Self>) -> Client {
        self.clients.send_modify(|n| *n += 1);
        *self.idle_since.lock().unwrap() = None;
        Client(self.clone())
    }

    pub fn clients(&self) -> usize {
        *self.clients.borrow()
    }

    pub fn started_at(&self) -> i64 {
        self.started_at
    }

    pub fn grace(&self) -> Duration {
        self.grace
    }

    /// With no window connected: what is left of the grace (what [`Life::wait`] sleeps).
    pub fn shutdown_in(&self) -> Option<Duration> {
        let since = (*self.idle_since.lock().unwrap())?;
        Some(self.grace.saturating_sub(since.elapsed()))
    }

    pub fn request(&self, exit: Exit) {
        self.exit.send_replace(Some(exit));
    }

    pub fn set_next_exe(&self, exe: PathBuf) {
        *self.next_exe.lock().unwrap() = Some(exe);
    }

    pub fn next_exe(&self) -> Option<PathBuf> {
        self.next_exe.lock().unwrap().clone()
    }

    /// #161: `gauge` drains; once it is drained on two looks in a row → [`Exit::Restart`]. Ends when
    /// the drain is taken back ([`Life::drain_cancel`]). One watch at a time. Inside a runtime.
    ///
    /// #400 `quiet` (a dev update): nothing is held — it waits until `gauge` is quiet (nothing runs
    /// or waits in any team, no job queued) on two looks, then drains and restarts if still quiet;
    /// otherwise it lets the queue go again and keeps waiting. A drain already holding the queue
    /// stays one; a quiet wait gives way to a drain.
    pub fn drain(self: &Arc<Self>, gauge: Arc<dyn DrainGauge>, quiet: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        let mut cur = self.drain.lock().unwrap();
        let running = cur.as_ref().is_some_and(|h| !h.is_finished());
        if running && (quiet || !self.quiet.load(SeqCst)) {
            return;
        }
        if let Some(h) = cur.take() {
            h.abort();
        }
        self.quiet.store(quiet, SeqCst);
        if !quiet {
            gauge.set(true);
        }
        let life = self.clone();
        *cur = Some(tokio::spawn(async move {
            let mut calm = 0;
            loop {
                tokio::time::sleep(DRAIN_POLL).await;
                let ok = if quiet { gauge.quiet() } else { gauge.drained() };
                calm = if ok { calm + 1 } else { 0 };
                if calm < 2 {
                    continue;
                }
                if quiet {
                    // Held from here on: a job dispatched between the look and now shows up. #427/#447:
                    // the drain is dex-core's gate (`Core::admit`) — every turn being started (a
                    // dispatch, a chat or leader-note paste, a start with a prompt) is counted until
                    // it shows, new ones wait; the jobs are one read — this look can't miss a turn.
                    gauge.set(true);
                    if !gauge.quiet() {
                        gauge.set(false);
                        calm = 0;
                        continue;
                    }
                }
                life.request(Exit::Restart);
                return;
            }
        }));
    }

    pub fn drain_cancel(&self, gauge: &dyn DrainGauge) {
        if let Some(h) = self.drain.lock().unwrap().take() {
            h.abort();
        }
        gauge.set(false);
    }

    /// #501: takes back the watch only if it is a quiet wait — the look and the take under one
    /// `drain` lock, so a drain asked for in between (it replaces the wait) stays. True: taken.
    pub fn quiet_cancel(&self, gauge: &dyn DrainGauge) -> bool {
        let mut cur = self.drain.lock().unwrap();
        if !(cur.as_ref().is_some_and(|h| !h.is_finished()) && self.quiet.load(std::sync::atomic::Ordering::SeqCst)) {
            return false;
        }
        if let Some(h) = cur.take() {
            h.abort();
        }
        gauge.set(false);
        true
    }

    /// A drain or a quiet wait runs.
    pub fn draining(&self) -> bool {
        self.drain.lock().unwrap().as_ref().is_some_and(|h| !h.is_finished())
    }

    /// #400: the running watch is a quiet wait (an update waiting, nothing held).
    pub fn waiting_quiet(&self) -> bool {
        self.draining() && self.quiet.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Until an exit is requested, or the grace passes with no client (from the start, too: a shell
    /// that launched us and died never connects).
    pub async fn wait(&self) -> Exit {
        let (mut clients, mut exit) = (self.clients.subscribe(), self.exit.subscribe());
        loop {
            if let Some(e) = *exit.borrow_and_update() {
                return e;
            }
            let idle = *clients.borrow_and_update() == 0;
            tokio::select! {
                _ = clients.changed() => {}
                _ = exit.changed() => {}
                _ = tokio::time::sleep(self.grace), if idle => return Exit::Idle,
            }
        }
    }
}

/// #161: what a drain watches — the core (`Core::drain`, `drain_blockers`); a stub in tests.
pub trait DrainGauge: Send + Sync + 'static {
    fn set(&self, on: bool);
    fn drained(&self) -> bool;
    /// #400: drained, and no job waits in the queue either.
    fn quiet(&self) -> bool;
}

impl DrainGauge for dex_core::Core {
    fn set(&self, on: bool) {
        self.drain(on);
    }
    fn drained(&self) -> bool {
        self.drain_blockers().is_ok_and(|b| b.is_empty())
    }
    fn quiet(&self) -> bool {
        self.quiet_blockers().is_ok_and(|b| b.is_empty())
    }
}

/// #161: `src` (a dexd build: the app's bundled one, `cargo build`'s) copied next to this exe under
/// the shell's copy name (src-tauri core_proc.rs `run_copy`) — Windows can't replace a running
/// .exe, and the shell tells builds apart by that name (`exe` in the discovery).
pub fn stage(src: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: dosya değil", src.display()));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("exe klasörü yok")?;
    let dst = dir.join(copy_name(&meta));
    if !dst.exists() {
        let tmp = dst.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::copy(src, &tmp).and_then(|_| std::fs::rename(&tmp, &dst)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", dst.display())
        })?;
    }
    Ok(dst)
}

/// The same as src-tauri core_proc.rs `run_copy`'s name (`dexd-<len>-<mtime>`): keep them in step.
pub fn copy_name(meta: &std::fs::Metadata) -> String {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    format!("dexd-{}-{mtime}{}", meta.len(), std::env::consts::EXE_SUFFIX)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_secs() as i64)
}

/// `dexd.local.json`. The token is not in it (its own file), so status readers never touch it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub pid: u32,
    pub port: u16,
    pub url: String,
    pub token_file: PathBuf,
    pub commit: String,
    pub commit_time: i64,
    pub started_at: i64,
    /// #161: this build's file name (a staged copy: `dexd-<len>-<mtime>.exe`) — the shell compares
    /// it with its bundled build's copy name to tell an outdated core.
    #[serde(default)]
    pub exe: String,
}

impl Discovery {
    pub fn read(data_dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(data_dir.join(DISCOVERY_FILE)).ok()?).ok()
    }

    fn write(data_dir: &Path, port: u16, token: &str) -> Result<Self, String> {
        let d = Self {
            pid: std::process::id(),
            port,
            url: format!("ws://127.0.0.1:{port}"),
            token_file: data_dir.join(TOKEN_FILE),
            commit: env!("DEX_COMMIT").into(),
            commit_time: env!("DEX_COMMIT_TIME").parse().unwrap_or(0),
            started_at: unix_now(),
            exe: std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default(),
        };
        // Token first: a reader that sees the new discovery finds its token.
        write_private(&d.token_file, token.as_bytes())?;
        let json = serde_json::to_vec_pretty(&d).map_err(|e| e.to_string())?;
        write_private(&data_dir.join(DISCOVERY_FILE), &json)?;
        Ok(d)
    }

    /// Only our own files: a newer instance may already have written its own.
    fn remove(&self, data_dir: &Path) {
        if Self::read(data_dir).is_some_and(|d| d.pid == self.pid) {
            let _ = std::fs::remove_file(data_dir.join(DISCOVERY_FILE));
            let _ = std::fs::remove_file(&self.token_file);
        }
    }
}

/// Writes `path` atomically (temp + rename), readable and writable by this user only — on
/// Windows a protected DACL with one ACE (this user, full), set at creation; elsewhere 0600.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let r = (|| {
        let mut f = create_private(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    r.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(windows)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL};
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    // P: no inherited ACEs; one ACE: this user, full control.
    let sddl = format!("D:P(A;;FA;;;{})", win::user_sid()?);
    unsafe {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let w = wide(sddl.as_ref());
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        // CREATE_NEW: an existing file would keep its own DACL.
        let h = CreateFileW(wide(path.as_os_str()).as_ptr(), GENERIC_WRITE, 0, &sa, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        let err = std::io::Error::last_os_error();
        LocalFree(sd as _);
        if h == INVALID_HANDLE_VALUE {
            return Err(err);
        }
        Ok(std::fs::File::from_raw_handle(h as _))
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(windows)]
pub(crate) mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// This process's user, as an SDDL SID string (`S-1-5-21-…`).
    pub fn user_sid() -> std::io::Result<String> {
        unsafe {
            let mut tok: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(tok, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(tok);
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            sid_string((*(buf.as_ptr() as *const TOKEN_USER)).User.Sid)
        }
    }

    pub fn sid_string(sid: windows_sys::Win32::Security::PSID) -> std::io::Result<String> {
        unsafe {
            let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
            if ConvertSidToStringSidW(sid, &mut p) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            LocalFree(p as _);
            Ok(s)
        }
    }
}

/// Started by the shell in the background: no console of its own, stdout/stderr appended to
/// `<logs>/dexd-stderr.log` (the structured log is `diagnostics`, same folder).
#[cfg(windows)]
fn background(logs: &Path) -> Result<(), String> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{FreeConsole, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    std::fs::create_dir_all(logs).map_err(|e| e.to_string())?;
    let f = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("dexd-stderr.log")).map_err(|e| e.to_string())?;
    let h = f.into_raw_handle();
    unsafe {
        // The schtasks start path gets a console window; let it go.
        FreeConsole();
        SetStdHandle(STD_OUTPUT_HANDLE, h as _);
        SetStdHandle(STD_ERROR_HANDLE, h as _);
    }
    Ok(())
}

#[cfg(not(windows))]
fn background(_: &Path) -> Result<(), String> {
    Ok(())
}

pub struct Opts {
    pub paths: Paths,
    pub grace: Duration,
    /// `--wait-lock`: a restart — the previous instance still holds `dex.lock` for a moment.
    pub wait_lock: bool,
    pub background: bool,
    /// The command line, for `core_restart`.
    pub args: Vec<String>,
}

/// Keep the listener future alive until admitted RPC replies and connection cleanup finish.
pub(crate) async fn serve_until_exit(listener: tokio::net::TcpListener, srv: Arc<Server>, life: Arc<Life>) -> Exit {
    let serving = crate::serve(listener, srv.clone());
    tokio::pin!(serving);
    let exit = tokio::select! {
        _ = &mut serving => return Exit::Signal,
        e = life.wait() => e,
        _ = tokio::signal::ctrl_c() => Exit::Signal,
    };
    srv.stop();
    // A blocked core call or a peer that stops reading must not prevent process exit.
    if tokio::time::timeout(Duration::from_secs(3), &mut serving).await.is_err() {
        eprintln!("dexd: yerel bağlantı temizliği zaman aşımı");
    }
    exit
}

pub fn run(o: Opts) -> Result<(), String> {
    let data_dir = o.paths.data_dir.clone();
    if o.background {
        background(&data_dir.join("logs"))?;
    }
    if o.wait_lock {
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while let Err(e) = dex_core::instance::acquire(&data_dir) {
            if std::time::Instant::now() > until {
                return Err(e);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    dex_core::diagnostics::init(&data_dir.join("logs"))?;
    let tailnet = TailnetState::open(data_dir.clone());
    let life = Life::new(o.grace);
    // dex.lock: a second instance stops here.
    let (srv, token) = Server::start_local(o.paths, tailnet.clone(), life.clone())?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let r = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("yerel uç açılamadı: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let disc = Discovery::write(&data_dir, port, &token)?;
        println!("dexd: ws://127.0.0.1:{port}");
        eprintln!("dexd: yerel çekirdek (pid {}); pencere yokken {} sn sonra kapanır", disc.pid, o.grace.as_secs());
        tokio::spawn(tailnet.clone().start_on_launch(srv.core.clone()));
        let exit = serve_until_exit(listener, srv.clone(), life.clone()).await;
        eprintln!("dexd: kapanıyor ({exit:?})");
        disc.remove(&data_dir);
        Ok::<_, String>(exit)
    });
    rt.block_on(tailnet.stop());
    // #161: the agents still run (they go with this process): the leaders among them are noted.
    if matches!(r, Ok(Exit::Restart)) {
        if let Err(e) = srv.core.restart_mark() {
            eprintln!("dexd: yeniden başlatma işareti yazılamadı ({e}); kuyruk ve liderler bu sefer korunmaz");
        }
    }
    srv.core.shutdown();
    rt.shutdown_timeout(Duration::from_secs(2));
    if r? == Exit::Restart {
        restart(life.next_exe(), &o.args)?;
    }
    Ok(())
}

/// The same arguments again — this binary, or the staged update (`next`) — detached, told to wait
/// for the data dir.
fn restart(next: Option<PathBuf>, args: &[String]) -> Result<(), String> {
    let exe = match next {
        Some(p) => p,
        None => std::env::current_exe().map_err(|e| e.to_string())?,
    };
    eprintln!("dexd: yeniden başlıyor: {}", exe.display());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args.iter().filter(|a| *a != "--wait-lock")).arg("--wait-lock");
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    spawn_detached(&mut cmd).map(drop).map_err(|e| format!("yeniden başlatılamadı: {e}"))
}

/// Outside this process's console and job, so it outlives us.
pub fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // A job without BREAKAWAY_OK refuses the flag (access denied); then stay in it.
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        if let Ok(c) = cmd.spawn() {
            return Ok(c);
        }
        cmd.creation_flags(base);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn exits_after_grace_without_clients_and_a_return_keeps_it() {
        let g = Duration::from_secs(120);
        let life = Life::new(g);
        // From the start: nobody ever connects.
        let t = tokio::time::Instant::now();
        assert_eq!(life.shutdown_in(), Some(g));
        assert_eq!(life.wait().await, Exit::Idle);
        assert_eq!(life.shutdown_in(), Some(Duration::ZERO));
        assert_eq!(t.elapsed(), g);

        // A window, then it leaves; one comes back at 119 s → the timer restarts when it leaves.
        let c = life.enter();
        let l = life.clone();
        let waiter = tokio::spawn(async move {
            let t = tokio::time::Instant::now();
            (l.wait().await, t.elapsed())
        });
        tokio::time::sleep(Duration::from_secs(500)).await;
        assert!(!waiter.is_finished(), "a connected window keeps it");
        assert_eq!(life.shutdown_in(), None, "no countdown while a window is connected");
        drop(c);
        tokio::time::sleep(Duration::from_secs(119)).await;
        assert_eq!(life.shutdown_in(), Some(Duration::from_secs(1)), "the tray's countdown");
        let back = life.enter();
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert!(!waiter.is_finished(), "came back in time");
        assert_eq!(life.clients(), 1);
        drop(back);
        let (exit, took) = waiter.await.unwrap();
        assert_eq!(exit, Exit::Idle);
        assert_eq!(took, Duration::from_secs(500 + 119 + 300 + 120));
    }

    #[tokio::test(start_paused = true)]
    async fn a_requested_exit_wins_over_clients() {
        let life = Life::new(GRACE);
        let _c = life.enter();
        let l = life.clone();
        let waiter = tokio::spawn(async move { l.wait().await });
        tokio::time::sleep(Duration::from_secs(1)).await;
        life.request(Exit::Restart);
        assert_eq!(waiter.await.unwrap(), Exit::Restart);
    }

    #[derive(Default)]
    struct Gauge {
        on: std::sync::atomic::AtomicBool,
        busy: std::sync::atomic::AtomicBool,
        queued: std::sync::atomic::AtomicBool,
        /// A job dispatched in the moment the drain goes on (it shows up in the next look).
        race: std::sync::atomic::AtomicBool,
        /// `set(true)` calls.
        holds: std::sync::atomic::AtomicUsize,
    }
    impl DrainGauge for Gauge {
        fn set(&self, on: bool) {
            use std::sync::atomic::Ordering::SeqCst;
            self.on.store(on, SeqCst);
            if on {
                self.holds.fetch_add(1, SeqCst);
                if self.race.swap(false, SeqCst) {
                    self.busy.store(true, SeqCst);
                }
            }
        }
        fn drained(&self) -> bool {
            !self.busy.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn quiet(&self) -> bool {
            self.drained() && !self.queued.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// #161: a drain restarts only once nothing runs, on two looks in a row; taken back, it never does.
    #[tokio::test(start_paused = true)]
    async fn drain_restarts_once_idle_and_a_cancel_stops_it() {
        use std::sync::atomic::Ordering::SeqCst;
        let life = Life::new(GRACE);
        let _c = life.enter();
        let g = Arc::new(Gauge { busy: true.into(), ..Default::default() });
        let l = life.clone();
        let waiter = tokio::spawn(async move { l.wait().await });
        life.drain(g.clone(), false);
        assert!(g.on.load(SeqCst) && life.draining(), "jobs held");
        tokio::time::sleep(Duration::from_millis(600_500)).await;
        // (Half-way between two looks from here on: a look and a change never land together.)
        assert!(!waiter.is_finished(), "a running job: no restart, however long");
        g.busy.store(false, SeqCst);
        tokio::time::sleep(DRAIN_POLL + Duration::from_millis(10)).await;
        assert!(!waiter.is_finished(), "one drained look is not enough");
        g.busy.store(true, SeqCst);
        tokio::time::sleep(DRAIN_POLL).await;
        g.busy.store(false, SeqCst);
        tokio::time::sleep(DRAIN_POLL * 2 + Duration::from_millis(10)).await;
        assert_eq!(waiter.await.unwrap(), Exit::Restart);

        // Taken back: the queue is delivered again and nothing restarts.
        let life = Life::new(GRACE);
        let _c = life.enter();
        let g = Arc::new(Gauge { busy: true.into(), ..Default::default() });
        life.drain(g.clone(), false);
        life.drain_cancel(&*g);
        assert!(!g.on.load(SeqCst) && !life.draining());
        g.busy.store(false, SeqCst);
        let l = life.clone();
        let waiter = tokio::spawn(async move { l.wait().await });
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!waiter.is_finished());
    }

    /// #400: a dev update's quiet wait holds nothing and restarts only when no agent works or waits
    /// and no job runs or is queued (two looks, then once more with the queue held); a job that
    /// slips in at that moment lets the queue go again. A drain asked for meanwhile takes over.
    #[tokio::test(start_paused = true)]
    async fn quiet_wait_restarts_only_when_nothing_runs_or_is_queued() {
        use std::sync::atomic::Ordering::SeqCst;
        let life = Life::new(GRACE);
        let _c = life.enter();
        let g = Arc::new(Gauge { busy: true.into(), queued: true.into(), ..Default::default() });
        let l = life.clone();
        let waiter = tokio::spawn(async move { l.wait().await });
        life.drain(g.clone(), true);
        assert!(life.waiting_quiet() && !g.on.load(SeqCst), "nothing held: jobs keep going out");
        tokio::time::sleep(Duration::from_millis(600_500)).await;
        assert!(!waiter.is_finished(), "a working agent: no restart");
        g.busy.store(false, SeqCst);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!waiter.is_finished() && g.holds.load(SeqCst) == 0, "a queued job: no restart, nothing held");
        // Quiet, but a job goes out just as the queue is held: let it go, keep waiting.
        g.race.store(true, SeqCst);
        g.queued.store(false, SeqCst);
        tokio::time::sleep(DRAIN_POLL * 2).await;
        assert!(!waiter.is_finished(), "the job that slipped in runs");
        assert!(g.holds.load(SeqCst) == 1 && !g.on.load(SeqCst), "held for one look, then let go");
        // A quiet ask again changes nothing.
        life.drain(g.clone(), true);
        g.busy.store(false, SeqCst);
        tokio::time::sleep(DRAIN_POLL * 2).await;
        assert_eq!(waiter.await.unwrap(), Exit::Restart);
        assert!(g.on.load(SeqCst), "the queue is held into the restart (the marker keeps it)");

        // A drain asked for during a quiet wait takes over (holds at once); a quiet ask leaves a drain.
        let life = Life::new(GRACE);
        let g = Arc::new(Gauge { busy: true.into(), ..Default::default() });
        life.drain(g.clone(), true);
        life.drain(g.clone(), false);
        assert!(g.on.load(SeqCst) && life.draining() && !life.waiting_quiet());
        life.drain(g.clone(), true);
        assert!(g.on.load(SeqCst) && !life.waiting_quiet(), "still a drain");
        life.drain_cancel(&*g);
        assert!(!g.on.load(SeqCst) && !life.draining());
    }

    /// #501: the shell's start-up takes back a quiet wait it found; a drain the patron asked for
    /// between that look and the take (it replaces the wait) is kept, and so is the queue's hold.
    #[tokio::test(start_paused = true)]
    async fn quiet_cancel_takes_back_only_a_quiet_wait() {
        use std::sync::atomic::Ordering::SeqCst;
        let life = Life::new(GRACE);
        let _c = life.enter();
        let g = Arc::new(Gauge { busy: true.into(), ..Default::default() });
        life.drain(g.clone(), true);
        assert!(life.quiet_cancel(&*g) && !life.draining() && !g.on.load(SeqCst));
        assert!(!life.quiet_cancel(&*g), "nothing runs: nothing to take");

        // The look saw a quiet wait; the patron's drain comes in before the take.
        life.drain(g.clone(), true);
        assert!(life.waiting_quiet());
        life.drain(g.clone(), false);
        assert!(!life.quiet_cancel(&*g), "a drain is not taken back");
        assert!(life.draining() && !life.waiting_quiet() && g.on.load(SeqCst), "the drain holds the queue");
        let l = life.clone();
        let waiter = tokio::spawn(async move { l.wait().await });
        g.busy.store(false, SeqCst);
        tokio::time::sleep(DRAIN_POLL * 2 + Duration::from_millis(10)).await;
        assert_eq!(waiter.await.unwrap(), Exit::Restart, "the patron's restart happens");
    }

    #[test]
    fn stage_copies_next_to_this_exe_under_the_shells_name() {
        let d = dir("stage");
        let src = d.join("dexd-build.exe");
        std::fs::write(&src, b"new build").unwrap();
        let dst = stage(&src).unwrap();
        assert_eq!(dst.parent(), std::env::current_exe().unwrap().parent());
        assert_eq!(dst.file_name().unwrap().to_string_lossy(), copy_name(&std::fs::metadata(&src).unwrap()));
        assert_eq!(std::fs::read(&dst).unwrap(), b"new build");
        assert_eq!(stage(&src).unwrap(), dst, "once per build");
        assert!(stage(&d).is_err(), "a folder is not a build");
        let _ = std::fs::remove_file(&dst);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dexd-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discovery_round_trips_and_only_its_writer_removes_it() {
        let d = dir("disc");
        let disc = Discovery::write(&d, 4242, "secret-token").unwrap();
        assert_eq!(Discovery::read(&d).unwrap(), disc);
        assert_eq!(disc.url, "ws://127.0.0.1:4242");
        assert_eq!(std::fs::read_to_string(&disc.token_file).unwrap(), "secret-token");
        let text = std::fs::read_to_string(d.join(DISCOVERY_FILE)).unwrap();
        assert!(!text.contains("secret-token"), "{text}");
        // Another instance's file stays.
        let other = Discovery { pid: disc.pid + 1, ..disc.clone() };
        other.remove(&d);
        assert!(Discovery::read(&d).is_some());
        disc.remove(&d);
        assert!(Discovery::read(&d).is_none() && !disc.token_file.exists());
        // Rewrite over an existing file (a restart).
        Discovery::write(&d, 1, "a").unwrap();
        Discovery::write(&d, 2, "b").unwrap();
        assert_eq!(Discovery::read(&d).unwrap().port, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Owner-only: a protected DACL with exactly one ACE, for this user.
    #[cfg(windows)]
    #[test]
    fn private_files_are_this_users_only() {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::*;
        use std::os::windows::ffi::OsStrExt;
        let d = dir("acl");
        let p = d.join("x.token");
        write_private(&p, b"t").unwrap();
        write_private(&p, b"t2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"t2");
        let w: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            let (mut dacl, mut sd): (*mut ACL, PSECURITY_DESCRIPTOR) = (std::ptr::null_mut(), std::ptr::null_mut());
            let r = GetNamedSecurityInfoW(w.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd);
            assert_eq!(r, 0);
            let (mut control, mut rev) = (0u16, 0u32);
            assert_ne!(GetSecurityDescriptorControl(sd, &mut control, &mut rev), 0);
            assert_ne!(control & SE_DACL_PROTECTED, 0, "inherited ACEs must not apply");
            let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
            assert_ne!(GetAclInformation(dacl, (&mut info as *mut ACL_SIZE_INFORMATION).cast(), std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation), 0);
            assert_eq!(info.AceCount, 1);
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            assert_ne!(GetAce(dacl, 0, &mut ace), 0);
            let ace = ace as *const ACCESS_ALLOWED_ACE;
            assert_eq!((*ace).Header.AceType, 0, "ACCESS_ALLOWED_ACE_TYPE");
            let sid = std::ptr::addr_of!((*ace).SidStart) as PSID;
            assert_eq!(win::sid_string(sid).unwrap(), win::user_sid().unwrap());
            LocalFree(sd as _);
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
