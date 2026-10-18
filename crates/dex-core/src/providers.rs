//! DEX-13 — sağlayıcılar: Claude Code'un yönlendirilebildiği Anthropic-uyumlu uçlar.
//!
//! Dex shim sarmalamaz; GERÇEK `claude` ikilisini sağlayıcı env'iyle başlatır:
//! `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` (+ sağlayıcıya özgü model env'i).
//! `anthropic` (varsayılan) env'e DOKUNMAZ.
//!
//! Sağlayıcı tanımları Anthropic-uyumlu uçları ve model yönlendirmesini belirtir:
//! `zai` = GLM (api.z.ai), `muse-spark` = Meta'nın birinci taraf Anthropic Messages ucu.
//!
//! Jetonlar DB'ye/repoya/loga/düz config'e ASLA yazılmaz — Credential Manager
//! (`keyring`, service `dex-provider`, hesap = sağlayıcı id; başsız dexd'de `secrets::Store::Files`).
//! Çıktılarda yalnız maske görünür.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ModelDef {
    pub id: &'static str,
    pub label: &'static str,
    /// Görev #120: geçerli ama seçicide sunulmaz (eski kayıtlar çalışmaya devam etsin diye).
    pub legacy: bool,
}

const fn m(id: &'static str, label: &'static str) -> ModelDef {
    ModelDef { id, label, legacy: false }
}

const fn alias(id: &'static str, label: &'static str) -> ModelDef {
    ModelDef { id, label, legacy: true }
}

/// Görev #120: yeni Claude ajanının varsayılanları (sürümlü kimlik; takma ad değil).
pub const CLAUDE_STRONG: &str = "claude-opus-5-5";
pub const CLAUDE_MID: &str = "claude-sonnet-5-5";
pub const CLAUDE_LIGHT: &str = "claude-haiku-4-5-20251001";
/// Zincirle Claude'a geçişte model: `--model` verilmez, CLI kendi varsayılanını seçer.
pub const CLAUDE_DEFAULT: &str = "default";

pub struct ProviderDef {
    pub id: &'static str,
    pub label: &'static str,
    /// `None` = yerleşik Anthropic (env'e dokunulmaz).
    pub base_url: Option<&'static str>,
    pub models: &'static [ModelDef],
    /// BASE_URL/AUTH_TOKEN ötesi env; `{model}` = seçilen native model (CLI opus/sonnet/haiku
    /// katmanlarını kendi içinde yönlendirir — hepsini aynı uca bağlamazsak tanımsız Anthropic
    /// modeline düşer).
    pub env: &'static [(&'static str, &'static str)],
    /// DEX-25: adres kullanıcıdan (kasada), modeller sunucudan canlı — `base_url`/`models` boş.
    pub live: bool,
}

pub fn get(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Görev #120: sürümlü kimlikler (`claude --model` tam adı kabul eder). Takma adlar
/// (CLI'nın "en güncel"i) eski kayıtlar için geçerli kalır, seçicide sunulmaz.
const CLAUDE_MODELS: &[ModelDef] = &[
    m("claude-fable-5-1", "Fable 5.1"),
    m(CLAUDE_STRONG, "Opus 5.5"),
    m(CLAUDE_MID, "Sonnet 5.5"),
    m(CLAUDE_LIGHT, "Haiku 4.5"),
    alias("opus", "opus (takma ad — en güncel Opus)"),
    alias("sonnet", "sonnet (takma ad — en güncel Sonnet)"),
    alias("haiku", "haiku (takma ad — en güncel Haiku)"),
];

pub static PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "anthropic",
        label: "Claude",
        base_url: None,
        models: CLAUDE_MODELS,
        env: &[],
        live: false,
    },
    // #2019: ikinci Claude abonelik hesabı — aynı CLI ve modeller, kendi `CLAUDE_CONFIG_DIR`'ı
    // (`claude_accounts`; env'i pty.rs basar). Jeton kasada değil, CLI'ın kimlik dosyasında.
    ProviderDef {
        id: "anthropic-2",
        label: "Claude 2",
        base_url: None,
        models: CLAUDE_MODELS,
        env: &[],
        live: false,
    },
    ProviderDef {
        id: "glm",
        label: "GLM",
        base_url: Some("https://api.z.ai/api/anthropic"),
        // `sonnet`/`haiku` CLI alias'ları env eşlemesiyle GLM'e çevrilir;
        // doğrudan native model adları da kabul edilir.
        models: &[
            m("sonnet", "GLM-5.3 (varsayılan)"),
            m("haiku", "GLM-5.3 Flash (ucuz)"),
            m("GLM-5.3", "GLM-5.3 (doğrudan)"),
            m("GLM-5.3-Flash", "GLM-5.3 Flash (doğrudan)"),
        ],
        env: &[
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", "GLM-5.3"),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", "GLM-5.3"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "GLM-5.3-Flash"),
            // z.ai kota dolunca da 429 döner; 2 deneme kalıcı kota hatasını geciktirmezken
            // kısa süreli 429 için yeniden denemeye izin verir.
            ("CLAUDE_CODE_MAX_RETRIES", "2"),
        ],
        live: false,
    },
    ProviderDef {
        id: "muse",
        label: "Muse",
        base_url: Some("https://api.meta.ai"),
        // CLAUDE_CODE_MAX_CONTEXT_TOKENS ŞART: CLI bu modeli katalogda tanımıyor, 200k sanıp
        // auto-compact yapıyor; pencere model başına `CONTEXT` tablosundan.
        // Sabit liste yedek + etiket kaynağı; sağlayıcının sunduğu yeni modeller `models()`'te eklenir.
        models: &[
            m("muse-spark-1.3", "Muse Spark 1.3"),
            m("muse-spark-1.3-contributor", "Muse Spark 1.3 — contributor (veriniz ürün geliştirmede kullanılır)"),
            m("muse-spark-1.2", "Muse Spark 1.2"),
        ],
        env: &[
            ("ANTHROPIC_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "{model}"),
            ("CLAUDE_CODE_SUBAGENT_MODEL", "{model}"),
            ("CLAUDE_CODE_MAX_CONTEXT_TOKENS", "{context}"),
            // #839: Muse, Artifact aracının file_paths JSON Schema dalını 400 ile reddediyor;
            // bayrak o aracı kapatır (Dex'te Artifact kullanılmıyor). Geri alma: satırı sil.
            ("CLAUDE_CODE_DISABLE_ARTIFACT", "1"),
        ],
        live: false,
    },
    // DEX-25: yerel ağdaki LiteLLM (Anthropic `/v1/messages` servis ediyor). Model adı sunucudaki
    // kayıt adıdır; liste `local_status` ile canlı çekilir.
    ProviderDef {
        id: "local",
        label: "Yerel LLM",
        base_url: None,
        models: &[],
        env: &[
            ("ANTHROPIC_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", "{model}"),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "{model}"),
            ("CLAUDE_CODE_SUBAGENT_MODEL", "{model}"),
            // CLI bilinmeyen modeli 200k sanıp sunucunun penceresini aşabilir;
            // yerel sağlayıcı için 131.072 token sınırı kullanılır.
            ("CLAUDE_CODE_MAX_CONTEXT_TOKENS", "131072"),
        ],
        live: true,
    },
];

/// Bağlam penceresi (`{context}`) — (sağlayıcı, model öneki, token). `/v1/models` pencere vermiyor
/// (GLM/Muse cevabında alan yok). Eşleşmeyen modelde env basılmaz: CLI 200k varsayar, erken
/// auto-compact olur ama sağlayıcı penceresini aşmaz. Muse Wonder 1M: Meta duyurusu/aracı kataloglar.
const CONTEXT: &[(&str, &str, &str)] = &[("muse", "muse-spark-", "1048576"), ("muse", "muse-wonder-", "1048576")];

