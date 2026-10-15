//! Sağlayıcıların sunduğu modeller sorulur. GLM ve Muse'de cevap KATALOG olur (#1728): sabit
//! listede olmayan sohbet modelleri seçicide görünür (`providers::models`). Son başarılı liste
//! `settings.models_catalog`'ta önbellek; ağ/jeton yoksa önbellek, o da yoksa sabit liste kullanılır.
//! Claude kataloğu sabit kalır; orada yalnız haber verilir. Kataloğa giren yeni model için tek
//! seferlik "yeni model" haberi çıkar; kapatılan haber `settings.models_seen`'e yazılır, bir daha çıkmaz.
//!
//! Yöntem: GLM ve Muse, Dex kasasındaki (`dex-provider`) jetonla
//! `GET {base}/v1/models` + `Authorization: Bearer` (Muse `x-api-key`'i 401 ile reddeder).
//! GLM Anthropic biçimi döner (`data[].id/display_name/created_at`), Muse OpenAI biçimi (`data[].id`).
//! Claude: CLI'da liste alt komutu yok; Claude Code'un abonelik OAuth jetonu YALNIZ
//! `GET api.anthropic.com/v1/models` için kullanılır. Jeton yalnız bellekte; süresi
//! geçmişse yenilenmez (dosyaya yazmayız), Claude o açılışta atlanır.
//!
//! Sıklık: GLM/Muse saatte, Claude 24 saatte en çok bir kez (açılış, saatlik UI zamanlayıcısı ve
//! model seçici açılışı tetikler). Son BAŞARILI kontrol `settings.models_checked`'te (sağlayıcı → ms);
//! cevapsız sağlayıcı zamanı güncellemez, sonraki tetikte yeniden denenir.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::providers::{self, LiveModel, ProviderDef, KEYRING_SERVICE};

const SEEN: &str = "models_seen";
const CHECKED: &str = "models_checked";
const CATALOG: &str = "models_catalog";
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Sağlayıcı → sohbet DIŞI aileler (önek; Muse ses/görsel/segmentasyon da listeler). Bilinmeyen yeni
/// aile sohbet modeli sayılır: yanlışsa başlatmada sağlayıcı hata döner, patron görür.
const SOURCES: &[(&str, &[&str])] = &[("anthropic", &[]), ("glm", &[]), ("muse", &["sam-", "muse-voice-", "muse-image-"])];

/// Kataloğu canlı listeden kurulan sağlayıcılar (saatlik). Claude yalnız haber (günlük).
const LIVE_CATALOG: &[&str] = &["glm", "muse"];

type Offered = Vec<(String, Vec<(String, String)>)>;

const ANTHROPIC_MODELS: &str = "https://api.anthropic.com/v1/models?limit=1000";

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewModel {
    pub provider: String,
    pub provider_label: String,
    pub id: String,
    pub label: String,
    /// Model seçicide görünüyor (GLM/Muse canlı katalog); `false` = Claude, yalnız haber.
    pub listed: bool,
}

impl NewModel {
    /// `settings.models_seen` anahtarı.
    pub fn key(&self) -> String {
        format!("{}:{}", self.provider, self.id.to_ascii_lowercase())
    }
}

/// Sağlayıcının `/v1/models` cevabından `(id, görünen ad)`.
fn parse_models(v: &serde_json::Value) -> Vec<(String, String)> {
    v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let id = r.get("id")?.as_str()?.to_string();
            let label = r.get("display_name").and_then(|x| x.as_str()).unwrap_or(&id).to_string();
            Some((id, label))
        })
        .collect()
}

/// Claude Code'un OAuth erişim jetonu: `<CLAUDE_CONFIG_DIR|~/.claude>/.credentials.json`, yoksa
/// işletim sistemi kasasındaki `Claude Code-credentials` girdisi (aynı JSON). Yalnız okunur.
pub(crate) fn claude_token() -> Option<String> {
    let dir = crate::claude_accounts::main_dir();
    let from_file = dir.and_then(|d| std::fs::read_to_string(d.join(".credentials.json")).ok());
    let raw = from_file.or_else(|| {
        let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok()?;
        crate::secrets::get("Claude Code-credentials", &user).ok().flatten()
    })?;
    oauth_access(&serde_json::from_str(&raw).ok()?, now_ms())
}

