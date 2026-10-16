//! #1861 (kullanım şeridi Aşama 2): şeridin verisini Token Monitor yerine Dex'in kendisi üretir.
//! Sağlayıcı başına 5 saatlik + haftalık pencerede kalan yüzde ve sıfırlanma, bir de bugünün token'ı.
//! dexd verir (`usage_get` + `usage-snapshot` olayı): UI kaynağı bilmez, uzak dexd'de de çalışır.
//!
//! Kota kaynakları (hepsi hesap geneli; Dex dışındaki kullanım dahil):
//! - Claude: `quota.rs`'nin `/api/oauth/usage` ölçümü. İkinci sorgu yok; `quota_tick_for` buraya yazar.
//!   #2019: ikinci hesap (`anthropic-2`) kendi jetonuyla ayrı ölçülür; bağlı değilse şeritte yok.
//! - GLM: `GET api.z.ai/api/monitor/usage/quota/limit`, Dex'in GLM jetonuyla (`dex-provider/glm`).
//!   Token pencereleri (`TOKENS_LIMIT`/`CREDIT_LIMIT`) süreye göre sıralanır: en kısası 5 sa, en
//!   uzunu haftalık; tek pencere ≤6 sa ise 5 sa sayılır. Aylık MCP (`TIME_LIMIT`) şeride girmez.
//! - Muse: `POST api.meta.ai/muse-code/key` (Muse CLI'ın kendi anahtar basma ucu, #1987;
//!   fork'taki yoklamayla aynı uç). Kimlik `~/.config/muse/auth.json` OAuth `access_token`
//!   (`mechanism: oauth`), başlık `x-client-id: tbh:tui`, gövde `{}`, yönlendirme yok. Her ölçüm
//!   hesapta yeni bir API anahtarı basar (patron kabulü). Yanıtın `key` alanı hiçbir yere
//!   yazılmaz/loglanmaz ([`parse_muse`] onu okumaz bile). Son başarılı okuma `settings`
//!   satırında saklanır; `subs_usage` gelmezse ya da hata olursa son değer `measured_at` ile
//!   gösterilir. Süresi geçmiş pencere (`resets_at` < şimdi) geçersizdir: "--" ve
//!   [`MUSE_STALE_NOTE`]. Hiç değer yoksa "--" ve ölçüm notu.
//! Ölçüm aralığı quota.rs'nin kademesi ([`crate::quota::next_delay`]); bayatlık sınırı da onun
//! ([`crate::quota::STALE_MS`]): daha eski ölçüm "--" gösterilir — yalnız Muse'ta saklanan son
//! değer pencereleri geçerliyse `measured_at` ile gösterilmeye devam eder. Jeton, anahtar ve
//! yanıt gövdesi loglanmaz; hata yalnız durum kodu ya da kısa neden.
//!
//! Bugünün token'ı: Dex ajanlarının transcript kullanımı (`cost.rs`, sağlayıcı-farkında), bu
//! makinenin yerel gece yarısından beri input + output + cache okuma + cache yazma. Dex dışındaki
//! CLI kullanımı sayılmaz.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// Şeritteki sağlayıcılar (Dex kimlikleri), bu sırayla.
pub const PROVIDERS: [&str; 4] = ["anthropic", "anthropic-2", "glm", "muse"];

const ZAI_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const MUSE_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
/// #1987: son başarılı Muse okuması yokken yoklama `subs_usage` vermezse gösterilen neden.
pub const MUSE_NOTE: &str = "Muse anahtar ucu kullanım bildirmiyor";
/// #1987: saklanan Muse değerinin pencereleri süresi geçmişse gösterilen neden.
pub const MUSE_STALE_NOTE: &str = "pencere sıfırlandı";
/// İlk ölçüm açılıştan biraz sonra (kısa ömürlü çekirdekler uca istek atmasın).
const FIRST_POLL_MS: u64 = 5_000;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Kalan yüzde (0-100).
    pub remaining_percent: f64,
    /// RFC 3339.
    pub resets_at: Option<String>,
}

fn window(used: f64, resets_at: Option<String>) -> Window {
    Window { remaining_percent: (100.0 - used).clamp(0.0, 100.0), resets_at }
}

/// Süresi geçmiş Muse pencerelerini düşür (`resets_at` < şimdi): o pencerenin yüzdesi artık
/// geçersizdir. `resets_at` yoksa ya da okunamazsa pencere korunur (geçerli sayılır).
fn drop_expired(limits: &Limits, now: i64) -> Limits {
    let live = |w: &Option<Window>| match w {
        Some(w) => match w.resets_at.as_deref().and_then(crate::chain::parse_rfc3339) {
            Some(reset) if reset < now => None,
            _ => Some(w.clone()),
        },
        None => None,
    };
    Limits { five_hour: live(&limits.five_hour), weekly: live(&limits.weekly) }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub weekly: Option<Window>,
}