fn context_tokens(provider: &str, model: &str) -> Option<&'static str> {
    let model = model.to_ascii_lowercase();
    CONTEXT.iter().find(|(p, pre, _)| *p == provider && model.starts_with(pre)).map(|c| c.2)
}

// ── Canlı katalog (GLM/Muse) ─────────────────────────────────────────────────

/// Sağlayıcının sunduğu, sabit listede olmayan sohbet modeli (`model_news::live_catalog` süzer).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, Serialize)]
pub struct LiveModel {
    pub id: String,
    pub label: String,
    pub legacy: bool,
}

/// DB önbelleğinin (`settings.models_catalog`) bellek görüntüsü: doğrulama noktaları (ajan, plan,
/// öneri) bağlantısız ve kilit dışında okur. Açılışta ve her başarılı tazelemede değiştirilir.
static LIVE: std::sync::RwLock<std::collections::BTreeMap<String, Vec<LiveModel>>> =
    std::sync::RwLock::new(std::collections::BTreeMap::new());

/// Değiştiyse `true` (UI'a `providers-changed` gider).
pub fn set_live(catalog: std::collections::BTreeMap<String, Vec<LiveModel>>) -> bool {
    let mut live = LIVE.write().unwrap_or_else(|e| e.into_inner());
    let changed = *live != catalog;
    *live = catalog;
    changed
}

/// Seçilebilir katalog: sabit liste (sıra/etiket korunur; ilki varsayılan) + canlı listedeki yeniler.
pub fn models(def: &ProviderDef) -> Vec<ModelInfo> {
    let live = LIVE.read().unwrap_or_else(|e| e.into_inner());
    merge(def, live.get(catalog_id(def)).map(Vec::as_slice).unwrap_or_default())
}

/// #2019: Claude hesapları Anthropic'in canlı kataloğunu paylaşır (aynı modeller).
fn catalog_id(def: &ProviderDef) -> &'static str {
    if crate::claude_accounts::is_claude(def.id) { "anthropic" } else { def.id }
}

fn merge(def: &ProviderDef, live: &[LiveModel]) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> =
        def.models.iter().map(|m| ModelInfo { id: m.id.into(), label: m.label.into(), legacy: m.legacy }).collect();
    for m in live {
        if !out.iter().any(|o| o.id.eq_ignore_ascii_case(&m.id)) {
            out.push(ModelInfo { id: m.id.clone(), label: m.label.clone(), legacy: m.legacy });
        }
    }
    out
}

/// Model bu sağlayıcıda geçerli mi (sabit + canlı; legacy dahil). Canlı sağlayıcıda (yerel) sunucu karar verir.
pub fn has_model(def: &ProviderDef, model: &str) -> bool {
    def.models.iter().any(|m| m.id == model)
        || LIVE.read().unwrap_or_else(|e| e.into_inner()).get(catalog_id(def)).is_some_and(|l| l.iter().any(|m| m.id == model))
}

// ── Çocuk env'i ──────────────────────────────────────────────────────────────

/// Sağlayıcının çocuk claude env'i. `None` → anthropic: env'e dokunma.
/// Err → bilinmeyen sağlayıcı ya da jeton yok (mesaj kullanıcıya ne yapacağını söyler).
pub fn spawn_env(provider: Option<&str>, model: Option<&str>) -> Result<Option<Vec<(String, String)>>, String> {
    spawn_env_with(&keyring_secret, provider, model)
}

/// Kasa hesabı → değer. `Ok(None)` = kayıt yok; `Err` = kasa açılamadı. Testler kendi haritasını
/// verir (DEX-34): Credential Manager süreçler arası tek kasa, paralel `cargo test`'ler çakışıyordu.
type Secret<'a> = &'a dyn Fn(&str) -> Result<Option<String>, String>;

fn keyring_secret(account: &str) -> Result<Option<String>, String> {
    crate::secrets::get(KEYRING_SERVICE, account)
}

fn spawn_env_with(
    secret: Secret,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<Option<Vec<(String, String)>>, String> {
    // Claude hesapları env'e dokunmaz (ek hesabın `CLAUDE_CONFIG_DIR`'ı pty.rs'te).
    let Some(id) = provider.filter(|p| !crate::claude_accounts::is_claude(p)) else {
        return Ok(None);
    };
    let def = get(id).ok_or_else(|| format!("bilinmeyen sağlayıcı: {id}"))?;
    let token = secret(id)?.ok_or_else(|| format!("{id} jetonu yok — Takım → Sağlayıcılar'dan içe aktar ya da gir"))?;
    let model = model
        .map(str::to_string)
        .or_else(|| def.models.first().map(|m| m.id.to_string()));
    let base_url = if def.live {
        model.as_ref().ok_or_else(|| format!("{} için model seçilmeli", def.label))?;
        base_url_from(secret)?
    } else {
        def.base_url.unwrap_or_default().to_string()
    };
    let mut env = vec![
        ("ANTHROPIC_BASE_URL".to_string(), base_url),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), token),
    ];
    for (k, v) in def.env {
        if *v == "{context}" {
            if let Some(n) = model.as_deref().and_then(|m| context_tokens(id, m)) {
                env.push((k.to_string(), n.to_string()));
            }
            continue;
        }
        let v = model.as_deref().map(|m| v.replace("{model}", m)).unwrap_or_else(|| v.to_string());
        env.push((k.to_string(), v));
    }
    Ok(Some(env))
}

/// `pty_spawn` argümanlarından `--model` değerini okur (muse env'i modele göre kurulur).
pub fn model_flag(args: &[String]) -> Option<String> {
    args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1)).cloned()
}

// ── Gerçek claude ikilisi ─────────────────────────────────────────────────────

/// Yönlendirme kurulumunda PATH'teki `claude`, `~/.claude-route/bin/claude.exe` SHİM'ine çözülebilir.
/// Dex çift yönlendirme istemez: önce routes.conf kök `real =` satırı, sonra
/// `~/.local/bin/claude[.exe]`, hiçbiri yoksa girdi olduğu gibi (PATH). `claude` değilse dokunmaz.
pub fn resolve_claude(cmd: &str) -> String {
    let is_claude = std::path::Path::new(cmd)
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("claude"));
    if !is_claude {
        return cmd.to_string();
    }
    if let Ok(text) = std::fs::read_to_string(route_dir().join("routes.conf")) {
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                break; // yalnız kök bölüm; uç tanımlarındaki anahtarlar karışmasın
            }
            if let Some((k, v)) = l.split_once('=') {
                if k.trim().eq_ignore_ascii_case("real") {
                    let p = v.trim().trim_matches('"');
                    if std::path::Path::new(p).is_file() {
                        return p.to_string();
                    }
                }
            }
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home {
        for name in [r".local\bin\claude.exe", ".local/bin/claude"] {
            let p = PathBuf::from(&home).join(name);
            if p.is_file() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    cmd.to_string()
}

fn route_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let base = home.map(PathBuf::from).unwrap_or_default();
    base.join(".claude-route")
}

// ── Jeton kasası (Credential Manager) ─────────────────────────────────────────

pub const KEYRING_SERVICE: &str = "dex-provider";

pub fn set_token(id: &str, token: &str) -> Result<(), String> {
    let token = token.trim();
    (!token.is_empty()).then_some(()).ok_or_else(|| "jeton boş olamaz".to_string())?;
    get(id).ok_or_else(|| format!("bilinmeyen sağlayıcı: {id}"))?;
    crate::secrets::set(KEYRING_SERVICE, id, token).map_err(|e| format!("jeton yazılamadı: {e}"))
}

