//! #146 (A0): the desktop's loopback endpoint (`Server::local`) in-process — only its own token
//! opens it; no pairing, no device tokens; a team folder outside the roots is fine (folder picker).

use dex_core::events::NullSink;
use dex_core::{Core, Events, Paths};
use dexd::auth::LocalToken;
use dexd::server::CLOSE_UNAUTHORIZED;
use dexd::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

async fn first_frame(url: &str, method: &str, params: Value) -> (Value, Option<u16>) {
    let (mut ws, _) = connect_async(url).await.unwrap();
    ws.send(Message::Text(json!({"id": 1, "method": method, "params": params}).to_string().into())).await.unwrap();
    let mut reply = Value::Null;
    loop {
        match timeout(Duration::from_secs(10), ws.next()).await.expect("10 sn içinde çerçeve yok") {
            Some(Ok(Message::Text(t))) => {
                reply = serde_json::from_str(t.as_str()).unwrap();
                if reply.get("error").is_none() {
                    return (reply, None);
                }
            }
            Some(Ok(Message::Close(f))) => return (reply, f.map(|f| u16::from(f.code))),
            Some(Ok(_)) => {}
            _ => return (reply, None),
        }
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn call(ws: &mut Ws, next: &mut u64, method: &str, params: Value) -> Value {
    *next += 1;
    ws.send(Message::Text(json!({"id": *next, "method": method, "params": params}).to_string().into())).await.unwrap();
    loop {
        let Some(Ok(Message::Text(t))) = timeout(Duration::from_secs(10), ws.next()).await.unwrap() else { continue };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        if v["id"] == *next {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_local_token_opens_the_local_endpoint() {
    let root = std::env::temp_dir().join(format!("dexd-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let core = Core::start_bare(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(NullSink)).unwrap();
    let (verifier, token) = LocalToken::new().unwrap();
    let srv: Arc<Server> = Server::local(core.clone(), verifier).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(dexd::serve(listener, srv));

    for (method, params) in [
        ("auth.hello", json!({"protocol": 1, "token": "0".repeat(64)})),
        ("auth.hello", json!({"protocol": 1, "token": ""})),
        // No pairing here: the code would never be checked, the endpoint has one credential.
        ("auth.pair", json!({"protocol": 1, "code": "ABCD-EFGH", "name": "x"})),
        ("team_list", json!({})),
    ] {
        let (reply, close) = first_frame(&url, method, params).await;
        assert_eq!(reply["error"]["code"], "unauthorized", "{method}: {reply}");
        assert_eq!(close, Some(CLOSE_UNAUTHORIZED), "{method}");
    }

    let (mut ws, _) = connect_async(&url).await.unwrap();
    let mut next = 0;
    let hello = call(&mut ws, &mut next, "auth.hello", json!({"protocol": 1, "token": token})).await;
    assert_eq!(hello["result"]["device"]["id"], "local", "{hello}");
    let methods = hello["result"]["methods"].as_array().unwrap();
    assert!(methods.iter().any(|m| m == "team_list") && methods.iter().any(|m| m == "pty_spawn"), "{hello}");
    assert_eq!(call(&mut ws, &mut next, "auth.hello", json!({"protocol": 1, "token": token})).await["error"]["code"], "bad_request");

    // Outside every root: the desktop's own picker, no root check (a remote host refuses this).
    let folder = root.join("anywhere");
    std::fs::create_dir_all(&folder).unwrap();
    let team = call(&mut ws, &mut next, "team_create", json!({"team": {"name": "Yerel", "kind": "main", "cwd": folder.to_string_lossy()}})).await;
    assert!(team.get("error").is_none(), "{team}");
    let list = call(&mut ws, &mut next, "team_list", json!({})).await;
    assert!(list["result"].as_array().unwrap().iter().any(|t| t["name"] == "Yerel"), "{list}");

    drop(ws);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
