//! DEX-17 canlı: GLM kotası dolu → StopFailure → transcript özetiyle yeni Codex oturumu.
//!
//! `DEX_LIVE_CHAIN=1 cargo test -p dex-core --test chain_live -- --nocapture` (~2-3 dk).
//! Gerçek kota harcar: GLM turu 429'da hızlı batar (harcama yok); Codex tek kısa devam turu yapar.
//! Kasa yalnız OKUNUR; DB/config geçici dizinde.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dex_core::agents::StartOptions;
use dex_core::agents::Status;
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
    fn find(&self, f: impl Fn(&str, &serde_json::Value) -> bool) -> Option<serde_json::Value> {
        self.0.lock().unwrap().iter().find(|(e, p)| f(e, p)).map(|(_, p)| p.clone())
    }
    fn dump(&self) -> String {
        self.0.lock().unwrap().iter().map(|(e, p)| format!("{e}: {p}")).collect::<Vec<_>>().join("\n")
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
    fn tail(&self) -> String {
        let raw = String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned();
        let plain: String = raw.replace('\r', "\n");
        let chars: Vec<char> = plain.chars().collect();
        chars[chars.len().saturating_sub(2000)..].iter().collect()
    }
}

#[test]
fn glm_quota_fails_over_with_resume() {
    if std::env::var("DEX_LIVE_CHAIN").is_err() {
        return;
    }
    // Test süreci üst süreçten miras uç env'i taşıyabilir (muse yönlendirme) —
    // anthropic yolunun CLI'nın kendi girişini kullanması için temizle.
    for v in ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_MODEL"] {
        std::env::remove_var(v);
    }
    let root = std::env::temp_dir().join(format!("dex-chain-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = root.join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(".claude.json"), r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &config);

    let rec = Arc::new(Rec::default());
    let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(Sink(rec.clone()))).unwrap();
    let team = core.team_create(teams::NewTeam { profile: None, name: "DEX-17 live".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None }).unwrap();
    let members: Vec<_> = core.agent_list().unwrap().into_iter().filter(|a| a.team_id == team.id).collect();
    let leader = members.iter().find(|a| a.kind == "leader").unwrap().id.clone();
    let id = members.iter().find(|a| a.kind == "worker").unwrap().id.clone();
    let delegation_id = dex_core::db::with(&core.db, |c| {
        teams::update_agent(c, &id, teams::AgentPatch { provider: Some("glm".into()), model: Some("sonnet".into()), ..Default::default() }).map(|_| ())?;
        // Kota senaryosu: diğer krediler yok; GLM'den sonraki son halka Codex.
        dex_core::chain::set_order(c, &["glm".into(), "codex".into(), "anthropic".into(), "muse".into(), "local".into()])?;
        let caller = dex_core::delegate::caller(c, &leader)?;
        let delegation = dex_core::delegate::insert(c, dex_core::delegate::NewDelegation {
            from: &caller,
            to_agent: id.clone(),
            team_id: team.id.clone(),
            service_team_id: None,
            prompt: "DEX-17 canlı zincir işi".into(),
            task_id: None, patron_words: None, effort: None,
        })?;
        dex_core::delegate::set_status(c, delegation.id, "running")?;
        Ok(delegation.id)
    })
    .unwrap();
    println!("ajan {id} GLM'de; ilk tur başlıyor");

    core.agent_start(&id, StartOptions { prompt: Some("Tek kelimeyle cevap ver: 2+2 kaç?".into()), ..Default::default() }).unwrap();
    let screen = Screen::default();
    core.ptys.subscribe(&id, screen.clone()).unwrap();
    // Yeni workdir → güven diyaloğu ÇİZİLİP DURUNCA onayla (FIX-1: yerleşmeden önce
    // yazılan tuşlar yutulur; imleç "No"da başlar → aşağı + Enter = güven).
    let mut trusted = false;
    let dialog = || core.agent_states.pre_session(&id) && core.agent_states.get(&id).status == Status::Waiting;
    let chained = || rec.find(|e, p| e == "dex-chain" && p["agentId"] == id.as_str()).is_some();
    let end = Instant::now() + Duration::from_secs(240);
    while Instant::now() < end && !chained() {
        if !trusted && dialog() {
            trusted = true;
            core.ptys.write(&id, b"\x1b[B").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            core.ptys.write(&id, b"\r").unwrap();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !chained() {
        println!("=== OLAYLAR ===\n{}\n=== EKRAN ===\n{}\n=== DURUM: {:?} çalışıyor: {} ===",
            rec.dump(), screen.tail(), core.agent_states.get(&id), core.ptys.is_running(&id));
        panic!("240 sn içinde olmadı: zincir geçişi (dex-chain)");
    }
    let ev = rec.find(|e, p| e == "dex-chain" && p["agentId"] == id.as_str()).unwrap();
    println!("dex-chain: {ev}");
    assert_eq!(ev["from"], "glm");
    assert_eq!(ev["to"], "codex");
    assert_eq!(ev["activeEngine"], "codex");

    // Aynı worker devretmesi motor geçişinde parçalanmaz/failed olmaz; lider tek satır not alır.
    let note_end = Instant::now() + Duration::from_secs(2);
    let (delegation, note_count, saved_engine) = loop {
        let state = dex_core::db::with(&core.db, |c| {
            let delegation = dex_core::delegate::get(c, delegation_id)?;
            let notes = c
                .query_row(
                    "SELECT COUNT(*) FROM leader_notes WHERE agent_id = ?1 AND text LIKE '%iş bölünmedi%'",
                    [&leader],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())?;
            let engine = c
                .query_row("SELECT engine FROM agent_sessions WHERE agent_id = ?1", [&id], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            Ok((delegation, notes, engine))
        })
        .unwrap();
        if state.1 > 0 || Instant::now() >= note_end {
            break state;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(delegation.status, "running", "StopFailure devretmeyi failed yapmamalı");
    assert!(note_count > 0, "lider zincir geçiş notunu almalı");
    assert_eq!(saved_engine, "claude", "kaynak StopFailure oturumu Codex diye kaydedilmemeli");
    println!("devretme #{delegation_id} running kaldı; lider notu ve Claude oturum kaydı doğrulandı");

    // Kalıcı durum: GLM tükenen kayda girdi, ajan geçicide.
    let (gone, a) = dex_core::db::with(&core.db, |c| {
        Ok((dex_core::chain::exhausted(c)?, teams::get_agent(c, &id)?))
    })
    .unwrap();
    println!("tükenen: {gone:?} · aktif: {:?}/{:?}/{:?}", a.active_engine, a.active_provider, a.active_model);
    assert!(gone.iter().any(|x| x.provider == "glm"), "GLM tükenen kayda girmeli");
    assert_eq!(a.provider, "glm", "asıl sağlayıcı değişmez");
    assert_eq!(a.engine, "claude", "asıl motor değişmez");
    assert_eq!(a.active_engine.as_deref(), Some("codex"), "geçici motor Codex olmalı");
    assert!(a.active_provider.is_none(), "Codex sağlayıcı yönlendirmesi kullanmaz");
    assert!(core.ptys.is_running(&id), "geçiş PTY'si açık olmalı");

    // Devam turu cevap verene kadar bekle (sıradaki sağlayıcıda tek kısa tur).
    let end = Instant::now() + Duration::from_secs(240);
    while Instant::now() < end
        && rec.find(|e, p| e == "dex-hook" && p["agentId"] == id.as_str() && p["event"] == "Stop").is_none()
    {
        std::thread::sleep(Duration::from_millis(500));
    }
    if rec.find(|e, p| e == "dex-hook" && p["agentId"] == id.as_str() && p["event"] == "Stop").is_none() {
        println!("=== OLAYLAR ===\n{}\n=== EKRAN ===\n{}\n=== DURUM: {:?} çalışıyor: {} ===",
            rec.dump(), screen.tail(), core.agent_states.get(&id), core.ptys.is_running(&id));
        panic!("240 sn içinde olmadı: devam turu cevabı (Stop)");
    }
    println!("devam turu Stop aldı; ajan yeni sağlayıcıda çalışıyor");

    // Diğer limit biçimleri gerçek hook HTTP alıcısına elle POST edilir. Bu iki probe'da zincir
    // ajan başına kapalıdır: amaç ingress/auth + payload şekli; sınıflandırma birim testlerinde.
    let probes: Vec<String> = core
        .agent_list()
        .unwrap()
        .into_iter()
        .filter(|a| a.team_id == team.id && a.id != id)
        .take(2)
        .map(|a| a.id)
        .collect();
    for (probe, provider, model, error, message) in [
        (&probes[0], "anthropic", "sonnet", "billing_error", "Credit balance is too low; resets in 1 hour"),
        (&probes[1], "muse", "muse-spark-1.3", "rate_limit", "[1113] insufficient balance; quota resets at 1790346821"),
    ] {
        dex_core::db::with(&core.db, |c| {
            teams::update_agent(c, probe, teams::AgentPatch {
                provider: Some(provider.into()), model: Some(model.into()), chain_off: Some(true), ..Default::default()
            }).map(|_| ())
        })
        .unwrap();
        let key = core.mcp.key_for(probe);
        let response = ureq::post(&format!("http://127.0.0.1:{}/hook", core.hooks.port))
            .set("Authorization", &format!("Bearer {}", core.hooks.token))
            .set("X-Dex-Agent", probe)
            .set("X-Dex-Key", &key)
            .send_json(serde_json::json!({
                "hook_event_name": "StopFailure", "error": error, "last_assistant_message": message
            }))
            .unwrap();
        assert!((200..300).contains(&response.status()), "hook HTTP {}", response.status());
        println!("simüle hook POST kabul edildi: {provider} StopFailure");
    }

    core.ptys.kill(&id);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