/// Jetonun rapor/Çİ'de görünebilen tek hâli.
pub fn mask(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() > 8 {
        format!("{}…{}", chars[..4].iter().collect::<String>(), chars[chars.len() - 4..].iter().collect::<String>())
    } else {
        "•••".to_string()
    }
}

// ── routes.conf içe aktarma ───────────────────────────────────────────────────

/// routes.conf bölümü → Dex sağlayıcısı. `[local]`'dan adres de alınır (DEX-25).
const CONF_SECTIONS: &[(&str, &str)] = &[("zai", "glm"), ("muse-spark", "muse"), ("local", "local")];

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportRow {
    pub provider: String,
    pub base_url: Option<String>,
    /// MASKELİ — ham jeton asla dönmmez.
    pub token: Option<String>,
    pub note: String,
}

/// `~/.claude-route/routes.conf`'un `[zai]`/`[muse-spark]` bölümlerinden jetonu kümeye alır.
/// base_url/env satırları yalnız DOĞRULANIR (kod tanımıyla uyuşmazsa nota yazılır) — tek
/// gerçek kaynağı providers.rs; conf sadece jeton taşır.
pub fn import_routes() -> Result<Vec<ImportRow>, String> {
    let path = route_dir().join("routes.conf");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("routes.conf bulunamadı: {} — jetonu elle de girebilirsin", path.display()))?;
    let sections = parse_conf(&text);
    let mut out = Vec::new();
    for (sec, pid) in CONF_SECTIONS {
        let Some(s) = sections.get(*sec) else {
            out.push(ImportRow {
                provider: pid.to_string(),
                base_url: None,
                token: None,
                note: format!("[{sec}] bölümü yok"),
            });
            continue;
        };
        let def = get(pid).expect("CONF_SECTIONS providers.rs tanımlarıyla uyumlu");
        let mut note = format!("{} env satırı alındı", s.env.len());
        if let (true, Some(url)) = (def.live, &s.base_url) {
            set_base_url(url)?;
            note = format!("{note}; adres alındı");
        } else if let Some(url) = &s.base_url {
            if def.base_url != Some(url.as_str()) {
                note = format!("{note}; UYARI: base_url kod tanımından farklı: {url}");
            }
        }
        // token = doğrudan; token_env = süreç ortam değişkeninden (conf'taki belgeleme).
        let token = s.token.clone().or_else(|| s.token_env.as_ref().and_then(|v| std::env::var(v).ok()));
        match token {
            Some(t) if !t.trim().is_empty() => {
                set_token(pid, &t)?;
                out.push(ImportRow {
                    provider: pid.to_string(),
                    base_url: s.base_url.clone(),
                    token: Some(mask(t.trim())),
                    note,
                });
            }
            _ => out.push(ImportRow {
                provider: pid.to_string(),
                base_url: s.base_url.clone(),
                token: None,
                note: format!("{note}; token/token_env satırı yok — elle gir"),
            }),
        }
    }
    Ok(out)
}

#[derive(Default, Clone)]
struct ConfSection {
    base_url: Option<String>,
    token: Option<String>,
    token_env: Option<String>,
    env: Vec<(String, String)>,
}

/// INI benzeri ayrıştırıcı: `[bölüm]` başlıkları, `anahtar = değer`, `#`/`;` yorumları.
fn parse_conf(text: &str) -> HashMap<String, ConfSection> {
    let mut map: HashMap<String, ConfSection> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with(';') {
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = name.trim().to_string();
            map.entry(section.clone()).or_default();
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim().trim_matches('"').to_string());
        let Some(s) = map.get_mut(&section) else { continue };
        match k {
            "base_url" => s.base_url = Some(v),
            "token" => s.token = Some(v),
            "token_env" => s.token_env = Some(v),
            _ if k.starts_with("env.") => s.env.push((k[4..].to_string(), v)),
            _ => {}
        }
    }
    // Eski `*-local` profil adları da içe alınır; açık `[local]` her zaman önceliklidir.
    // Birden fazla eski profil varsa belirsiz bir adres/jeton seçilmez.
    if !map.contains_key("local") {
        let mut aliases = map.keys().filter(|name| name.strip_suffix("-local").is_some_and(|prefix| !prefix.is_empty()));
        if let (Some(alias), None) = (aliases.next(), aliases.next()) {
            let local = map[alias].clone();
            map.insert("local".into(), local);
        }
    }
    map
}

// ── Uç hatası → sade Türkçe uyarı ─────────────────────────────────────────────

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentError {
    pub agent_id: String,
    pub text: String,
    /// Sağlayıcının bildirdiği kota sıfırlanma anı (RFC 3339, ofsetli); UI yerel saate çevirir.
    pub reset_at: Option<String>,
}

/// StopFailure hook'u (tur API hatasıyla bitti) → `dex-agent-error`: pane + hover tek sade cümle
/// gösterir, ham gövde UI'a gitmez. Sağlayıcı DB'den okunur, webview'ın sözüne güvenilmez.
pub fn on_stop_failure(db: &crate::Db, ev: &crate::Events, agent: &str, body: &serde_json::Value) {
    let provider = db
        .0
        .lock()
        .ok()
        .and_then(|c| c.query_row("SELECT provider FROM agents WHERE id = ?1", [agent], |r| r.get::<_, String>(0)).ok());
    let field = |k: &str| body.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
    let err = failure(agent, provider.as_deref(), field("error"), field("last_assistant_message"));
    let _ = ev.emit("dex-agent-error", &err);
}

/// `error` = CLI'nin StopFailure sınıfı; `message` = CLI'nin yazdığı hata satırı (uç gövdesini taşır).
/// Tür kararı tek kaynaktan ([`crate::chain::classify`]) — burada yalnız türe göre cümle seçilir.
pub fn failure(agent: &str, provider: Option<&str>, error: &str, message: &str) -> AgentError {
    use crate::chain::{Kind, classify};
    let label = provider.and_then(get).map_or("Claude", |p| p.label);
    let text = match classify(error, message) {
        // z.ai iş kodları (gövdede `[kod]`): 1113 bakiye yok, 1308/1310 kullanım sınırı.
        // Kota yanıtı örneği: 429 + "[1308][Usage limit reached for 5 hour. Your limit will reset at …]".
        Kind::Credit => format!("{label} kredisi bitti — sağlayıcı hesabına bakiye yükle."),
        Kind::Quota => format!("{label} kullanım kotası doldu."),
        Kind::Auth => format!("{label} jetonu reddedildi — Takım → Sağlayıcılar'dan jetonu yenile."),
        Kind::Transient if error == "rate_limit" => format!("{label} hız sınırına takıldı — biraz bekleyip tekrar dene."),
        Kind::Transient | Kind::Network => format!("{label} ucu şu an yanıt vermiyor — sonra tekrar dene."),
        Kind::Truncated => format!("{label} cevabı çıktı sınırında kesildi."),
        Kind::Protocol if error == "model_not_found" => format!("{label} bu modeli tanımıyor — üyenin modelini değiştir."),
        Kind::Protocol | Kind::Unknown => format!("{label} ucu hata verdi, tur tamamlanamadı."),
    };
    let reset_at = (provider == Some("glm")).then(|| zai_reset(message)).flatten();
    AgentError { agent_id: agent.to_string(), text, reset_at }
}

