//! Real Codex CLI + real Dex permit receiver, with a local Responses fixture (no account/network).
//! Explicit: cargo test -p dex-core --test codex_policy_smoke -- --ignored --nocapture
use dex_core::{codex, Core, Events, Paths};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn provider(commands: Vec<String>) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for turn in 0..=commands.len() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(s) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(e) => panic!("fixture accept: {e}"),
                }
            };
            stream.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let size = headers.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().unwrap())
                    }).unwrap();
                    if request.len() >= end + 4 + size { break; }
                }
            }
            let item = if let Some(cmd) = commands.get(turn) {
                json!({"type":"function_call","id":format!("fc_{turn}"),"call_id":format!("call_{turn}"),
                    "name":"exec_command","arguments":json!({"cmd":cmd,"login":false,"max_output_tokens":1000}).to_string()})
            } else {
                json!({"type":"message","id":"msg_done","role":"assistant","content":[{"type":"output_text","text":"Smoke complete.","annotations":[]}]})
            };
            let response = json!({"id":format!("resp_{turn}"),"object":"response","status":"completed","output":[item.clone()],
                "usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20}});
            let mut created = response.clone(); created["status"] = "in_progress".into(); created["output"] = json!([]);
            let events = [
                json!({"type":"response.created","response":created}),
                json!({"type":"response.output_item.added","output_index":0,"item":item.clone()}),
                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                json!({"type":"response.completed","response":response}),
            ];
            let body: String = events.iter().map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap())).collect();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (port, server)
}

#[test]
#[ignore = "requires installed Codex 0.160 and pnpm; runs real tools in a scratch repository"]
fn codex_full_access_keeps_dex_denies() {
    let root = std::env::current_dir().unwrap().join("../../.dex/tmp").join(format!("codex-policy-{}", std::process::id()));
    let project = root.join("project");
    let home = root.join("codex-home");
    std::fs::create_dir_all(project.join(".codex")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let project = PathBuf::from(dex_core::teams::normalize_dir(&project.to_string_lossy()).unwrap());
    assert!(Command::new("git").args(["init", "--quiet"]).arg(&project).status().unwrap().success());
    let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(dex_core::events::NullSink)).unwrap();
    {
        let conn = core.db.0.lock().unwrap();
        conn.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [project.to_string_lossy()]).unwrap();
        conn.execute("UPDATE agents SET engine = 'codex' WHERE id IN ('ayla', 'timur')", []).unwrap();
    }
    let bin = codex::resolve("codex");
    assert!(Command::new(&bin).arg("--version").output().unwrap().status.success());
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if cfg!(windows) {
        dirs.insert(0, PathBuf::from("C:/Program Files/nodejs"));
        if let Some(appdata) = std::env::var_os("APPDATA") { dirs.insert(0, PathBuf::from(appdata).join("npm")); }
    }
    for agent in ["ayla", "timur"] {
        let worker = agent == "ayla";
        if worker { core.hooks.fail_closed.lock().unwrap().insert(agent.into()); }
        let (profile, _) = codex::write_profile(&home, agent, &codex::Profile { worker, instructions: "", port: core.hooks.port, browser: None, trust: project.to_str().into_iter().collect(), migration: None }).unwrap();
        // Exercise the real generated permit hook without session/Stop delivery side effects.
        let profile_path = home.join(format!("{profile}.config.toml"));
        let text = std::fs::read_to_string(&profile_path).unwrap();
        let pre = text.split("[[hooks.PreToolUse]]").nth(1).unwrap().split("[[hooks.PostToolUse]]").next().unwrap();
        std::fs::write(&profile_path, format!("{}[[hooks.PreToolUse]]{pre}", text.split("[[hooks.SessionStart]]").next().unwrap())).unwrap();
        let protected = ["AGENTS.md", "CLAUDE.md", ".codex/config.toml", ".git/config"];
        let commands = std::iter::once("pnpm --version".into()).chain(std::iter::once("git status --short".into()))
            .chain(protected.iter().map(|p| format!("echo DEX_BAD > {p}"))).collect();
        let (port, server) = provider(commands);
        let mut child = Command::new(&bin).args(["-a", "never", "exec", "--ephemeral", "--skip-git-repo-check", "--enable", "hooks", "-s", "danger-full-access", "--dangerously-bypass-hook-trust", "--json", "-m", "gpt-6.1-sol", "-p", &profile])
            .args(["-c", "model_provider=\"smoke\"", "-c", &format!("model_providers.smoke={{name=\"smoke\",base_url=\"http://127.0.0.1:{port}\",wire_api=\"responses\",requires_openai_auth=false}}")])
            .arg("Run the fixture tools.").current_dir(&project)
            .env("CODEX_HOME", &home).env("PATH", std::env::join_paths(&dirs).unwrap())
            .env("DEX_HOOK_TOKEN", &core.hooks.token).env("DEX_AGENT_ID", agent).env("DEX_MCP_KEY", core.mcp.key_for(agent))
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout = std::thread::spawn(move || { let mut b = Vec::new(); stdout.read_to_end(&mut b).unwrap(); b });
        let stderr = std::thread::spawn(move || { let mut b = Vec::new(); stderr.read_to_end(&mut b).unwrap(); b });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline { child.kill().unwrap(); break; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let output = std::process::Output { status: child.wait().unwrap(), stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() };
        server.join().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let executions: Vec<_> = rows.iter().filter(|r| r["type"] == "item.completed" && r["item"]["type"] == "command_execution").collect();
        assert_eq!(executions.len(), 2, "only pnpm and git may execute: stdout={} stderr={}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        for row in executions {
            assert_eq!(row["item"]["exit_code"], 0, "{row}");
            assert!(!row["item"]["aggregated_output"].as_str().unwrap_or("").contains("not recognized"), "{row}");
        }
        assert!(!project.join("AGENTS.md").exists() && !project.join("CLAUDE.md").exists() && !project.join(".codex/config.toml").exists());
        assert!(!std::fs::read_to_string(project.join(".git/config")).unwrap().contains("DEX_BAD"));
        assert_eq!(String::from_utf8_lossy(&output.stderr).matches("Command blocked by PreToolUse hook: Dex politikası").count(), protected.len());
        println!("{agent}: pnpm/git passed; four protected writes denied");
    }
    core.shutdown();
}
