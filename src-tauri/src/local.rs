//! #146 (A0): the in-process core, also served to this app's own webview over a loopback dexd
//! endpoint (`DEX_LOCAL_WS=1`, off by default). The webview then talks to it like to any host
//! (`WsTransport`) — the step before the core moves into its own process (.dex/results/188.md).
//! The token is random per process and reaches the webview only through `local_host` (memory; no
//! file, no keyring). Tauri-only commands (keyring, snip, usage, tailnet, logs) stay `invoke`.

use crate::host::CoreState;
use dexd::auth::LocalToken;
use dexd::Server;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

pub fn enabled() -> bool {
    std::env::var("DEX_LOCAL_WS").is_ok_and(|v| v == "1")
}

#[derive(Clone, Serialize)]
pub struct LocalHost {
    url: String,
    token: String,
}

pub struct LocalWs {
    pub server: Arc<Server>,
    host: LocalHost,
}

/// `None` = the flag is off: the webview keeps `invoke` + Tauri events.
pub struct LocalState(pub Option<LocalWs>);

impl LocalWs {
    /// Binds 127.0.0.1:0 now (the url is known before the webview asks) and serves on Tauri's runtime.
    pub fn start(core: CoreState) -> Result<Self, String> {
        let (verifier, token) = LocalToken::new()?;
        let server = Server::local(core, verifier)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("yerel uç açılamadı: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let url = format!("ws://{}", listener.local_addr().map_err(|e| e.to_string())?);
        let srv = server.clone();
        tauri::async_runtime::spawn(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => dexd::serve(l, srv).await,
                Err(e) => eprintln!("dex: yerel uç: {e}"),
            }
        });
        Ok(Self { server, host: LocalHost { url, token } })
    }
}

#[tauri::command]
pub fn local_host(state: State<LocalState>) -> Option<LocalHost> {
    state.0.as_ref().map(|l| l.host.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dex_core::events::NullSink;
    use dex_core::{Core, Events, Paths};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    async fn hello(req: impl IntoClientRequest + Unpin, token: &str) -> Value {
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let req = json!({"id": 1, "method": "auth.hello", "params": {"protocol": 1, "token": token}});
        ws.send(Message::Text(req.to_string().into())).await.unwrap();
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    /// The endpoint is on loopback, and it opens with this process's token only.
    #[tokio::test(flavor = "multi_thread")]
    async fn serves_loopback_with_its_own_token_only() {
        let root = std::env::temp_dir().join(format!("dex-local-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start_bare(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(NullSink)).unwrap();
        let ws = LocalWs::start(core.clone()).unwrap();
        let LocalHost { url, token } = ws.host.clone();
        assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
        assert!(token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, LocalWs::start(core.clone()).unwrap().host.token, "random per start");

        let ok = hello(url.as_str(), &token).await;
        assert_eq!(ok["result"]["device"]["id"], "local", "{ok}");
        for bad in ["", "x", &token[..63], &token.to_uppercase()] {
            let r = hello(url.as_str(), bad).await;
            assert_eq!(r["error"]["code"], "unauthorized", "{bad:?}: {r}");
        }

        // #192: a browser handshake from anywhere but this webview is refused before any frame.
        let with_origin = |o: &str| {
            let mut req = url.as_str().into_client_request().unwrap();
            req.headers_mut().insert("Origin", o.parse().unwrap());
            req
        };
        for o in ["https://evil.example", "http://localhost:5173"] {
            match tokio_tungstenite::connect_async(with_origin(o)).await {
                Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403, "{o}"),
                other => panic!("{o}: {:?}", other.map(|_| ())),
            }
        }
        let ok = hello(with_origin("http://tauri.localhost"), &token).await;
        assert_eq!(ok["result"]["device"]["id"], "local", "{ok}");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}
