//! One WebSocket endpoint. A connection must authenticate with its first frame (`auth.hello`
//! with a device token, or `auth.pair` with a one-time code) within [`AUTH_TIMEOUT`]; anything else
//! closes it (4001). After that: requests run concurrently on the blocking pool (the core's calls
//! block), every core event is broadcast, PTY bytes flow on per-connection channels.
//!
//! PTYs belong to the server, not to a connection: a dropped connection only silences its sinks,
//! the processes keep running and another connection can `pty.attach` to them.

use dex_core::pty::Spawn;
use dex_core::{Core, EventSink, Events, Paths, PtySink};
use futures_util::{SinkExt, StreamExt};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request as HttpRequest, Response};
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{Auth, Device, LocalToken};
use crate::fs::Roots;
use crate::local::{Exit, Life};
use crate::methods;
use crate::protocol::{self, arg, Error, Request, PROTOCOL};
use crate::sessions::Sessions;
use crate::tailnet::TailnetState;

pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const PREAUTH_LIMIT: usize = 32;
const MAX_WS_FRAME: usize = 1024 * 1024;
const MAX_WS_MESSAGE: usize = 1024 * 1024;
const PTY_INPUT_LIMIT: usize = 16;
const REMOTE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
/// How soon a `dexd devices revoke` (another process) cuts an open connection.
pub const RECHECK: Duration = Duration::from_secs(15);
/// Close codes (4000–4999 are the application's).
pub const CLOSE_UNAUTHORIZED: u16 = 4001;
pub const CLOSE_REVOKED: u16 = 4003;

#[cfg(test)]
type RequestHook = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Server {
    pub core: Arc<Core>,
    pub auth: Arc<Auth>,
    bus: broadcast::Sender<Arc<str>>,
    sessions: Sessions,
    /// S4: folders a client may browse and put a team in.
    pub roots: Roots,
    /// #146 (A0): the desktop's loopback endpoint — only its own webview (this token), only from
    /// 127.0.0.1/::1; no pairing, no device tokens, no root check (the desktop's folder picker).
    local: Option<LocalToken>,
    /// #150 (B): `dexd --local` — connected windows keep it alive; `core_shutdown`/`core_restart`.
    life: Option<Arc<Life>>,
    /// The tailnet listener this local core owns (Settings → Ağ erişimi; local endpoint only).
    tailnet: OnceLock<Arc<TailnetState>>,
    /// One endpoint's lifetime; the desktop and tailnet use separate Server instances.
    shutdown: watch::Sender<bool>,
    #[cfg(test)]
    pub(crate) request_hook: Mutex<Option<RequestHook>>,
}

/// Core events → every authenticated connection (serialized once).
struct Bus(broadcast::Sender<Arc<str>>);

/// #150: the local core's events → its own connections and its tailnet listener's.
struct LocalBus(broadcast::Sender<Arc<str>>, Arc<TailnetState>);

impl EventSink for LocalBus {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        self.1.publish(event, payload.clone());
        let _ = self.0.send(protocol::event(event, &payload).into());
        Ok(())
    }
}

impl EventSink for Bus {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        // No connection listening is not an error (the desktop's emit doesn't fail either).
        let _ = self.0.send(protocol::event(event, &payload).into());
        Ok(())
    }
}

impl Server {
    /// Starts the core in `paths` and opens the device store `<data_dir>/dexd.db`.
    /// FIX-5: the server never seeds a default team — it starts empty, clients create teams.
    pub fn start(paths: Paths, roots: Roots) -> Result<Arc<Self>, String> {
        let auth = Arc::new(Auth::open(&paths.data_dir.join("dexd.db"))?);
        let (bus, _) = broadcast::channel(1024);
        let core = Core::start_bare(paths, Events::new(Bus(bus.clone())))?;
        Ok(Arc::new(Self {
            core,
            auth,
            bus,
            sessions: Sessions,
            roots,
            local: None,
            life: None,
            tailnet: OnceLock::new(),
            shutdown: watch::channel(false).0,
            #[cfg(test)]
            request_hook: Mutex::default(),
        }))
    }

    /// #150 (B): `dexd --local` — the desktop's own core (seeded, like the app's), its token (for
    /// the discovery file) and the tailnet listener it publishes events to.
    pub fn start_local(paths: Paths, tailnet: Arc<TailnetState>, life: Arc<Life>) -> Result<(Arc<Self>, String), String> {
        let (verifier, token) = LocalToken::new()?;
        let (bus, _) = broadcast::channel(1024);
        let core = Core::start(paths, Events::new(LocalBus(bus.clone(), tailnet.clone())))?;
        let srv = Self {
            core,
            auth: Arc::new(Auth::open(std::path::Path::new(":memory:"))?),
            bus,
            sessions: Sessions,
            roots: Roots::new(&[crate::fs::default_root()])?,
            local: Some(verifier),
            life: Some(life),
            tailnet: OnceLock::from(tailnet),
            shutdown: watch::channel(false).0,
            #[cfg(test)]
            request_hook: Mutex::default(),
        };
        Ok((Arc::new(srv), token))
    }

    /// The in-app core (`inproc-core`) hands its tailnet listener to its local endpoint.
    pub fn set_tailnet(&self, tailnet: Arc<TailnetState>) {
        let _ = self.tailnet.set(tailnet);
    }

    /// Serves an already-running host core. The caller remains responsible for its lifecycle and
    /// must forward its core events through [`Server::publish_event`].
    pub fn from_core(
        core: Arc<Core>,
        auth_path: impl AsRef<std::path::Path>,
        roots: Roots,
    ) -> Result<Arc<Self>, String> {
        let auth = Arc::new(Auth::open(auth_path.as_ref())?);
        let (bus, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            core,
            auth,
            bus,
            sessions: Sessions,
            roots,
            local: None,
            life: None,
            tailnet: OnceLock::new(),
            shutdown: watch::channel(false).0,
            #[cfg(test)]
            request_hook: Mutex::default(),
        }))
    }

    /// #146 (A0): the desktop's in-process core for its own webview. Like [`Server::from_core`] (the
    /// caller owns the core and forwards its events), but the only credential is `token`.
    pub fn local(core: Arc<Core>, token: LocalToken) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            core,
            // Never consulted (`auth_request` takes the local branch); in memory, no file.
            auth: Arc::new(Auth::open(std::path::Path::new(":memory:"))?),
            bus: broadcast::channel(1024).0,
            sessions: Sessions,
            roots: Roots::new(&[crate::fs::default_root()])?,
            local: Some(token),
            life: None,
            tailnet: OnceLock::new(),
            shutdown: watch::channel(false).0,
            #[cfg(test)]
            request_hook: Mutex::default(),
        }))
    }

    /// A local endpoint talks to this machine only (it binds 127.0.0.1 too; this is the backstop).
    pub fn admits(&self, peer: std::net::IpAddr) -> bool {
        self.local.is_none() || peer.is_loopback()
    }

    /// #192: a local endpoint's handshake must come from this app's webview or from no browser at
    /// all; any other page the user opens could otherwise reach 127.0.0.1 too (the token still
    /// guards it — this is the second wall). A device endpoint takes any Origin (no cookies: CSWSH
    /// has nothing to ride on, and the phone's webview is not ours to list).
    fn admits_origin(&self, origin: Option<&[u8]>) -> bool {
        self.local.is_none() || local_origin_ok(origin)
    }

    /// For hosts whose event sink is owned elsewhere (the desktop shell).
    pub fn publish_event(&self, event: &str, payload: Value) {
        let _ = self.bus.send(protocol::event(event, &payload).into());
    }

    /// Stop admitting work and disconnect every socket owned by this endpoint. Await `serve`
    /// afterwards to wait for socket cleanup. Remote blocking requests get a bounded drain.
    pub fn stop(&self) {
        self.shutdown.send_replace(true);
    }
}

