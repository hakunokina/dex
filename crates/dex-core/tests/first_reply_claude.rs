//! FIX-3 with the real `claude`: its API endpoint is a local server that accepts and never answers
//! (a local LLM stuck loading a model). The first-reply mark must come; "Yeniden dene" must send the
//! same prompt again (Claude puts an interrupted, unanswered prompt back into its input box); "İptal"
//! must end the turn without a resend. No tokens are spent: nothing leaves 127.0.0.1.
//!
//! `DEX_LIVE_CLAUDE=1 cargo test -p dex-core --test first_reply_claude -- --nocapture` (~1 min).

use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dex_core::agents::{StartOptions, Status};
use dex_core::{teams, Core, EventSink, Events, Paths};

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
}

/// Everything the PTY printed, for a failure message.
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
    fn tail(&self) -> String {
        let raw = String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned();
        let mut out = String::new();
        let mut it = raw.chars().peekable();
        while let Some(c) = it.next() {
            if c != '' {
                out.push(c);
            } else if it.next() == Some('[') {
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        }
        let chars: Vec<char> = out.chars().collect();
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
    panic!("{secs} sn içinde olmadı: {what}
--- ekran ---
{}", screen.tail());
}

/// Accepts, reads the request, never answers; counts `/v1/messages` bodies carrying the marker.
fn hanging_api(marker: &'static str) -> (u16, Arc<Mutex<usize>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut s in l.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = vec![0u8; 256 * 1024];
            let mut got = Vec::new();
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            let text = String::from_utf8_lossy(&got);
            if text.contains("/v1/messages") && text.contains(marker) {
                *h.lock().unwrap() += 1;
            }
            held.push(s); // never answered, never closed
        }
    });
    (port, hits)
}

#[test]
fn retry_resends_and_cancel_stops_with_real_claude() {
    if std::env::var("DEX_LIVE_CLAUDE").is_err() {
        return;
    }
    const MARK: &str = "FIXTHREEPING";
    let (port, api_hits) = hanging_api(MARK);
    std::env::set_var("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"));
    std::env::set_var("ANTHROPIC_AUTH_TOKEN", "fake-local-token");
    std::env::set_var("CLAUDE_CODE_MAX_RETRIES", "0");

    let root = std::env::temp_dir().join(format!("dex-fr-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let rec = Arc::new(Rec::default());
    let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(Sink(rec.clone()))).unwrap();
    core.first_reply_set("anthropic", 10).unwrap();
    let team = core.team_create(teams::NewTeam { profile: None, name: "FR live".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
    let id = core.agent_list().unwrap().into_iter().find(|a| a.team_id == team.id).unwrap().id;

    let prompts = |rec: &Rec| rec.count(|e, p| e == "dex-hook" && p["agentId"] == id.as_str() && p["event"] == "UserPromptSubmit");
    let marks = |rec: &Rec, late: bool| rec.count(|e, p| e == "dex-first-reply" && p["agentId"] == id.as_str() && p["late"] == late);

    core.agent_start(&id, StartOptions { prompt: Some(format!("{MARK} sadece ok yaz")), ..Default::default() }).unwrap();
    let screen = Screen::default();
    core.ptys.subscribe(&id, screen.clone()).unwrap();
    // A fresh temp workdir opens on the folder-trust dialog (FIX-1 marks it waiting): accept it.
    let mut trusted = false;
    wait(&screen, "ilk UserPromptSubmit", 90, || {
        if !trusted && core.agent_states.pre_session(&id) && core.agent_states.get(&id).status == Status::Waiting {
            trusted = true;
            // The cursor starts on "No, exit": down to "Yes, I trust this folder", then Enter.
            core.ptys.write(&id, b"\x1b[B").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            core.ptys.write(&id, b"\r").unwrap();
        }
        prompts(&rec) >= 1
    });
    let t0 = Instant::now();
    wait(&screen, "yanıt gecikiyor işareti", 25, || marks(&rec, true) >= 1);
    println!("işaret {:.1} sn sonra geldi; API isteği: {}", t0.elapsed().as_secs_f32(), api_hits.lock().unwrap());
    let text = rec.0.lock().unwrap().iter().rev().find(|(e, _)| e == "dex-first-reply").unwrap().1["text"].clone();
    assert_eq!(text, "Claude yanıt vermiyor (10 sn).");
    assert!(*api_hits.lock().unwrap() >= 1, "istek sahte uca gitmeli");

    // Yeniden dene: Esc + Enter → the same prompt is submitted again.
    let hits_before = *api_hits.lock().unwrap();
    core.agent_first_reply_retry(&id).unwrap();
    assert!(marks(&rec, false) >= 1, "retry clears the mark");
    wait(&screen, "yeniden gönderilen UserPromptSubmit", 20, || prompts(&rec) >= 2);
    wait(&screen, "aynı istemle yeni API isteği", 20, || *api_hits.lock().unwrap() > hits_before);
    println!("retry: UserPromptSubmit={} API isteği={}", prompts(&rec), api_hits.lock().unwrap());

    // İptal: Esc only — no new turn, the agent is idle, the PTY lives.
    wait(&screen, "ikinci işaret", 25, || marks(&rec, true) >= 2);
    core.agent_first_reply_cancel(&id).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(prompts(&rec), 2, "cancel must not resend");
    assert_eq!(core.agent_states.get(&id).status, Status::Idle);
    assert!(core.ptys.is_running(&id), "PTY öldürülmez");
    println!("iptal: UserPromptSubmit={} durum=idle PTY canlı", prompts(&rec));

    core.ptys.kill(&id);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