/// z.ai reset sözleşmesi: ofsetsiz tarih-saat Pekin saati (UTC+8) olarak yorumlanır.
/// Sentetik örnek: "…reset at 2002-01-01 08:00:00…" → "2002-01-01T08:00:00+08:00".
fn zai_reset(message: &str) -> Option<String> {
    let i = message.find("reset at ")? + "reset at ".len();
    let ts = message.get(i..i + 19)?;
    let ok = ts.bytes().enumerate().all(|(j, b)| match j {
        4 | 7 => b == b'-',
        10 => b == b' ',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    });
    ok.then(|| format!("{}T{}+08:00", &ts[..10], &ts[11..]))
}

// ── Arayüz uçları (host komutları) ───────────────────────────────────────────────────────────

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    /// Görev #120: yalnız bu modeli zaten kullanan ajan için gösterilir.
    pub legacy: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub base_url: Option<String>,
    pub models: Vec<ModelInfo>,
    pub has_token: bool,
    pub token_mask: Option<String>,
    /// #2019: ek Claude hesabının bağlantısı (Core doldurur); başka sağlayıcıda yok.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::claude_accounts::Account>,
}

pub fn list() -> Result<Vec<ProviderInfo>, String> {
    Ok(PROVIDERS
        .iter()
        .map(|p| {
            let token = if p.base_url.is_some() || p.live {
                keyring_secret(p.id).ok().flatten()
            } else {
                None
            };
            ProviderInfo {
                id: p.id.to_string(),
                label: p.label.to_string(),
                base_url: if p.live { base_url_from(&keyring_secret).ok() } else { p.base_url.map(str::to_string) },
                // Yerel sağlayıcının modelleri `local_status`'tan gelir; GLM/Muse'nin canlı kataloğu
                // önbellekten (liste açılışını ağa bağlamaz).
                models: models(p),
                has_token: token.as_deref().map(|t| !t.is_empty()).unwrap_or(false),
                token_mask: token.as_deref().map(mask),
                account: None,
            }
        })
        .collect())
}

// ── DEX-25: Yerel LLM (LiteLLM) ───────────────────────────────────────────────
//
// Veritabanısız LiteLLM kurulumunda modeller sunucunun config dosyasından gelir
// (`db_model: false`); `/model/new` "No DB Connected" ile reddedilir.
// Desteklenen arka uç örneği: `hosted_vllm/*` → indirme/pull API'si olmayan model değiştirici.
// Bu yüzden "Ekle" arka motorda modeli ÇEKMEZ; motorun sunduğunu doğrular, LiteLLM'e mevcut
// kaydın desenini kopyalayarak kaydeder. Silme yalnız Dex'in eklediği (DB) kayıtlara izinli.

/// Adres gizli değil ama jetonla aynı yerde tutulur: DB migration'ı ve ayrı config dosyası gerekmez.
const BASE_URL_ACCOUNT: &str = "local.base_url";
const LOCAL: &str = "local";

pub const REGISTRY_OFF: &str = "Sunucuda model kaydı kapalı: LiteLLM'e veritabanı bağlı değil, modeller sunucunun \
config dosyasından geliyor. Açmak için LiteLLM'e DATABASE_URL + STORE_MODEL_IN_DB=True verilmeli; o zamana dek \
yeni model sunucu config'ine elle yazılır.";

/// #2190: oMLX'te kayıt API'si yok; modeller sunucunun model klasöründen kendiliğinden bulunur.
pub const OMLX_MANAGED: &str = "Sunucu oMLX: modeller sunucunun model klasöründen kendiliğinden bulunur, Dex'ten ekleme/silme yok — modeli sunucudaki oMLX model klasörüne koy ya da oradan kaldır.";

pub fn set_base_url(url: &str) -> Result<(), String> {
    let u = url.trim().trim_end_matches('/');
    (u.starts_with("http://") || u.starts_with("https://"))
        .then_some(())
        .ok_or_else(|| "adres http:// ya da https:// ile başlamalı".to_string())?;
    crate::secrets::set(KEYRING_SERVICE, BASE_URL_ACCOUNT, u).map_err(|e| format!("adres yazılamadı: {e}"))
}

fn base_url_from(secret: Secret) -> Result<String, String> {
    secret(BASE_URL_ACCOUNT)?
        .ok_or_else(|| "Yerel LLM adresi yok — Takım → Sağlayıcılar'dan gir ya da routes.conf'tan içe aktar".to_string())
}

fn local_conn() -> Result<(String, String), String> {
    let base = base_url_from(&keyring_secret)?;
    let token = keyring_secret(LOCAL)?.ok_or_else(|| "Yerel LLM jetonu yok — Takım → Sağlayıcılar'dan gir".to_string())?;
    Ok((base, token))
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(20))
        .build()
}

/// Ham gövde UI'a gitmez: bilinen durumlar sade cümleye çevrilir.
fn call(req: ureq::Request, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(r) => r.into_json().map_err(|_| "Yerel LLM sunucusu anlaşılır bir cevap dönmedi.".to_string()),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string))
                .unwrap_or_default();
            Err(http_error(code, &msg))
        }
        Err(ureq::Error::Transport(_)) => Err("Yerel LLM sunucusuna ulaşılamadı — adresi ve ağı kontrol et.".to_string()),
    }
}

fn http_error(code: u16, msg: &str) -> String {
    let m = msg.to_ascii_lowercase();
    if m.contains("no db connected") || m.contains("no connected db") {
        REGISTRY_OFF.to_string()
    } else if code == 401 || code == 403 {
        "Yerel LLM jetonu reddedildi — model kaydı yönetici (master) anahtarı ister.".to_string()
    } else {
        format!("Yerel LLM sunucusu isteği reddetti (HTTP {code}).")
    }
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    /// Ajanın `--model`'i: LiteLLM'deki kayıt adı.
    pub id: String,
    /// `litellm_params.model` (ör. `hosted_vllm/qwen3-coder-next`).
    pub backend: Option<String>,
    /// Dex'ten silinebilir mi (yalnız DB kaydı; config kayıtları korunur).
    pub removable: bool,
}

/// #2192: yerel sunucunun türü — arayüz metinleri ve Ekle/Sil buna göre.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum LocalKind {
    Litellm,
    Omlx,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LocalStatus {
    pub base_url: String,
    pub models: Vec<LocalModel>,
    /// LiteLLM'e DB bağlı mı → `/model/new` çalışır mı (oMLX'te hep `false`).
    pub registry: bool,
    pub kind: LocalKind,
}

fn parse_model_info(v: &serde_json::Value) -> Vec<LocalModel> {
    let rows = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            // `/model/info` → model_name; `/v1/models` yedeği → id.
            let id = r.get("model_name").or_else(|| r.get("id"))?.as_str()?.to_string();
            let backend = r.pointer("/litellm_params/model").and_then(|m| m.as_str()).map(str::to_string);
            let removable = r.pointer("/model_info/db_model").and_then(|b| b.as_bool()).unwrap_or(false);
            Some(LocalModel { id, backend, removable })
        })
        .collect()
}

fn model_info(base: &str, token: &str) -> Result<serde_json::Value, String> {
    let auth = format!("Bearer {token}");
    call(http().get(&format!("{base}/model/info")).set("Authorization", &auth), None)
        .or_else(|_| call(http().get(&format!("{base}/v1/models")).set("Authorization", &auth), None))
}

