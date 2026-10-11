//! The desktop's optional tailnet-only endpoint (Settings → Ağ erişimi). It deliberately borrows
//! the local core: opening a second core against the desktop database would split sessions and
//! hook ownership. #150 (B): owned by `dexd --local` (moved from src-tauri); its methods are
//! answered on the local endpoint only. The in-app core (`inproc-core`) still uses it as before.
//! Its connections are paired devices with the root check on (`team_folder_allowed`).

use crate::auth::{Device, CODE_TTL};
use crate::fs::{default_root, Roots};
use crate::Server;
use dex_core::Core;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

type CoreState = Arc<Core>;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    enabled: bool,
    #[serde(default = "default_port")]
    port: u16,
}

const fn default_port() -> u16 {
    crate::DEFAULT_PORT
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub enabled: bool,
    pub port: u16,
    pub listening: Option<String>,
    pub error: Option<String>,
    pub firewall_command: String,
}

struct Running {
    server: Arc<Server>,
    task: tokio::task::JoinHandle<()>,
    url: String,
}

struct Inner {
    saved: Saved,
    running: Option<Running>,
    error: Option<String>,
}

pub struct TailnetState {
    file: PathBuf,
    inner: Mutex<Inner>,
    /// Serialize changes across windows and the startup task, including connection draining.
    lifecycle: tokio::sync::Mutex<()>,
    #[cfg(test)]
    ip: fn() -> Result<Ipv4Addr, String>,
}

impl TailnetState {
    pub fn open(data_dir: PathBuf) -> Arc<Self> {
        let file = data_dir.join("tailnet.json");
        let saved = std::fs::read(&file)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_else(|| Saved {
                enabled: false,
                port: default_port(),
            });
        Arc::new(Self {
            file,
            lifecycle: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            ip: tailnet_ip,
            inner: Mutex::new(Inner {
                saved,
                running: None,
                error: None,
            }),
        })
    }

    fn save(&self, saved: &Saved) -> Result<(), String> {
        let bytes = serde_json::to_vec(saved).map_err(|e| e.to_string())?;
        std::fs::write(&self.file, bytes).map_err(|e| e.to_string())
    }

    pub fn view(&self) -> View {
        let inner = self.inner.lock().unwrap();
        let port = inner.saved.port;
        View {
            enabled: inner.saved.enabled,
            port,
            listening: inner.running.as_ref().map(|r| r.url.clone()),
            error: inner.error.clone(),
            firewall_command: firewall_command(port),
        }
    }

    pub fn publish(&self, event: &str, payload: serde_json::Value) {
        if let Some(r) = self.inner.lock().unwrap().running.as_ref() {
            r.server.publish_event(event, payload);
        }
    }

    async fn start(&self, core: CoreState, port: u16) -> Result<(), String> {
        #[cfg(not(test))]
        let ip = tailnet_ip()?;
        #[cfg(test)]
        let ip = (self.ip)()?;
        let roots = Roots::new(&[default_root()])?;
        let server = Server::from_core(
            core.clone(),
            core.paths.data_dir.join("dexd-tailnet.db"),
            roots,
        )?;
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(ip), port))
            .await
            .map_err(|e| format!("tailnet ucu açılamadı: {e}"))?;
        let url = format!("ws://{}", listener.local_addr().map_err(|e| e.to_string())?);
        let mut inner = self.inner.lock().unwrap();
        if !inner.saved.enabled {
            return Ok(());
        }
        let task = tokio::spawn(crate::serve(listener, server.clone()));
        inner.running = Some(Running { server, task, url });
        inner.error = None;
        Ok(())
    }

    pub async fn set(&self, core: CoreState, enabled: bool, port: u16) -> Result<View, String> {
        if port == 0 {
            return Err("port 1-65535 aralığında olmalı".into());
        }
        let _lifecycle = self.lifecycle.lock().await;
        let old = {
            let mut inner = self.inner.lock().unwrap();
            let saved = Saved { enabled, port };
            self.save(&saved)?;
            inner.saved = saved;
            inner.running.take()
        };
        if let Some(r) = old {
            r.server.stop();
            r.task.await.map_err(|e| format!("tailnet ucu kapatılamadı: {e}"))?;
        }
        if enabled {
            if let Err(e) = self.start(core, port).await {
                self.inner.lock().unwrap().error = Some(e.clone());
                return Err(e);
            }
        }
        Ok(self.view())
    }

    /// The saved setting, applied once at startup (the caller spawns it on its runtime).
    pub async fn start_on_launch(self: Arc<Self>, core: CoreState) {
        let _lifecycle = self.lifecycle.lock().await;
        let (enabled, port) = {
            let i = self.inner.lock().unwrap();
            (i.saved.enabled, i.saved.port)
        };
        if enabled && self.inner.lock().unwrap().running.is_none() {
            if let Err(e) = self.start(core, port).await {
                self.inner.lock().unwrap().error = Some(e);
            }
        }
    }

    pub fn pair_code(&self) -> Result<String, String> {
        let inner = self.inner.lock().unwrap();
        let server = inner.running.as_ref().ok_or("Önce ağ erişimini açın.")?;
        server.server.auth.new_pair_code(CODE_TTL)
    }

    pub fn devices(&self) -> Result<Vec<Device>, String> {
        let inner = self.inner.lock().unwrap();
        inner.running.as_ref().ok_or("Ağ erişimi kapalı.")?.server.auth.list()
    }

    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let inner = self.inner.lock().unwrap();
        inner.running.as_ref().ok_or("Ağ erişimi kapalı.")?.server.auth.revoke(id)
    }

    /// Process exit: cancel sockets and wait, bounded so a blocking core call cannot hang exit.
    pub async fn stop(&self) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let _lifecycle = tokio::time::timeout_at(deadline, self.lifecycle.lock()).await.ok();
        let old = {
            let mut inner = self.inner.lock().unwrap();
            inner.saved.enabled = false;
            inner.running.take()
        };
        if let Some(mut r) = old {
            r.server.stop();
            if tokio::time::timeout_at(deadline, &mut r.task).await.is_err() {
                r.task.abort();
                let _ = r.task.await;
            }
        }
    }
}

