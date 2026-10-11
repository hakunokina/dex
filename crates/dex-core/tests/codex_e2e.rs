//! DEX-26 live check: a leader delegates to a codex worker over the real Dex MCP endpoint; the
//! worker is started by dex-core without a connected client. Needs a logged-in codex CLI ≥ 0.154 and spends a few model
//! turns, so it only runs on request: `cargo test -p dex-core --test codex_e2e -- --ignored --nocapture`.

use dex_core::{Core, EventSink, Events, Paths};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Log {
    hooks: Mutex<Vec<(String, String, Option<String>)>>,
    out: Mutex<Vec<u8>>,
    session: Mutex<Option<String>>,
}

/// Records hook events only; no client participates in starting or driving the worker.
struct Front {
    log: Arc<Log>,
}

impl EventSink for Front {
    fn emit(&self, event: &str, p: Value) -> Result<(), String> {
        match event {
            "dex-hook" => {
                // Like agents.ts → session.ts: the SessionStart id is what ▶ / the next job resumes.
                if p["event"] == "SessionStart" {
                    *self.log.session.lock().unwrap() = p["sessionId"].as_str().map(String::from);
                }
                self.log.hooks.lock().unwrap().push((
                    p["agentId"].as_str().unwrap_or("").into(),
                    p["event"].as_str().unwrap_or("").into(),
                    p["status"].as_str().map(String::from),
                ))
            }
            _ => {}
        }
        Ok(())
    }
}

fn mcp(port: u16, token: &str, agent: &str, key: &str, body: Value) -> Value {
    let body = body.to_string();
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nX-Dex-Agent: {agent}\r\nX-Dex-Key: {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap_or("null")).unwrap_or(Value::Null)
}

#[test]
#[ignore]
fn codex_worker_takes_a_delegation() {
    let root = std::env::temp_dir().join(format!("dex-codex-e2e-{}", std::process::id()));
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = dex_core::teams::normalize_dir(&project.to_string_lossy()).unwrap();
    let memory = std::path::Path::new(&project).join(".dex").join("memory").to_string_lossy().into_owned();
    // A memory record the worker must see through the SessionStart index.
    let own_dir = std::path::Path::new(&memory).join("shared");
    std::fs::create_dir_all(&own_dir).unwrap();
    std::fs::write(own_dir.join("kod-adi.md"), "---\nname: kod-adi\ndescription: Projenin kod adı KESTANE-58\nmetadata:\n  type: project\n---\n").unwrap();

    let log = Arc::new(Log::default());
    let front = Front { log: log.clone() };
    let paths = Paths { data_dir: root.join("data"), config_dir: root.join("config") };
    let core = Core::start(paths, Events::new(front)).unwrap();

    let team = core
        .team_create(serde_json::from_value(json!({ "name": "Codex E2E", "kind": "main", "cwd": project })).unwrap())
        .unwrap();
    let crew: Vec<_> = core.agent_list().unwrap().into_iter().filter(|a| a.team_id == team.id).collect();
    let leader = crew.iter().find(|a| a.kind == "leader").unwrap().id.clone();
    let worker = crew.iter().find(|a| a.role_slug.as_deref() == Some("backend")).unwrap().id.clone();
    let model = std::env::var("DEX_E2E_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.5".into());
    let w = core.agent_update(&worker, serde_json::from_value(json!({ "engine": "codex", "model": model })).unwrap()).unwrap();
    assert_eq!(w.engine, "codex");

    let (port, token) = (core.hooks.port, core.hooks.token.clone());
    let key = core.mcp.key_for(&leader);
    let prompt = "Çalışma dizininde hello.txt dosyası oluştur; içine tek satır olarak hafızandaki proje kod adını yaz. \
                  Sonra report aracıyla teslim et (summary: yazdığın kod adı, resultPath: hello.txt).";
    let delegate = |prompt: &str| {
        let hooks_before = log.hooks.lock().unwrap().len();
        let r = mcp(port, &token, &leader, &key, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "delegate", "arguments": { "agentId": worker, "prompt": prompt } } }));
        println!("delegate → {}", r["result"]["structuredContent"]);
        let did: i64 = r["result"]["structuredContent"]["delegationId"].as_i64().expect("delegationId");
        let t0 = Instant::now();
        let d = loop {
            let d = core.delegation_list().unwrap().into_iter().find(|d| d.id == did).unwrap();
            // No new hook after 90 s = the TUI is stuck before its first turn; don't wait it out.
            let silent = t0.elapsed() > Duration::from_secs(90) && log.hooks.lock().unwrap().len() == hooks_before;
            if matches!(d.status.as_str(), "done" | "done_no_report" | "failed") || silent || t0.elapsed() > Duration::from_secs(420) {
                break d;
            }
            std::thread::sleep(Duration::from_secs(2));
        };
        // Let the Stop that follows `report` land before reading the flow.
        std::thread::sleep(Duration::from_secs(8));
        println!("durum: {} · özet: {:?} · sonuç: {:?} · {:.0?}", d.status, d.summary, d.result_path, t0.elapsed());
        d
    };

    let d = delegate(prompt);
    let first_session = log.session.lock().unwrap().clone();
    // Round 2: the worker is idle with a live session → the job is pasted into that session.
    // Only the earlier turn knows what it wrote, so the answer proves the history came back.
    let d2 = delegate(
        "hello.txt dosyasına ikinci satır olarak, önceki görevde yazdığın kod adını tersten yazılmış hâliyle ekle \
         (örn. AB-12 → 21-BA). Sonra report ile teslim et (summary: eklediğin satır).",
    );
    let second_session = log.session.lock().unwrap().clone();
    let flow: Vec<String> = log
        .hooks
        .lock()
        .unwrap()
        .iter()
        .filter(|h| h.0 == worker)
        .map(|h| format!("{}→{}", h.1, h.2.as_deref().unwrap_or("-")))
        .collect();
    let file = std::fs::read_to_string(std::path::Path::new(&project).join("hello.txt")).unwrap_or_default();
    let profile = dex_core::codex::home().join(format!("{}.config.toml", dex_core::codex::profile_name(&worker)));
    println!("hello.txt: {file:?}");
    println!("hook akışı: {}", flow.join(", "));
    println!("oturum: {first_session:?} → {second_session:?}");
    println!("profil var: {}", profile.is_file());
    if d.status != "done" || d2.status != "done" {
        let dump = root.join("pty-out.txt");
        let _ = std::fs::write(&dump, &*log.out.lock().unwrap());
        println!("pty çıktısı: {}", dump.display());
    }

    core.ptys.kill(&worker);
    core.shutdown();
    assert!(!profile.exists(), "profil çıkışta silinmeli");
    assert_eq!(d.status, "done");
    assert!(file.contains("KESTANE-58"), "hafıza indeksi SessionStart ile gelmeli: {file:?}");
    assert_eq!(d2.status, "done");
    assert!(file.contains("85-ENATSEK"), "resume geçmişi taşımalı: {file:?}");
    assert!(first_session.is_some() && first_session == second_session, "resume aynı oturumu sürdürmeli");
    for want in ["SessionStart→idle", "UserPromptSubmit→working", "PreToolUse→working", "Stop→idle"] {
        assert!(flow.iter().any(|f| f == want), "{want} yok: {flow:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