/// Bağlantı testi + canlı model listesi (üye formu da bunu kullanır).
pub fn local_status() -> Result<LocalStatus, String> {
    let (base, token) = local_conn()?;
    let models = parse_model_info(&model_info(&base, &token)?);
    let kind = local_kind(models_status(&base, &token).as_ref());
    let registry = kind == LocalKind::Litellm
        && call(http().get(&format!("{base}/health/readiness")), None)
            .ok()
            .and_then(|v| v.get("db").and_then(|d| d.as_str()).map(|d| d.eq_ignore_ascii_case("connected")))
            .unwrap_or(false);
    Ok(LocalStatus { base_url: base, models, registry, kind })
}

fn check_model_name(name: &str) -> Result<&str, String> {
    let n = name.trim();
    let ok = !n.is_empty() && n.len() <= 128 && n.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/".contains(c));
    ok.then_some(n).ok_or_else(|| format!("geçersiz model adı: {n} (harf, rakam, . _ : - / kullan)"))
}

/// Mevcut sade bir kaydın (`extra_body`'siz) `litellm_params`'ını kopyalar, `model`'i yeni adla
/// değiştirir. Bool bayrakları sunucu varsayılanıdır, `api_key` maskeli döner — ikisi de kopyalanmaz.
fn registration(info: &serde_json::Value, name: &str) -> Result<serde_json::Value, String> {
    let rows = info.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    let tpl = rows
        .iter()
        .filter_map(|r| r.get("litellm_params")?.as_object())
        .find(|p| p.get("extra_body").is_none() && p.get("model").and_then(|m| m.as_str()).is_some_and(|m| m.contains('/')))
        .ok_or("sunucuda kopyalanacak örnek kayıt yok")?;
    let prefix = tpl["model"].as_str().unwrap_or_default().split('/').next().unwrap_or_default().to_string();
    let mut params: serde_json::Map<String, serde_json::Value> = tpl
        .iter()
        .filter(|(k, v)| !v.is_boolean() && k.as_str() != "api_key")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    params.insert("model".into(), format!("{prefix}/{name}").into());
    Ok(serde_json::json!({ "model_name": name, "litellm_params": params }))
}

/// `api_base` sunucunun kendi döngü adresiyse (127.0.0.1) dışarıdan LiteLLM'in host'uyla erişilir.
fn backend_api(base: &str, api_base: &str) -> Option<String> {
    let host = base.split("://").nth(1)?.split(['/', ':']).next()?;
    let api = api_base.trim_end_matches('/');
    Some(
        ["127.0.0.1", "localhost", "0.0.0.0"]
            .iter()
            .find(|h| api.contains(&format!("//{h}")))
            .map_or(api.to_string(), |h| api.replacen(*h, host, 1)),
    )
}

fn backend_models_url(base: &str, api_base: &str) -> Option<String> {
    Some(format!("{}/models", backend_api(base, api_base)?))
}

/// Model değiştiricinin `/health`'i OpenAI önekinin (`/v1`) dışında.
fn backend_health_url(base: &str, api_base: &str) -> Option<String> {
    Some(format!("{}/health", backend_api(base, api_base)?.trim_end_matches("/v1")))
}

/// FIX-3: ilk yanıt gecikince nedeni. Model değiştiricinin `/health`'i `current`'ta yüklü modeli
/// verir (boşta örnek: `{"status":"ok","current":null,"active":[]}`); istenen model
/// yüklü değilse ya da durum "load…" diyorsa sunucu onu yüklüyordur. Sunucuya ulaşılamazsa `None`.
pub fn local_loading(model: &str) -> Option<String> {
    let (base, token) = local_conn().ok()?;
    let info = model_info(&base, &token).ok()?;
    let Some(row) = info.get("data")?.as_array()?.iter().find(|r| r.get("model_name").and_then(|n| n.as_str()) == Some(model)) else {
        // oMLX: LiteLLM kaydı yok; ipucu motorun `/v1/models/status`'unda.
        return omlx_loading_hint(&models_status(&base, &token)?, model);
    };
    let url = backend_health_url(&base, row.pointer("/litellm_params/api_base")?.as_str()?)?;
    let backend = row.pointer("/litellm_params/model").and_then(|m| m.as_str()).unwrap_or(model);
    let health = call(http().get(&url).timeout(std::time::Duration::from_secs(5)), None).ok()?;
    loading_hint(&health, backend.rsplit('/').next().unwrap_or(backend))
}

fn loading_hint(health: &serde_json::Value, want: &str) -> Option<String> {
    let name = |v: Option<&serde_json::Value>| v.and_then(|c| c.as_str()).map(|c| c.rsplit('/').next().unwrap_or(c).to_string());
    let status = health.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status.to_ascii_lowercase().contains("load") {
        return Some(format!("model yükleniyor: {}", name(health.get("loading")).or(name(health.get("current"))).unwrap_or(want.into())));
    }
    (name(health.get("current")).as_deref() != Some(want)).then(|| format!("model yükleniyor: {want}"))
}

/// #2190: oMLX `/v1/models/status` → `{"models":[{"id","loaded","is_loading",…}]}`. LiteLLM'de bu
/// yol yok (hata ya da `models`'siz gövde) — oMLX'i bundan tanırız.
fn models_status(base: &str, token: &str) -> Option<serde_json::Value> {
    let req = http().get(&format!("{base}/v1/models/status")).set("Authorization", &format!("Bearer {token}"));
    call(req.timeout(std::time::Duration::from_secs(5)), None).ok()
}

fn is_omlx(status: &serde_json::Value) -> bool {
    status.get("models").is_some_and(|m| m.is_array())
}

fn local_kind(status: Option<&serde_json::Value>) -> LocalKind {
    if status.is_some_and(is_omlx) { LocalKind::Omlx } else { LocalKind::Litellm }
}

/// oMLX isteğe göre yükler: model yüklü değilse ya da yükleniyorsa ilk yanıt onu bekler.
fn omlx_loading_hint(status: &serde_json::Value, want: &str) -> Option<String> {
    let row = status.get("models")?.as_array()?.iter().find(|m| m.get("id").and_then(|i| i.as_str()) == Some(want))?;
    let flag = |k: &str| row.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
    (flag("is_loading") || !flag("loaded")).then(|| format!("model yükleniyor: {want}"))
}

fn registry_guard(status: Option<serde_json::Value>) -> Result<(), String> {
    match status {
        Some(s) if is_omlx(&s) => Err(OMLX_MANAGED.to_string()),
        _ => Ok(()),
    }
}

/// Metinle model kaydı: motor modeli sunuyor mu → LiteLLM `/model/new` → güncel liste.
pub fn local_add(name: &str) -> Result<LocalStatus, String> {
    let name = check_model_name(name)?;
    let (base, token) = local_conn()?;
    registry_guard(models_status(&base, &token))?;
    let info = model_info(&base, &token)?;
    if parse_model_info(&info).iter().any(|m| m.id == name) {
        return Err(format!("{name} zaten kayıtlı"));
    }
    let body = registration(&info, name)?;
    // Arka motor ulaşılabilirse modelin orada olduğunu doğrula; ulaşılamazsa (ağ/ACL) kayda geç.
    let backend = body
        .pointer("/litellm_params/api_base")
        .and_then(|a| a.as_str())
        .and_then(|a| backend_models_url(&base, a));
    // Model değiştirici yükleme sırasında cevabı geciktirebilir; probun bekleme süresini sınırla.
    let probe = |url: String| call(http().get(&url).timeout(std::time::Duration::from_secs(5)), None);
    if let Some(Ok(v)) = backend.map(probe) {
        let ids: Vec<&str> = v
            .get("data")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| m.get("id")?.as_str())
            .collect();
        if !ids.contains(&name) {
            return Err(format!(
                "Arka motor bu modeli sunmuyor: {name}. Sunduğu modeller: {}. Motorun indirme API'si yok — model önce sunucuya yüklenmeli.",
                ids.join(", ")
            ));
        }
    }
    call(http().post(&format!("{base}/model/new")).set("Authorization", &format!("Bearer {token}")), Some(body))?;
    local_status()
}

