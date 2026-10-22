//! #146 (A0): the desktop's loopback endpoint (`Server::local`) in-process — only its own token
//! opens it (#2288: proved over a challenge, never sent); no pairing, no device tokens; a team
//! folder outside the roots is fine (folder picker).

use dex_core::events::NullSink;
use dex_core::{Core, Events, Paths};
use dexd::auth::LocalToken;
use dexd::local_auth;
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

/// #2288: a valid `auth.hello` (no token) → the server's challenge and proof; then `method` as the
/// second frame. Returns the second reply, the close code if the socket was closed, and the offer.
async fn second_frame(url: &str, method: &str, params: impl FnOnce(&Value) -> Value) -> (Value, Option<u16>, Value) {
    let (mut ws, _) = connect_async(url).await.unwrap();
    let mut next = 0;
    let offer = call(&mut ws, &mut next, "auth.hello", json!({"protocol": 1})).await;
    assert!(offer["result"]["challenge"].is_string() && offer["result"]["proof"].is_string(), "{offer}");
    let params = params(&offer["result"]);
    ws.send(Message::Text(json!({"id": 2, "method": method, "params": params}).to_string().into())).await.unwrap();
    let mut reply = Value::Null;
    loop {
        match timeout(Duration::from_secs(10), ws.next()).await.expect("10 sn içinde çerçeve yok") {
            Some(Ok(Message::Text(t))) => {
                reply = serde_json::from_str(t.as_str()).unwrap();
                if reply.get("error").is_none() {
                    return (reply, None, offer);
                }
            }
            Some(Ok(Message::Close(f))) => return (reply, f.map(|f| u16::from(f.code)), offer),
            Some(Ok(_)) => {}
            _ => return (reply, None, offer),
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
    let port = listener.local_addr().unwrap().port();
    let url = format!("ws://127.0.0.1:{port}");
    tokio::spawn(dexd::serve(listener, srv));

    // The first frame: only a token-less `auth.hello` goes on. #2288: the raw token is never
    // accepted — the old plain hello is refused outright, right or wrong.
    for (method, params) in [
        ("auth.hello", json!({"protocol": 1, "token": token})),
        ("auth.hello", json!({"protocol": 1, "token": "0".repeat(64)})),
        ("auth.hello", json!({"protocol": 1, "token": ""})),
        ("auth.proof", json!({"proof": "0".repeat(64)})),
        // No pairing here: the code would never be checked, the endpoint has one credential.
        ("auth.pair", json!({"protocol": 1, "code": "ABCD-EFGH", "name": "x"})),
        ("team_list", json!({})),
    ] {
        let (reply, close) = first_frame(&url, method, params).await;
        assert_eq!(reply["error"]["code"], "unauthorized", "{method}: {reply}");
        assert_eq!(close, Some(CLOSE_UNAUTHORIZED), "{method}");
    }
    let (reply, _) = first_frame(&url, "auth.hello", json!({"protocol": 2})).await;
    assert_eq!(reply["error"]["code"], "protocol", "{reply}");

    // The second frame: a proof over this challenge, this port and this token — nothing else.
    let key = local_auth::key(&token);
    let other = local_auth::key(&LocalToken::new().unwrap().1);
    let raw = token.clone();
    let c = |offer: &Value| local_auth::unhex32(offer["challenge"].as_str().unwrap()).unwrap();
    let wrong: [(&str, Box<dyn Fn(&Value) -> Value>); 6] = [
        ("stale challenge", Box::new(move |_| json!({"proof": local_auth::hex(&local_auth::client_proof(&key, &[0; 32], port))}))),
        ("another port: a relay", Box::new(move |o| json!({"proof": local_auth::hex(&local_auth::client_proof(&key, &c(o), port.wrapping_add(1)))}))),
        ("another token", Box::new(move |o| json!({"proof": local_auth::hex(&local_auth::client_proof(&other, &c(o), port))}))),
        ("the server's own proof echoed", Box::new(|o| json!({"proof": o["proof"]}))),
        ("the raw token", Box::new(move |_| json!({"proof": raw.clone()}))),
        ("no proof", Box::new(|_| json!({}))),
    ];
    for (what, params) in wrong {
        let (reply, close, _) = second_frame(&url, "auth.proof", params).await;
        assert!(matches!(reply["error"]["code"].as_str(), Some("unauthorized" | "bad_request")), "{what}: {reply}");
        assert_eq!(close, Some(CLOSE_UNAUTHORIZED), "{what}");
    }
    let (reply, close, _) = second_frame(&url, "team_list", |_| json!({})).await;
    assert_eq!((reply["error"]["code"].as_str(), close), (Some("unauthorized"), Some(CLOSE_UNAUTHORIZED)), "{reply}");
    // The server's proof: a client with another token (or dialing another port) sends nothing.
    let (mut ws, _) = connect_async(&url).await.unwrap();
    assert!(dexd::local::client_handshake(&mut ws, &LocalToken::new().unwrap().1, port, false).await.is_err());
    let (mut ws, _) = connect_async(&url).await.unwrap();
    assert!(dexd::local::client_handshake(&mut ws, &token, port.wrapping_add(1), false).await.is_err());

    let (mut ws, _) = connect_async(&url).await.unwrap();
    let hello = dexd::local::client_handshake(&mut ws, &token, port, false).await.unwrap();
    assert_eq!(hello["result"]["device"]["id"], "local", "{hello}");
    let methods = hello["result"]["methods"].as_array().unwrap();
    assert!(methods.iter().any(|m| m == "team_list") && methods.iter().any(|m| m == "pty_spawn"), "{hello}");
    let mut next = 2;
    assert_eq!(call(&mut ws, &mut next, "auth.hello", json!({"protocol": 1})).await["error"]["code"], "bad_request");

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