impl Limits {
    /// Kademe için doluluk (pencere yoksa 0).
    fn tier(&self) -> crate::quota::Usage {
        let used = |w: &Option<Window>| w.as_ref().map_or(0.0, |w| 100.0 - w.remaining_percent);
        crate::quota::Usage { five_hour: used(&self.five_hour), seven_day: used(&self.weekly), ..Default::default() }
    }
}

impl From<&crate::quota::Usage> for Limits {
    fn from(u: &crate::quota::Usage) -> Self {
        Self {
            five_hour: Some(window(u.five_hour, u.five_hour_resets_at.clone())),
            weekly: Some(window(u.seven_day, u.seven_day_resets_at.clone())),
        }
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub id: &'static str,
    #[serde(flatten)]
    pub limits: Limits,
    /// Pencereler neden "--" (taze ölçüm yok): kısa, sır içermez.
    pub note: Option<String>,
    /// Gösterilen ölçümün anı (epoch ms).
    pub measured_at: Option<i64>,
    /// Bugün bu sağlayıcıdaki Dex ajanlarının token'ı; `None` = okunamadı.
    pub today_tokens: Option<u64>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub providers: Vec<ProviderUsage>,
    /// Bugün tüm sağlayıcılarda (yerel LLM dahil) Dex ajanlarının token'ı.
    pub today_tokens: Option<u64>,
    /// Son 7 gün (bugün dahil) aynı tanımla; `None` = okunamadı.
    pub week_tokens: Option<u64>,
    /// Epoch ms.
    pub updated_at: i64,
}

#[derive(Default)]
struct Reading {
    ok: Option<(i64, Limits)>,
    error: Option<String>,
}

/// Ölçüm kaynağı; testler sahtesini verir.
pub trait LimitSource: Send + Sync {
    fn read(&self) -> Result<Limits, String>;
}

type Sources = Vec<(&'static str, Arc<dyn LimitSource>)>;

/// Core'daki durum: sağlayıcı başına son ölçüm, son yayınlanan görüntü, ağ kaynakları (yalnız GLM).
pub struct Meter {
    readings: Mutex<HashMap<&'static str, Reading>>,
    last: Mutex<Option<UsageSnapshot>>,
    sources: Mutex<Sources>,
}

impl Default for Meter {
    fn default() -> Self {
        let sources: Sources = if cfg!(test) {
            Vec::new() // testler ağa çıkmaz
        } else {
            real_sources()
        };
        Self { readings: Mutex::default(), last: Mutex::default(), sources: Mutex::new(sources) }
    }
}

/// Ağa çıkan ölçüm kaynakları. Muse da GLM ile aynı döngüde yoklanır
/// ([`crate::Core::usage_watch`]); basılan anahtar saklanmaz, loglanmaz.
fn real_sources() -> Sources {
    vec![("glm", Arc::new(ZaiLimits)), ("muse", Arc::new(MuseLimits))]
}

/// Görüntü: taze ölçüm pencereleri gösterir; yoksa "--" ve nedeni.
fn build(
    readings: &HashMap<&'static str, Reading>,
    tokens: Option<&BTreeMap<String, u64>>,
    week: Option<&BTreeMap<String, u64>>,
    now: i64,
) -> UsageSnapshot {
    let providers = PROVIDERS
        .iter()
        .map(|&id| {
            let r = readings.get(id);
            let fresh = r.and_then(|r| r.ok.as_ref()).filter(|(at, _)| now - at <= crate::quota::STALE_MS);
            let today = tokens.map(|t| t.get(id).copied().unwrap_or(0));
            if id == "muse" {
                // #1987: bayatlık sınırı Muse'a işlemez — saklanan son değer, pencereleri
                // geçerliyse `measured_at` ile gösterilmeye devam eder.
                let shown =
                    fresh.map(|(at, l)| (*at, l.clone())).or_else(|| r.and_then(|r| r.ok.as_ref()).map(|(at, l)| (*at, l.clone())));
                if let Some((at, limits)) = shown {
                    let live = drop_expired(&limits, now);
                    if live.five_hour.is_some() || live.weekly.is_some() {
                        return ProviderUsage { id, limits: live, note: None, measured_at: Some(at), today_tokens: today };
                    }
                    return ProviderUsage {
                        id,
                        limits: Limits::default(),
                        note: Some(MUSE_STALE_NOTE.into()),
                        measured_at: None,
                        today_tokens: today,
                    };
                }
            }
            let note = match (fresh, r) {
                (Some(_), _) => None,
                (None, Some(r)) => Some(r.error.clone().unwrap_or_else(|| "ölçüm bayat".into())),
                (None, None) => Some("henüz ölçülmedi".into()),
            };
            ProviderUsage {
                id,
                limits: fresh.map(|(_, l)| l.clone()).unwrap_or_default(),
                note,
                measured_at: fresh.map(|(at, _)| *at),
                today_tokens: tokens.map(|t| t.get(id).copied().unwrap_or(0)),
            }
        })
        .collect();
    UsageSnapshot {
        providers,
        today_tokens: tokens.map(|t| t.values().sum()),
        week_tokens: week.map(|t| t.values().sum()),
        updated_at: now,
    }
}

/// Bu makinenin `days_ago` gün önceki yerel gece yarısı (epoch ms); 0 = bugün.
fn local_midnight(now: chrono::DateTime<chrono::Local>, days_ago: u64) -> i64 {
    (now.date_naive() - chrono::Days::new(days_ago))
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|| now.timestamp_millis() - (days_ago as i64 + 1) * 86_400_000)
}

// ── Yanıt ayrıştırma ─────────────────────────────────────────────────────────

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|n: &f64| n.is_finite())
}