/// #2019: ek Claude hesabının (kendi `CLAUDE_CONFIG_DIR`'ı) erişim jetonu — yalnız o dizindeki
/// `.credentials.json` (kasa yedeği yok: ek hesap dosyada durur). Yalnız okunur, bellekte kalır.
pub(crate) fn claude_token_in(dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(".credentials.json")).ok()?;
    oauth_access(&serde_json::from_str(&raw).ok()?, now_ms())
}

fn oauth_access(v: &serde_json::Value, now: i64) -> Option<String> {
    let o = v.get("claudeAiOauth")?;
    let expired = o.get("expiresAt").and_then(|e| e.as_i64()).is_some_and(|e| e <= now);
    let token = o.get("accessToken")?.as_str()?;
    (!expired && !token.is_empty()).then(|| token.to_string())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn fetch_one(provider: &str) -> Option<Vec<(String, String)>> {
    let (url, token, oauth) = if provider == "anthropic" {
        (ANTHROPIC_MODELS.to_string(), claude_token()?, true)
    } else {
        let base = providers::get(provider)?.base_url?;
        let token = crate::secrets::get(KEYRING_SERVICE, provider).ok().flatten().filter(|t| !t.is_empty())?;
        (format!("{base}/v1/models"), token, false)
    };
    let mut req = http()
        .get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-version", "2023-06-01");
    if oauth {
        req = req.set("anthropic-beta", "oauth-2025-04-20");
    }
    let v: serde_json::Value = req
        .call()
        .ok()?
        .into_json()
        .ok()?;
    // Boş liste cevap sayılmaz: önbellek boş bir listeyle ezilmesin.
    Some(parse_models(&v)).filter(|l| !l.is_empty())
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(10))
        .build()
}

/// Ağ çağrısı (sağlayıcı başına en çok ~13 sn); UI iş parçacığında koşturma. Cevapsız sağlayıcı yok sayılır.
pub fn fetch(only: &[&'static str]) -> Vec<(String, Vec<(String, String)>)> {
    let handles: Vec<_> = only
        .iter()
        .map(|&p| (p.to_string(), std::thread::spawn(move || fetch_one(p))))
        .collect();
    handles.into_iter().filter_map(|(p, h)| Some((p, h.join().ok()??))).collect()
}

/// Soy + sürüm: ilk sayıdan önceki parçalar soydur. `glm-5.3-flash` → (`glm`, (5, 3)),
/// `muse-wonder-1.0` → (`muse-wonder`, (1, 0)); tarih gibi uzun sayılar (20251001) sürüm sayılmaz.
fn line_version(id: &str) -> (String, Option<(u32, u32)>) {
    let mut line = Vec::new();
    let mut nums = Vec::new();
    for part in id.to_ascii_lowercase().split(['-', '.', '_']) {
        match part.parse::<u32>() {
            Ok(n) if part.len() <= 3 => nums.push(n),
            Ok(_) => {}
            Err(_) if nums.is_empty() => line.push(part.to_string()),
            Err(_) => {}
        }
    }
    let version = nums.first().map(|&a| (a, nums.get(1).copied().unwrap_or(0)));
    (line.join("-"), version)
}

/// Aynı soy ya da biri ötekinin alt soyu (`claude` ⊃ `claude-opus`; `muse-wonder` ⊃ `muse-wonder-full`).
fn related(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}-")) || b.starts_with(&format!("{a}-"))
}

fn is_chat(provider: &str, id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    SOURCES.iter().find(|(p, _)| *p == provider).is_some_and(|(_, skip)| !skip.iter().any(|s| id.starts_with(s)))
}

/// Görünen ad: sağlayıcı verdiyse o (GLM `display_name`), yoksa id'den (`muse-wonder-1.0` →
/// `Muse Wonder 1.0`); contributor eki sabit listedeki açıklamayla.
fn label_of(id: &str, given: &str) -> String {
    if given != id {
        return given.to_string();
    }
    let (base, contributor) = match id.strip_suffix("-contributor") {
        Some(b) => (b, true),
        None => (id, false),
    };
    let words: Vec<String> = base
        .split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        })
        .collect();
    let label = words.join(" ");
    if contributor {
        format!("{label} — contributor (veriniz ürün geliştirmede kullanılır)")
    } else {
        label
    }
}