fn tailnet_ip() -> Result<Ipv4Addr, String> {
    let output = Command::new("tailscale.exe")
        .args(["ip", "-4"])
        .output()
        .or_else(|_| Command::new("tailscale").args(["ip", "-4"]).output())
        .map_err(|_| "Tailscale bulunamadı; önce Tailscale’i kurup bağlanın.".to_string())?;
    if !output.status.success() {
        return Err("Tailscale bağlı değil; tailnet ucu açılmadı.".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let ip = text
        .lines()
        .find_map(|l| l.trim().parse::<Ipv4Addr>().ok())
        .ok_or("Tailscale IPv4 adresi alınamadı.")?;
    if !is_tailnet(ip) {
        return Err("Tailscale’in geçerli bir tailnet IPv4 adresi yok; uç açılmadı.".into());
    }
    Ok(ip)
}

fn is_tailnet(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
}

fn firewall_command(port: u16) -> String {
    format!("New-NetFirewallRule -DisplayName 'Dex tailnet {port}' -Direction Inbound -Action Allow -Protocol TCP -LocalPort {port} -RemoteAddress 100.64.0.0/10")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::LocalToken;
    use dex_core::{Events, Paths};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
    use tokio_tungstenite::tungstenite::Message;

    type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

    async fn call(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
        ws.send(Message::Text(json!({"id":id,"method":method,"params":params}).to_string().into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match ws.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let reply: Value = serde_json::from_str(&text).unwrap();
                        if reply["id"] == id { return reply; }
                    }
                    Some(Ok(Message::Binary(_))) => {},
                    other => panic!("RPC bağlantısı kapandı: {other:?}"),
                }
            }
        }).await.expect("RPC yanıtı gelmedi")
    }

    async fn closed(ws: &mut Ws) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match ws.next().await {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(_)) => {}
                }
            }
        }).await.expect("ağ erişimi kapandıktan sonra eski WS oturumu açık kaldı");
    }

    struct CoreGuard(CoreState);
    impl Drop for CoreGuard {
        fn drop(&mut self) { self.0.shutdown(); }
    }

    // Drop releases the blocking handler even when a regression assertion panics.
    struct HeldRequest(Arc<(Mutex<bool>, std::sync::Condvar)>);
    impl Drop for HeldRequest {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }

    struct Output(tokio::sync::mpsc::UnboundedSender<Vec<u8>>, std::sync::Weak<Core>);
    impl dex_core::PtySink for Output {
        fn data(&mut self, bytes: &[u8]) -> bool {
            // ConPTY waits for the terminal's cursor position before reading shell input.
            if bytes.windows(4).any(|b| b == b"\x1b[6n") {
                if let Some(core) = self.1.upgrade() { let _ = core.ptys.write("pending-shell",b"\x1b[1;1R"); }
            }
            self.0.send(bytes.to_vec()).is_ok()
        }
        fn exit(&mut self) {}
    }

    enum PendingWork { Rpc, Pty, PtyOverflow }

    async fn change_with_pending_work(enabled: bool, work: PendingWork) {
        let binary = !matches!(work, PendingWork::Rpc);
        let overflow = matches!(work, PendingWork::PtyOverflow);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("tailnet-pending-{enabled}-{binary}-{overflow}-{}",std::process::id()));
        let core = Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")},Events::new(dex_core::events::NullSink)).unwrap();
        let _guard = CoreGuard(core.clone());
        let mut state = TailnetState::open(core.paths.data_dir.clone());
        Arc::get_mut(&mut state).unwrap().ip = || Ok(Ipv4Addr::LOCALHOST);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let view = state.set(core.clone(),true,port).await.unwrap();
        let remote = state.inner.lock().unwrap().running.as_ref().unwrap().server.clone();
        let mut phone = connect_async(view.listening.unwrap()).await.unwrap().0;
        assert!(call(&mut phone,1,"auth.pair",json!({"protocol":1,"code":state.pair_code().unwrap()})).await.get("result").is_some());

        let (output_tx,mut output) = tokio::sync::mpsc::unbounded_channel();
        core.pty_spawn(dex_core::pty::Spawn {id:"pending-shell".into(),cmd:"shell".into(),cwd:Some(root.to_string_lossy().into()),cols:80,rows:24,..Default::default()},Output(output_tx,Arc::downgrade(&core))).unwrap();
        let attached = call(&mut phone,2,"pty.attach",json!({"id":"pending-shell","channel":1})).await;
        assert!(attached["result"]["subscriber"].is_u64(),"{attached}");

        let gate = HeldRequest(Arc::new((Mutex::new(false),std::sync::Condvar::new())));
        let wait = gate.0.clone();
        let (started_tx,mut started) = tokio::sync::mpsc::unbounded_channel();
        *remote.request_hook.lock().unwrap() = Some(Arc::new(move |method| {
            if method == if binary { "pty.write" } else { "fs.list" } {
                started_tx.send(()).unwrap();
                let _released = wait.1.wait_while(wait.0.lock().unwrap(),|released| !*released).unwrap();
            }
        }));
        let input = if binary {
            Message::Binary(crate::protocol::pty_frame(1,b"echo DEX_HELD_INPUT\r").into())
        } else {
            Message::Text(json!({"id":3,"method":"fs.list","params":{}}).to_string().into())
        };
        phone.send(input).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2),started.recv()).await.unwrap().unwrap();
        if binary {
            for _ in 0..if overflow { 17 } else { 1 } {
                phone.send(Message::Binary(crate::protocol::pty_frame(1,b"echo DEX_QUEUED_INPUT\r").into())).await.unwrap();
            }
        }

        let (verifier,token) = LocalToken::new().unwrap();
        let local = Server::local(core.clone(),verifier).unwrap();
        local.set_tailnet(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}",listener.local_addr().unwrap());
        let local_task = tokio::spawn(crate::serve(listener,local.clone()));
        let mut desktop = connect_async(url).await.unwrap().0;
        assert!(call(&mut desktop,1,"auth.hello",json!({"protocol":1,"token":token})).await.get("result").is_some());
        let next = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let next_port = next.local_addr().unwrap().port();
        drop(next);
        let change = call(&mut desktop,2,"tailnet_set",json!({"enabled":enabled,"port":next_port}));
        let close = async {
            tokio::time::timeout(Duration::from_secs(1),async {
                loop {
                    match phone.next().await {
                        Some(Ok(Message::Close(frame))) => {
                            if overflow { assert_eq!(u16::from(frame.unwrap().code),1008); }
                            return;
                        },
                        Some(Ok(_)) => {},
                        other => panic!("Close çerçevesi gelmedi: {other:?}"),
                    }
                }
            }).await.expect("bekleyen RPC serbest bırakılmadan uzak Close gelmedi");
        };
        let reply = if overflow {
            // Overflow itself must close the phone, before Settings cancels the endpoint.
            close.await;
            change.await
        } else {
            tokio::join!(change,close).0
        };
        assert_eq!(reply["result"]["enabled"],enabled,"{reply}");
        assert!(!*gate.0.0.lock().unwrap(),"RPC erken serbest bırakıldı");
        if binary { assert!(started.try_recv().is_err(),"PTY yazmaları eşzamanlı çalıştı"); }
        assert!(TcpStream::connect(format!("127.0.0.1:{port}")).await.is_err());

        // The PTY still runs; its new output must reach another sink but never this phone.
        core.ptys.write("pending-shell",b"echo DEX_AFTER_CANCEL\r").unwrap();
        tokio::time::timeout(Duration::from_secs(5),async {
            let mut seen = Vec::new();
            while let Some(bytes) = output.recv().await {
                seen.extend(bytes);
                if String::from_utf8_lossy(&seen).contains("DEX_AFTER_CANCEL") { return; }
            }
            panic!("PTY çıktısı yok");
        }).await.unwrap();
        if let Ok(Some(Ok(message))) = tokio::time::timeout(Duration::from_millis(100),phone.next()).await {
            assert!(!matches!(message,Message::Binary(_)),"iptal sonrası PTY çıktısı ulaştı");
        }
        assert!(call(&mut desktop,4,"server_info",json!({})).await.get("result").is_some());
        drop(gate);
        if binary {
            assert!(tokio::time::timeout(Duration::from_millis(100),started.recv()).await.is_err(),"kapanan bağlantının kuyruktaki girdisi çalıştırıldı");
        }
        core.ptys.kill("pending-shell");
        state.stop().await;
        local.stop();
        tokio::time::timeout(Duration::from_secs(2),local_task).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn disable_closes_subscribed_phone_before_pending_rpc_finishes() {
        change_with_pending_work(false,PendingWork::Rpc).await;
    }

    #[tokio::test]
    async fn port_change_closes_subscribed_phone_before_pending_rpc_finishes() {
        change_with_pending_work(true,PendingWork::Rpc).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disable_closes_phone_before_pending_pty_write_finishes() {
        change_with_pending_work(false,PendingWork::Pty).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn port_change_closes_phone_before_pending_pty_write_finishes() {
        change_with_pending_work(true,PendingWork::Pty).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pty_input_overflow_closes_phone_before_pending_write_finishes() {
        change_with_pending_work(false,PendingWork::PtyOverflow).await;
    }

    async fn local_exit_reply(method: &str, exit: crate::local::Exit, result: Value) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("local-{method}-{}",std::process::id()));
        let life = crate::local::Life::new(crate::local::GRACE);
        let state = TailnetState::open(root.join("data"));
        let (server,token) = Server::start_local(Paths {data_dir:root.join("data"),config_dir:root.join("config")},state.clone(),life.clone()).unwrap();
        let _guard = CoreGuard(server.core.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(crate::local::serve_until_exit(listener,server,life.clone()));
        let mut desktop = connect_async(format!("ws://{addr}")).await.unwrap().0;
        assert!(call(&mut desktop,1,"auth.hello",json!({"protocol":1,"token":token})).await.get("result").is_some());
        assert_eq!(life.clients(),1);
        assert_eq!(call(&mut desktop,2,method,json!({})).await,json!({"id":2,"result":result}));
        assert_eq!(tokio::time::timeout(Duration::from_secs(4),task).await.unwrap().unwrap(),exit);
        assert_eq!(life.clients(),0,"yerel bağlantı temizliği tamamlanmadı");
        closed(&mut desktop).await;
        assert!(TcpStream::connect(addr).await.is_err());
        state.stop().await;
    }

    #[tokio::test]
    async fn local_shutdown_delivers_rpc_reply_before_disconnect() {
        local_exit_reply("core_shutdown",crate::local::Exit::Shutdown,Value::Null).await;
    }

    #[tokio::test]
    async fn local_restart_delivers_rpc_reply_before_disconnect() {
        local_exit_reply("core_restart",crate::local::Exit::Restart,json!({"restarting":true})).await;
    }

    #[tokio::test]
    async fn port_change_disconnects_previous_sessions_and_reuses_device_credentials() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("tailnet-port-{}",std::process::id()));
        let core = Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")},Events::new(dex_core::events::NullSink)).unwrap();
        let _guard = CoreGuard(core.clone());
        let mut state = TailnetState::open(core.paths.data_dir.clone());
        Arc::get_mut(&mut state).unwrap().ip = || Ok(Ipv4Addr::LOCALHOST);
        let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (first_port,second_port) = (first.local_addr().unwrap().port(),second.local_addr().unwrap().port());
        drop(first);
        let first_view = state.set(core.clone(),true,first_port).await.unwrap();
        let mut old = connect_async(first_view.listening.unwrap()).await.unwrap().0;
        let pair = call(&mut old,1,"auth.pair",json!({"protocol":1,"code":state.pair_code().unwrap()})).await;
        let token = pair["result"]["token"].as_str().unwrap();
        let mut unauthenticated = connect_async(format!("ws://127.0.0.1:{first_port}")).await.unwrap().0;
        drop(second);
        let new_view = state.set(core,true,second_port).await.unwrap();
        closed(&mut old).await;
        closed(&mut unauthenticated).await;
        assert!(TcpStream::connect(format!("127.0.0.1:{first_port}")).await.is_err());
        let mut new = connect_async(new_view.listening.unwrap()).await.unwrap().0;
        assert!(call(&mut new,1,"auth.hello",json!({"protocol":1,"token":token})).await.get("error").is_none());
        assert!(call(&mut new,2,"pty.list",json!({})).await.get("error").is_none());
        tokio::time::timeout(Duration::from_secs(4),state.stop()).await.unwrap();
        closed(&mut new).await;
    }

    #[tokio::test(start_paused = true)]
    async fn process_stop_does_not_hang_on_a_stuck_listener_task() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("tailnet-stop-{}",std::process::id()));
        let core = Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")},Events::new(dex_core::events::NullSink)).unwrap();
        let _guard = CoreGuard(core.clone());
        let state = TailnetState::open(core.paths.data_dir.clone());
        let server = Server::from_core(core,root.join("auth.db"),Roots::new(&[root]).unwrap()).unwrap();
        let task = tokio::spawn(std::future::pending());
        let abort = task.abort_handle();
        state.inner.lock().unwrap().running = Some(Running {server,task,url:"fixture".into()});
        tokio::time::timeout(Duration::from_secs(4),state.stop()).await.expect("süreç çıkışı takıldı");
        assert!(abort.is_finished(),"takılan dinleyici görevi beklenmedi");
        assert!(state.view().listening.is_none());
    }

    #[tokio::test]
    async fn disable_disconnects_old_sessions_and_keeps_desktop_connected() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("tailnet-disable-{}", std::process::id()));
        let core = Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")}, Events::new(dex_core::events::NullSink)).unwrap();
        let _guard = CoreGuard(core.clone());
        let state = TailnetState::open(core.paths.data_dir.clone());
        let remote = Server::from_core(core.clone(),root.join("auth.db"),Roots::new(std::slice::from_ref(&root)).unwrap()).unwrap();
        let code = remote.auth.new_pair_code(CODE_TTL).unwrap();
        let token = remote.auth.pair(&code,"fixture").unwrap().1;
        // Exercise the real tailnet lifecycle without requiring a Tailscale installation.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");
        let task = tokio::spawn(crate::serve(listener,remote.clone()));
        state.inner.lock().unwrap().saved = Saved {enabled:true,port};
        state.inner.lock().unwrap().running = Some(Running {server:remote,task,url:url.clone()});
        let (verifier, local_token) = LocalToken::new().unwrap();
        let local = Server::local(core.clone(),verifier).unwrap();
        local.set_tailnet(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_url = format!("ws://{}",listener.local_addr().unwrap());
        let local_task = tokio::spawn(crate::serve(listener,local));
        let mut desktop = connect_async(local_url).await.unwrap().0;
        assert!(call(&mut desktop,1,"auth.hello",json!({"protocol":1,"token":local_token})).await.get("error").is_none());
        let mut phone = connect_async(&url).await.unwrap().0;
        assert!(call(&mut phone,1,"auth.hello",json!({"protocol":1,"token":token})).await.get("error").is_none());
        assert!(call(&mut phone,2,"pty.list",json!({})).await.get("error").is_none());
        let mut unauthenticated = connect_async(&url).await.unwrap().0;
        let mut incomplete = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
        let disabled = call(&mut desktop,2,"tailnet_set",json!({"enabled":false,"port":port})).await;
        assert_eq!(disabled["result"]["enabled"],false);
        closed(&mut phone).await;
        closed(&mut unauthenticated).await;
        let mut buf = [0;1];
        use tokio::io::AsyncReadExt;
        let read = tokio::time::timeout(Duration::from_secs(2),incomplete.read(&mut buf)).await.expect("HTTP el sıkışması açık kaldı");
        assert!(matches!(read,Ok(0)) || read.is_err());
        assert!(phone.send(Message::Text(json!({"id":3,"method":"team_list","params":{}}).to_string().into())).await.is_err());
        assert!(phone.send(Message::Binary(crate::protocol::pty_frame(1,b"blocked").into())).await.is_err());
        assert!(call(&mut desktop,3,"server_info",json!({})).await.get("error").is_none());
        local_task.abort();
    }

    #[test]
    fn only_cgnat_range_is_a_tailnet_ipv4() {
        assert!(is_tailnet("100.64.0.1".parse().unwrap()));
        assert!(!is_tailnet("192.168.1.5".parse().unwrap()));
        assert!(!is_tailnet("100.128.0.1".parse().unwrap()));
    }
}