/// Epoch (20e9 altı saniye, üstü ms) ya da RFC 3339 → RFC 3339; okunamayan = `None`.
fn reset_at(v: &Value) -> Option<String> {
    let ms = match v {
        Value::String(s) => return crate::chain::parse_rfc3339(s).map(|_| s.clone()),
        v => num(v)?,
    };
    let ms = if ms < 20_000_000_000.0 { ms * 1000.0 } else { ms };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter().map(|k| &v[*k]).find(|x| !x.is_null()).unwrap_or(&Value::Null)
}

/// z.ai pencere süresi (dk): `unit` 5 = dakika, 3 = saat, 1 = gün, 6 = hafta.
fn zai_minutes(l: &Value) -> Option<f64> {
    let (unit, n) = (num(&l["unit"])?, num(&l["number"]).filter(|n| *n > 0.0)?);
    match unit as i64 {
        5 => Some(n),
        3 => Some(n * 60.0),
        1 => Some(n * 1440.0),
        6 => Some(n * 10_080.0),
        _ => None,
    }
}

/// Kullanılan yüzde: toplam (`usage`) ve kalan/şimdiki değerden; yoksa uçun yüzdesi.
fn zai_used(l: &Value) -> Option<f64> {
    let total = num(&l["usage"]).filter(|t| *t > 0.0);
    let current = num(first(l, &["currentValue", "current_value"]));
    if let Some(total) = total {
        let used = match num(&l["remaining"]) {
            Some(rem) => Some(current.map_or(total - rem, |c| (total - rem).max(c))),
            None => current,
        };
        if let Some(used) = used {
            return Some(used.clamp(0.0, total) / total * 100.0);
        }
    }
    num(first(l, &["percentage", "usedPercent", "used_percent"])).map(|p| p.clamp(0.0, 100.0))
}

pub fn parse_zai(v: &Value) -> Result<Limits, String> {
    let code = v["code"].as_i64();
    if matches!(code, Some(401 | 403)) {
        return Err(format!("z.ai jetonu reddetti (kod {})", code.unwrap_or_default()));
    }
    let limits = v["data"]["limits"].as_array().ok_or_else(|| match code {
        Some(c) => format!("z.ai kota yanıtı yok (kod {c})"),
        None => "z.ai kota yanıtında pencere yok".to_string(),
    })?;
    let mut tokens: Vec<(f64, Window)> = limits
        .iter()
        .filter(|l| {
            let kind = first(l, &["type", "limit_type"]).as_str().unwrap_or("").trim().to_ascii_uppercase();
            kind == "TOKENS_LIMIT" || kind == "CREDIT_LIMIT"
        })
        .filter_map(|l| {
            let used = zai_used(l)?;
            Some((zai_minutes(l).unwrap_or(f64::MAX), window(used, reset_at(first(l, &["nextResetTime", "next_reset_time"])))))
        })
        .collect();
    tokens.sort_by(|a, b| a.0.total_cmp(&b.0));
    let out = match tokens.len() {
        0 => return Err("z.ai yanıtında token penceresi yok".into()),
        1 => {
            let (minutes, w) = tokens.remove(0);
            if minutes <= 360.0 {
                Limits { five_hour: Some(w), weekly: None }
            } else {
                Limits { five_hour: None, weekly: Some(w) }
            }
        }
        n => {
            let weekly = tokens.remove(n - 1).1;
            Limits { five_hour: Some(tokens.remove(0).1), weekly: Some(weekly) }
        }
    };
    Ok(out)
}

pub fn parse_muse(v: &Value) -> Result<Limits, String> {
    let usage = &v["subs_usage"];
    if v["is_subs_active"] == Value::Bool(false) {
        return Err("Muse aboneliği etkin değil".into());
    }
    // Alan yoksa hata (boşta hesap gibi): çağrıcı saklanan son değeri göstermeye devam eder.
    if !usage.is_object() {
        return Err(MUSE_NOTE.into());
    }
    let win = |w: &Value| {
        let used = num(first(w, &["used_percent", "usedPercent"]))?;
        Some(window(used.clamp(0.0, 100.0), reset_at(first(w, &["resets_at", "resetsAt"]))))
    };
    let out = Limits { five_hour: win(&usage["window"]), weekly: win(&usage["weekly"]) };
    if out.five_hour.is_none() && out.weekly.is_none() {
        return Err("Muse yanıtında pencere yok".into());
    }
    Ok(out)
}

// ── Gerçek kaynaklar ─────────────────────────────────────────────────────────

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(12))
        // Jeton yönlendirmeyle başka bir sunucuya gitmesin.
        .redirects(0)
        .build()
}