#[cfg(test)]
#[test]
fn from_core_keeps_the_callers_core() {
    struct NullSink;
    impl EventSink for NullSink {
        fn emit(&self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
    }
    let root = std::env::temp_dir().join(format!("dexd-existing-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let core = Core::start_bare(
        Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        },
        Events::new(NullSink),
    )
    .unwrap();
    let roots = Roots::new(&[root.join("projects")]).unwrap();

    let server = Server::from_core(core.clone(), root.join("data").join("dexd.db"), roots).unwrap();

    assert!(Arc::ptr_eq(&core, &server.core));
    core.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

/// The desktop webview's origins: macOS/Linux, Windows/Android (WebView2's custom-protocol host),
/// `tauri dev` (vite, tauri.conf.json `devUrl`).
pub const LOCAL_ORIGINS: &[&str] = &["tauri://localhost", "http://tauri.localhost", "http://localhost:1420"];

/// No Origin = not a browser (a native client); otherwise exactly one of [`LOCAL_ORIGINS`].
pub fn local_origin_ok(origin: Option<&[u8]>) -> bool {
    origin.is_none_or(|o| LOCAL_ORIGINS.iter().any(|a| a.as_bytes() == o))
}

async fn cancelled(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stopped| *stopped).await;
}

pub async fn serve(listener: TcpListener, srv: Arc<Server>) {
    let mut shutdown = srv.shutdown.subscribe();
    let mut connections = JoinSet::new();
    let preauth = Arc::new(Semaphore::new(PREAUTH_LIMIT));
    loop {
        let accepted = tokio::select! {
            biased;
            _ = cancelled(&mut shutdown) => break,
            _ = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(x) => x,
            Err(e) => {
                eprintln!("dexd: accept: {e}");
                continue;
            }
        };
        if !srv.admits(peer.ip()) {
            eprintln!("dexd: {peer}: yerel uç yalnız loopback kabul eder");
            continue;
        }
        // Do not queue overflow sockets or spawn a task for them.
        let Ok(permit) = preauth.clone().try_acquire_owned() else { continue };
        let srv = srv.clone();
        let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;
        connections.spawn(async move {
            if let Err(e) = connection(stream, srv, deadline, permit).await {
                eprintln!("dexd: {peer}: {e}");
            }
        });
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

type Tx = mpsc::UnboundedSender<Message>;

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code: CloseCode::from(code),
        reason: reason.to_string().into(),
    }))
}

async fn connection(stream: TcpStream, srv: Arc<Server>, deadline: tokio::time::Instant, permit: OwnedSemaphorePermit) -> Result<(), String> {
    let mut shutdown = srv.shutdown.subscribe();
    let _ = stream.set_nodelay(true);
    let origin_ok = |req: &HttpRequest, resp: Response| {
        if srv.admits_origin(req.headers().get(ORIGIN).map(|v| v.as_bytes())) {
            return Ok(resp);
        }
        let mut r = ErrorResponse::new(Some("origin".into()));
        *r.status_mut() = StatusCode::FORBIDDEN;
        Err(r)
    };
    let config = WebSocketConfig::default()
        .max_frame_size(Some(MAX_WS_FRAME))
        .max_message_size(Some(MAX_WS_MESSAGE));
    let ws = tokio::select! {
        biased;
        _ = cancelled(&mut shutdown) => return Ok(()),
        ws = tokio::time::timeout_at(deadline, tokio_tungstenite::accept_hdr_async_with_config(stream, origin_ok, Some(config))) => {
            ws.map_err(|_| "HTTP/WS el sıkışması zaman aşımı".to_string())?.map_err(|e| e.to_string())?
        },
    };
    let (mut out, mut inp) = ws.split();
    // One writer per socket; PTY sinks (reader threads) and request tasks all send through `tx`.
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    // Only the desktop drains admitted replies (including shutdown/restart). Remote cancellation
    // bypasses the output queue, even while a request or a socket write is blocked.
    let remote = srv.local.is_none();
    let mut writer_shutdown = shutdown.clone();
    let mut writer = JoinSet::new();
    writer.spawn(async move {
        loop {
            let m = tokio::select! {
                biased;
                _ = cancelled(&mut writer_shutdown), if remote => break,
                m = rx.recv() => match m { Some(m) => m, None => break },
            };
            let last = matches!(m, Message::Close(_));
            let sent = tokio::select! {
                biased;
                _ = cancelled(&mut writer_shutdown), if remote => break,
                sent = out.send(m) => sent,
            };
            if sent.is_err() || last {
                break;
            }
        }
        rx.close();
        if remote && *writer_shutdown.borrow() {
            let _ = out.send(Message::Close(None)).await;
        }
    });

    let auth = tokio::select! {
        biased;
        _ = cancelled(&mut shutdown) => None,
        auth = tokio::time::timeout_at(deadline, authenticate(&mut inp, &tx, &srv)) => auth.ok().flatten(),
    };
    let (device, probe) = match auth {
        Some(d) => d,
        None => {
            let _ = tx.send(close_frame(CLOSE_UNAUTHORIZED, "unauthorized"));
            finish_writer(writer).await;
            return Ok(());
        }
    };
    drop(permit);

    let conn = Arc::new(Conn {
        tx: tx.clone(),
        chans: Arc::default(),
        // #62: the desktop's own view stays the `local` size client it was over Tauri.
        client: if srv.local.is_some() { "local".into() } else { format!("ws{}", NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)) },
        view_client: format!("view{}", NEXT_CLIENT.fetch_add(1, Ordering::Relaxed)),
        view_state: Mutex::new(ViewState::default()),
        closed: AtomicBool::new(false),
    });
    // #150: a window keeps `dexd --local` alive while connected; a probe does not.
    let _client = srv.life.as_ref().filter(|_| !probe).map(|l| l.enter());
    let mut events = srv.bus.subscribe();
    let mut requests = JoinSet::new();
    let (input, input_worker) = pty_input_queue(&srv, &conn, &mut requests);
    let mut recheck = tokio::time::interval(RECHECK);
    recheck.tick().await;
    loop {
        if *shutdown.borrow() { break; }
        tokio::select! {
            biased;
            _ = cancelled(&mut shutdown) => break,
            _ = requests.join_next(), if !requests.is_empty() => {},
            msg = inp.next() => match msg {
                Some(Ok(Message::Text(t))) => on_text(&srv, &conn, t.as_str(), &mut requests),
                Some(Ok(Message::Binary(b))) => {
                    if !on_binary(&conn, &input, &b) {
                        // Never silently drop keystrokes or grow an unbounded input backlog.
                        let _ = tx.send(close_frame(1008, "PTY input queue full"));
                        break;
                    }
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            ev = events.recv() => match ev {
                Ok(text) => { let _ = tx.send(Message::Text(text.as_ref().into())); }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // The client missed events: it should refetch what it shows.
                    let _ = tx.send(Message::Text(protocol::event("dexd.lagged", &json!({ "missed": n })).into()));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = recheck.tick(), if srv.local.is_none() => {
                let (auth, id) = (srv.auth.clone(), device.id.clone());
                let exists = tokio::select! {
                    biased;
                    _ = cancelled(&mut shutdown) => break,
                    exists = tokio::task::spawn_blocking(move || auth.exists(&id)) => exists.unwrap_or(true),
                };
                if !exists {
                    let _ = tx.send(close_frame(CLOSE_REVOKED, "revoked"));
                    break;
                }
            }
        }
    }
    // Drop queued input independently of socket/reply cleanup. Aborting this async worker cannot
    // interrupt a write already running on the blocking pool, but it stops every subsequent one.
    drop(input);
    input_worker.abort();
    drop(inp);
    if remote {
        // Mute first; a late spawn/attach sees the closed state and cannot reopen a channel.
        let channels = conn.disconnect();
        let _ = tx.send(Message::Close(None));
        finish_writer(writer).await;
        // The socket is gone. Core locks and blocking handlers cannot delay Close or keep the
        // tailnet lifecycle waiting indefinitely. Running blocking work cannot be aborted, so
        // handlers also clean up any attachment/size claim they finish after this deadline.
        requests.spawn_blocking(move || {
            for c in channels { srv.sessions.detach(&srv.core, &c.id, c.subscriber); }
            srv.core.ptys.release_client(&conn.client);
            conn.release_view(&srv.core);
        });
        let _ = tokio::time::timeout(REMOTE_CLEANUP_TIMEOUT, async {
            while requests.join_next().await.is_some() {}
        }).await;
    } else {
        while requests.join_next().await.is_some() {}
        for c in conn.disconnect() { srv.sessions.detach(&srv.core, &c.id, c.subscriber); }
        srv.core.ptys.release_client(&conn.client);
        conn.release_view(&srv.core);
        let _ = tx.send(Message::Close(None));
        finish_writer(writer).await;
    }
    Ok(())
}

async fn finish_writer(mut writer: JoinSet<()>) {
    // A peer that no longer reads must not hold disconnect cleanup indefinitely.
    if tokio::time::timeout(Duration::from_secs(1), writer.join_next()).await.is_err() {
        writer.abort_all();
        let _ = writer.join_next().await;
    }
}

/// The first text frame decides. `None` = refused (the reply says why before the close). The flag:
/// a local `probe` hello (the shell checking the core is up — not a window).
async fn authenticate(
    inp: &mut (impl futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
              + Unpin),
    tx: &Tx,
    srv: &Arc<Server>,
) -> Option<(Device, bool)> {
    let text = loop {
        match inp.next().await?.ok()? {
            Message::Text(t) => break t,
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return None,
        }
    };
    let req: Request = match serde_json::from_str(text.as_str()) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Message::Text(
                protocol::reply(&Value::Null, Err(Error::bad_request(e.to_string()))).into(),
            ));
            return None;
        }
    };
    let srv2 = srv.clone();
    let id = req.id.clone();
    let probe = srv.local.is_some() && req.params.get("probe").and_then(Value::as_bool).unwrap_or(false);
    let r = tokio::task::spawn_blocking(move || auth_request(&srv2.auth, srv2.local.as_ref(), &req))
        .await
        .unwrap_or_else(|e| Err(Error::new("internal", e.to_string())));
    let _ = tx.send(Message::Text(
        protocol::reply(
            &id,
            r.as_ref().map(|(v, _)| v.clone()).map_err(Clone::clone),
        )
        .into(),
    ));
    r.ok().map(|(_, d)| (d, probe))
}