/// Sağlayıcının sunduğu sohbet modellerinden sabit listede olmayanlar. Eski sürüm (legacy: geçerli
/// ama seçicide yok): ilgili soyda sabit listede daha yeni sürüm varsa — GLM-4.x / Muse 1.1 gibi
/// bilerek listelenmeyen modeller seçiciyi ve haberleri doldurmasın. Sabit listede soyu olmayan
/// yeni aile (Muse Wonder) güncel sayılır.
pub fn live_catalog(def: &ProviderDef, offered: &[(String, String)]) -> Vec<LiveModel> {
    let known: Vec<(String, (u32, u32))> = def
        .models
        .iter()
        .filter_map(|m| {
            let (l, v) = line_version(m.id);
            Some((l, v?))
        })
        .collect();
    offered
        .iter()
        .filter(|(id, _)| is_chat(def.id, id) && !def.models.iter().any(|m| m.id.eq_ignore_ascii_case(id)))
        .map(|(id, label)| {
            let (line, version) = line_version(id);
            let newest = known.iter().filter(|k| related(&k.0, &line)).map(|k| k.1).max();
            LiveModel {
                id: id.clone(),
                label: label_of(id, label),
                legacy: matches!((version, newest), (Some(v), Some(n)) if v < n),
            }
        })
        .collect()
}

/// Sabit listede olmayan, eski sürüm olmayan ve daha önce kapatılmamış modeller. Contributor eşi
/// ayrı haber olmaz: ana model de yeniyse onun kartı yeter.
pub fn news(offered: &[(String, Vec<(String, String)>)], seen: &BTreeSet<String>) -> Vec<NewModel> {
    let mut out = Vec::new();
    for (provider, models) in offered {
        let Some(def) = providers::get(provider).filter(|d| SOURCES.iter().any(|(p, _)| *p == d.id)) else {
            continue;
        };
        let live = live_catalog(def, models);
        for m in live.iter().filter(|m| !m.legacy) {
            if m.id.strip_suffix("-contributor").is_some_and(|b| live.iter().any(|x| x.id.eq_ignore_ascii_case(b))) {
                continue;
            }
            let item = NewModel {
                provider: provider.clone(),
                provider_label: def.label.to_string(),
                id: m.id.clone(),
                label: m.label.clone(),
                listed: LIVE_CATALOG.contains(&def.id),
            };
            if !seen.contains(&item.key()) {
                out.push(item);
            }
        }
    }
    out
}

fn load<T: serde::de::DeserializeOwned + Default>(conn: &Connection, key: &str) -> T {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

fn store<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<(), String> {
    let v = serde_json::to_string(value).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Son başarılı kontrolü vadesini (GLM/Muse 1 saat, Claude 24 saat) doldurmuş ya da hiç olmamış sağlayıcılar.
pub fn due(conn: &Connection, now: i64) -> Vec<&'static str> {
    let checked: BTreeMap<String, i64> = load(conn, CHECKED);
    SOURCES
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| {
            let every = if LIVE_CATALOG.contains(p) { HOUR_MS } else { DAY_MS };
            checked.get(*p).is_none_or(|&t| now - t >= every || t > now)
        })
        .collect()
}

/// Model seçici açılırken arka planda tazeleme gerekir mi (yalnız katalog sağlayıcıları).
pub fn catalog_due(conn: &Connection, now: i64) -> bool {
    due(conn, now).iter().any(|p| LIVE_CATALOG.contains(p))
}

/// Önbellekteki son başarılı listeler (ham: sohbet dışı dahil — süzme okurken yapılır).
pub fn cached(conn: &Connection) -> Offered {
    load::<BTreeMap<String, Vec<(String, String)>>>(conn, CATALOG).into_iter().collect()
}

/// Cevap veren sağlayıcıların listesi önbelleği ezer; cevapsızlarınki olduğu gibi kalır.
pub fn store_offered(conn: &Connection, offered: &Offered) -> Result<(), String> {
    if offered.is_empty() {
        return Ok(());
    }
    let mut all: BTreeMap<String, Vec<(String, String)>> = load(conn, CATALOG);
    for (p, models) in offered {
        all.insert(p.clone(), models.clone());
    }
    store(conn, CATALOG, &all)
}

/// Önbellekten GLM/Muse'nin canlı kataloğu (`providers::set_live` girdisi).
pub fn catalogs(conn: &Connection) -> BTreeMap<String, Vec<LiveModel>> {
    cached(conn)
        .into_iter()
        .filter(|(p, _)| LIVE_CATALOG.contains(&p.as_str()))
        .filter_map(|(p, models)| Some((p.clone(), live_catalog(providers::get(&p)?, &models))))
        .collect()
}