/// Gövde yalnız ayrıştırılır; hata durum kodu/kısa neden, gövde ve başlık loglanmaz.
fn json(resp: Result<ureq::Response, ureq::Error>, label: &str) -> Result<Value, String> {
    match resp {
        Ok(r) if r.status() >= 300 => Err(format!("{label} HTTP {}", r.status())),
        Ok(r) => r.into_json().map_err(|_| format!("{label} yanıtı JSON değil")),
        Err(ureq::Error::Status(code, _)) => Err(format!("{label} HTTP {code}")),
        Err(ureq::Error::Transport(_)) => Err(format!("{label} ulaşılamadı")),
    }
}

/// GLM: Dex'in GLM jetonuyla z.ai kota ucu.
pub struct ZaiLimits;

impl LimitSource for ZaiLimits {
    fn read(&self) -> Result<Limits, String> {
        let token = crate::secrets::get(crate::providers::KEYRING_SERVICE, "glm")?
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or("GLM jetonu yok")?;
        let resp = agent()
            .get(ZAI_QUOTA_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .call();
        parse_zai(&json(resp, "z.ai kota ucu")?)
    }
}

/// Muse CLI'ın oturum dosyası: `$MUSE_AUTH_PATH`, yoksa `$XDG_CONFIG_HOME/muse`,
/// yoksa ev dizinindeki `.config/muse` (fork'taki `museAuthPath` ile aynı sıra).
fn muse_auth_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("MUSE_AUTH_PATH") {
        if !p.trim().is_empty() {
            return Some(p.into());
        }
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|h| std::path::PathBuf::from(h).join(".config"))
    })?;
    Some(base.join("muse").join("auth.json"))
}