fn auth_request(auth: &Auth, local: Option<&LocalToken>, req: &Request) -> Result<(Value, Device), Error> {
    let p = &req.params;
    if !matches!(req.method.as_str(), "auth.hello" | "auth.pair") {
        return Err(Error::new(
            "unauthorized",
            "önce auth.hello ya da auth.pair",
        ));
    }
    let client: u32 = arg(p, "protocol")?;
    if client != PROTOCOL {
        return Err(Error::new(
            "protocol",
            format!("protokol sürümü uyuşmuyor: sunucu {PROTOCOL}, istemci {client}"),
        ));
    }
    let hello = |device: &Device| {
        let mut v = server_info(local.is_some());
        v["device"] = json!(device);
        v
    };
    if let Some(local) = local {
        if req.method != "auth.hello" || !local.verify(&arg::<String>(p, "token")?) {
            return Err(Error::new("unauthorized", "yerel uç jetonu geçersiz"));
        }
        let device = Device { id: "local".into(), name: "yerel".into(), created_at: 0, last_seen: None };
        return Ok((hello(&device), device));
    }
    if req.method == "auth.pair" {
        let (device, token) = auth
            .pair(
                &arg::<String>(p, "code")?,
                &arg::<Option<String>>(p, "name")?.unwrap_or_default(),
            )
            .map_err(|e| Error::new("unauthorized", e))?;
        let mut v = hello(&device);
        v["token"] = token.into();
        return Ok((v, device));
    }
    let device = auth
        .verify(&arg::<String>(p, "token")?)
        .ok_or_else(|| Error::new("unauthorized", "cihaz jetonu geçersiz ya da iptal edilmiş"))?;
    Ok((hello(&device), device))
}

/// Methods `handle` serves itself (the connection's PTY channels, the roots); the rest are
/// `methods::METHODS`.
const SERVER_METHODS: [&str; 9] = ["pty_spawn", "pty.list", "pty.attach", "pty.detach", "pty_resize", "fs.list", "fs.mkdir", "server_info", "team_viewed"];
/// #150: also served by `handle`, on a local endpoint only (listed there only): the desktop's
/// tailnet listener and the core's own lifetime (the tray). A paired device gets `local_only`.
pub const LOCAL_METHODS: [&str; 10] = [
    "tailnet_get",
    "tailnet_set",
    "tailnet_pair_code",
    "tailnet_devices",
    "tailnet_device_revoke",
    "core_shutdown",
    "core_restart",
    "core_drain_cancel",
    "core_quiet_cancel",
    "core_life",
];

/// This build: in `auth.hello` / `auth.pair` and `server_info`. Additive (protocol stays 1): an
/// older client ignores it, a newer one hides what an older dexd lacks (`methods`) and shows how
/// old it is (`commit`, `commitTime` = unix secs, 0 unknown — crates/dex-core/build_commit.rs).
pub fn server_info(local: bool) -> Value {
    let own: &[&str] = if local { &LOCAL_METHODS } else { &[] };
    let methods: Vec<&str> = SERVER_METHODS.iter().chain(own).chain(methods::METHODS).copied().collect();
    json!({
        "protocol": PROTOCOL,
        "server": "dexd",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": env!("DEX_COMMIT"),
        "commitTime": env!("DEX_COMMIT_TIME").parse::<i64>().unwrap_or(0),
        "methods": methods,
        // #400: `core_restart {whenQuiet}` known — the shell's dev update needs it.
        "restartWhenQuiet": local,
    })
}

struct Chan {
    id: String,
    alive: Arc<AtomicBool>,
    subscriber: Option<u64>,
}

type Chans = Arc<Mutex<HashMap<u32, Chan>>>;

/// #62: a connection is one PTY-size client (`ws<n>`, unique next to the desktop's `local`).
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);
static NEXT_VIEW_EVENT: AtomicU64 = AtomicU64::new(1);

struct ViewState {
    connected: bool,
    latest: HashMap<String, u64>,
}

impl Default for ViewState {
    fn default() -> Self { Self { connected:true, latest:HashMap::new() } }
}

struct Conn {
    closed: AtomicBool,
    view_client: String,
    view_state: Mutex<ViewState>,
    tx: Tx,
    /// Channel id (the client's choice) → PTY id.
    chans: Chans,
    client: String,
}