/// Yalnız DB kayıtları silinir; config dosyasındakiler (mevcut kayıtlar) korunur.
pub fn local_delete(name: &str) -> Result<LocalStatus, String> {
    let name = check_model_name(name)?;
    let (base, token) = local_conn()?;
    registry_guard(models_status(&base, &token))?;
    let info = model_info(&base, &token)?;
    let rows: Vec<&serde_json::Value> = info
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|r| r.get("model_name").and_then(|n| n.as_str()) == Some(name))
        .collect();
    if rows.is_empty() {
        return Err(format!("{name} sunucuda kayıtlı değil"));
    }
    for r in rows {
        if r.pointer("/model_info/db_model").and_then(|b| b.as_bool()) != Some(true) {
            return Err(format!("{name} sunucunun config dosyasında tanımlı — Dex yalnız kendi eklediği kayıtları siler"));
        }
        let id = r.pointer("/model_info/id").and_then(|i| i.as_str()).ok_or("kayıt kimliği yok")?;
        call(
            http().post(&format!("{base}/model/delete")).set("Authorization", &format!("Bearer {token}")),
            Some(serde_json::json!({ "id": id })),
        )?;
    }
    local_status()
}

#[cfg(test)]
mod tests {
    use super::*;

    // DEX-34: testler gerçek kasaya (Credential Manager) hiç dokunmaz — o, süreçler arası tek kasa;
    // eşzamanlı iki `cargo test` ayrık hesaplarda bile okumayı boş döndürüp testi düşürüyordu.
    fn vault(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Result<Option<String>, String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |acct| Ok(m.get(acct).cloned())
    }

    #[test]
    fn anthropic_touches_nothing() {
        assert!(spawn_env(None, Some("sonnet")).unwrap().is_none());
        assert!(spawn_env(Some("anthropic"), Some("opus")).unwrap().is_none());
        // #2019: ikinci hesap da uç/jeton basmaz; yalnız kendi config dizini (pty.rs).
        assert!(spawn_env(Some("anthropic-2"), Some("opus")).unwrap().is_none());
        let two = get("anthropic-2").unwrap();
        assert_eq!((two.label, two.base_url), ("Claude 2", None));
        assert!(has_model(two, CLAUDE_STRONG) && has_model(two, "opus"), "modeller Claude ile aynı");
    }

    #[test]
    fn unknown_provider_rejected() {
        assert!(spawn_env(Some("bogus"), None).is_err());
    }

