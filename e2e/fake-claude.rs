// Stand-in for the `claude` CLI in e2e runs: dexd starts it for an agent pane (routes.conf `real =`
// points here). No network beyond Dex's loopback hook: like the real claude, it fires SessionStart
// once it has drawn (else Dex's pre-session watch reads the silent screen as a dialog and turns the
// agent `waiting` after 2 s), and no other hook. The console echoes typed keys (cooked input); every
// line that reaches the process comes back as `RX:<line>` — so a test can tell what got into the PTY.
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// POSTs SessionStart to Dex's hook receiver with the pane's env (pty.rs sets it for `claude`);
/// without it (a pane Dex started as a plain command) there is nothing to tell.
fn session_start() {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(port), Some(token), Some(agent)) = (env("DEX_HOOK_PORT"), env("DEX_HOOK_TOKEN"), env("DEX_AGENT_ID")) else { return };
    let key = env("DEX_MCP_KEY").unwrap_or_default();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let body = format!(
        r#"{{"hook_event_name":"SessionStart","source":"startup","session_id":"fake-{}-{nanos}","cwd":""}}"#,
        std::process::id()
    );
    let Ok(addr) = format!("127.0.0.1:{port}").parse() else { return };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else { return };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = write!(
        s,
        "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\n\
         X-Dex-Agent: {agent}\r\nX-Dex-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The answer (memory index as additionalContext) is not used; read it so Dex finishes the write.
    let _ = s.read_to_end(&mut Vec::new());
}

fn main() {
    let mut out = std::io::stdout();
    let _ = write!(out, "fake-claude hazir\r\n> ");
    let _ = out.flush();
    std::thread::spawn(session_start);
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let _ = write!(out, "RX:{line}\r\n> ");
        let _ = out.flush();
    }
}