impl Conn {
    fn disconnect(&self) -> Vec<Chan> {
        let mut chans = self.chans.lock().unwrap();
        self.closed.store(true, Ordering::Relaxed);
        chans.drain().map(|(_, c)| {
            c.alive.store(false, Ordering::Relaxed);
            c
        }).collect()
    }

    fn release_view(&self, core: &Core) {
        let mut state = self.view_state.lock().unwrap();
        state.connected = false;
        core.team_view_release(&self.view_client);
    }

    fn team_view(&self, core: &Core, p: &Value, seq: u64) -> Result<Value, Error> {
        let team: String = arg(p, "teamId")?;
        let viewed: bool = arg(p, "viewed")?;
        let mut state = self.view_state.lock().unwrap();
        if !state.connected || self.closed.load(Ordering::Relaxed) { return Err(Error::bad_request("bağlantı kapalı")); }
        // Blocking handlers can finish out of arrival order; stale visibility never wins.
        if state.latest.get(&team).is_some_and(|last| *last >= seq) { return Ok(Value::Null); }
        core.team_viewed_for(&team, &self.view_client, viewed).map_err(Error::failed)?;
        state.latest.insert(team, seq);
        Ok(Value::Null)
    }
    fn send(&self, text: String) {
        if !self.closed.load(Ordering::Relaxed) { let _ = self.tx.send(Message::Text(text.into())); }
    }

    /// Registered before the PTY can produce a byte; the client picked the id, so output that
    /// races the spawn reply still lands on a known channel.
    fn open(&self, channel: u32, id: &str) -> Result<WsSink, Error> {
        if channel == 0 {
            return Err(Error::bad_request("kanal 0 ayrılmış"));
        }
        let alive = Arc::new(AtomicBool::new(true));
        let mut chans = self.chans.lock().unwrap();
        if self.closed.load(Ordering::Relaxed) { return Err(Error::bad_request("bağlantı kapalı")); }
        if chans.contains_key(&channel) {
            return Err(Error::bad_request(format!("kanal {channel} kullanımda")));
        }
        chans.insert(
            channel,
            Chan {
                id: id.to_string(),
                alive: alive.clone(),
                subscriber: None,
            },
        );
        Ok(WsSink {
            tx: self.tx.clone(),
            channel,
            id: id.to_string(),
            alive,
            chans: Arc::downgrade(&self.chans),
        })
    }

    fn close(&self, channel: u32) -> Option<Chan> {
        let c = self.chans.lock().unwrap().remove(&channel)?;
        c.alive.store(false, Ordering::Relaxed);
        Some(c)
    }

    fn set_subscriber(&self, core: &Core, channel: u32, id: &str, subscriber: u64) {
        let mut chans = self.chans.lock().unwrap();
        if let Some(c) = chans.get_mut(&channel) {
            c.subscriber = Some(subscriber);
        } else {
            // attach can have been inside the core when disconnect drained the channels.
            drop(chans);
            core.ptys.detach(id, subscriber);
        }
    }
}

/// A PTY subscriber on one connection channel. `alive = false` (detach, disconnect) makes the
/// stream drop it on the next write.
struct WsSink {
    tx: Tx,
    channel: u32,
    id: String,
    alive: Arc<AtomicBool>,
    chans: Weak<Mutex<HashMap<u32, Chan>>>,
}

impl PtySink for WsSink {
    fn data(&mut self, bytes: &[u8]) -> bool {
        self.alive.load(Ordering::Relaxed)
            && self
                .tx
                .send(Message::Binary(
                    protocol::pty_frame(self.channel, bytes).into(),
                ))
                .is_ok()
    }

    /// Same socket as the bytes, so the view resizes before the redraw that follows.
    fn resized(&mut self, cols: u16, rows: u16) {
        if self.alive.load(Ordering::Relaxed) {
            let payload = json!({ "channel": self.channel, "id": self.id, "cols": cols, "rows": rows });
            let _ = self.tx.send(Message::Text(protocol::event("pty.size", &payload).into()));
        }
    }

    fn exit(&mut self) {
        if !self.alive.swap(false, Ordering::Relaxed) {
            return;
        }
        if let Some(chans) = self.chans.upgrade() {
            let mut chans = chans.lock().unwrap();
            if chans
                .get(&self.channel)
                .is_some_and(|c| Arc::ptr_eq(&c.alive, &self.alive))
            {
                chans.remove(&self.channel);
            }
        }
        let _ = self.tx.send(Message::Text(
            protocol::event(
                "pty.exit",
                &json!({ "channel": self.channel, "id": self.id }),
            )
            .into(),
        ));
    }
}

fn on_text(srv: &Arc<Server>, conn: &Arc<Conn>, text: &str, requests: &mut JoinSet<()>) {
    let req: Request = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return conn.send(protocol::reply(
                &Value::Null,
                Err(Error::bad_request(e.to_string())),
            ))
        }
    };
    let view_seq = (req.method == "team_viewed").then(|| NEXT_VIEW_EVENT.fetch_add(1, Ordering::Relaxed));
    let shutdown = srv.shutdown.subscribe();
    let (srv, conn) = (srv.clone(), conn.clone());
    requests.spawn(async move {
        let id = req.id.clone();
        let c = conn.clone();
        let r = tokio::task::spawn_blocking(move || {
            if *shutdown.borrow() || c.closed.load(Ordering::Relaxed) { return Err(Error::bad_request("bağlantı kapalı")); }
            #[cfg(test)]
            {
                let hook = srv.request_hook.lock().unwrap().clone();
                if let Some(hook) = hook { hook(&req.method); }
            }
            let r = match view_seq {
                Some(seq) => c.team_view(&srv.core, &req.params, seq),
                None => handle(&srv, &c, &req),
            };
            if c.closed.load(Ordering::Relaxed) {
                srv.core.ptys.release_client(&c.client);
                c.release_view(&srv.core);
            }
            r
        })
            .await
            .unwrap_or_else(|e| Err(Error::new("internal", e.to_string())));
        conn.send(protocol::reply(&id, r));
    });
}

type PtyInput = (String, Vec<u8>);

/// One blocking write at a time per connection, with no idle blocking-pool thread.
fn pty_input_queue(srv: &Arc<Server>, conn: &Arc<Conn>, requests: &mut JoinSet<()>) -> (mpsc::Sender<PtyInput>, tokio::task::AbortHandle) {
    let (tx, mut rx) = mpsc::channel::<PtyInput>(PTY_INPUT_LIMIT);
    let (srv, conn) = (srv.clone(), conn.clone());
    let worker = requests.spawn(async move {
        while let Some((id, bytes)) = rx.recv().await {
            let (srv, conn) = (srv.clone(), conn.clone());
            let shutdown = srv.shutdown.subscribe();
            if tokio::task::spawn_blocking(move || {
                if *shutdown.borrow() || conn.closed.load(Ordering::Relaxed) { return; }
                #[cfg(test)]
                {
                    let hook = srv.request_hook.lock().unwrap().clone();
                    if let Some(hook) = hook { hook("pty.write"); }
                }
                let _ = srv.core.ptys.write(&id, &bytes);
            }).await.is_err() { break; }
        }
    });
    (tx, worker)
}

/// Resolve the channel when input arrives; enqueue without blocking the socket's cancellation.
fn on_binary(conn: &Conn, input: &mpsc::Sender<PtyInput>, frame: &[u8]) -> bool {
    let Some((channel, bytes)) = protocol::parse_pty_frame(frame) else {
        return true;
    };
    let id = conn
        .chans
        .lock()
        .unwrap()
        .get(&channel)
        .map(|c| c.id.clone());
    if let Some(id) = id {
        return input.try_send((id, bytes.to_vec())).is_ok();
    }
    true
}

