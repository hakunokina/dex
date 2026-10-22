//! #150 (B): the real `dexd --local` binary as a process (tests/local.rs: A0's in-process endpoint)
//! — discovery, token, one per data dir, the no-window grace (shortened with `--idle-exit`),
//! `core_shutdown` / `core_restart`.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn data_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dexd-local-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(dir: &Path, idle: u64) -> Child {
    Command::new(env!("CARGO_BIN_EXE_dexd"))
        .args(["--local", "--data-dir"])
        .arg(dir)
        .args(["--idle-exit", &idle.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn discovery(dir: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(dir.join("dexd.local.json")).ok()?).ok()
}

/// The discovery file of process `pid` (or any, `None`), within 30 s.
fn wait_discovery(dir: &Path, pid: Option<u32>) -> Value {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(d) = discovery(dir).filter(|d| pid.is_none_or(|p| d["pid"] == p)) {
            return d;
        }
        assert!(Instant::now() < until, "no discovery in {}", dir.display());
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// #2288: the token is read from its fixed name under the data dir — never from a path the
/// discovery file names (the shell does the same).
fn token(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("dexd.local.token")).unwrap()
}

/// #2288: the endpoint is `127.0.0.1:<port>` from the discovery's numeric `port` only.
fn port_of(d: &Value) -> u16 {
    u16::try_from(d["port"].as_u64().unwrap()).unwrap()
}

fn exited_within(child: &mut Child, secs: u64) -> Option<std::process::ExitStatus> {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// The challenge handshake (#2288) against the core `d` describes; the final reply with the socket.
async fn connect(d: &Value, token: &str, probe: bool) -> (Ws, Value) {
    let port = port_of(d);
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}")).await.unwrap();
    let r = dexd::local::client_handshake(&mut ws, token, port, probe).await.unwrap();
    (ws, r)
}

async fn reply(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v.get("id").is_some() {
                    return v;
                }
            }
            Some(Ok(_)) => {}
            other => panic!("closed: {other:?}"),
        }
    }
}

