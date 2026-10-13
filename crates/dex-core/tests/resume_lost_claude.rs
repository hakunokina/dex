//! FIX-4 with the real `claude` (API endpoint = a local server answering "ok"; no tokens spent):
//! 1. folder trust refused ("No, exit") leaves no resumable session;
//! 2. a saved session the CLI can't find ("No conversation found") → the record goes and the same
//!    start runs once more without `--resume` (prompt kept), with a `dex-resume-lost` note;
//! 3. a real session (one answered turn) is resumed on the next start.
//!
//! `DEX_LIVE_CLAUDE=1 cargo test -p dex-core --test resume_lost_claude -- --nocapture` (~1 min).

use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dex_core::agents::{record_session, resumable_session, StartOptions, Status};
use dex_core::{db, teams, Core, EventSink, Events, Paths};

#[derive(Default)]
struct Rec(Mutex<Vec<(String, serde_json::Value)>>);
struct Sink(Arc<Rec>);
impl EventSink for Sink {
    fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
        self.0 .0.lock().unwrap().push((event.into(), payload));
        Ok(())
    }
}
impl Rec {
    fn count(&self, f: impl Fn(&str, &serde_json::Value) -> bool) -> usize {
        self.0.lock().unwrap().iter().filter(|(e, p)| f(e, p)).count()
    }
    fn hooks(&self, agent: &str) -> Vec<String> {
        let events = self.0.lock().unwrap();
        events
            .iter()
            .filter(|(e, p)| e == "dex-hook" && p["agentId"] == agent)
            .map(|(_, p)| format!("{}:{}", p["event"].as_str().unwrap_or(""), p["sessionId"].as_str().unwrap_or("-")))
            .collect()
    }
}

#[derive(Clone, Default)]
struct Screen(Arc<Mutex<Vec<u8>>>);
impl dex_core::PtySink for Screen {
    fn data(&mut self, b: &[u8]) -> bool {
        self.0.lock().unwrap().extend_from_slice(b);
        true
    }
    fn exit(&mut self) {}
}
impl Screen {
    fn text(&self) -> String {
        let raw = String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned();
        let mut out = String::new();
        let mut it = raw.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\u{1b}' {
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
    fn tail(&self) -> String {
        let chars: Vec<char> = self.text().chars().collect();
        chars[chars.len().saturating_sub(3000)..].iter().collect()
    }
}

fn wait(screen: &Screen, what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{secs} sn içinde olmadı: {what}\n--- ekran ---\n{}", screen.tail());
}

/// Answers every `/v1/messages` with a one-word streamed reply ("ok"), anything else with 404.
fn answering_api() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || answer(s));
        }
    });
    port
}