static REFRESHING: AtomicBool = AtomicBool::new(false);

struct Refreshing;

impl Drop for Refreshing {
    fn drop(&mut self) {
        REFRESHING.store(false, Ordering::SeqCst);
    }
}

/// Vadesi gelen sağlayıcıları sorar; önbelleği ve bellek kataloğunu tazeler, katalog değiştiyse
/// `providers-changed` yayınlar (model seçici ve haber kartı yeniden çeker). Ağ çağrısı DB kilidi
/// dışında (en çok ~13 sn); aynı anda tek tazeleme, ikincisi beklemeden döner.
pub fn refresh(db: &crate::db::Db, events: &crate::events::Events) -> Result<(), String> {
    if REFRESHING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let _guard = Refreshing;
    let now = now_ms();
    let due = crate::db::with(db, |c| Ok(due(c, now)))?;
    if due.is_empty() {
        return Ok(());
    }
    let offered = fetch(&due);
    let changed = crate::db::with(db, |c| {
        mark_checked(c, &offered.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), now)?;
        store_offered(c, &offered)?;
        Ok(providers::set_live(catalogs(c)))
    })?;
    if changed {
        let _ = events.emit("providers-changed", &());
    }
    Ok(())
}

/// Cevap veren sağlayıcıların kontrol zamanı; cevapsızlar listede yoktur, zamanları değişmez.
pub fn mark_checked(conn: &Connection, answered: &[String], now: i64) -> Result<(), String> {
    if answered.is_empty() {
        return Ok(());
    }
    let mut checked: BTreeMap<String, i64> = load(conn, CHECKED);
    for p in answered {
        checked.insert(p.clone(), now);
    }
    store(conn, CHECKED, &checked)
}

pub fn seen(conn: &Connection) -> BTreeSet<String> {
    load(conn, SEEN)
}

