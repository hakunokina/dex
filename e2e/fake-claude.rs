// Stand-in for the `claude` CLI in e2e runs: dexd starts it for an agent pane (routes.conf `real =`
// points here). No network beyond Dex's loopback hook: like the real claude, it fires SessionStart
// once it has drawn (else Dex's pre-session watch reads the silent screen as a dialog and turns the
// agent `waiting` after 2 s), and no other hook. The console echoes typed keys (cooked input); every
// line that reaches the process comes back as `RX:<line>` — so a test can tell what got into the PTY.
// A start with a prompt is a conversation (a transcript Dex can `--resume`); a resumed start draws
// the session and the prompt it was given first (`RESUME:` / `PROMPT:`).
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What agents.rs gave it: `[--add-dir d]… [--model m] [--resume s] [prompt]`.
struct Args {
    resume: Option<String>,
    prompt: Option<String>,
}

fn args() -> Args {
    let (mut resume, mut prompt, mut args) = (None, None, std::env::args().skip(1));
    while let Some(a) = args.next() {
        match a.as_str() {
            "--resume" => resume = args.next(),
            "--add-dir" | "--model" => drop(args.next()),
            _ => prompt = Some(a),
        }
    }
    Args { resume, prompt }
}

/// POSTs SessionStart to Dex's hook receiver with the pane's env (pty.rs sets it for `claude`);
/// without it (a pane Dex started as a plain command) there is nothing to tell. A resume goes on
/// in its session. With a prompt the session has a conversation line, as claude's own transcript
/// once a turn is going: Dex saves only such a session (`resumable_transcript`), #1875 resumes it.
fn session_start(args: &Args) {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(port), Some(token), Some(agent)) = (env("DEX_HOOK_PORT"), env("DEX_HOOK_TOKEN"), env("DEX_AGENT_ID")) else { return };
    let key = env("DEX_MCP_KEY").unwrap_or_default();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let session = args.resume.clone().unwrap_or_else(|| format!("fake-{}-{nanos}", std::process::id()));
    // The fixture's HOME is its own temp folder: the transcript goes with it.
    let transcript = args.prompt.as_ref().zip(env("HOME").or_else(|| env("USERPROFILE"))).and_then(|(_, home)| {
        let dir = std::path::Path::new(&home).join("fake-claude");
        let path = dir.join(format!("{session}.jsonl"));
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, "{\"type\":\"user\"}\n")).ok()?;
        Some(path.to_string_lossy().replace('\\', "\\\\"))
    });
    let transcript = transcript.map_or_else(String::new, |p| format!(r#","transcript_path":"{p}""#));
    let body = format!(r#"{{"hook_event_name":"SessionStart","source":"startup","session_id":"{session}","cwd":""{transcript}}}"#);
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
    let args = args();
    let mut out = std::io::stdout();
    if let Some(session) = &args.resume {
        let _ = write!(out, "RESUME:{session}\r\nPROMPT:{}\r\n", args.prompt.as_deref().unwrap_or_default());
    }
    let _ = write!(out, "fake-claude hazir\r\n> ");
    let _ = out.flush();
    std::thread::spawn(move || session_start(&args));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let _ = write!(out, "RX:{line}\r\n> ");
        let _ = out.flush();
    }
}