fn answer(mut s: std::net::TcpStream) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut got = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    let (head, len) = loop {
        let Ok(n) = s.read(&mut buf) else { return };
        if n == 0 {
            return;
        }
        got.extend_from_slice(&buf[..n]);
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).to_string();
            let len = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            break (head, end + 4 + len);
        }
    };
    while got.len() < len {
        let Ok(n) = s.read(&mut buf) else { return };
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    use std::io::Write;
    if !head.starts_with("POST /v1/messages?") && !head.starts_with("POST /v1/messages ") {
        let _ = write!(s, "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
        return;
    }
    let events = [
        ("message_start", r#"{"type":"message_start","message":{"id":"msg_fix4","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let body: String = events.iter().map(|(e, d)| format!("event: {e}\ndata: {d}\n\n")).collect();
    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

#[test]
fn trust_refusal_leaves_no_session_and_a_lost_resume_starts_fresh() {
    if std::env::var("DEX_LIVE_CLAUDE").is_err() {
        return;
    }
    const MARK: &str = "FIXFOURPING";
    std::env::set_var("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", answering_api()));
    std::env::set_var("ANTHROPIC_AUTH_TOKEN", "fake-local-token");
    std::env::set_var("CLAUDE_CODE_MAX_RETRIES", "0");

    let root = std::env::temp_dir().join(format!("dex-fix4-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Own claude config: the patron's ~/.claude.json may trust an ancestor (no dialog) and keeps
    // no trust entries from this test.
    let config = root.join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(".claude.json"), r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &config);
    let rec = Arc::new(Rec::default());
    let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(Sink(rec.clone()))).unwrap();
    let team = core.team_create(teams::NewTeam { profile: None, name: "FIX4 live".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
    let id = core.agent_list().unwrap().into_iter().find(|a| a.team_id == team.id).unwrap().id;
    let saved = || db::with(&core.db, |c| resumable_session(c, &id, "claude")).unwrap();
    let waiting = || core.agent_states.pre_session(&id) && core.agent_states.get(&id).status == Status::Waiting;
    let lost = |rec: &Rec| rec.count(|e, p| e == "dex-resume-lost" && p["agentId"] == id.as_str());

    // 1) Trust dialog → "No, exit" (the cursor starts on it): no session may be kept.
    core.agent_start(&id, StartOptions::default()).unwrap();
    let screen = Screen::default();
    core.ptys.subscribe(&id, screen.clone()).unwrap();
    wait(&screen, "güven diyaloğu", 60, waiting);
    core.ptys.write(&id, b"\r").unwrap();
    wait(&screen, "güven reddiyle çıkış", 30, || !core.ptys.is_running(&id));
    std::thread::sleep(Duration::from_millis(1_000));
    println!("1) güven reddi hook'ları: {:?} · kayıt: {:?}", rec.hooks(&id), saved());
    assert_eq!(saved(), None, "güven reddi resume kaydı bırakmamalı");

    // 2) A saved id the CLI can't find (QA-1b's leftover): trust → "No conversation found" → fresh.
    let bogus = "5f028052-ba87-4421-8607-673b50b4f7a1";
    db::with(&core.db, |c| record_session(c, &id, bogus, "claude", None)).unwrap();
    core.agent_start(&id, StartOptions { prompt: Some(format!("{MARK} sadece ok yaz")), ..Default::default() }).unwrap();
    let screen = Screen::default();
    core.ptys.subscribe(&id, screen.clone()).unwrap();
    let mut trusted = false;
    let answered = |rec: &Rec| rec.count(|e, p| e == "dex-hook" && p["agentId"] == id.as_str() && p["event"] == "Stop");
    wait(&screen, "resume'suz yeni oturumda cevaplanan tur", 90, || {
        if !trusted && waiting() {
            trusted = true;
            core.ptys.write(&id, b"\x1b[B").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            core.ptys.write(&id, b"\r").unwrap();
        }
        answered(&rec) >= 1
    });
    assert!(screen.text().contains("No conversation found"), "CLI geçersiz resume'u reddetmeli");
    assert_eq!(lost(&rec), 1, "tek not, tek deneme");
    let note = rec.0.lock().unwrap().iter().find(|(e, _)| e == "dex-resume-lost").unwrap().1.clone();
    std::thread::sleep(Duration::from_secs(2));
    let fresh = saved().unwrap_or_else(|| panic!("cevaplanan oturum kaydedilmeli; hook: {:?}
{}", rec.hooks(&id), screen.tail()));
    println!("2) not: {} · hook'lar: {:?} · yeni kayıt: {fresh}", note["text"], rec.hooks(&id));
    assert_ne!(fresh, bogus);
    assert!(core.ptys.is_running(&id));

    // 3) That session is real (one answered turn): the next start resumes it, no retry.
    core.ptys.kill(&id);
    let starts = |rec: &Rec| rec.count(|e, p| e == "dex-hook" && p["agentId"] == id.as_str() && p["event"] == "SessionStart" && p["sessionId"] == fresh.as_str());
    let before = starts(&rec);
    core.agent_start(&id, StartOptions::default()).unwrap();
    let screen = Screen::default();
    core.ptys.subscribe(&id, screen.clone()).unwrap();
    wait(&screen, "aynı oturumla SessionStart", 60, || starts(&rec) > before);
    std::thread::sleep(Duration::from_secs(3));
    assert!(core.ptys.is_running(&id), "geçerli resume açık kalmalı");
    assert_eq!(lost(&rec), 1, "geçerli resume yeniden deneme tetiklememeli");
    assert_eq!(saved().as_deref(), Some(fresh.as_str()));
    println!("3) geçerli resume: SessionStart {fresh}, PTY canlı");

    core.ptys.kill(&id);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