/// OAuth oturumunun `access_token`i (`mechanism: oauth` şart; ham `META_API_KEY` yok).
/// Yoksa/giriş yoksa `None` — çağrıcı ağa çıkmaz.
fn muse_access_token() -> Option<String> {
    let raw = std::fs::read_to_string(muse_auth_path()?).ok()?;
    if raw.len() > 256 * 1024 {
        return None;
    }
    let v: Value = serde_json::from_str(&raw).ok()?;
    let meta = &v["providers"]["meta"];
    if meta["mechanism"].as_str()?.trim().to_ascii_lowercase() != "oauth" {
        return None;
    }
    let t = meta["access_token"].as_str()?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Muse: CLI'ın anahtar basma ucundan `subs_usage`. Basılan `key` yanıttan alınmaz,
/// saklanmaz, loglanmaz; istek gövdesi `{}` ve yönlendirme kapalı (fork ile aynı).
pub struct MuseLimits;

impl LimitSource for MuseLimits {
    fn read(&self) -> Result<Limits, String> {
        let token = muse_access_token().ok_or("Muse girişi yok (`muse login`)")?;
        let url = std::env::var("TBH_MINT_BASE_URL")
            .ok()
            .filter(|b| !b.trim().is_empty())
            .map(|b| format!("{}/muse-code/key", b.trim_end_matches('/')))
            .unwrap_or_else(|| MUSE_MINT_URL.into());
        let resp = agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("x-client-id", "tbh:tui")
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string("{}");
        parse_muse(&json(resp, "Muse anahtar ucu")?)
    }
}

// ── Son başarılı Muse okumasının kalıcılığı (`settings` satırı; migration yok) ──

/// `settings` anahtarı: JSON `{measuredAt, limits}`; satır yoksa/bozuksa ölçüm yok.
const MUSE_LAST_KEY: &str = "usage_muse_last";

#[derive(Serialize, Deserialize)]
struct MuseLast {
    #[serde(rename = "measuredAt")]
    measured_at: i64,
    limits: Limits,
}

fn save_muse_last(conn: &Connection, at: i64, limits: &Limits) -> Result<(), String> {
    let v = serde_json::to_string(&MuseLast { measured_at: at, limits: limits.clone() }).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![MUSE_LAST_KEY, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_muse_last(conn: &Connection) -> Option<(i64, Limits)> {
    let raw: Option<String> =
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [MUSE_LAST_KEY], |r| r.get(0)).ok()?;
    let last: MuseLast = serde_json::from_str(raw?.as_str()).ok()?;
    Some((last.measured_at, last.limits))
}

// ── Core ─────────────────────────────────────────────────────────────────────

impl crate::Core {
    /// GLM arka plan ölçümü: kaynak başına bir iş parçacığı, aralık quota.rs'nin kademesi.
    pub(crate) fn usage_watch(&self) {
        for (id, source) in self.usage.sources.lock().unwrap().clone() {
            let weak = self.weak_self.clone();
            std::thread::spawn(move || {
                let mut delay = FIRST_POLL_MS;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    let Some(core) = weak.upgrade() else { return };
                    if core.instance.lock().unwrap().is_none() {
                        return; // shutdown
                    }
                    let read = source.read();
                    let tier = read.as_ref().ok().map(Limits::tier);
                    core.usage_record(id, read);
                    delay = crate::quota::next_delay(delay, tier.as_ref());
                }
            });
        }
    }

    /// Bir ölçümü yazar ve görüntüyü yayınlar. Hata son başarılı ölçümü silmez (Muse'ta
    /// bayatlayınca bile gösterilir; penceresi geçmişse "--").
    pub(crate) fn usage_record(&self, id: &'static str, read: Result<Limits, String>) {
        let now = crate::model_news::now_ms();
        {
            let mut readings = self.usage.readings.lock().unwrap();
            let r = readings.entry(id).or_default();
            match read {
                Ok(limits) => {
                    r.ok = Some((now, limits.clone()));
                    r.error = None;
                    if id == "muse" {
                        // dexd yeniden başlasa da kalsın; yazılamazsa ölçüm yine de gösterilir.
                        let saved = crate::db::with(&self.db, |c| save_muse_last(c, now, &limits));
                        if let Err(err) = saved {
                            eprintln!("dex: Muse son ölçüm saklanamadı: {err}");
                        }
                    }
                }
                Err(err) => {
                    // Kalıcı neden (jeton yok) her denemede log'u doldurmasın: yalnız değişince.
                    if r.error.as_deref() != Some(err.as_str()) {
                        eprintln!("dex: {id} kullanımı okunamadı: {err}");
                    }
                    r.error = Some(err);
                }
            }
        }
        self.usage_publish();
    }

    /// Açılışta saklanan son Muse okumasını belleğe alır (yoksa/bozuksa "--" aynen kalır).
    pub(crate) fn usage_restore_muse(&self) {
        let loaded = crate::db::with(&self.db, |c| Ok(load_muse_last(c)));
        if let Ok(Some((at, limits))) = loaded {
            self.usage.readings.lock().unwrap().insert("muse", Reading { ok: Some((at, limits)), error: None });
        }
    }

    /// Bugünün kuyruğunu okuyup görüntüyü kurar; değiştiyse `usage-snapshot` yayınlar.
    fn usage_publish(&self) {
        let snap = self.usage_build(true);
        let mut last = self.usage.last.lock().unwrap();
        let same = last.as_ref().is_some_and(|l| {
            l.providers == snap.providers && l.today_tokens == snap.today_tokens && l.week_tokens == snap.week_tokens
        });
        if !same {
            *last = Some(snap.clone());
            drop(last);
            let _ = self.events.emit("usage-snapshot", &snap);
        }
    }

    fn usage_build(&self, ingest: bool) -> UsageSnapshot {
        let now = chrono::Local::now();
        // Kuyruklar bir kez, haftanın penceresiyle okunur; bugün aynı tablodan toplanır.
        let week = self.cost_tokens_since(local_midnight(now, 6), ingest).ok();
        let tokens = self.cost_tokens_since(local_midnight(now, 0), false).ok();
        let readings = self.usage.readings.lock().unwrap();
        let mut snap = build(&readings, tokens.as_ref(), week.as_ref(), crate::model_news::now_ms());
        // #2019: bağlı olmayan ek Claude hesabının halkası yok.
        snap.providers.retain(|p| {
            crate::claude_accounts::config_dir(&self.paths.data_dir, p.id).is_none_or(|d| crate::claude_accounts::connected(&d))
        });
        snap
    }

    /// #2019: hesabın bağlantısı kalktı — son ölçümü unutulur, görüntü yayınlanır.
    pub(crate) fn usage_forget(&self, id: &str) {
        self.usage.readings.lock().unwrap().remove(id);
        self.usage_publish();
    }

    /// Şeridin anlık görüntüsü (dosya okumaz; kuyruklar ölçüm iş parçacıklarında okunur).
    pub fn usage_get(&self) -> UsageSnapshot {
        self.usage_build(false)
    }

    #[cfg(test)]
    pub(crate) fn usage_set_sources(&self, sources: Sources) {
        *self.usage.sources.lock().unwrap() = sources;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn w(remaining: f64, reset: Option<&str>) -> Window {
        Window { remaining_percent: remaining, resets_at: reset.map(str::to_string) }
    }

    #[test]
    fn zai_two_token_windows_map_to_five_hour_and_weekly() {
        let body = json!({"code": 200, "success": true, "data": {"limits": [
            {"type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "currentValue": 900, "remaining": 100, "nextResetTime": 1_792_000_000_000_i64},
            {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "usage": 1000, "currentValue": 250, "remaining": 750, "nextResetTime": 1_792_100_000_000_i64},
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 12},
        ]}});
        let l = parse_zai(&body).unwrap();
        assert_eq!(l.five_hour, Some(w(88.0, None)), "en kısa pencere 5 sa; toplam yoksa uçun yüzdesi");
        assert_eq!(l.weekly, Some(w(75.0, Some("2026-10-15T21:33:20.000Z"))), "aylık MCP (TIME_LIMIT) girmez");
    }

    #[test]
    fn zai_used_takes_the_larger_of_remaining_and_current() {
        // `usage - remaining` ile `currentValue`'dan büyüğü; toplamı aşmaz.
        assert_eq!(zai_used(&json!({"usage": 100, "remaining": 70, "currentValue": 40})), Some(40.0));
        assert_eq!(zai_used(&json!({"usage": "100", "remaining": "90", "current_value": 20})), Some(20.0));
        assert_eq!(zai_used(&json!({"usage": 100, "currentValue": 250})), Some(100.0));
        assert_eq!(zai_used(&json!({"usage": 0, "percentage": 140})), Some(100.0));
        assert_eq!(zai_used(&json!({"usage": 100})), None);
    }

    #[test]
    fn zai_single_window_by_length_and_errors() {
        let one = |unit: i64, number: i64| json!({"data": {"limits": [{"type": "CREDIT_LIMIT", "unit": unit, "number": number, "percentage": 30}]}});
        assert_eq!(parse_zai(&one(3, 5)).unwrap(), Limits { five_hour: Some(w(70.0, None)), weekly: None });
        assert_eq!(parse_zai(&one(1, 7)).unwrap(), Limits { five_hour: None, weekly: Some(w(70.0, None)) });
        assert!(parse_zai(&json!({"code": 401, "msg": "token expired"})).unwrap_err().contains("401"));
        assert!(parse_zai(&json!({"code": 500, "msg": "no subscription"})).unwrap_err().contains("500"));
        assert!(parse_zai(&json!({"data": {"limits": [{"type": "TIME_LIMIT", "percentage": 3}]}})).is_err());
    }

    #[test]
    fn muse_reads_subscription_windows_and_ignores_the_key() {
        let body = json!({"key": "sentinel-minted", "is_subs_active": true, "subs_usage": {
            "window": {"used_percent": 22.5, "resets_at": 1_792_000_000, "window_duration_mins": 300},
            "weekly": {"usedPercent": "61", "resetsAt": "2026-10-20T00:00:00Z"},
        }});
        let l = parse_muse(&body).unwrap();
        assert_eq!(l.five_hour, Some(w(77.5, Some("2026-10-14T17:46:40.000Z"))));
        assert_eq!(l.weekly, Some(w(39.0, Some("2026-10-20T00:00:00Z"))));
        assert!(!format!("{l:?}").contains("sentinel"));
        assert_eq!(parse_muse(&json!({"is_subs_active": false, "subs_usage": {}})).unwrap_err(), "Muse aboneliği etkin değil");
        assert_eq!(parse_muse(&json!({"is_subs_active": true})).unwrap_err(), "Muse anahtar ucu kullanım bildirmiyor");
        assert!(parse_muse(&json!({"subs_usage": {"window": {}}})).is_err());
    }

    /// #1987: Muse GLM ile aynı döngüde ölçülür; hiç değer yokken şerit "--" gösterir.
    #[test]
    fn muse_is_measured_like_glm() {
        let ids: Vec<_> = real_sources().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["glm", "muse"], "Muse ölçüm kaynağı olmalı");
        let meter = Meter::default();
        let s = build(&meter.readings.lock().unwrap(), None, None, 1_000);
        let muse = s.providers.iter().find(|p| p.id == "muse").unwrap();
        assert_eq!(
            (&muse.limits, muse.note.as_deref(), muse.measured_at),
            (&Limits::default(), Some("henüz ölçülmedi"), None)
        );
    }

    /// #1987: `subs_usage` gelmezse ya da hata olursa son değer `measured_at` ile durur
    /// (bayatlık sınırı Muse'a işlemez).
    #[test]
    fn muse_holds_last_value_when_subs_usage_missing() {
        let now = 10_000_000;
        let limits =
            Limits { five_hour: Some(w(77.5, Some("2026-10-20T00:00:00Z"))), weekly: Some(w(39.0, None)) };
        let at = now - crate::quota::STALE_MS - 60_000;
        let mut readings = HashMap::new();
        readings.insert("muse", Reading { ok: Some((at, limits.clone())), error: Some(MUSE_NOTE.into()) });
        let s = build(&readings, None, None, now);
        let muse = s.providers.iter().find(|p| p.id == "muse").unwrap();
        assert_eq!((&muse.limits, muse.note.as_deref(), muse.measured_at), (&limits, None, Some(at)));
    }

    /// #1987: süresi geçmiş pencere geçersizdir — "--" + sıfırlanma notu; geçerli
    /// pencere tek başına durur.
    #[test]
    fn muse_expired_window_shows_reset() {
        let now = 10_000_000;
        let held = |five: Option<Window>, weekly: Option<Window>| {
            let mut readings = HashMap::new();
            readings.insert("muse", Reading { ok: Some((now - 1_000, Limits { five_hour: five, weekly })), error: None });
            build(&readings, None, None, now).providers.into_iter().find(|p| p.id == "muse").unwrap()
        };
        let past = || Some(w(10.0, Some("1970-01-01T00:00:01Z")));
        let future = || Some(w(20.0, Some("2026-10-20T00:00:00Z")));
        let muse = held(past(), past());
        assert_eq!((&muse.limits, muse.note.as_deref(), muse.measured_at), (&Limits::default(), Some(MUSE_STALE_NOTE), None));
        let muse = held(past(), future());
        assert_eq!(muse.limits, Limits { five_hour: None, weekly: future() });
        assert_eq!((muse.note.as_deref(), muse.measured_at), (None, Some(now - 1_000)));
        // `resets_at` yoksa/okunamazsa pencere geçerli sayılır.
        let muse = held(Some(w(10.0, None)), None);
        assert_eq!(muse.limits.five_hour, Some(w(10.0, None)));
    }

    /// #1987: son Muse okuması `settings` satırında gidip gelir; bozuk satır "--"a düşer.
    #[test]
    fn muse_last_round_trips_through_settings() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)", []).unwrap();
        assert_eq!(load_muse_last(&c), None);
        let limits = Limits { five_hour: Some(w(77.5, Some("2026-10-20T00:00:00Z"))), weekly: None };
        save_muse_last(&c, 1_792_000_000_000, &limits).unwrap();
        assert_eq!(load_muse_last(&c), Some((1_792_000_000_000, limits.clone())));
        save_muse_last(&c, 1_792_000_001_000, &limits).unwrap();
        assert_eq!(load_muse_last(&c).unwrap().0, 1_792_000_001_000, "upsert idempotent");
        c.execute("UPDATE settings SET value = 'bozuk' WHERE key = 'usage_muse_last'", []).unwrap();
        assert_eq!(load_muse_last(&c), None);
    }

    /// #1987: kimlik yalnız OAuth oturumundan okunur; ham anahtar/girişsizlik ağa çıkarmaz.
    #[test]
    fn muse_login_reads_only_oauth_session() {
        let dir = std::env::temp_dir().join(format!("dex-muse-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        let prev = std::env::var_os("MUSE_AUTH_PATH");
        unsafe { std::env::set_var("MUSE_AUTH_PATH", &path) };
        let token = |body: &str| {
            std::fs::write(&path, body).unwrap();
            muse_access_token()
        };
        assert_eq!(muse_auth_path(), Some(path.clone()));
        assert_eq!(
            token(r#"{"providers":{"meta":{"mechanism":"oauth","access_token":" tok ","user_email":"a@b.c"}}}"#).as_deref(),
            Some("tok")
        );
        assert_eq!(token(r#"{"providers":{"meta":{"mechanism":"apikey","access_token":"x"}}}"#), None);
        assert_eq!(token("bozuk"), None);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(muse_access_token(), None);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("MUSE_AUTH_PATH", v),
                None => std::env::remove_var("MUSE_AUTH_PATH"),
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn reset_accepts_seconds_millis_and_rfc3339() {
        assert_eq!(reset_at(&json!(1_792_000_000)), Some("2026-10-14T17:46:40.000Z".into()));
        assert_eq!(reset_at(&json!(1_792_000_000_000_i64)), reset_at(&json!(1_792_000_000)));
        assert_eq!(reset_at(&json!("2026-10-20T00:00:00+03:00")), Some("2026-10-20T00:00:00+03:00".into()));
        assert_eq!(reset_at(&json!("yarın")), None);
        assert_eq!(reset_at(&Value::Null), None);
    }

    #[test]
    fn snapshot_shows_only_fresh_readings_and_says_why() {
        let now = 10_000_000;
        let limits = Limits { five_hour: Some(w(40.0, None)), weekly: Some(w(90.0, None)) };
        let mut readings = HashMap::new();
        readings.insert("anthropic", Reading { ok: Some((now - 1_000, limits.clone())), error: Some("geçici".into()) });
        readings.insert("glm", Reading { ok: Some((now - crate::quota::STALE_MS - 1, limits.clone())), error: Some("z.ai kota ucu HTTP 500".into()) });
        let tokens: BTreeMap<String, u64> = [("anthropic".into(), 1_000), ("local".into(), 5)].into();
        let week: BTreeMap<String, u64> = [("anthropic".into(), 3_000), ("glm".into(), 7)].into();
        let s = build(&readings, Some(&tokens), Some(&week), now);
        assert_eq!(s.providers.iter().map(|p| p.id).collect::<Vec<_>>(), PROVIDERS);
        let [claude, claude2, glm, muse] = &s.providers[..] else { panic!() };
        assert_eq!(claude2.note.as_deref(), Some("henüz ölçülmedi"), "ikinci hesap ayrı satır");
        assert_eq!((&claude.limits, claude.note.as_deref(), claude.measured_at), (&limits, None, Some(now - 1_000)));
        assert_eq!((&glm.limits, glm.note.as_deref()), (&Limits::default(), Some("z.ai kota ucu HTTP 500")), "bayat ölçüm gösterilmez");
        assert_eq!(muse.note.as_deref(), Some("henüz ölçülmedi"));
        assert_eq!((claude.today_tokens, glm.today_tokens, s.today_tokens), (Some(1_000), Some(0), Some(1_005)));
        assert_eq!(s.week_tokens, Some(3_007));
        let none = build(&readings, None, None, now);
        assert_eq!((none.today_tokens, none.week_tokens), (None, None), "okunamayan token uydurulmaz");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["providers"][0]["fiveHour"]["remainingPercent"], 40.0);
        assert_eq!(v["providers"][2]["weekly"], Value::Null);
    }

    #[test]
    fn tier_follows_quota_pattern() {
        let l = Limits { five_hour: Some(w(25.0, None)), weekly: None };
        assert_eq!(crate::quota::tier_delay(&l.tier()), crate::quota::MIN_POLL_MS, "%75 dolu → 60 sn");
        assert_eq!(crate::quota::tier_delay(&Limits::default().tier()), crate::quota::MAX_POLL_MS);
    }

    #[test]
    fn midnight_is_local_start_of_day() {
        use chrono::TimeZone;
        let t = chrono::Local.with_ymd_and_hms(2026, 10, 15, 13, 45, 0).unwrap();
        let at = |m| chrono::Local.timestamp_millis_opt(m).unwrap();
        assert_eq!(at(local_midnight(t, 0)), chrono::Local.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap());
        assert_eq!(at(local_midnight(t, 6)), chrono::Local.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap(), "7 gün: bugün dahil");
    }

    struct Fixed(Result<Limits, String>);
    impl LimitSource for Fixed {
        fn read(&self) -> Result<Limits, String> {
            self.0.clone()
        }
    }

    /// Claude `quota_tick`'ten, diğerleri kaynaktan; değişen görüntü bir kez yayınlanır.
    #[test]
    fn core_records_publishes_on_change_and_serves_usage_get() {
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<Value>>>);
        impl crate::EventSink for Sink {
            fn emit(&self, name: &str, value: Value) -> Result<(), String> {
                if name == "usage-snapshot" {
                    self.0.lock().unwrap().push(value);
                }
                Ok(())
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/usage-{}", std::process::id()));
        let sink = Sink::default();
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(sink.clone()),
        )
        .unwrap();
        struct Claude;
        impl crate::quota::UsageSource for Claude {
            fn read(&self) -> Result<crate::quota::Usage, String> {
                Ok(crate::quota::Usage { five_hour: 30.0, seven_day: 55.0, ..Default::default() })
            }
        }
        core.quota_set_source(Arc::new(Claude));
        assert!(core.quota_tick());
        let glm = Limits { five_hour: Some(w(10.0, None)), weekly: None };
        core.usage_set_sources(vec![("glm", Arc::new(Fixed(Ok(glm.clone()))))]);
        let source = core.usage.sources.lock().unwrap()[0].1.clone();
        core.usage_record("glm", source.read());
        let got = core.usage_get();
        assert_eq!(got.providers[0].limits.five_hour, Some(w(70.0, None)));
        assert_eq!(got.providers[0].limits.weekly, Some(w(45.0, None)));
        assert_eq!(got.providers[1].limits, glm);
        assert_eq!((&got.providers[2].limits, got.providers[2].note.as_deref()), (&Limits::default(), Some("henüz ölçülmedi")));
        assert_eq!((got.today_tokens, got.week_tokens), (Some(0), Some(0)));
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "her ölçüm görüntüyü değiştirdi");
        assert_eq!(events[1]["providers"][1]["fiveHour"]["remainingPercent"], 10.0);
        // #1987: Muse hatası son değeri silmez; başarılı okuma `settings`'e yazılır.
        let muse = Limits { five_hour: Some(w(30.0, Some("2026-10-20T00:00:00Z"))), weekly: None };
        core.usage_record("muse", Ok(muse.clone()));
        core.usage_record("muse", Err(MUSE_NOTE.into()));
        let got = core.usage_get();
        assert_eq!(&got.providers[2].limits, &muse, "hata son değeri silmez");
        let at = got.providers[2].measured_at.unwrap();
        assert_eq!(crate::db::with(&core.db, |c| Ok(load_muse_last(c))).unwrap(), Some((at, muse)));
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Canlı salt okuma: GLM ucu gerçek jetonla. Yalnız ayrıştırılmış yüzdeler basılır.
    #[test]
    #[ignore = "ağ + gerçek GLM jetonu ister"]
    fn live_limits() {
        match ZaiLimits.read() {
            Ok(l) => println!("glm: 5sa={:?} hafta={:?}", l.five_hour, l.weekly),
            Err(e) => println!("glm: hata {e}"),
        }
    }

    /// Canlı salt okuma: Muse anahtar ucu gerçek OAuth oturumuyla (#1987). Yalnız
    /// ayrıştırılmış yüzdeler basılır; anahtar/token basılmaz. Her çağrı hesapta
    /// yeni bir anahtar basar — seyrek koş.
    #[test]
    #[ignore = "ağ + gerçek Muse girişi ister"]
    fn live_muse() {
        match MuseLimits.read() {
            Ok(l) => println!("muse: 5sa={:?} hafta={:?}", l.five_hour, l.weekly),
            Err(e) => println!("muse: hata {e}"),
        }
    }
}