fn handle(srv: &Server, conn: &Conn, req: &Request) -> Result<Value, Error> {
    let p = &req.params;
    match req.method.as_str() {
        "team_viewed" => {
            conn.team_view(&srv.core, p, NEXT_VIEW_EVENT.fetch_add(1, Ordering::Relaxed))
        }
        "pty_spawn" => {
            let id: String = arg(p, "id")?;
            let spawn = Spawn {
                id: id.clone(),
                cmd: arg(p, "cmd")?,
                args: arg::<Option<Vec<String>>>(p, "args")?.unwrap_or_default(),
                cwd: arg(p, "cwd")?,
                memory_dir: arg(p, "memoryDir")?,
                provider: arg(p, "provider")?,
                cols: arg(p, "cols")?,
                rows: arg(p, "rows")?,
                env: Vec::new(),
            };
            let channel = arg(p, "channel")?;
            let sink = conn.open(channel, &id)?;
            srv.core.pty_spawn(spawn, sink).map_err(|e| {
                conn.close(channel);
                Error::failed(e)
            })?;
            srv.sessions.spawned(&id);
            Ok(Value::Null)
        }
        "pty.list" => serde_json::to_value(srv.sessions.list(&srv.core))
            .map_err(|e| Error::failed(e.to_string())),
        "pty.attach" => {
            let (id, channel): (String, u32) = (arg(p, "id")?, arg(p, "channel")?);
            let sink = conn.open(channel, &id)?;
            let attachment = srv.sessions.attach(&srv.core, &id, sink).map_err(|e| {
                conn.close(channel);
                Error::failed(e)
            })?;
            conn.set_subscriber(&srv.core, channel, &id, attachment.subscriber);
            serde_json::to_value(attachment).map_err(|e| Error::failed(e.to_string()))
        }
        "pty.detach" => {
            if let Some(c) = conn.close(arg(p, "channel")?) {
                srv.sessions.detach(&srv.core, &c.id, c.subscriber);
                if !conn.chans.lock().unwrap().values().any(|o| o.id == c.id) {
                    srv.core.ptys.release(&c.id, &conn.client);
                }
            }
            Ok(Value::Null)
        }
        "pty_resize" => {
            let claim = arg::<Option<bool>>(p, "claim")?.unwrap_or(false);
            srv.core
                .ptys
                .resize(&arg::<String>(p, "id")?, &conn.client, arg(p, "cols")?, arg(p, "rows")?, claim)
                .map_err(Error::failed)?;
            Ok(Value::Null)
        }
        "fs.list" => serde_json::to_value(
            request_roots(srv, p)?
                .list(arg::<Option<String>>(p, "path")?.as_deref())
                .map_err(Error::failed)?,
        )
        .map_err(|e| Error::failed(e.to_string())),
        "fs.mkdir" => Ok(request_roots(srv, p)?
            .mkdir(&arg::<String>(p, "parent")?, &arg::<String>(p, "name")?)
            .map_err(Error::failed)?
            .into()),
        "server_info" => Ok(server_info(srv.local.is_some())),
        "tailnet_get" => Ok(json!(tailnet(srv)?.view())),
        "tailnet_set" => {
            let (t, core) = (tailnet(srv)?, srv.core.clone());
            let (enabled, port) = (arg(p, "enabled")?, arg(p, "port")?);
            let view = tokio::runtime::Handle::current().block_on(t.set(core, enabled, port)).map_err(Error::failed)?;
            Ok(json!(view))
        }
        "tailnet_pair_code" => Ok(tailnet(srv)?.pair_code().map_err(Error::failed)?.into()),
        "tailnet_devices" => Ok(json!(tailnet(srv)?.devices().map_err(Error::failed)?)),
        "tailnet_device_revoke" => Ok(tailnet(srv)?.revoke(&arg::<String>(p, "id")?).map_err(Error::failed)?.into()),
        "core_shutdown" => {
            life(srv)?.request(Exit::Shutdown);
            Ok(Value::Null)
        }
        // #161: `exe` — come back as that build (an update); `whenIdle` — drain first. #400:
        // `whenQuiet` (sent with `whenIdle`, which an older dexd keeps to) — hold nothing, wait.
        "core_restart" => {
            let l = life(srv)?;
            if let Some(src) = p.get("exe").and_then(Value::as_str) {
                l.set_next_exe(crate::local::stage(std::path::Path::new(src)).map_err(Error::failed)?);
            }
            if p.get("whenIdle").and_then(Value::as_bool) == Some(true) {
                let quiet = p.get("whenQuiet").and_then(Value::as_bool) == Some(true);
                let _rt = tokio::runtime::Handle::current().enter();
                l.drain(srv.core.clone(), quiet);
                return Ok(json!({ "draining": !l.waiting_quiet(), "updatePending": l.waiting_quiet() }));
            }
            l.request(Exit::Restart);
            Ok(json!({ "restarting": true }))
        }
        "core_drain_cancel" => {
            life(srv)?.drain_cancel(&*srv.core);
            Ok(Value::Null)
        }
        // #501: the shell's dev start-up — a quiet wait only; a drain asked for meanwhile stays.
        "core_quiet_cancel" => Ok(json!({ "cancelled": life(srv)?.quiet_cancel(&*srv.core) })),
        "core_life" => {
            let l = life(srv)?;
            let agents = srv.core.ptys.list().iter().filter(|p| p.alive && p.kind == "agent").count();
            // #400: a quiet wait holds nothing: not `draining`, an update pending.
            let (pending, draining) = (l.waiting_quiet(), l.draining() && !l.waiting_quiet());
            let blockers = if pending {
                srv.core.quiet_blockers().map_err(Error::failed)?
            } else if draining {
                srv.core.drain_blockers().map_err(Error::failed)?
            } else {
                Vec::new()
            };
            Ok(json!({
                "pid": std::process::id(),
                "commit": env!("DEX_COMMIT"),
                "commitTime": env!("DEX_COMMIT_TIME").parse::<i64>().unwrap_or(0),
                "startedAt": l.started_at(),
                "clients": l.clients(),
                "graceSecs": l.grace().as_secs(),
                "shutdownInSecs": l.shutdown_in().map(|d| d.as_secs_f64().ceil() as u64),
                "agents": agents,
                "draining": draining,
                "updatePending": pending,
                "drainBlockers": blockers,
            }))
        }
        "auth.hello" | "auth.pair" => Err(Error::bad_request("bağlantı zaten doğrulandı")),
        m => {
            if srv.local.is_none() {
                let team_id = match m {
                    "project_create" | "project_update" => p.get("teamId").and_then(Value::as_str),
                    "team_update" => p.get("id").and_then(Value::as_str),
                    _ => None,
                };
                let roots = scoped_roots(srv, team_id).map_err(|e| {
                    if e.code=="team_not_found" && matches!(m,"project_create"|"project_update") {
                        Error::new("project_not_found","project_not_found")
                    } else { e }
                })?;
                team_folder_allowed(&roots, m, p)?;
                if m == "team_update" {
                    if let Some(raw) = p.pointer("/patch/workspaceRoot").and_then(Value::as_str) {
                        if roots.check(raw).is_err() {
                            let adoptable = dex_core::db::with(&srv.core.db, |c| dex_core::teams::remote_adoptable_suggestion(c,&srv.core.paths.data_dir,team_id.ok_or("team_not_found")?)).map_err(Error::failed)?;
                            let normalized = dex_core::teams::normalize_dir(raw).map_err(Error::failed)?;
                            if adoptable.as_deref() != Some(normalized.as_str()) {
                                return Err(Error::failed("workspace root izinli klasörlerin dışında"));
                            }
                        }
                    }
                }
            }
            methods::call(&srv.core, m, p)
        }
    }
}

