//! The real binary in a temp dir: `dexd pair` → `dexd serve` → a WS client pairs, creates a
//! team, opens a shell PTY, sees its output (and a second connection attaches), kills it; then
//! the device is revoked with the CLI.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const BIN: &str = env!("CARGO_BIN_EXE_dexd");

fn cli(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).arg("--data-dir").arg(dir).output().unwrap();
    assert!(out.status.success(), "dexd {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(dir: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .args(["serve", "--bind", "127.0.0.1:0", "--secrets", "file", "--data-dir"])
        .arg(dir)
        .arg("--root")
        .arg(dir.join("projects"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let url = line.trim().strip_prefix("dexd: ").unwrap_or_else(|| panic!("ilk satır: {line:?}")).to_string();
    (Server(child), url)
}

struct Client {
    ws: Ws,
    next: u64,
    /// Events and PTY frames that arrived while waiting for a reply.
    events: Vec<(String, Value)>,
    pty: Vec<(u32, Vec<u8>)>,
}

impl Client {
    async fn connect(url: &str) -> Self {
        Self { ws: connect_async(url).await.unwrap().0, next: 0, events: vec![], pty: vec![] }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({"id": id, "method": method, "params": params}).to_string().into())).await.unwrap();
        loop {
            match self.recv().await {
                Some(Message::Text(t)) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap();
                    if v["id"] == id {
                        return match v.get("error") {
                            Some(e) => Err(e.clone()),
                            None => Ok(v["result"].clone()),
                        };
                    }
                    self.stash(v);
                }
                Some(Message::Binary(b)) => self.pty.push((u32::from_be_bytes(b[..4].try_into().unwrap()), b[4..].to_vec())),
                Some(_) => {}
                None => panic!("bağlantı kapandı ({method})"),
            }
        }
    }

    fn stash(&mut self, v: Value) {
        if let Some(e) = v["event"].as_str() {
            self.events.push((e.to_string(), v["payload"].clone()));
        }
    }

    async fn recv(&mut self) -> Option<Message> {
        timeout(Duration::from_secs(30), self.ws.next()).await.expect("30 sn içinde çerçeve yok").map(Result::unwrap)
    }

    /// Reads until `done(output of channel)` or the deadline; answers ConPTY's cursor query.
    async fn pty_until(&mut self, channel: u32, done: impl Fn(&str) -> bool, probe: Option<&[u8]>) -> String {
        let mut out: String = self.pty.drain(..).filter(|(c, _)| *c == channel).map(|(_, b)| String::from_utf8_lossy(&b).into_owned()).collect();
        let mut sent = probe.is_none();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(&out) && Instant::now() < deadline {
            match timeout(Duration::from_millis(400), self.ws.next()).await {
                Ok(Some(Ok(Message::Binary(b)))) => {
                    let (c, body) = (u32::from_be_bytes(b[..4].try_into().unwrap()), &b[4..]);
                    if c != channel {
                        continue;
                    }
                    let chunk = String::from_utf8_lossy(body);
                    if chunk.contains("\x1b[6n") {
                        self.write(channel, b"\x1b[1;1R").await;
                    }
                    out.push_str(&chunk);
                }
                Ok(Some(Ok(Message::Text(t)))) => self.stash(serde_json::from_str(t.as_str()).unwrap()),
                Ok(other) => panic!("beklenmeyen: {other:?}"),
                // Quiet: the prompt is up — type the probe once.
                Err(_) if !sent && !out.is_empty() => {
                    sent = true;
                    self.write(channel, probe.unwrap()).await;
                }
                Err(_) => {}
            }
        }
        out
    }

    async fn write(&mut self, channel: u32, bytes: &[u8]) {
        let mut f = channel.to_be_bytes().to_vec();
        f.extend_from_slice(bytes);
        self.ws.send(Message::Binary(f.into())).await.unwrap();
    }

    async fn event(&mut self, name: &str) -> Value {
        loop {
            if let Some(i) = self.events.iter().position(|(e, _)| e == name) {
                return self.events.remove(i).1;
            }
            match self.recv().await {
                Some(Message::Text(t)) => self.stash(serde_json::from_str(t.as_str()).unwrap()),
                Some(_) => {}
                None => panic!("bağlantı kapandı ({name} beklenirken)"),
            }
        }
    }
}