async fn call(ws: &mut Ws, method: &str) -> Value {
    ws.send(Message::Text(json!({"id": 2, "method": method, "params": {}}).to_string().into())).await.unwrap();
    reply(ws).await
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_token_and_one_instance_per_data_dir() {
    let dir = data_dir("disc");
    let mut first = start(&dir, 60);
    let d = wait_discovery(&dir, Some(first.id()));
    let url = d["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("ws://127.0.0.1:"), "{d}");
    assert_eq!(d["port"].as_u64().unwrap().to_string(), url.rsplit(':').next().unwrap());
    assert!(d["commit"].is_string() && d["commitTime"].is_i64() && d["startedAt"].is_i64(), "{d}");
    assert!(d.get("token").is_none(), "the token has its own file: {d}");
    let tok = token(&dir);
    assert!(tok.len() == 64 && tok.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(d["tokenFile"].as_str().map(Path::new), Some(dir.join("dexd.local.token").as_path()), "{d}");

    let (mut ws, ok) = connect(&d, &tok, false).await;
    assert_eq!(ok["result"]["device"]["id"], "local", "{ok}");
    let methods = ok["result"]["methods"].as_array().unwrap();
    for m in ["core_shutdown", "core_restart", "core_life", "tailnet_get", "team_list"] {
        assert!(methods.iter().any(|v| v == m), "{m}");
    }
    // Seeded like the app's core (not `start_bare`).
    let teams = call(&mut ws, "team_list").await;
    assert!(!teams["result"].as_array().unwrap().is_empty(), "{teams}");
    let tn = call(&mut ws, "tailnet_get").await;
    assert_eq!(tn["result"]["enabled"], false, "{tn}");
    // The tray / Ayarlar: this window counts, so no countdown.
    let life = call(&mut ws, "core_life").await;
    let l = &life["result"];
    assert_eq!(l["pid"], first.id(), "{life}");
    assert_eq!((l["clients"].as_u64(), l["agents"].as_u64(), l["graceSecs"].as_u64()), (Some(1), Some(0), Some(60)), "{life}");
    assert!(l["shutdownInSecs"].is_null() && l["commit"] == d["commit"], "{life}");
    // #2288: a wrong token never gets past the server's proof (the client sends nothing more);
    // the raw token as the first frame, or a forged proof, is refused by the server.
    for bad in ["", "x", &tok[..63]] {
        let (mut wrong, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert!(dexd::local::client_handshake(&mut wrong, bad, port_of(&d), false).await.is_err(), "{bad:?}");
    }
    let (mut plain, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    plain.send(Message::Text(json!({"id": 1, "method": "auth.hello", "params": {"protocol": 1, "token": tok}}).to_string().into())).await.unwrap();
    assert_eq!(reply(&mut plain).await["error"]["code"], "unauthorized", "the plain hello is gone");
    let (mut forged, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    forged.send(Message::Text(json!({"id": 1, "method": "auth.hello", "params": {"protocol": 1}}).to_string().into())).await.unwrap();
    assert!(reply(&mut forged).await["result"]["challenge"].is_string());
    forged.send(Message::Text(json!({"id": 2, "method": "auth.proof", "params": {"proof": "0".repeat(64)}}).to_string().into())).await.unwrap();
    assert_eq!(reply(&mut forged).await["error"]["code"], "unauthorized", "a forged proof");
    // #192 kept: a page in the user's browser is refused at the handshake.
    let mut req = url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());

    // A second core on the same data dir stops at dex.lock; the first's discovery stays.
    let second = Command::new(env!("CARGO_BIN_EXE_dexd")).args(["--local", "--data-dir"]).arg(&dir).output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("başka bir Dex"), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(discovery(&dir).unwrap()["pid"], first.id());

    // core_shutdown: answered, then the process exits and takes its files with it.
    let r = call(&mut ws, "core_shutdown").await;
    assert!(r.get("error").is_none(), "{r}");
    assert!(exited_within(&mut first, 15).is_some_and(|s| s.success()), "core_shutdown");
    assert!(discovery(&dir).is_none() && !dir.join("dexd.local.token").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn lives_while_a_window_is_connected_and_exits_after_the_grace() {
    let dir = data_dir("life");
    let mut child = start(&dir, 3);
    let d = wait_discovery(&dir, Some(child.id()));
    let tok = token(&dir);

    // First window, kept past the grace.
    let (ws, _) = connect(&d, &tok, false).await;
    std::thread::sleep(Duration::from_secs(5));
    assert!(child.try_wait().unwrap().is_none(), "a connected window keeps it");
    // The window reloads: gone for a moment, back inside the grace.
    drop(ws);
    std::thread::sleep(Duration::from_secs(1));
    let (ws, r) = connect(&d, &tok, false).await;
    assert_eq!(r["result"]["device"]["id"], "local");
    std::thread::sleep(Duration::from_secs(5));
    assert!(child.try_wait().unwrap().is_none(), "came back in time");
    // Probes (the shell checking it is up) do not keep it.
    drop(ws);
    let started = Instant::now();
    for _ in 0..4 {
        let (_p, r) = connect(&d, &tok, true).await;
        assert_eq!(r["result"]["device"]["id"], "local");
        std::thread::sleep(Duration::from_millis(500));
    }
    let status = exited_within(&mut child, 15).expect("exits after the grace");
    assert!(status.success());
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(3) && took < Duration::from_secs(12), "{took:?}");
    assert!(discovery(&dir).is_none(), "discovery removed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn core_restart_brings_up_a_new_instance_on_the_same_data_dir() {
    let dir = data_dir("restart");
    let mut child = start(&dir, 60);
    let d = wait_discovery(&dir, Some(child.id()));
    let old_token = token(&dir);
    let (mut ws, _) = connect(&d, &old_token, false).await;
    let r = call(&mut ws, "core_restart").await;
    assert!(r.get("error").is_none(), "{r}");
    assert!(exited_within(&mut child, 15).is_some(), "the old one exits");
    // A new pid, a new token; it took the data dir over (dex.lock) once the old one let go.
    let until = Instant::now() + Duration::from_secs(30);
    let new = loop {
        if let Some(n) = discovery(&dir).filter(|n| n["pid"] != child.id()) {
            break n;
        }
        assert!(Instant::now() < until, "no new instance");
        std::thread::sleep(Duration::from_millis(100));
    };
    let tok = token(&dir);
    assert_ne!(tok, old_token, "fresh token");
    let (mut ws, ok) = connect(&new, &tok, false).await;
    assert_eq!(ok["result"]["device"]["id"], "local", "{ok}");
    // Clean up the detached one.
    let r = call(&mut ws, "core_shutdown").await;
    assert!(r.get("error").is_none(), "{r}");
    let until = Instant::now() + Duration::from_secs(15);
    while discovery(&dir).is_some() {
        assert!(Instant::now() < until, "restarted instance did not shut down");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&dir);
}

async fn call_with(ws: &mut Ws, method: &str, params: Value) -> Value {
    ws.send(Message::Text(json!({"id": 3, "method": method, "params": params}).to_string().into())).await.unwrap();
    reply(ws).await
}

fn job_status(dir: &Path, id: i64) -> String {
    let c = rusqlite::Connection::open(dir.join("dex.db")).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c.query_row("SELECT status FROM delegations WHERE id = ?1", [id], |r| r.get(0)).unwrap()
}

fn wait_new(dir: &Path, old: u32) -> Value {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(n) = discovery(dir).filter(|n| n["pid"] != old) {
            return n;
        }
        assert!(Instant::now() < until, "no new instance");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// #161 (C): `core_restart {whenIdle, exe}` drains (nothing runs here: it goes after two looks),
/// comes back as the given build (staged under the shell's copy name) and keeps the queue; the
/// patron's `core_shutdown` then a plain start fails it as before.
#[tokio::test(flavor = "multi_thread")]
async fn drained_restart_comes_back_as_the_new_build_and_keeps_the_queue() {
    let dir = data_dir("drain");
    let mut child = start(&dir, 60);
    let d = wait_discovery(&dir, Some(child.id()));
    assert_eq!(d["exe"], format!("dexd{}", std::env::consts::EXE_SUFFIX), "{d}");
    let (mut ws, ok) = connect(&d, &token(&dir), false).await;
    assert!(ok["result"]["methods"].as_array().unwrap().iter().any(|m| m == "core_drain_cancel"), "{ok}");
    // A job for a worker that never takes it (no such agent): it stays queued wherever it is.
    let job = {
        let c = rusqlite::Connection::open(dir.join("dex.db")).unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c.execute("INSERT INTO delegations (from_agent, to_agent, team_id, prompt, created_at, updated_at) VALUES ('timur','nobody','default','x',0,0)", []).unwrap();
        c.last_insert_rowid()
    };
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_dexd"));
    let r = call_with(&mut ws, "core_restart", json!({"exe": bin, "whenIdle": true})).await;
    assert_eq!(r["result"]["draining"], true, "{r}");
    let life = call(&mut ws, "core_life").await;
    // It may already be through its two looks; while it drains it says so.
    if life.get("result").is_some() {
        assert!(life["result"]["drainBlockers"].as_array().unwrap().is_empty(), "{life}");
    }
    assert!(exited_within(&mut child, 15).is_some(), "drained → exits");
    let new = wait_new(&dir, child.id());
    let staged = bin.with_file_name(new["exe"].as_str().unwrap());
    assert!(new["exe"].as_str().unwrap().starts_with("dexd-") && staged.exists(), "came back as the staged build: {new}");
    assert_eq!(job_status(&dir, job), "queued", "a restart keeps the queue");
    assert!(!dir.join("dex.restart.json").exists(), "the new core took the marker");

    // The patron's "Çekirdeği kapat": no marker — a later start fails the queue.
    let (mut ws, _) = connect(&new, &token(&dir), false).await;
    let r = call(&mut ws, "core_shutdown").await;
    assert!(r.get("error").is_none(), "{r}");
    let until = Instant::now() + Duration::from_secs(15);
    while discovery(&dir).is_some() {
        assert!(Instant::now() < until, "did not shut down");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(!dir.join("dex.restart.json").exists(), "no marker on a shutdown");
    let mut again = start(&dir, 60);
    let d = wait_discovery(&dir, Some(again.id()));
    assert_eq!(job_status(&dir, job), "failed", "a plain start fails the queue");
    let (mut ws, _) = connect(&d, &token(&dir), false).await;
    call(&mut ws, "core_shutdown").await;
    assert!(exited_within(&mut again, 15).is_some());
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir_all(&dir);
}
