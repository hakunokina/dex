//! DEX-29: a `cmd = "shell"` PTY is a real PowerShell with none of the agent wiring and none of
//! the Claude session markers Dex inherits when launched from inside Claude Code.
#![cfg(windows)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use dex_core::pty::Spawn;
use dex_core::{Core, EventSink, Events, Paths, PtySink};

struct NoEvents;
impl EventSink for NoEvents {
    fn emit(&self, _: &str, _: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
}

struct Chan(mpsc::Sender<Vec<u8>>);
impl PtySink for Chan {
    fn data(&mut self, bytes: &[u8]) -> bool {
        self.0.send(bytes.to_vec()).is_ok()
    }
    fn exit(&mut self) {}
}

const PROBE: &[u8] =
    b"echo \"MARK[$($PSVersionTable.PSEdition)][$env:CLAUDECODE][$env:CLAUDE_CODE_ENTRYPOINT][$env:DEX_AGENT_ID][$env:ANTHROPIC_BASE_URL]END\"\r";

#[test]
fn shell_runs_powershell_without_claude_markers() {
    let root = std::env::temp_dir().join(format!("dex-shell-pty-{}", std::process::id()));
    let paths = Paths { data_dir: root.join("data"), config_dir: root.join("config") };
    let core = Core::start(paths, Events::new(NoEvents)).unwrap();
    // As if Dex was started from a Claude Code session.
    std::env::set_var("CLAUDECODE", "1");
    std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "cli");

    let (tx, rx) = mpsc::channel();
    let id = "sh-test".to_string();
    // A provider on a shell request must be ignored (no ANTHROPIC_* env).
    let spawn = Spawn { id: id.clone(), cmd: "shell".into(), cols: 120, rows: 30, provider: Some("glm".into()), ..Default::default() };
    core.pty_spawn(spawn, Chan(tx)).unwrap();

    let found = |o: &str| strip_ansi(o).lines().any(|l| l.trim_start().starts_with("MARK["));
    let mut out = String::new();
    let mut sent = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !found(&out) {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(b) => {
                let chunk = String::from_utf8_lossy(&b);
                // ConPTY asks for the cursor position before it renders; xterm.js answers this itself.
                if chunk.contains("\x1b[6n") {
                    core.ptys.write(&id, b"\x1b[1;1R").unwrap();
                }
                out.push_str(&chunk);
            }
            // Output went quiet: the prompt is up, type the probe once.
            Err(_) if !sent && !out.is_empty() => {
                sent = true;
                core.ptys.write(&id, PROBE).unwrap();
            }
            Err(_) => {}
        }
    }
    core.ptys.kill(&id);
    let plain = strip_ansi(&out);
    let line = plain
        .lines()
        .find(|l| l.trim_start().starts_with("MARK["))
        .unwrap_or_else(|| panic!("çıktı yok:\n{plain:?}"));
    // Edition is Core (pwsh) or Desktop (powershell.exe); every marker and agent/provider var is empty.
    assert!(line.contains("MARK[Core][][][][]END") || line.contains("MARK[Desktop][][][][]END"), "{line}");
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
        } else if it.next() == Some('[') {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}