#[cfg(windows)]
const PROBE: &[u8] = b"echo \"MARK[$(6*7)]\"\r";
#[cfg(not(windows))]
const PROBE: &[u8] = b"echo \"MARK[$((6*7))]\"\r";
#[cfg(windows)]
const PROBE2: &[u8] = b"echo \"MARK[$(7*7)]\"\r";
#[cfg(not(windows))]
const PROBE2: &[u8] = b"echo \"MARK[$((7*7))]\"\r";

#[tokio::test(flavor = "multi_thread")]
async fn pair_team_shell_pty_revoke() {
    let dir = std::env::temp_dir().join(format!("dexd-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let code = cli(&dir, &["pair"]).trim().to_string();
    let (_srv, url) = start(&dir);

    // No token → the first request is refused and the socket closed with 4001.
    let mut anon = Client::connect(&url).await;
    let e = anon.call("team_list", Value::Null).await.unwrap_err();
    assert_eq!(e["code"], "unauthorized");
    match anon.recv().await {
        Some(Message::Close(Some(f))) => assert_eq!(u16::from(f.code), 4001),
        other => panic!("kapanış yok: {other:?}"),
    }

    // Wrong protocol version is refused before the code is spent.
    let mut c = Client::connect(&url).await;
    let e = c.call("auth.pair", json!({"protocol": 99, "code": code, "name": "e2e"})).await.unwrap_err();
    assert_eq!(e["code"], "protocol");

    let mut c = Client::connect(&url).await;
    let hello = c.call("auth.pair", json!({"protocol": 1, "code": code, "name": "e2e"})).await.unwrap();
    let token = hello["token"].as_str().unwrap().to_string();
    assert_eq!((hello["protocol"].as_u64(), hello["device"]["name"].as_str()), (Some(1), Some("e2e")));

    // The code is spent; the token opens a new connection.
    let mut again = Client::connect(&url).await;
    assert_eq!(again.call("auth.pair", json!({"protocol": 1, "code": code})).await.unwrap_err()["code"], "unauthorized");
    let mut c2 = Client::connect(&url).await;
    let hello = c2.call("auth.hello", json!({"protocol": 1, "token": token})).await.unwrap();
    let device = hello["device"]["id"].as_str().unwrap().to_string();

    // #142: the build and its method list ride along with hello; `server_info` repeats them.
    let methods: Vec<&str> = hello["methods"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    for m in ["memory_graph", "chat_history", "model_news", "pty_spawn", "fs.list", "server_info"] {
        assert!(methods.contains(&m), "{m} yok: {methods:?}");
    }
    assert!(!hello["commit"].as_str().unwrap().is_empty() && hello["commitTime"].is_i64() && hello["version"].is_string());
    let info = c2.call("server_info", Value::Null).await.unwrap();
    assert_eq!((&info["commit"], &info["methods"]), (&hello["commit"], &hello["methods"]));

    // FIX-5: a fresh server starts empty — no seeded default team; the client creates teams.
    assert_eq!(c.call("team_list", Value::Null).await.unwrap(), Value::Array(vec![]));

    // A Tauri command, same name and argument shape; its event reaches every connection.
    let team = c.call("team_create", json!({"team": {"name": "E2E", "kind": "service"}})).await.unwrap();
    assert_eq!(team["kind"], "service");
    assert_eq!(c.event("teams-changed").await, Value::Null);
    assert_eq!(c2.event("teams-changed").await, Value::Null);
    let teams = c.call("team_list", Value::Null).await.unwrap();
    assert!(teams.as_array().unwrap().iter().any(|t| t["id"] == team["id"]));

    // S4: the folder picker sees only the roots; a team folder outside them is refused.
    let top = c.call("fs.list", json!({"path": null})).await.unwrap();
    let root = top["entries"][0]["path"].as_str().unwrap().to_string();
    let made = c.call("fs.mkdir", json!({"parent": root, "name": "uzak-proje"})).await.unwrap();
    let inside = c.call("fs.list", json!({"path": root})).await.unwrap();
    assert_eq!(inside["entries"][0]["name"], "uzak-proje");
    assert_eq!(inside["parent"], Value::Null);
    let out = dir.to_string_lossy();
    assert_eq!(c.call("fs.list", json!({"path": out})).await.unwrap_err()["code"], "failed");
    let refused = c.call("team_create", json!({"team": {"name": "Dışarı", "kind": "main", "cwd": out}})).await.unwrap_err();
    assert!(refused["message"].as_str().unwrap().contains("izinli"), "{refused}");
    let main = c.call("team_create", json!({"team": {"name": "Uzak", "kind": "main", "cwd": made}})).await.unwrap();
    assert_eq!(main["cwd"], made);

    // S4: `--secrets file` → a provider token is a 0600 file under the data dir, never the keyring.
    c.call("provider_set_token", json!({"id": "glm", "token": "e2e-token-0123456789"})).await.unwrap();
    let on_disk = std::fs::read_to_string(dir.join("secrets").join("dex-provider.glm")).unwrap();
    assert_eq!(on_disk, "e2e-token-0123456789");
    let glm = c.call("provider_list", Value::Null).await.unwrap();
    let glm = glm.as_array().unwrap().iter().find(|p| p["id"] == "glm").unwrap().clone();
    assert_eq!((glm["hasToken"].as_bool(), glm["tokenMask"].as_str()), (Some(true), Some("e2e-…6789")));

    // A shell on channel 1; output comes back as binary frames on it.
    let spawn = json!({"id": "sh-e2e", "cmd": "shell", "cwd": team["workdir"], "cols": 120, "rows": 30, "channel": 1});
    assert_eq!(c.call("pty_spawn", spawn).await.unwrap(), Value::Null);
    let out = c.pty_until(1, |o| o.contains("MARK[42]"), Some(PROBE)).await;
    assert!(out.contains("MARK[42]"), "kabuk çıktısı yok:\n{out:?}");
    let listed = c.call("pty.list", Value::Null).await.unwrap();
    let listed = listed.as_array().unwrap().iter().find(|p| p["id"] == "sh-e2e").unwrap();
    assert_eq!((listed["kind"].as_str(), listed["alive"].as_bool()), (Some("shell"), Some(true)));
    assert_eq!((listed["cols"].as_u64(), listed["rows"].as_u64()), (Some(120), Some(30)));

    // A second connection attaches (live stream; the S3 snapshot is still null) and sees new output.
    let attached = c2.call("pty.attach", json!({"id": "sh-e2e", "channel": 9})).await.unwrap();
    assert!(attached["subscriber"].as_u64().is_some());
    assert_eq!((attached["snapshot"]["cols"].as_u64(), attached["snapshot"]["rows"].as_u64()), (Some(120), Some(30)));
    let snapshot = attached["snapshot"]["ansi"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect::<Vec<_>>();
    assert!(String::from_utf8_lossy(&snapshot).contains("MARK[42]"));
    c.write(1, PROBE2).await;
    let seen = c2.pty_until(9, |o| o.contains("MARK[49]"), None).await;
    assert!(seen.contains("MARK[49]"), "ikinci bağlantı çıktı görmedi:\n{seen:?}");

    // Kill → both channels hear the exit, the list empties.
    c.call("pty_kill", json!({"id": "sh-e2e"})).await.unwrap();
    assert_eq!(c.event("pty.exit").await, json!({"channel": 1, "id": "sh-e2e"}));
    assert_eq!(c2.event("pty.exit").await, json!({"channel": 9, "id": "sh-e2e"}));
    assert_eq!(c.call("pty.list", Value::Null).await.unwrap(), json!([]));

    // Revoked with the CLI → the token no longer opens a connection.
    assert!(cli(&dir, &["devices", "list"]).contains(&device));
    cli(&dir, &["devices", "revoke", &device]);
    let mut late = Client::connect(&url).await;
    assert_eq!(late.call("auth.hello", json!({"protocol": 1, "token": token})).await.unwrap_err()["code"], "unauthorized");
}