fn local_only() -> Error {
    Error::new("local_only", "yalnız bu makinenin masaüstü uygulaması çağırabilir")
}

fn tailnet(srv: &Server) -> Result<&Arc<TailnetState>, Error> {
    srv.local.as_ref().ok_or_else(local_only)?;
    srv.tailnet.get().ok_or_else(|| Error::failed("ağ erişimi bu çekirdekte yok"))
}

fn life(srv: &Server) -> Result<&Arc<Life>, Error> {
    srv.local.as_ref().ok_or_else(local_only)?;
    srv.life.as_ref().ok_or_else(|| Error::failed("bu çekirdek uygulamanın içinde; uygulamayı kapatın"))
}

/// S4: a remote team's folder is one the client could have picked — inside the roots.
/// #838 G2: a project's folder alike (its branch only names a branch of an already bound root).
fn team_folder_allowed(roots: &Roots, method: &str, p: &Value) -> Result<(), Error> {
    let cwd = match method {
        "team_create" => p.pointer("/team/cwd"),
        "team_update" => p.pointer("/patch/cwd"),
        "project_create" | "project_update" => p.pointer("/root"),
        _ => None,
    };
    match cwd.and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
        Some(c) => roots.check(c).map_err(Error::failed),
        None => Ok(()),
    }
}