/// Kapatılan haberler bir daha gösterilmez. Anahtar `NewModel::key` biçiminde olmalı.
pub fn mark_seen(conn: &Connection, keys: &[String]) -> Result<(), String> {
    let mut all = seen(conn);
    for k in keys {
        let k = k.trim().to_ascii_lowercase();
        if k.split_once(':').is_some_and(|(p, id)| providers::get(p).is_some() && !id.is_empty()) {
            all.insert(k);
        }
    }
    store(conn, SEEN, &all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(p: &str, ids: &[&str]) -> (String, Vec<(String, String)>) {
        (p.into(), ids.iter().map(|i| (i.to_string(), i.to_uppercase())).collect())
    }

    /// Örnek model listeleri: yalnız yeni, legacy olmayan model (FlashX) haber olur.
    #[test]
    fn only_new_non_legacy_models() {
        let glm = offered("glm", &[
            "glm-4.5", "glm-4.5-air", "glm-4.6", "glm-4.7", "glm-5", "glm-5-turbo", "glm-5.1", "glm-5.2",
            "glm-5.3", "glm-5.3-flash", "glm-5.3-flashx",
        ]);
        let muse = offered("muse", &[
            "sam-3.1", "muse-spark-1.3-contributor", "muse-voice-transcribe-1.0", "muse-spark-1.3", "muse-image-1.0",
            "muse-spark-1.2-contributor", "muse-spark-1.2", "muse-spark-1.1",
        ]);
        let n = news(&[glm, muse], &BTreeSet::new());
        let ids: Vec<&str> = n.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["glm-5.3-flashx"]);
        assert_eq!((n[0].provider_label.as_str(), n[0].label.as_str(), n[0].key().as_str()), ("GLM", "GLM-5.3-FLASHX", "glm:glm-5.3-flashx"));
    }

    #[test]
    fn claude_old_generations_are_not_news() {
        let claude = offered("anthropic", &[
            "claude-fable-5-1", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5-20251001",
            "claude-opus-4-1-20250805", "claude-sonnet-4-5-20250929", "claude-3-5-haiku-20241022", "claude-opus-6-0",
        ]);
        let ids: Vec<String> = news(&[claude], &BTreeSet::new()).into_iter().map(|m| m.key()).collect();
        assert_eq!(ids, ["anthropic:claude-opus-6-0"]);
    }

    #[test]
    fn oauth_token_only_while_valid() {
        let v = serde_json::json!({"claudeAiOauth": {"accessToken": "tok", "expiresAt": 2000}});
        assert_eq!(oauth_access(&v, 1000).as_deref(), Some("tok"));
        assert_eq!(oauth_access(&v, 2000), None, "süresi geçmiş jeton kullanılmaz");
        assert_eq!(oauth_access(&serde_json::json!({}), 0), None);
    }

    #[test]
    fn newer_versions_and_new_families_are_news() {
        let n = news(&[offered("muse", &["muse-spark-1.4", "muse-spark-2.0-pro"]), offered("glm", &["glm-6"])], &BTreeSet::new());
        assert_eq!(n.len(), 3);
    }

    #[test]
    fn dismissed_stays_dismissed() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let list = [offered("glm", &["glm-5.3-flashx"])];
        let first = news(&list, &seen(&c));
        assert_eq!(first.len(), 1);
        mark_seen(&c, &[first[0].key(), "bogus:x".into(), "glm:".into()]).unwrap();
        mark_seen(&c, &[first[0].key()]).unwrap();
        assert_eq!(seen(&c).into_iter().collect::<Vec<_>>(), ["glm:glm-5.3-flashx"]);
        assert!(news(&list, &seen(&c)).is_empty());
    }

    #[test]
    fn catalog_hourly_claude_daily_and_retries_failures() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let t0 = 1_000_000_000_000;
        assert_eq!(due(&c, t0), ["anthropic", "glm", "muse"], "hiç kontrol yok → hepsi");
        assert!(catalog_due(&c, t0));
        // Yalnız GLM cevap verdi: Claude ve Muse bir sonraki tetikte yeniden denenir.
        mark_checked(&c, &["glm".into()], t0).unwrap();
        assert_eq!(due(&c, t0 + 1000), ["anthropic", "muse"]);
        // Hiçbiri cevap vermedi (ağ yok): zaman değişmez.
        mark_checked(&c, &[], t0 + 2000).unwrap();
        assert_eq!(due(&c, t0 + HOUR_MS - 1), ["anthropic", "muse"], "1 saat dolmadan GLM atlanır");
        assert_eq!(due(&c, t0 + HOUR_MS), ["anthropic", "glm", "muse"], "1 saat dolunca yeniden");
        mark_checked(&c, &["anthropic".into(), "glm".into(), "muse".into()], t0).unwrap();
        assert!(!catalog_due(&c, t0 + HOUR_MS - 1));
        assert_eq!(due(&c, t0 + HOUR_MS), ["glm", "muse"], "Claude yalnız haber: 24 saatte bir");
        assert_eq!(due(&c, t0 + DAY_MS), ["anthropic", "glm", "muse"]);
        // Saat geri alındıysa (kayıt gelecekte) kontrol kilitlenmesin.
        assert_eq!(due(&c, t0 - 1), ["anthropic", "glm", "muse"]);
    }

    #[test]
    fn line_and_version() {
        assert_eq!(line_version("claude-haiku-4-5-20251001"), ("claude-haiku".into(), Some((4, 5))));
        assert_eq!(line_version("GLM-5.3-Flash"), ("glm".into(), Some((5, 3))));
        assert_eq!(line_version("glm-5"), ("glm".into(), Some((5, 0))));
        assert_eq!(line_version("muse-wonder-full-1.0-contributor"), ("muse-wonder-full".into(), Some((1, 0))));
        assert_eq!(line_version("claude-3-5-haiku-20241022"), ("claude".into(), Some((3, 5))));
        assert_eq!(line_version("sonnet"), ("sonnet".into(), None));
    }

    /// 2026-10-14 canlı Muse listesi: Muse Wonder (yeni soy) seçicide, ses/görsel/SAM ve 1.1/1.2-contributor değil.
    #[test]
    fn muse_wonder_enters_catalog() {
        let muse = providers::get("muse").unwrap();
        let ids = [
            "muse-wonder-full-1.0-contributor", "muse-wonder-full-1.0", "muse-wonder-1.0-contributor", "muse-wonder-1.0",
            "sam-3.1", "muse-spark-1.3-contributor", "muse-voice-transcribe-1.0", "muse-spark-1.3", "muse-image-1.0",
            "muse-spark-1.2-contributor", "muse-spark-1.2", "muse-spark-1.1",
        ];
        let offered: Vec<(String, String)> = ids.iter().map(|i| (i.to_string(), i.to_string())).collect();
        let live = live_catalog(muse, &offered);
        let shown: Vec<&str> = live.iter().filter(|m| !m.legacy).map(|m| m.id.as_str()).collect();
        assert_eq!(shown, ["muse-wonder-full-1.0-contributor", "muse-wonder-full-1.0", "muse-wonder-1.0-contributor", "muse-wonder-1.0"]);
        let legacy: Vec<&str> = live.iter().filter(|m| m.legacy).map(|m| m.id.as_str()).collect();
        assert_eq!(legacy, ["muse-spark-1.2-contributor", "muse-spark-1.1"], "eski sürüm geçerli kalır ama seçicide yok");
        assert_eq!(live[3].label, "Muse Wonder 1.0");
        assert_eq!(live[2].label, "Muse Wonder 1.0 — contributor (veriniz ürün geliştirmede kullanılır)");
        // Haber: contributor eşi ana modelin kartında; seçicide olduğu için `listed`.
        let n = news(&[("muse".into(), offered)], &BTreeSet::new());
        assert_eq!(n.iter().map(NewModel::key).collect::<Vec<_>>(), ["muse:muse-wonder-full-1.0", "muse:muse-wonder-1.0"]);
        assert!(n.iter().all(|m| m.listed));
    }

    #[test]
    fn unknown_family_is_chat_known_non_chat_is_not() {
        assert!(is_chat("muse", "muse-nova-2.0"));
        assert!(!is_chat("muse", "MUSE-VOICE-tts-1.0"));
        assert!(!is_chat("muse", "sam-4"));
        assert!(is_chat("glm", "glm-6"));
        assert!(!is_chat("bogus", "x"));
    }

    /// Önbellek: cevap veren sağlayıcı ezer, cevapsızınki kalır; boş cevap önbelleği silmez.
    #[test]
    fn cache_keeps_last_successful_list() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert!(catalogs(&c).is_empty(), "önbellek yok → sabit liste (canlı ek yok)");
        store_offered(&c, &vec![offered("muse", &["muse-wonder-1.0"]), offered("glm", &["glm-5.3-flashx"]), offered("anthropic", &["claude-opus-6-0"])]).unwrap();
        store_offered(&c, &vec![offered("glm", &["glm-6"])]).unwrap();
        store_offered(&c, &vec![]).unwrap();
        let cat = catalogs(&c);
        assert_eq!(cat.keys().collect::<Vec<_>>(), ["glm", "muse"], "Claude kataloğu canlıdan kurulmaz");
        assert_eq!(cat["glm"].iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["glm-6"]);
        assert_eq!(cat["muse"][0].id, "muse-wonder-1.0");
        // Haber önbellekten: Claude yalnız haber (`listed=false`).
        let n = news(&cached(&c), &seen(&c));
        let keys: Vec<(String, bool)> = n.iter().map(|m| (m.key(), m.listed)).collect();
        assert_eq!(keys, [("anthropic:claude-opus-6-0".into(), false), ("glm:glm-6".into(), true), ("muse:muse-wonder-1.0".into(), true)]);
        // İkinci yazım idempotent.
        store_offered(&c, &vec![offered("glm", &["glm-6"])]).unwrap();
        assert_eq!(catalogs(&c), cat);
    }

    /// Canlı: kasadaki jetonlarla gerçek uçlar (`cargo test -p dex-core --lib live_fetch -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn live_fetch() {
        let got = fetch(&SOURCES.iter().map(|s| s.0).collect::<Vec<_>>());
        for (p, models) in &got {
            println!("{p}: {}", models.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(", "));
        }
        println!("news: {:?}", news(&got, &BTreeSet::new()).iter().map(NewModel::key).collect::<Vec<_>>());
    }

    #[test]
    fn parses_both_shapes() {
        let a = serde_json::json!({"data":[{"id":"glm-5.3","display_name":"GLM-5.3"}]});
        let o = serde_json::json!({"object":"list","data":[{"id":"muse-spark-1.3","object":"model"}]});
        assert_eq!(parse_models(&a), [("glm-5.3".to_string(), "GLM-5.3".to_string())]);
        assert_eq!(parse_models(&o), [("muse-spark-1.3".to_string(), "muse-spark-1.3".to_string())]);
    }
}

