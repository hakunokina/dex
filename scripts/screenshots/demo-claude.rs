// Stand-in for the `claude` CLI in README screenshots (scripts/screenshots, `pnpm screenshots`): like
// e2e/fake-claude.rs it fires SessionStart through Dex's loopback hook, but draws a demo screen
// instead of the e2e markers. The screen is `$HOME/demo-screens/<DEX_AGENT_ID>.ans` (written by the
// shot script, ANSI allowed); `<DEX_AGENT_ID>.busy` beside it also sends UserPromptSubmit, so the
// agent is "working" in Dex as with a real turn going. No network beyond Dex's loopback hook.
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn screens() -> Option<PathBuf> {
    Some(PathBuf::from(env("HOME").or_else(|| env("USERPROFILE"))?).join("demo-screens"))
}

fn hook(event: &str, session: &str, extra: &str) {
    let (Some(port), Some(token), Some(agent)) = (env("DEX_HOOK_PORT"), env("DEX_HOOK_TOKEN"), env("DEX_AGENT_ID")) else { return };
    let key = env("DEX_MCP_KEY").unwrap_or_default();
    let body = format!(r#"{{"hook_event_name":"{event}","session_id":"{session}","cwd":""{extra}}}"#);
    let Ok(addr) = format!("127.0.0.1:{port}").parse() else { return };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else { return };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = write!(
        s,
        "POST /hook HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\n\
         X-Dex-Agent: {agent}\r\nX-Dex-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = s.read_to_end(&mut Vec::new());
}

fn main() {
    let agent = env("DEX_AGENT_ID").unwrap_or_default();
    let dir = screens();
    let screen = dir.as_ref().and_then(|d| std::fs::read_to_string(d.join(format!("{agent}.ans"))).ok());
    let busy = dir.as_ref().is_some_and(|d| d.join(format!("{agent}.busy")).exists());
    let mut out = std::io::stdout();
    let _ = write!(out, "{}", screen.unwrap_or_else(|| "\x1b[38;5;209m✻\x1b[0m Hazır.\r\n".into()).replace("\r\n", "\n").replace('\n', "\r\n"));
    let _ = out.flush();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let session = format!("demo-{}-{nanos}", std::process::id());
    std::thread::spawn(move || {
        hook("SessionStart", &session, r#","source":"startup""#);
        if busy {
            std::thread::sleep(Duration::from_millis(300));
            hook("UserPromptSubmit", &session, r#","prompt":"demo""#);
        }
    });
    for line in std::io::stdin().lock().lines() {
        if line.is_err() {
            break;
        }
    }
}