fn scoped_roots(srv: &Server, team_id: Option<&str>) -> Result<Roots, Error> {
    let workspace = match team_id {
        Some(id) => dex_core::db::with(&srv.core.db, |c| {
            c.query_row("SELECT workspace_root FROM teams WHERE id=?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .map_err(|e| e.to_string())
        })
        .map_err(Error::failed)?
        .ok_or_else(|| Error::new("team_not_found", "team_not_found"))?,
        None => None,
    };
    srv.roots.with_workspace(workspace.as_deref()).map_err(Error::failed)
}

fn request_roots(srv: &Server, p: &Value) -> Result<Roots, Error> {
    scoped_roots(srv, arg::<Option<String>>(p, "teamId")?.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

    async fn network_fixture(name: &str) -> (Arc<Server>, std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("network-{name}-{}", std::process::id()));
        let core = Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")},Events::new(dex_core::events::NullSink)).unwrap();
        let srv = Server::from_core(core,root.join("auth.db"),Roots::new(&[root]).unwrap()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(listener,srv.clone()));
        (srv,addr,task)
    }

    async fn network_stop(srv: Arc<Server>, task: tokio::task::JoinHandle<()>) {
        srv.stop();
        tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap();
        srv.core.shutdown();
    }

    async fn hello(ws: &mut Ws, srv: &Server) {
        let code = srv.auth.new_pair_code(crate::auth::CODE_TTL).unwrap();
        let token = srv.auth.pair(&code,"fixture").unwrap().1;
        ws.send(Message::Text(json!({"id":1,"method":"auth.hello","params":{"protocol":1,"token":token}}).to_string().into())).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2),ws.next()).await.unwrap().unwrap().unwrap();
        let Message::Text(reply) = reply else { panic!("auth yanıtı yok") };
        assert!(serde_json::from_str::<Value>(&reply).unwrap().get("result").is_some());
    }

    #[test]
    fn disconnect_mutes_sinks_and_rejects_late_spawn_or_attach_channels() {
        let (tx,mut rx) = mpsc::unbounded_channel();
        let conn = Conn {closed:AtomicBool::new(false),tx,chans:Arc::default(),client:"late".into(),view_client:"late-view".into(),view_state:Mutex::new(ViewState::default())};
        let mut sink = conn.open(1,"shell").unwrap();
        assert!(sink.data(b"before"));
        assert!(matches!(rx.try_recv().unwrap(),Message::Binary(_)));
        assert_eq!(conn.disconnect().len(),1);
        assert!(!sink.data(b"after"));
        sink.resized(80,24);
        sink.exit();
        conn.send("late reply".into());
        assert!(rx.try_recv().is_err(),"kapalı bağlantı yeniden çıktı üretti");
        assert!(conn.open(2,"late-spawn").is_err());
        assert!(conn.open(3,"late-attach").is_err());
    }

    #[tokio::test]
    async fn attach_finishing_after_disconnect_removes_its_subscriber() {
        struct Silent;
        impl PtySink for Silent {
            fn data(&mut self, _: &[u8]) -> bool { true }
            fn exit(&mut self) {}
        }
        let (srv,_,task) = network_fixture("late-attach").await;
        srv.core.pty_spawn(Spawn {id:"late-shell".into(),cmd:"shell".into(),cols:80,rows:24,..Default::default()},Silent).unwrap();
        let (tx,mut rx) = mpsc::unbounded_channel();
        let conn = Conn {closed:AtomicBool::new(false),tx,chans:Arc::default(),client:"late".into(),view_client:"late-view".into(),view_state:Mutex::new(ViewState::default())};
        // The handler already opened a channel but has not returned from core.attach yet.
        let sink = conn.open(1,"late-shell").unwrap();
        conn.disconnect();
        let attachment = srv.sessions.attach(&srv.core,"late-shell",sink).unwrap();
        conn.set_subscriber(&srv.core,1,"late-shell",attachment.subscriber);
        assert!(!srv.core.ptys.detach("late-shell",attachment.subscriber),"geç attach abone bıraktı");
        assert!(rx.try_recv().is_err());
        srv.core.ptys.kill("late-shell");
        network_stop(srv,task).await;
    }

    #[tokio::test]
    async fn incomplete_http_handshake_times_out() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (srv,addr,task) = network_fixture("http-timeout").await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: fixture\r\n").await.unwrap();
        let mut buf = [0;1];
        let read = tokio::time::timeout(AUTH_TIMEOUT + Duration::from_secs(1),stream.read(&mut buf)).await;
        network_stop(srv,task).await;
        assert!(matches!(read,Ok(Ok(0)) | Ok(Err(_))),"eksik HTTP el sıkışması 10 sn sonunda kapanmadı: {read:?}");
    }

    #[tokio::test]
    async fn preauth_limit_rejects_excess_connections_and_releases_after_auth() {
        use tokio::io::AsyncWriteExt;
        let (srv,addr,task) = network_fixture("preauth-limit").await;
        let url = format!("ws://{addr}");
        let mut waiting = Vec::new();
        for _ in 0..PREAUTH_LIMIT-1 {
            waiting.push(tokio_tungstenite::connect_async(&url).await.unwrap().0);
        }
        let mut incomplete = TcpStream::connect(addr).await.unwrap();
        incomplete.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let excess = tokio::time::timeout(Duration::from_secs(2),tokio_tungstenite::connect_async(&url)).await;
        let rejected = matches!(excess,Ok(Err(_)));
        hello(&mut waiting[0],&srv).await;
        let admitted = tokio::time::timeout(Duration::from_secs(2),tokio_tungstenite::connect_async(&url)).await;
        network_stop(srv,task).await;
        assert!(rejected,"doğrulama öncesi bağlantı sınırı aşılınca yeni bağlantı reddedilmedi");
        assert!(matches!(admitted,Ok(Ok(_))),"auth tamamlandığında bağlantı kapasitesi serbest kalmadı");
    }

    #[tokio::test]
    async fn slow_upgrade_does_not_restart_first_frame_deadline() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        use tokio_tungstenite::tungstenite::protocol::Role;
        let (srv,addr,task) = network_fixture("absolute-deadline").await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
        let upgrade = format!("GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n");
        stream.write_all(upgrade.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(8)).await;
        stream.write_all(b"\r\n").await.unwrap();
        let mut headers = Vec::new();
        tokio::time::timeout(Duration::from_secs(1),async {
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
        }).await.expect("Upgrade tamamlanmadı");
        assert!(headers.starts_with(b"HTTP/1.1 101"));
        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(stream,Role::Client,None).await;
        let closed = tokio::time::timeout(Duration::from_secs(3),ws.next()).await;
        network_stop(srv,task).await;
        assert!(matches!(closed,Ok(Some(Ok(Message::Close(Some(f))))) if u16::from(f.code) == CLOSE_UNAUTHORIZED),"Upgrade sonrası 10 sn bütçesi yeniden başladı");
    }

    #[tokio::test]
    async fn oversized_frames_and_fragmented_messages_are_rejected() {
        use tokio_tungstenite::tungstenite::protocol::frame::{Frame, coding::{Data, OpCode}};
        let (srv,addr,task) = network_fixture("ws-size").await;
        let url = format!("ws://{addr}");
        let payload = json!({"id":2,"method":"server_info","params":{"padding":"x".repeat(1024*1024)}}).to_string();
        let mut outcomes = Vec::new();
        for fragmented in [false,true] {
            let mut ws = tokio_tungstenite::connect_async(&url).await.unwrap().0;
            hello(&mut ws,&srv).await;
            if fragmented {
                let (a,b) = payload.as_bytes().split_at(payload.len()/2);
                ws.send(Message::Frame(Frame::message(a.to_vec(),OpCode::Data(Data::Text),false))).await.unwrap();
                ws.send(Message::Frame(Frame::message(b.to_vec(),OpCode::Data(Data::Continue),true))).await.unwrap();
            } else {
                ws.send(Message::Text(payload.clone().into())).await.unwrap();
            }
            let received = tokio::time::timeout(Duration::from_secs(2),ws.next()).await;
            outcomes.push(matches!(received,Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None)));
        }
        network_stop(srv,task).await;
        assert_eq!(outcomes,vec![true,true],"tek frame veya parçalı mesaj boyut sınırı uygulanmadı");
    }

    #[test]
    fn team_view_rpc_validates_visibility_and_rejects_events_after_disconnect() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("view-rpc-{}", std::process::id()));
        let core = Core::start(Paths { data_dir:root.join("data"),config_dir:root.join("config") }, Events::new(dex_core::events::NullSink)).unwrap();
        let srv = Server::from_core(core.clone(), root.join("auth.db"), Roots::new(&[root.clone()]).unwrap()).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let conn = Conn { closed:AtomicBool::new(false), tx, chans:Arc::default(), client:"local".into(), view_client:"view-fixture".into(), view_state:Mutex::new(ViewState::default()) };
        let rpc = |params| handle(&srv, &conn, &Request {id:json!(1),method:"team_viewed".into(),params});
        assert_eq!(rpc(json!({"teamId":"default","viewed":true})).unwrap(), Value::Null);
        assert_eq!(rpc(json!({"teamId":"default","viewed":false})).unwrap(), Value::Null);
        assert!(rpc(json!({"teamId":"default"})).is_err());
        assert!(rpc(json!({"teamId":"default","viewed":"true"})).is_err());
        assert!(rpc(json!({"teamId":"missing","viewed":true})).is_err());
        let seq = NEXT_VIEW_EVENT.fetch_add(2, Ordering::Relaxed);
        conn.team_view(&core, &json!({"teamId":"default","viewed":false}), seq+1).unwrap();
        conn.team_view(&core, &json!({"teamId":"default","viewed":true}), seq).unwrap();
        assert_eq!(conn.view_state.lock().unwrap().latest["default"], seq+1);
        conn.release_view(&core);
        assert!(rpc(json!({"teamId":"default","viewed":true})).is_err());
        core.shutdown();
    }

    #[test]
    fn team_folder_requests_are_scoped_and_clone_rpc_is_read_only() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/team-rpc-1059").join(std::process::id().to_string());
        let paths = Paths {data_dir:base.join("data"),config_dir:base.join("config")};
        let core = Core::start_bare(paths,Events::new(dex_core::events::NullSink)).unwrap();
        let workspace = base.join("Team");
        std::fs::create_dir_all(workspace.join("TeamMain")).unwrap();
        std::fs::write(workspace.join(".git"),"gitdir: missing-fixture-git-dir").unwrap();
        std::fs::create_dir_all(base.join("Other")).unwrap();
        let t = core.team_create(serde_json::from_value(json!({"name":"Team","kind":"main","cwd":workspace.join("TeamMain")})).unwrap()).unwrap();
        let srv = Server::from_core(core.clone(),base.join("dexd.db"),Roots::new(&[base.join("global")]).unwrap()).unwrap();
        let (tx,_rx) = tokio::sync::mpsc::unbounded_channel();
        let conn = Conn {closed:AtomicBool::new(false),tx,chans:Arc::new(Mutex::new(HashMap::new())),client:"test".into(),view_client:"test-view".into(),view_state:Mutex::new(ViewState::default())};
        let rpc = |method:&str,params:Value| handle(&srv,&conn,&Request {id:json!(1),method:method.into(),params});
        let adopted = rpc("team_update",json!({"id":t.id,"patch":{"workspaceRoot":workspace}})).unwrap();
        assert!(adopted["suggestedWorkspaceRoot"].is_null());
        let made = rpc("fs.mkdir",json!({"teamId":t.id,"parent":workspace,"name":"Sibling"})).unwrap();
        assert!(rpc("fs.list",json!({"teamId":t.id,"path":workspace})).is_ok());
        assert_eq!(rpc("fs.mkdir",json!({"parent":workspace,"name":"NoTeam"})).unwrap_err().code,"failed");
        assert!(rpc("fs.mkdir",json!({"teamId":t.id,"parent":base.join("Other"),"name":"Denied"})).is_err());
        let p = rpc("project_create",json!({"teamId":t.id,"project":{"name":"Sibling"},"root":made})).unwrap();
        assert_eq!(p["name"],"Sibling");
        assert_eq!(rpc("project_update",json!({"teamId":"missing","id":p["id"],"patch":{"name":"Renamed"}})).unwrap_err().code,"project_not_found");
        assert_eq!(rpc("project_create",json!({"teamId":t.id,"project":{"name":"Other"},"root":base.join("Other")})).unwrap_err().code,"failed");
        assert!(rpc("team_update",json!({"id":t.id,"patch":{"workspaceRoot":base.join("Other")}})).is_err());
        let p = rpc("project_create",json!({"teamId":t.id,"project":{"name":"New App"},"createFolder":true})).unwrap();
        assert!(p["rootPath"].as_str().unwrap().ends_with("NewApp"));
        let before = dex_core::db::with(&core.db,|c|Ok(c.total_changes())).unwrap();
        let template = rpc("team_clone_template",json!({"teamId":t.id})).unwrap();
        assert_eq!(template.as_object().unwrap().len(),3);
        assert_eq!(template["profile"],"dev");
        assert!(template["members"].as_array().unwrap().iter().all(|a| a.get("id").is_none()));
        assert_eq!(before,dex_core::db::with(&core.db,|c|Ok(c.total_changes())).unwrap());
        assert!(methods::METHODS.contains(&"team_clone_template"));
        assert!(rpc("team_clone_template",json!({})).is_err());
        let before = dex_core::db::with(&core.db, |c| Ok(c.total_changes())).unwrap();
        let name = format!("Dex Preview 1062 {}", std::process::id());
        let preview = rpc("team_folder_preview", json!({"name":name,"home":base})).unwrap();
        let host_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
        let home = dex_core::teams::normalize_dir(&std::path::PathBuf::from(host_home).to_string_lossy()).unwrap();
        let folder = dex_core::teams::folder_name(&name).unwrap();
        assert_eq!(preview["folderName"], folder);
        assert_eq!(preview["workspaceRoot"], std::path::Path::new(&home).join(&folder).to_string_lossy().as_ref());
        assert_eq!(preview["mainRoot"], std::path::Path::new(&home).join(&folder).join(format!("{folder}Main")).to_string_lossy().as_ref());
        assert_eq!(preview["exists"], json!({"workspace":false,"main":false}));
        assert!(preview.get("owner").is_none());
        assert!(!std::path::Path::new(preview["workspaceRoot"].as_str().unwrap()).exists());
        assert_eq!(before, dex_core::db::with(&core.db, |c| Ok(c.total_changes())).unwrap());
        assert!(methods::METHODS.contains(&"team_folder_preview"));
        assert!(rpc("team_folder_preview", json!({})).is_err());
        assert_eq!(rpc("team_folder_preview", json!({"name":"   "})).unwrap_err().message, "invalid_folder_name");
        core.shutdown();
    }

    /// Lens #2146: the wider #2137 suggestion (a non-`<Name>Main` project's parent) is UI advice —
    /// a remote client cannot adopt it outside its roots; the local desktop still can.
    #[test]
    fn remote_cannot_adopt_wide_suggestion_outside_roots() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/team-rpc-2147").join(std::process::id().to_string());
        let core = Core::start_bare(Paths {data_dir:base.join("data"),config_dir:base.join("config")},Events::new(dex_core::events::NullSink)).unwrap();
        let category = base.join("JezDev");
        std::fs::create_dir_all(category.join("no-more-clickbait")).unwrap();
        let t = core.team_create(serde_json::from_value(json!({"name":"Jez Dev","kind":"main","cwd":category.join("no-more-clickbait")})).unwrap()).unwrap();
        let normalized = dex_core::teams::normalize_dir(&category.to_string_lossy()).unwrap();
        assert_eq!(t.suggested_workspace_root.as_deref(), Some(normalized.as_str()));
        let (tx,_rx) = tokio::sync::mpsc::unbounded_channel();
        let conn = Conn {closed:AtomicBool::new(false),tx,chans:Arc::new(Mutex::new(HashMap::new())),client:"test".into(),view_client:"test-view".into(),view_state:Mutex::new(ViewState::default())};
        let patch = json!({"id":t.id,"patch":{"workspaceRoot":category}});
        let remote = Server::from_core(core.clone(),base.join("dexd.db"),Roots::new(&[base.join("global")]).unwrap()).unwrap();
        let denied = handle(&remote,&conn,&Request {id:json!(1),method:"team_update".into(),params:patch.clone()}).unwrap_err();
        assert_eq!(denied.message,"workspace root izinli klasörlerin dışında");
        let local = Server::local(core.clone(),LocalToken::new().unwrap().0).unwrap();
        let adopted = handle(&local,&conn,&Request {id:json!(1),method:"team_update".into(),params:patch}).unwrap();
        assert_eq!(adopted["workspaceRoot"],normalized);
        assert!(adopted["suggestedWorkspaceRoot"].is_null());
        core.shutdown();
    }

    /// #146: the local endpoint admits loopback peers only; a paired-device endpoint any peer.
    #[test]
    fn local_endpoint_is_loopback_only() {
        let root = std::env::temp_dir().join(format!("dexd-admits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        let core = Core::start_bare(paths, Events::new(dex_core::events::NullSink)).unwrap();
        let local = Server::local(core.clone(), LocalToken::new().unwrap().0).unwrap();
        let paired = Server::from_core(core.clone(), root.join("dexd.db"), Roots::new(&[root.join("p")]).unwrap()).unwrap();
        for (ip, ok) in [("127.0.0.1", true), ("::1", true), ("100.64.0.1", false), ("192.168.1.5", false), ("0.0.0.0", false)] {
            let ip: std::net::IpAddr = ip.parse().unwrap();
            assert_eq!(local.admits(ip), ok, "{ip}");
            assert!(paired.admits(ip), "{ip}");
        }
        // #192: the webview's origins (release on each platform, `tauri dev`) or none (a native
        // client); anything else — another page, a lookalike, a different port — is refused.
        let cases: [(Option<&str>, bool); 11] = [
            (None, true),
            (Some("tauri://localhost"), true),
            (Some("http://tauri.localhost"), true),
            (Some("http://localhost:1420"), true),
            (Some("https://evil.example"), false),
            (Some("http://localhost:5173"), false),
            (Some("http://127.0.0.1:1420"), false),
            (Some("http://tauri.localhost.evil.example"), false),
            (Some("HTTP://TAURI.LOCALHOST"), false),
            (Some("null"), false),
            (Some(""), false),
        ];
        for (origin, ok) in cases {
            assert_eq!(local.admits_origin(origin.map(str::as_bytes)), ok, "{origin:?}");
            assert!(paired.admits_origin(origin.map(str::as_bytes)), "{origin:?}");
        }
        assert!(!local.admits_origin(Some(b"http://tauri.localhost\xff")), "not UTF-8");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #838 G2: a remote client binds a project only to a folder inside the roots (create and
    /// a root move); a name/branch-only update has no folder to check.
    #[test]
    fn remote_project_roots_stay_inside_the_roots() {
        let root = std::env::temp_dir().join(format!("dexd-project-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("p").join("app")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let roots = Roots::new(&[root.join("p")]).unwrap();
        let (inside, outside) = (root.join("p").join("app"), root.join("outside"));
        for m in ["project_create", "project_update"] {
            assert!(team_folder_allowed(&roots, m, &json!({"root": inside})).is_ok(), "{m}");
            assert_eq!(team_folder_allowed(&roots, m, &json!({"root": outside})).unwrap_err().code, "failed", "{m}");
        }
        assert!(team_folder_allowed(&roots, "project_update", &json!({"patch": {"name": "x"}, "branch": "main"})).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `SERVER_METHODS` is exactly what `handle` answers itself (else `auth.hello` lies), and
    /// nothing is listed twice.
    #[test]
    fn server_methods_match_handle() {
        // A CRLF checkout (core.autocrlf) must parse the same.
        let src = include_str!("server.rs").replace("\r\n", "\n");
        let body = &src[src.find("fn handle(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let mut arms: Vec<&str> = body
            .lines()
            .filter_map(|l| l.strip_prefix("        \"")?.split_once("\" =>").map(|(n, _)| n))
            .filter(|n| !n.starts_with("auth."))
            .collect();
        arms.sort();
        let mut listed: Vec<&str> = SERVER_METHODS.iter().chain(&LOCAL_METHODS).copied().collect();
        listed.sort();
        assert_eq!(arms, listed);
        assert!(SERVER_METHODS.iter().chain(&LOCAL_METHODS).all(|m| !methods::METHODS.contains(m)));
        // A paired device is not told about the local-only ones.
        let has = |local: bool, m: &str| server_info(local)["methods"].as_array().unwrap().iter().any(|v| v == m);
        assert!(LOCAL_METHODS.iter().all(|m| !has(false, m) && has(true, m)));
    }
}