    #[test]
    fn glm_env_maps_cli_layers() {
        let env = spawn_env_with(&vault(&[("glm", "glmtoken123456")]), Some("glm"), Some("sonnet")).unwrap().unwrap();
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).unwrap().1.clone();
        assert_eq!(get("ANTHROPIC_BASE_URL"), "https://api.z.ai/api/anthropic");
        assert_eq!(get("ANTHROPIC_AUTH_TOKEN"), "glmtoken123456");
        assert_eq!(get("ANTHROPIC_DEFAULT_SONNET_MODEL"), "GLM-5.3");
        assert_eq!(get("ANTHROPIC_DEFAULT_HAIKU_MODEL"), "GLM-5.3-Flash");
        assert_eq!(get("CLAUDE_CODE_MAX_RETRIES"), "2");
        assert!(!env.iter().any(|(k, _)| k == "CLAUDE_CODE_MAX_CONTEXT_TOKENS"), "routes.conf da GLM için pencere basmıyor");
    }

    #[test]
    fn muse_env_is_model_aware_and_sets_context_window() {
        let env = spawn_env_with(&vault(&[("muse", "musetoken123456")]), Some("muse"), Some("muse-spark-1.2")).unwrap().unwrap();
        for k in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"] {
            assert_eq!(env.iter().find(|(ek, _)| ek == k).unwrap().1, "muse-spark-1.2", "{k}");
        }
        assert_eq!(env.iter().find(|(ek, _)| ek == "CLAUDE_CODE_MAX_CONTEXT_TOKENS").unwrap().1, "1048576");
        assert_eq!(env.iter().find(|(ek, _)| ek == "CLAUDE_CODE_DISABLE_ARTIFACT").unwrap().1, "1");
    }

    #[test]
    fn context_window_per_model() {
        let env = |m: &str| spawn_env_with(&vault(&[("muse", "musetoken123456")]), Some("muse"), Some(m)).unwrap().unwrap();
        let ctx = |m: &str| env(m).into_iter().find(|(k, _)| k == "CLAUDE_CODE_MAX_CONTEXT_TOKENS").map(|e| e.1);
        assert_eq!(ctx("muse-wonder-1.0").as_deref(), Some("1048576"));
        assert_eq!(ctx("muse-wonder-full-1.0-contributor").as_deref(), Some("1048576"));
        assert_eq!(ctx("muse-nova-1.0"), None, "bilinmeyen modelde pencere basılmaz (CLI varsayılanı)");
        assert!(!env("muse-nova-1.0").iter().any(|(_, v)| v.contains('{')), "yer tutucu sızmaz");
    }

    #[test]
    fn live_models_append_after_static_and_keep_labels() {
        let muse = get("muse").unwrap();
        let live = |id: &str, legacy| LiveModel { id: id.into(), label: id.to_uppercase(), legacy };
        let got = merge(muse, &[live("MUSE-SPARK-1.3", false), live("muse-wonder-1.0", false), live("muse-spark-1.1", true)]);
        let ids: Vec<(&str, bool)> = got.iter().map(|m| (m.id.as_str(), m.legacy)).collect();
        assert_eq!(
            ids,
            [("muse-spark-1.3", false), ("muse-spark-1.3-contributor", false), ("muse-spark-1.2", false), ("muse-wonder-1.0", false), ("muse-spark-1.1", true)]
        );
        assert_eq!(got[0].label, "Muse Spark 1.3", "sabit etiket korunur");
        assert_eq!(merge(muse, &[]).len(), muse.models.len(), "önbellek yoksa sabit liste");
    }

    #[test]
    fn missing_token_is_actionable_error() {
        let err = spawn_env_with(&vault(&[]), Some("muse"), None).unwrap_err();
        assert!(err.contains("jetonu yok"), "{err}");
        assert!(err.contains("Sağlayıcılar"), "{err}");
    }

    #[test]
    fn mask_hides_all_but_edges() {
        assert_eq!(mask("synthetic-token-abcdefgh"), "synt…efgh");
        assert_eq!(mask("short"), "•••");
    }

    #[test]
    fn model_flag_reads_args() {
        let args = vec!["--model".to_string(), "GLM-5.3".to_string(), "--resume".to_string(), "x".to_string()];
        assert_eq!(model_flag(&args).as_deref(), Some("GLM-5.3"));
        assert_eq!(model_flag(&[]), None);
    }

    #[test]
    fn resolve_claude_stays_claude_and_real() {
        // PATH'te shim varsa çözüm ya olduğu gibi döner ya da VAR OLAN gerçek ikili.
        let r = resolve_claude("claude");
        assert!(std::path::Path::new(&r).file_stem().unwrap().eq_ignore_ascii_case("claude"));
        if r != "claude" {
            assert!(std::path::Path::new(&r).is_file(), "{r} yok");
            assert!(!r.contains("claude-route"), "shim'e çözüldü: {r}");
        }
        assert_eq!(resolve_claude("C:\\tools\\node.exe"), "C:\\tools\\node.exe");
    }

    #[test]
    fn failures_become_plain_turkish() {
        // Sentetik StopFailure `last_assistant_message`: kota kodu ve reset biçimi korunur.
        let msg = "API Error: Request rejected (429) · [1308][Usage limit reached for 5 hour. Your limit will reset at 2002-01-01 08:00:00][example-request]";
        let e = failure("example-agent", Some("glm"), "rate_limit", msg);
        assert_eq!(e.text, "GLM kullanım kotası doldu.");
        assert_eq!(e.reset_at.as_deref(), Some("2002-01-01T08:00:00+08:00"));
        assert!(!e.text.contains("1308") && !e.text.contains("429"), "ham gövde sızmamalı");

        let credit = failure("a", Some("glm"), "rate_limit", "API Error: 429 [1113][Insufficient balance or no resource package]");
        assert_eq!(credit.text, "GLM kredisi bitti — sağlayıcı hesabına bakiye yükle.");
        assert_eq!(failure("a", Some("muse"), "authentication_failed", "401 Unauthorized").text,
            "Muse jetonu reddedildi — Takım → Sağlayıcılar'dan jetonu yenile.");
        assert_eq!(failure("a", Some("anthropic"), "billing_error", "Credit balance is too low").text,
            "Claude kredisi bitti — sağlayıcı hesabına bakiye yükle.");
        assert_eq!(failure("a", None, "rate_limit", "429").text, "Claude hız sınırına takıldı — biraz bekleyip tekrar dene.");
        assert_eq!(failure("a", Some("muse"), "rate_limit", "reset at 2002-01-01 08:00:00").reset_at, None, "yalnız z.ai saati bilinir");
        assert_eq!(zai_reset("reset at 2002-1-1 8:00"), None);
    }

    #[test]
    fn conf_parser_reads_sections_and_ignores_noise() {
        let text = "# yorum\nreal = C:\\x\\claude.exe\n\n[zai]\nbase_url = https://api.z.ai/api/anthropic\ntoken = abc.def123456789\nenv.ANTHROPIC_DEFAULT_SONNET_MODEL = GLM-5.3\n\n[engine-slots]\ncrush = zai\n";
        let m = parse_conf(text);
        assert_eq!(m["zai"].base_url.as_deref(), Some("https://api.z.ai/api/anthropic"));
        assert_eq!(m["zai"].token.as_deref(), Some("abc.def123456789"));
        assert_eq!(m["zai"].env, vec![("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "GLM-5.3".to_string())]);
        assert!(m["engine-slots"].env.is_empty());
    }

    #[test]
    fn conf_local_import_accepts_generic_and_legacy_sections() {
        assert!(CONF_SECTIONS.contains(&("local", "local")));
        for section in ["local", "archive-local"] {
            let text = format!("[{section}]\nbase_url = http://192.0.2.10:4000\ntoken = synthetic-local-token\ntoken_env = LOCAL_TOKEN\nenv.ANTHROPIC_MODEL = example-model\n");
            let m = parse_conf(&text);
            let s = m.get("local").expect("local import section");
            assert_eq!(s.base_url.as_deref(), Some("http://192.0.2.10:4000"));
            assert_eq!(s.token.as_deref(), Some("synthetic-local-token"));
            assert_eq!(s.token_env.as_deref(), Some("LOCAL_TOKEN"));
            assert_eq!(s.env, vec![("ANTHROPIC_MODEL".into(), "example-model".into())]);
        }
    }

    #[test]
    fn conf_local_import_prefers_explicit_section_in_either_order() {
        let local = "[local]\nbase_url = http://192.0.2.10:4000\ntoken = synthetic-current-token\n";
        let legacy = "[archive-local]\nbase_url = http://192.0.2.20:4000\ntoken = synthetic-legacy-token\n";
        for text in [format!("{local}{legacy}"), format!("{legacy}{local}")] {
            let m = parse_conf(&text);
            assert_eq!(m["local"].base_url.as_deref(), Some("http://192.0.2.10:4000"));
            assert_eq!(m["local"].token.as_deref(), Some("synthetic-current-token"));
        }
    }

    #[test]
    fn conf_local_import_does_not_choose_ambiguous_or_unrelated_profiles() {
        for text in ["[first-local]\ntoken = first\n[second-local]\ntoken = second\n", "[local-other]\ntoken = unrelated\n[-local]\ntoken = unrelated\n"] {
            assert!(!parse_conf(text).contains_key("local"));
        }
        let m = parse_conf("[zai]\ntoken = glm-token\n[muse-spark]\ntoken = muse-token\n[archive-local]\ntoken = local-token\n");
        assert_eq!(m["zai"].token.as_deref(), Some("glm-token"));
        assert_eq!(m["muse-spark"].token.as_deref(), Some("muse-token"));
        assert_eq!(m["local"].token.as_deref(), Some("local-token"));
    }

    // ── DEX-25 ──

    /// Ölçülen `/model/info` biçimi (kısaltıldı): config kaydı, extra_body'li kayıt, DB kaydı.
    fn sample_info() -> serde_json::Value {
        serde_json::json!({ "data": [
            { "model_name": "gpt-oss-120b-low",
              "litellm_params": { "api_base": "http://127.0.0.1:8000/v1", "use_in_pass_through": false,
                "model": "hosted_vllm/gpt-oss-120b-heretic-v2", "extra_body": { "chat_template_kwargs": { "reasoning_effort": "low" } } },
              "model_info": { "id": "aaa", "db_model": false } },
            { "model_name": "qwen3-coder-next",
              "litellm_params": { "api_base": "http://127.0.0.1:8000/v1", "use_in_pass_through": false,
                "model": "hosted_vllm/qwen3-coder-next", "api_key": "sk-****" },
              "model_info": { "id": "bbb", "db_model": false } },
            { "model_name": "dex-added", "litellm_params": { "model": "hosted_vllm/dex-added" },
              "model_info": { "id": "ccc", "db_model": true } }
        ]})
    }

    #[test]
    fn local_env_uses_stored_base_url_and_model() {
        let v = vault(&[(BASE_URL_ACCOUNT, "http://10.0.0.5:4000"), ("local", "localtoken123456")]);
        let env = spawn_env_with(&v, Some("local"), Some("qwen3-coder-next")).unwrap().unwrap();
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).unwrap().1.clone();
        assert_eq!(get("ANTHROPIC_BASE_URL"), "http://10.0.0.5:4000");
        for k in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"] {
            assert_eq!(get(k), "qwen3-coder-next", "{k}");
        }
        assert_eq!(get("CLAUDE_CODE_MAX_CONTEXT_TOKENS"), "131072");
        // Canlı sağlayıcıda varsayılan model yok — `{model}` env'e sızmasın.
        assert!(spawn_env_with(&v, Some("local"), None).unwrap_err().contains("model seçilmeli"));
    }

    #[test]
    fn local_missing_base_url_is_actionable() {
        let err = spawn_env_with(&vault(&[("local", "t0ken-123456")]), Some("local"), Some("x")).unwrap_err();
        assert!(err.contains("adresi yok"), "{err}");
    }

    #[test]
    fn model_info_marks_only_db_rows_removable() {
        let m = parse_model_info(&sample_info());
        assert_eq!(m.len(), 3);
        assert_eq!(m[1], LocalModel { id: "qwen3-coder-next".into(), backend: Some("hosted_vllm/qwen3-coder-next".into()), removable: false });
        assert!(m[2].removable);
        // `/v1/models` yedeği: yalnız id.
        let v = serde_json::json!({ "data": [{ "id": "glm-4.7-flash", "object": "model" }] });
        assert_eq!(parse_model_info(&v)[0].id, "glm-4.7-flash");
    }

    #[test]
    fn registration_copies_plain_row_pattern() {
        let body = registration(&sample_info(), "dex-test-model").unwrap();
        assert_eq!(body["model_name"], "dex-test-model");
        let p = body["litellm_params"].as_object().unwrap();
        assert_eq!(p["model"], "hosted_vllm/dex-test-model");
        assert_eq!(p["api_base"], "http://127.0.0.1:8000/v1");
        assert!(!p.contains_key("extra_body"), "extra_body'li kayıt örnek alınmaz");
        assert!(!p.contains_key("api_key"), "maskeli anahtar kopyalanmaz");
        assert!(!p.contains_key("use_in_pass_through"), "bool bayraklar sunucu varsayılanı");
        assert!(registration(&serde_json::json!({ "data": [] }), "x").is_err());
    }

    #[test]
    fn backend_url_swaps_server_loopback_for_litellm_host() {
        assert_eq!(backend_models_url("http://192.0.2.10:4000", "http://127.0.0.1:8000/v1").as_deref(), Some("http://192.0.2.10:8000/v1/models"));
        assert_eq!(backend_models_url("http://h:4000/", "http://gpu.lan:8000/v1/").as_deref(), Some("http://gpu.lan:8000/v1/models"));
        assert_eq!(backend_health_url("http://192.0.2.10:4000", "http://127.0.0.1:8000/v1").as_deref(), Some("http://192.0.2.10:8000/health"));
    }

    #[test]
    fn health_says_which_model_is_loading() {
        let h = |v: serde_json::Value| loading_hint(&v, "qwen3-coder-next");
        // Boşta yanıt örneği: yüklü model yok → istenen yükleniyor.
        assert_eq!(h(serde_json::json!({ "status": "ok", "current": null, "active": [] })).as_deref(), Some("model yükleniyor: qwen3-coder-next"));
        assert_eq!(h(serde_json::json!({ "status": "ok", "current": "gpt-oss-120b", "active": [] })).as_deref(), Some("model yükleniyor: qwen3-coder-next"));
        assert_eq!(h(serde_json::json!({ "status": "ok", "current": "qwen3-coder-next", "active": ["x"] })), None, "yüklü: yalnız yavaş");
        assert_eq!(h(serde_json::json!({ "status": "loading", "current": "hosted_vllm/glm-4.7-flash" })).as_deref(), Some("model yükleniyor: glm-4.7-flash"));
    }

    // ── #2190: oMLX ──

    #[test]
    fn omlx_models_list_from_v1_models() {
        // oMLX'te `/model/info` yok; `model_info` `/v1/models`'a düşer.
        let v = serde_json::json!({ "object": "list", "data": [
            { "id": "gpt-oss-120b-heretic-v2", "object": "model", "owned_by": "omlx", "max_model_len": 131072 }
        ]});
        assert_eq!(parse_model_info(&v), vec![LocalModel { id: "gpt-oss-120b-heretic-v2".into(), backend: None, removable: false }]);
    }

    #[test]
    fn omlx_status_says_which_model_is_loading() {
        let st = |loaded, loading| serde_json::json!({ "models": [
            { "id": "gpt-oss-120b-heretic-v2", "loaded": loaded, "is_loading": loading, "model_context_length": 131072 }
        ]});
        let h = |v: serde_json::Value| omlx_loading_hint(&v, "gpt-oss-120b-heretic-v2");
        assert_eq!(h(st(false, false)).as_deref(), Some("model yükleniyor: gpt-oss-120b-heretic-v2"), "yüklü değil: ilk istek yükler");
        assert_eq!(h(st(false, true)).as_deref(), Some("model yükleniyor: gpt-oss-120b-heretic-v2"));
        assert_eq!(h(st(true, false)), None, "yüklü: yalnız yavaş");
        assert_eq!(omlx_loading_hint(&st(false, true), "baska-model"), None, "listede yok → bilinmiyor");
    }

    #[test]
    fn omlx_rejects_add_and_delete_litellm_passes() {
        let omlx = serde_json::json!({ "models": [{ "id": "m", "loaded": false, "is_loading": false }] });
        assert_eq!(local_kind(Some(&omlx)), LocalKind::Omlx);
        assert_eq!(registry_guard(Some(omlx)).unwrap_err(), OMLX_MANAGED);
        assert_eq!(registry_guard(Some(serde_json::json!({ "models": [] }))).unwrap_err(), OMLX_MANAGED);
        // LiteLLM: yol yok (hata → None) ya da `models`'siz gövde → kayıt akışı aynen sürer.
        assert_eq!(local_kind(None), LocalKind::Litellm);
        assert!(registry_guard(None).is_ok());
        assert!(registry_guard(Some(serde_json::json!({ "error": { "message": "not found" } }))).is_ok());
    }

    #[test]
    fn model_names_and_http_errors() {
        assert_eq!(check_model_name("  qwen3:8b ").unwrap(), "qwen3:8b");
        assert!(check_model_name("a b").is_err());
        assert!(check_model_name("").is_err());
        assert_eq!(http_error(500, "{'error': \"No DB Connected. Here's how…\"}"), REGISTRY_OFF);
        assert!(http_error(401, "bad key").contains("reddedildi"));
        assert_eq!(http_error(502, "<html>upstream</html>"), "Yerel LLM sunucusu isteği reddetti (HTTP 502).");
    }

    #[test]
    fn set_base_url_rejects_non_http() {
        assert!(set_base_url("192.0.2.10:4000").is_err());
    }

    /// Canlı sunucu ölçümü: `DEX_LIVE_LOCAL=1 cargo test -p dex-core live_local -- --nocapture`.
    /// Gerçek kasadaki adres/jetonla; kayıt kapalıysa ekleme sade REGISTRY_OFF döner, liste bozulmaz.
    #[test]
    fn live_local() {
        if std::env::var("DEX_LIVE_LOCAL").is_err() {
            return;
        }
        // UI'daki "routes.conf'tan içe aktar" ile aynı yol: [local] → adres + jeton kasaya.
        for r in import_routes().unwrap() {
            println!("içe aktarım {}: {:?} — {}", r.provider, r.token, r.note);
        }
        let st = local_status().unwrap();
        let ids: Vec<&str> = st.models.iter().map(|m| m.id.as_str()).collect();
        println!("adres={} kayıt={} modeller={}", st.base_url, st.registry, ids.join(","));
        assert!(ids.contains(&"qwen3-coder-next"));
        // Motorda olmayan ad → motor doğrulaması reddeder (sunucuya yazılmaz).
        let e = local_add("dex-test-probe").unwrap_err();
        println!("motorda olmayan ad: {e}");
        assert!(e.contains("Arka motor"));
        // Motorda olan ama LiteLLM'de o adla kayıtsız model → `/model/new`'e gider.
        match local_add("gpt-oss-120b-heretic-v2") {
            Ok(after) => {
                println!("eklendi; siliniyor");
                assert!(after.models.iter().any(|m| m.id == "gpt-oss-120b-heretic-v2" && m.removable));
                local_delete("gpt-oss-120b-heretic-v2").unwrap();
            }
            Err(e) => println!("ekleme reddi: {e}"),
        }
        let err = local_delete("qwen3-coder-next").unwrap_err();
        println!("config kaydı silme reddi: {err}");
        assert!(err.contains("config"));
        assert_eq!(local_status().unwrap().models.len(), st.models.len(), "sunucu listesi değişmemeli");
    }
}
