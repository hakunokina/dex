//! DEX-17 — motor zinciri: kredi/limit bitince otomatik sağlayıcı geçişi + resume.
//!
//! Akış event-driven: birincil sinyal Claude Code'un `StopFailure` hook'u. Geçici hatalarda
//! (5xx, ağ, timeout) zincir ÇALIŞMAZ; yalnız kalıcı kota/kredi bitiminde çalışır.
//!
//! Karar backend'de verilir (istemci kapalıyken de çalışır). Tükenen sağlayıcı host'ta kalıcı
//! saklanır (`provider_exhausted`); saati dolunca kaydı kalkar ama ajan OTOMATİK geri taşınmaz —
//! yalnız "geri dön" düğmesi belirir. Ajanın ayarlardaki asıl sağlayıcısı değişmez; zincirle
//! geçilen hedef `agents.active_engine` / `active_provider` / `active_model`'da tutulur.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Ayarlar panelinin okuduğu zincir görünümü.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChainView {
    pub order: Vec<String>,
    pub disabled: Vec<String>,
    pub exhausted: Vec<Exhausted>,
}

/// Varsayılan zincir: Claude Code sağlayıcıları.
pub const DEFAULT_CHAIN: &[&str] = &["anthropic", "muse", "glm", "local"];

const CHAIN_ORDER_KEY: &str = "chain_order";
const CHAIN_DISABLED_KEY: &str = "chain_disabled";

/// Sağlayıcı başına varsayılan tükenme süresi (sıfırlama saati ayrıştırılamazsa): 5 saat.
pub const DEFAULT_EXHAUST_MS: i64 = 5 * 3_600_000;

/// Geçici sağlayıcı yanıt vermezse (ağ/timeout) "unreachable" sayıldığı süre: 15 dk, sabit.
pub const UNREACHABLE_EXHAUST_MS: i64 = 15 * 60_000;

/// Hata türü: zincir yalnız kota/kredi bitiminde çalışır. Tek kaynak: [`classify`].
/// xsai deseni (`network-error`, `truncated-stream`, `protocol-error` gibi makine-okunur
/// kodlar): yapılandırılmış sinyal (CLI `error` alanı, HTTP durumu, sağlayıcı kodu) her zaman
/// metin aramasını ezer; metin yalnız son çaredir.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    /// Kullanım kotası doldu → zincir çalışır.
    Quota,
    /// Bakiye/kredi bitti → zincir çalışır (bildirim metni kotadan ayrıdır).
    Credit,
    /// Jeton reddedildi → zincir çalışmaz (yanlış jetonla sessizce başka hesaba geçilmez;
    /// kullanıcı jetonu yeniler).
    Auth,
    /// 5xx, aşırı yük, hız sınırı → zincir çalışmaz, normal retry/StopFailure akışı.
    Transient,
    /// Ağ/timeout/bağlantı → asıl sağlayıcıda zincir çalışmaz; ZİNCİR GEÇİCİ sağlayıcıdaysa
    /// o sağlayıcı kısa süreyle "unreachable" sayılır, zincir ilerler.
    Network,
    /// Yanıt çıktı sınırında kesildi → zincir çalışmaz.
    Truncated,
    /// İstem/protokol sorunu (bilinmeyen model, hatalı istek) → zincir çalışmaz.
    Protocol,
    /// Bilinmeyen → güvenli tarafta kal, zincir çalışmaz.
    Unknown,
}

impl Kind {
    /// Zinciri çalıştıran türler: kota ya da kredi bitimi.
    pub fn chains(self) -> bool {
        matches!(self, Kind::Quota | Kind::Credit)
    }
}

/// CLI'nin StopFailure metnindeki `⎿`/`●` gibi terminal öneklerini temizler. Dex'te hook
/// payload'undaki alanlar kullanılır; bu temizlik yalnız sınıflandırıcıyı önek-güvenli yapar
/// (satır-başı regex'i `⎿` önekli satırı kaçırabilir).
fn clean(message: &str) -> String {
    message
        .lines()
        .map(|l| l.trim_start_matches(['⎿', '●', '>', '│', ' ']).trim_end())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `error` = CLI'nin StopFailure sınıfı; `message` = hata satırı (uç gövdesini taşır).
/// Öncelik: (1) yapılandırılmış `error` alanı, (2) sözcük-sınırlı HTTP durum kodu,
/// (3) sağlayıcıya özel kodlar (z.ai `[1113]`/`[1308]`/`[1310]`; Muse'un belgeli
/// köşeli-parantez kodu yok — durum/metin katmanına düşer), (4) metin araması son çare.
pub fn classify(error: &str, message: &str) -> Kind {
    // (1) Yapılandırılmış alan: kodda/testlerde/kayıtlarda görülen gerçek CLI sınıfları.
    match error {
        "billing_error" => return Kind::Credit,
        "authentication_failed" => return Kind::Auth,
        "overloaded" | "server_error" => return Kind::Transient,
        "timeout" | "network_error" | "api_connection" | "api_timeout" => return Kind::Network,
        "max_output_tokens" => return Kind::Truncated,
        "model_not_found" => return Kind::Protocol,
        _ => {}
    }
    let m = clean(message).to_ascii_lowercase();
    // `rate_limit` tek başına belirsizdir (kota ya da hız sınırı): kodlu katmanlar karar verir.
    let rate_limited = error == "rate_limit";
    // (2) HTTP durum kodu — rakam-sınırlı: "4010", "15003", istek kimliği ya da saat
    // ("2002-01-01 17:42:19") içinde eşleşmez; "(503)", " 503", "503:" eşleşir.
    if has_status(&m, "402") {
        return Kind::Credit;
    }
    if has_status(&m, "401") || has_status(&m, "403") {
        return Kind::Auth;
    }
    if has_status(&m, "404") || has_status(&m, "400") {
        return Kind::Protocol;
    }
    if has_status(&m, "408") || has_status(&m, "500") || has_status(&m, "502") || has_status(&m, "503") || has_status(&m, "529") {
        return Kind::Transient;
    }
    // (3) Sağlayıcıya özel kodlar.
    if m.contains("[1113]") {
        return Kind::Credit;
    }
    if m.contains("[1308]") || m.contains("[1310]") {
        return Kind::Quota;
    }
    // 429 + kota sinyali = kota; yalnız 429 = hız sınırı (geçici).
    if has_status(&m, "429") {
        return if quota_text(&m) { Kind::Quota } else { Kind::Transient };
    }
    if rate_limited {
        if credit_text(&m) {
            return Kind::Credit;
        }
        return if quota_text(&m) { Kind::Quota } else { Kind::Transient };
    }
    // (4) Metin araması — yalnız son çare (sağlayıcı cümleyi değiştirirse sessizce
    // yanlış türe düşmek yerine bu katmanın daralması gerekir).
    if credit_text(&m) {
        return Kind::Credit;
    }
    if quota_text(&m) {
        return Kind::Quota;
    }
    if m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("authentication failed")
        || m.contains("incorrect api key")
        || m.contains("unauthorized")
    {
        return Kind::Auth;
    }
    if m.contains("timed out")
        || m.contains("timeout")
        || m.contains("econnreset")
        || m.contains("econnrefused")
        || m.contains("enotfound")
        || m.contains("network")
        || m.contains("connection refused")
        || m.contains("connection reset")
        || m.contains("failed to connect")
    {
        return Kind::Network;
    }
    if m.contains("overloaded") || m.contains("server error") {
        return Kind::Transient;
    }
    if m.contains("truncated") || m.contains("cut off") || m.contains("max_output_tokens") || m.contains("max_tokens") {
        return Kind::Truncated;
    }
    if m.contains("model not found") || m.contains("unknown model") || m.contains("invalid request") || m.contains("bad request") {
        return Kind::Protocol;
    }
    Kind::Unknown
}

/// Kota cümleleri (kredi/bakiye hariç — o [`credit_text`]'te).
fn quota_text(m: &str) -> bool {
    m.contains("usage limit")
        || m.contains("quota exhaust")
        || m.contains("quota_exhaust")
        || m.contains("quota exceeded")
        || m.contains("limit will reset")
        || m.contains("limit reset")
}

/// Kredi/bakiye cümleleri.
fn credit_text(m: &str) -> bool {
    m.contains("insufficient balance") || m.contains("credit balance") || m.contains("balance is too low")
}

/// Rakam-sınırlı durum kodu: komşu karakter rakam değilse eşleşir ("(503)", " 503,").
/// Böylece "4010", "15003", "2002-01-01 17:42:19" yanlış-pozitif vermez.
fn has_status(hay: &str, code: &str) -> bool {
    let (h, c) = (hay.as_bytes(), code.as_bytes());
    if c.len() > h.len() {
        return false;
    }
    (0..=h.len() - c.len()).any(|i| {
        &h[i..i + c.len()] == c
            && !i.checked_sub(1).is_some_and(|p| h[p].is_ascii_digit())
            && !h.get(i + c.len()).is_some_and(|b| b.is_ascii_digit())
    })
}

/// Ajan ZİNCİR GEÇİCİ sağlayıcıdayken (asıl sağlayıcısı değil) bu sinyal gelirse sağlayıcı
/// kısa süreyle "unreachable" sayılır ve zincir ilerler. Yalnız [`Kind::Network`] —
/// 5xx/aşırı yük/hız sınırı (`rate_limit`) dahil DEĞİL: onlar normal retry akışında kalır.
/// Ölçüm/poll/health check yok, yalnız mevcut StopFailure sinyali.
pub fn unreachable_signal(error: &str, message: &str) -> bool {
    classify(error, message) == Kind::Network
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Sıfırlama saati ayrıştırıcıları ──────────────────────────────────────────

/// GLM (z.ai): ofsetsiz reset tarih-saati sağlayıcının Pekin saati (UTC+8) sözleşmesiyle epoch ms olur.
/// Sentetik örnek: `Your limit will reset at 2002-01-01 20:00:00`.
pub fn parse_glm_reset(message: &str) -> Option<i64> {
    let text = clean(message);
    let i = text.find("reset at ")? + "reset at ".len();
    let ts = text.get(i..i + 19)?;
    let ok = ts.bytes().enumerate().all(|(j, b)| match j {
        4 | 7 => b == b'-',
        10 => b == b' ',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    });
    if !ok {
        return None;
    }
    let (y, mo, d, h, mi, s): (i32, i32, i32, i32, i32, i32) = (
        ts[0..4].parse().ok()?,
        ts[5..7].parse().ok()?,
        ts[8..10].parse().ok()?,
        ts[11..13].parse().ok()?,
        ts[14..16].parse().ok()?,
        ts[17..19].parse().ok()?,
    );
    Some(beijing_to_ms(y, mo, d, h, mi, s))
}

/// Gün-saat → epoch ms (Pekin = UTC+8). Takvim hesabı `std` ile (chrono bağımlılığı yok):
/// civil-from-days algoritması (Howard Hinnant).
fn beijing_to_ms(y: i32, mo: i32, d: i32, h: i32, mi: i32, s: i32) -> i64 {
    civil_to_ms(y, mo, d, h, mi, s) - 8 * 3_600_000
}

/// UTC gün-saat → epoch ms.
fn civil_to_ms(y: i32, mo: i32, d: i32, h: i32, mi: i32, s: i32) -> i64 {
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days as i64) * 86_400_000 + (h as i64) * 3_600_000 + (mi as i64) * 60_000 + (s as i64) * 1000
}

/// RFC 3339 (`2002-01-01T12:00:00.123456+00:00`, `…Z`) → epoch ms; kesir atılır. Biçim dışı → `None`.
pub fn parse_rfc3339(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    let ok = b.len() >= 20
        && b[..19].iter().enumerate().all(|(j, c)| match j {
            4 | 7 => *c == b'-',
            10 => *c == b'T' || *c == b't' || *c == b' ',
            13 | 16 => *c == b':',
            _ => c.is_ascii_digit(),
        });
    if !ok {
        return None;
    }
    let n = |r: std::ops::Range<usize>| ts[r].parse::<i32>().ok();
    let base = civil_to_ms(n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let rest = ts[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    // Ofset bayt düzeyinde: ASCII olmayan son ek karakter sınırında dilim paniği vermesin
    // (çağıran `quota` kilidini tutuyor olabilir).
    let r = rest.as_bytes();
    let offset = match r {
        b"Z" | b"z" => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] if [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit()) => {
            let d = |c: &u8| i64::from(c - b'0');
            let m = (d(h1) * 10 + d(h2)) * 60 + d(m1) * 10 + d(m2);
            if *sign == b'-' { -m } else { m }
        }
        _ => return None,
    };
    Some(base - offset * 60_000)
}

/// Anthropic: `resets at <epoch>` / `resets in <n> <birim>` / `resets 3pm` biçimleri.
/// Epoch saniye ya da ms kabul edilir; `3pm` bugünün (geçtiyse yarının) o saatidir (UTC).
pub fn parse_anthropic_reset(message: &str) -> Option<i64> {
    let text = clean(message).to_ascii_lowercase();
    // "resets in 42 minutes" / "reset in 2 hours".
    if let Some(i) = text.find("reset in ").or_else(|| text.find("resets in ")) {
        let rest = &text[i..];
        let num: String = rest.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
        let n: i64 = num.parse().ok()?;
        let mult = if rest.contains("hour") {
            3_600_000
        } else if rest.contains("min") {
            60_000
        } else {
            1000
        };
        return Some(now_ms() + n * mult);
    }
    // "resets at 3pm" / "reset at 15:30" / "resets at <epoch>".
    if let Some(i) = text.find("reset at ").or_else(|| text.find("resets at ")) {
        // "reset at"ten sonraki ilk zaman jetonu ("3pm", "1790…"); mesajdaki erken sayılar atlanır.
        let after = &text[i..];
        // Muse: "resets at 2026-10-19T00:00:00Z." (RFC 3339; metin küçük harfe çevrildi, t/z kabul).
        let iso: String = after.split_once(" at ").map(|(_, r)| r).unwrap_or("").chars().take_while(|c| !c.is_whitespace()).collect();
        if let Some(t) = parse_rfc3339(iso.trim_end_matches('.')) {
            return Some(t);
        }
        let tok: String = after
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_alphanumeric() || *c == ':')
            .collect();
        // epoch (sn ya da ms)?
        if tok.chars().all(|c| c.is_ascii_digit()) && tok.len() >= 10 {
            if let Ok(epoch) = tok.parse::<i64>() {
                return Some(if tok.len() > 10 { epoch } else { epoch * 1000 });
            }
        }
        // "3pm" / "3:30pm" / "15:30".
        let mut h: i64;
        let mut mi: i64 = 0;
        let pm = tok.contains("pm");
        let am = tok.contains("am");
        let num = tok.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let parts: Vec<&str> = num.split(':').collect();
        h = parts.first()?.trim().parse().ok()?;
        if parts.len() > 1 {
            mi = parts[1].parse().ok()?;
        }
        if pm && h < 12 {
            h += 12;
        }
        if am && h == 12 {
            h = 0;
        }
        if !(0..24).contains(&h) || !(0..60).contains(&mi) {
            return None;
        }
        let day = now_ms() / 86_400_000;
        let mut t = day * 86_400_000 + h * 3_600_000 + mi * 60_000;
        if t <= now_ms() {
            t += 86_400_000; // geçtiyse yarın
        }
        return Some(t);
    }
    None
}

/// Muse (Meta): epoch ya da "in <n> <birim>" biçimi; GLM biçimi de kabul edilir.
pub fn parse_muse_reset(message: &str) -> Option<i64> {
    parse_anthropic_reset(message).or_else(|| parse_glm_reset(message))
}

/// Sağlayıcıya göre sıfırlama saati; bulunamazsa `None` (çağıran varsayılanı basar).
pub fn parse_reset(provider: &str, message: &str) -> Option<i64> {
    match provider {
        "glm" => parse_glm_reset(message),
        "muse" => parse_muse_reset(message),
        _ => parse_anthropic_reset(message),
    }
}

// ── Kalıcı durum: tükenen sağlayıcılar ───────────────────────────────────────

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Exhausted {
    pub provider: String,
    pub reset_at: i64,
    pub reason: String,
}

/// Tükenen sağlayıcıyı kaydet (saat bulunamazsa varsayılan 5 sa).
pub fn mark_exhausted(conn: &Connection, provider: &str, reset_at: Option<i64>, reason: &str) -> Result<(), String> {
    let reset_at = reset_at.unwrap_or_else(|| now_ms() + DEFAULT_EXHAUST_MS);
    conn.execute(
        "INSERT INTO provider_exhausted (provider, reset_at, reason) VALUES (?1, ?2, ?3)
         ON CONFLICT(provider) DO UPDATE SET reset_at = excluded.reset_at, reason = excluded.reason",
        params![provider, reset_at, reason],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Süresi dolmuş kayıtları temizler; hâlâ tükenmişleri döndürür.
pub fn exhausted(conn: &Connection) -> Result<Vec<Exhausted>, String> {
    let now = now_ms();
    conn.execute("DELETE FROM provider_exhausted WHERE reset_at <= ?1", [now]).map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare("SELECT provider, reset_at, reason FROM provider_exhausted ORDER BY reset_at")
        .map_err(|e| e.to_string())?;
    let rows: Vec<Exhausted> = st
        .query_map([], |r| {
            Ok(Exhausted { provider: r.get(0)?, reset_at: r.get(1)?, reason: r.get(2)? })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn clear_exhausted(conn: &Connection, provider: &str) -> Result<(), String> {
    conn.execute("DELETE FROM provider_exhausted WHERE provider = ?1", [provider]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Zincirdeki (kapalı olmayan) tükenmiş motorların en erken sıfırlanması (`None` = tükenmiş yok).
pub fn earliest_reset(conn: &Connection) -> Result<Option<i64>, String> {
    let (targets, off) = (order(conn), disabled(conn));
    Ok(exhausted(conn)?
        .into_iter()
        .filter(|e| targets.contains(&e.provider) && !off.contains(&e.provider))
        .map(|e| e.reset_at)
        .min())
}

// ── Zincir sırası (Ayarlar) ──────────────────────────────────────────────────

/// Kayıtlı sıra (yoksa varsayılan). Bilinmeyen id'ler düşer, eksik bilinenler sona eklenir.
pub fn order(conn: &Connection) -> Vec<String> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [CHAIN_ORDER_KEY], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    let mut stored: Vec<String> = raw.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    stored.retain(|p| valid_target(p));
    for p in DEFAULT_CHAIN {
        if !stored.contains(&p.to_string()) {
            stored.push(p.to_string());
        }
    }
    stored
}

pub fn set_order(conn: &Connection, order: &[String]) -> Result<Vec<String>, String> {
    if order.is_empty() {
        return Err("zincir boş olamaz".into());
    }
    for p in order {
        if !valid_target(p) {
            return Err(format!("bilinmeyen zincir hedefi: {p}"));
        }
    }
    let mut dedup: Vec<String> = Vec::new();
    for p in order {
        if !dedup.contains(p) {
            dedup.push(p.clone());
        }
    }
    let v = serde_json::to_string(&dedup).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![CHAIN_ORDER_KEY, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(self::order(conn))
}

/// Zincirden KAPALI sağlayıcılar (sırada durur, atlanır). Kapalı olmak ajanın asıl
/// sağlayıcısı olarak seçilmesini engellemez. Boş liste = hepsi açık.
pub fn disabled(conn: &Connection) -> Vec<String> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [CHAIN_DISABLED_KEY], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    let mut list: Vec<String> = raw.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    list.retain(|p| valid_target(p));
    list.sort();
    list.dedup();
    list
}

pub fn set_disabled(conn: &Connection, ids: &[String]) -> Result<Vec<String>, String> {
    let mut dedup: Vec<String> = Vec::new();
    for p in ids {
        if !valid_target(p) {
            return Err(format!("bilinmeyen zincir hedefi: {p}"));
        }
        if !dedup.contains(p) {
            dedup.push(p.clone());
        }
    }
    let v = serde_json::to_string(&dedup).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![CHAIN_DISABLED_KEY, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(self::disabled(conn))
}

// ── Seçim ────────────────────────────────────────────────────────────────────

fn valid_target(id: &str) -> bool {
    crate::providers::get(id).is_some()
}

/// Sıradaki hedefi seç: jetonu/oturumu olmayan, kapalı ya da tükenmiş sağlayıcı atlanır.
/// `available`: sağlayıcıda kullanılabilir jeton var mı.
/// `current`: az önce tükenen (dışlanır). `None` = zincir bitti, ajan bekler.
pub fn pick_next(
    conn: &Connection,
    current: &str,
    available: &dyn Fn(&str) -> bool,
) -> Result<Option<String>, String> {
    let gone: Vec<String> = exhausted(conn)?.into_iter().map(|e| e.provider).collect();
    let off = disabled(conn);
    let targets = order(conn);
    let start = targets.iter().position(|p| p == current).map_or(0, |i| i + 1);
    for p in targets.into_iter().skip(start) {
        if p == current || gone.contains(&p) || off.contains(&p) {
            continue;
        }
        if valid_target(&p) && (p == "anthropic" || available(&p)) {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// #1027: zincirde geçilecek motor yokken nedeni — kota mı bitti, jeton/erişim mi yok. Bakılan
/// küme [`pick_next`]'inki (`current`'tan sonraki açık motorlar) artı ajanın asıl sağlayıcısı
/// `home` (kapalı olsa da dönülebilir). `skipped` = seçimde erişimi olmadığı için atlanan vardı;
/// `available` = jeton/oturum.
pub fn none_reason(conn: &Connection, current: &str, home: &str, skipped: bool, available: &dyn Fn(&str) -> bool) -> &'static str {
    let gone: Vec<String> = exhausted(conn).unwrap_or_default().into_iter().map(|e| e.provider).collect();
    let off = disabled(conn);
    let targets = order(conn);
    let start = targets.iter().position(|p| p == current).map_or(0, |i| i + 1);
    let mut rest: Vec<String> = targets.into_iter().skip(start).filter(|p| p != current && !off.contains(p)).collect();
    if home != current && !rest.iter().any(|p| p == home) {
        rest.push(home.to_string());
    }
    let quota = rest.iter().any(|p| gone.contains(p));
    let access = skipped || rest.iter().any(|p| !gone.contains(p) && p != "anthropic" && !available(p));
    match (quota, access) {
        (true, false) => "zincirde kotası olan motor yok",
        (false, true) => "zincirde jetonu/erişimi olan motor yok",
        (true, true) => "zincirde kotası olan motor yok, kalanların jetonu/erişimi yok",
        (false, false) => "zincirde kullanılabilir hedef yok",
    }
}

/// Model eşleme: ajanın katmanı yeni sağlayıcının eşdeğerine.
/// opus → en güçlü, sonnet → orta, haiku → hafif. `local`'da katman yok — kayıt adı aynen kalır.
pub fn map_model(provider: &str, model: &str) -> String {
    match provider {
        "glm" => if is_light(model) { "GLM-5.3-Flash".into() } else { "GLM-5.3".into() },
        "muse" => {
            if model.starts_with("muse-") {
                model.into()
            } else {
                "muse-spark-1.3".into()
            }
        }
        "anthropic" => match model {
            m if m.starts_with("claude-") || matches!(m, "opus" | "sonnet" | "haiku") => model.into(),
            // Başka sağlayıcıdan (muse/glm/local) geçiş: CLI kendi varsayılan modelini seçer.
            _ => crate::providers::CLAUDE_DEFAULT.into(),
        },
        _ => model.into(), // local: sunucu kayıt adı
    }
}

/// Hafif katman: Haiku (takma ad ya da sürümlü) ya da "flash" modeller.
fn is_light(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m == "haiku" || m.starts_with("claude-haiku") || m.contains("flash")
}

/// Zincir geçişinde PTY'ye verilen kısa devam turu. `[Dex-Sistem]` ile başlar: tur Dex turu
/// sayılır (delegate::on_prompt), böylece sonraki iş patron oturumuna değil yeni oturuma gider.
pub fn continue_prompt(from_label: &str, to_label: &str) -> String {
    format!(
        "[{}] Kaldığın yerden devam et: az önce {from_label} sağlayıcısının kotası dolduğu için {to_label} üzerinden devam ediyorsun. \
Yarım kalan işin neyse onu sürdür; bitmiş işi baştan yapma, kullanıcıya quoting yapmadan tek cümleyle geçişi belirt.",
        crate::dexmsg::SISTEM
    )
}

/// Görev #250: proaktif eşik geçişinin devam turu (`why` = "Claude %85 (5 sa)").
pub fn continue_prompt_threshold(why: &str, to_label: &str) -> String {
    format!(
        "[{}] Kaldığın yerden devam et: {why} kullanım eşiğini geçtiği için {to_label} üzerinden devam ediyorsun. Yarım kalan işin neyse onu sürdür; bitmiş işi baştan yapma, kullanıcıya quoting yapmadan tek cümleyle geçişi belirt.",
        crate::dexmsg::SISTEM
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Görev #95: geçiş turu "[Dex]" ile başlamazsa patron turu sayılır ve sonraki iş dolu oturuma yapışır.
    #[test]
    fn continue_prompt_is_a_dex_turn() {
        assert!(crate::dexmsg::is_dex_turn(&continue_prompt("GLM", "Muse")));
        assert!(continue_prompt("GLM", "Muse").starts_with("[Dex-Sistem] Kaldığın yerden devam et:"));
    }

    #[test]
    fn classify_permanent_transient_auth() {
        // (sağlayıcı, error, message) → beklenen tür. Sentetik tarih/saat ve istek
        // kimlikleri korunur; ham gövde bildirimlere sızmaz (providers.rs testi).
        let table: &[(&str, &str, &str, Kind)] = &[
            // GLM / z.ai: 429 + köşeli-parantez iş kodu.
            ("glm", "rate_limit", "API Error: Request rejected (429) · [1308][Usage limit reached for 5 hour. Your limit will reset at 2002-01-01 17:42:19][example-request]", Kind::Quota),
            ("glm", "rate_limit", "⎿  API Error: Request rejected (429) · [1308][Usage limit reached]", Kind::Quota),
            ("glm", "rate_limit", "● [1310][Usage limit reached]", Kind::Quota),
            ("glm", "rate_limit", "API Error 429 [1113][Insufficient balance]", Kind::Credit),
            ("glm", "billing_error", "Credit balance is too low", Kind::Credit),
            // Anthropic: 429 + kota cümlesi; yalnız 429 = hız sınırı.
            ("anthropic", "rate_limit", "429 quota exhausted, limit reset soon", Kind::Quota),
            ("anthropic", "rate_limit", "429 Too Many Requests — slow down", Kind::Transient),
            ("anthropic", "overloaded", "Overloaded", Kind::Transient),
            ("anthropic", "", "API Error 529 Overloaded", Kind::Transient),
            // Muse (Meta): 503 aşırı yük — CLI ~3 dk retry sonrası StopFailure.
            ("muse", "server_error", "API Error: 503 The service is temporarily overloaded. Please retry shortly.", Kind::Transient),
            ("muse", "authentication_failed", "401 Unauthorized", Kind::Auth),
            ("muse", "", "Invalid API key", Kind::Auth),
            // Ağ/timeout sınıfı (geçici sağlayıcıda unreachable).
            ("local", "timeout", "request timed out", Kind::Network),
            ("local", "network_error", "API Error", Kind::Network),
            ("local", "", "connect ECONNRESET 1.2.3.4", Kind::Network),
            // Yarım akış / protokol.
            ("anthropic", "max_output_tokens", "response stopped at max tokens", Kind::Truncated),
            ("anthropic", "model_not_found", "model not found", Kind::Protocol),
            ("anthropic", "", "404 Not Found: model xyz", Kind::Protocol),
            ("anthropic", "", "some weird thing", Kind::Unknown),
        ];
        for (prov, error, msg, want) in table {
            assert_eq!(classify(error, msg), *want, "{prov} | {error} | {msg}");
            assert_eq!(want.chains(), matches!(want, Kind::Quota | Kind::Credit), "{prov} | {error}");
        }
    }

    #[test]
    fn classify_structured_field_beats_message() {
        // Yapılandırılmış alan her zaman metni ezer.
        assert_eq!(classify("server_error", "[1308][Usage limit reached]"), Kind::Transient);
        assert_eq!(classify("authentication_failed", "Overloaded, try again later"), Kind::Auth);
        assert_eq!(classify("billing_error", "timeout while charging"), Kind::Credit);
        assert_eq!(classify("timeout", "429 quota exhausted"), Kind::Network);
    }

    #[test]
    fn classify_status_codes_need_digit_boundaries() {
        // Eski " 401"/" 500" içerir-arama bu girdileri yanlış sınıflandırırdı.
        assert_eq!(classify("", "Error 4010: upstream rejected the batch"), Kind::Unknown);
        assert_eq!(classify("", "job 14019 failed at 2002-01-01 10:00:00"), Kind::Unknown);
        assert_eq!(classify("", "bulk 5001 items queued for retry"), Kind::Unknown);
        // Saat/reset satırı durum kodu sanılmaz; kotasız hız sınırı geçici kalır.
        assert_eq!(classify("", "retry at 2002-01-01 17:42:19"), Kind::Unknown);
        assert_eq!(classify("rate_limit", "reset at 2002-01-01 08:00:00"), Kind::Transient);
        // Gerçek kodlar sınır karakterleriyle eşleşir.
        assert_eq!(classify("", "API Error: 503 Service Unavailable"), Kind::Transient);
        assert_eq!(classify("", "request failed (401)"), Kind::Auth);
        assert_eq!(classify("", "HTTP 403: Forbidden"), Kind::Auth);
        assert_eq!(classify("", "402 Payment Required: top up balance"), Kind::Credit);
    }

    #[test]
    fn glm_reset_parses_synthetic_line() {
        // Sentetik saat: 17:42:19 Pekin = 09:42:19 UTC.
        let ms = parse_glm_reset("…Your limit will reset at 2002-01-01 17:42:19…").unwrap();
        assert_eq!(ms, 1_009_878_139_000, "{ms}");
        assert_eq!(parse_glm_reset("⎿  …reset at 2002-01-01 17:42:19"), Some(1_009_878_139_000));
        assert_eq!(parse_glm_reset("reset at 2002-1-1 8:00"), None);
        assert_eq!(parse_glm_reset("no date here"), None);
        // providers.rs'taki z.ai ayrıştırıcıyla aynı an.
        assert_eq!(ms % 86_400_000, (9 * 3_600 + 42 * 60 + 19) * 1000);
    }

    #[test]
    fn anthropic_reset_variants() {
        let now = now_ms();
        let r = parse_anthropic_reset("rate limited, resets in 42 minutes").unwrap();
        assert!((r - (now + 42 * 60_000)).abs() < 5_000, "{r}");
        let r = parse_anthropic_reset("resets in 2 hours").unwrap();
        assert!((r - (now + 2 * 3_600_000)).abs() < 5_000, "{r}");
        let r = parse_anthropic_reset("resets at 1009886400").unwrap();
        assert_eq!(r, 1_009_886_400_000);
        let r = parse_anthropic_reset("resets at 3pm, slow down").unwrap();
        let day = now / 86_400_000 * 86_400_000;
        assert!(r == day + 15 * 3_600_000 || r == day + 15 * 3_600_000 + 86_400_000, "{r}");
        assert_eq!(parse_anthropic_reset("just slow down"), None);
        // Muse epoch'u da okur.
        assert_eq!(parse_muse_reset("quota resets at 1009886400").unwrap(), 1_009_886_400_000);
        // Muse 429 metni: RFC 3339 saat (5 sa varsayılanına düşmemeli).
        let msg = "API Error: Request rejected (429) · Subscription quota exhausted. Your usage window resets at 2002-01-01T12:00:00Z.";
        assert_eq!(parse_muse_reset(msg).unwrap(), 1_009_886_400_000);
    }

    #[test]
    fn model_mapping_by_tier() {
        assert_eq!(map_model("glm", "opus"), "GLM-5.3");
        assert_eq!(map_model("glm", "sonnet"), "GLM-5.3");
        assert_eq!(map_model("glm", "haiku"), "GLM-5.3-Flash");
        assert_eq!(map_model("glm", "GLM-5.3-Flash"), "GLM-5.3-Flash");
        assert_eq!(map_model("muse", "opus"), "muse-spark-1.3");
        assert_eq!(map_model("muse", "muse-spark-1.2"), "muse-spark-1.2");
        assert_eq!(map_model("anthropic", "GLM-5.3"), "default");
        assert_eq!(map_model("anthropic", "muse-spark-1.3"), "default");
        assert_eq!(map_model("anthropic", "opus"), "opus", "eski takma ad korunur");
        assert_eq!(map_model("anthropic", "claude-fable-5-1"), "claude-fable-5-1");
        assert_eq!(map_model("glm", "claude-haiku-4-5-20251001"), "GLM-5.3-Flash");
        assert_eq!(map_model("glm", "claude-opus-5-5"), "GLM-5.3");
        assert_eq!(map_model("local", "qwen3-coder-next"), "qwen3-coder-next");
    }

    #[test]
    fn order_defaults_validates_and_dedups() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(order(&c), vec!["anthropic", "muse", "glm", "local"]);
        assert_eq!(set_order(&c, &["glm".into(), "glm".into(), "anthropic".into(), "local".into()]).unwrap(),
            vec!["glm", "anthropic", "local", "muse"]);
        assert!(set_order(&c, &[]).is_err());
        assert!(set_order(&c, &["bogus".into()]).is_err());
        // Kaldırılan hedefler (Codex motoru, gateway) artık zincire yazılamaz.
        assert!(set_order(&c, &["codex".into()]).is_err());
        assert!(set_order(&c, &["gateway".into()]).is_err());
        assert_eq!(order(&c), vec!["glm", "anthropic", "local", "muse"], "reddedilen yazım kaydı değiştirmez");
    }

    #[test]
    fn disabled_providers_are_skipped_but_selectable() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert!(disabled(&c).is_empty());
        assert_eq!(set_disabled(&c, &["local".into(), "local".into()]).unwrap(), vec!["local"]);
        assert!(set_disabled(&c, &["bogus".into()]).is_err());
        // Kapalı sağlayıcı sırada durur ama atlanır.
        assert!(order(&c).contains(&"local".to_string()));
        let next = pick_next(&c, "glm", &|_| true).unwrap();
        assert_eq!(next, None, "kapalı local atlanır; zincir başa sarmaz");
        // Kapatma kalkınca zincire döner.
        assert!(set_disabled(&c, &[]).unwrap().is_empty());
        let next = pick_next(&c, "glm", &|_| true).unwrap();
        assert_eq!(next.as_deref(), Some("local"));
    }

    #[test]
    fn unreachable_signal_only_network_class() {
        assert!(unreachable_signal("timeout", "Request timed out · attempt 4/10"));
        assert!(unreachable_signal("network_error", "API Error"));
        assert!(unreachable_signal("api_connection", "x"));
        assert!(unreachable_signal("api_timeout", "x"));
        assert!(unreachable_signal("", "connect ECONNRESET 1.2.3.4"));
        assert!(unreachable_signal("", "network unreachable"));
        assert!(unreachable_signal("", "Connection refused"));
        // 5xx/aşırı yük/hız sınırı unreachable DEĞİL (normal retry akışı).
        assert!(!unreachable_signal("overloaded", "Overloaded"));
        assert!(!unreachable_signal("server_error", "API Error: 503 Service Unavailable"));
        assert!(!unreachable_signal("rate_limit", "429 Too Many Requests — slow down"));
        assert!(!unreachable_signal("authentication_failed", "401 Unauthorized"));
        assert!(!unreachable_signal("", "model not found"));
    }

    #[test]
    fn rfc3339_parses_usage_resets() {
        assert_eq!(parse_rfc3339("2002-01-01T09:42:19Z"), Some(1_009_878_139_000));
        assert_eq!(parse_rfc3339("2002-01-01T09:42:19.123456+00:00"), Some(1_009_878_139_000));
        assert_eq!(parse_rfc3339("2002-01-01T12:42:19+03:00"), Some(1_009_878_139_000));
        assert_eq!(parse_rfc3339("2002-01-01"), None);
        assert_eq!(parse_rfc3339("2002-01-01T12:00:00+0300"), None);
        // ASCII olmayan 6 baytlık son ek: karakter sınırı dışı dilim panik vermez.
        assert_eq!(parse_rfc3339("2002-01-01T12:00:00+0é00"), None);
        assert_eq!(parse_rfc3339("2002-01-01T12:00:00é:00"), None);
        assert_eq!(parse_rfc3339("2002-01-01T12:00:00+0a:00"), None);
    }

    #[test]
    fn earliest_reset_skips_disabled_and_unknown() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(earliest_reset(&c).unwrap(), None);
        let now = now_ms();
        mark_exhausted(&c, "muse", Some(now + 7_200_000), "kota").unwrap();
        mark_exhausted(&c, "glm", Some(now + 3_600_000), "kota").unwrap();
        assert_eq!(earliest_reset(&c).unwrap(), Some(now + 3_600_000));
        set_disabled(&c, &["glm".into()]).unwrap();
        assert_eq!(earliest_reset(&c).unwrap(), Some(now + 7_200_000), "kapalı motor sayılmaz");
    }

    #[test]
    fn none_reason_tells_quota_from_access() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        set_disabled(&c, &["local".into()]).unwrap();
        let later = now_ms() + 3_600_000;
        // Claude'dan sonra açık: Muse, GLM.
        let no_glm = |p: &str| p != "glm";
        // Muse seçimde atlandı, GLM jetonsuz: kota değil erişim.
        assert_eq!(none_reason(&c, "anthropic", "anthropic", true, &no_glm), "zincirde jetonu/erişimi olan motor yok");
        mark_exhausted(&c, "muse", Some(later), "kota").unwrap();
        assert_eq!(none_reason(&c, "anthropic", "anthropic", false, &no_glm), "zincirde kotası olan motor yok, kalanların jetonu/erişimi yok");
        set_disabled(&c, &["local".into(), "glm".into()]).unwrap();
        assert_eq!(none_reason(&c, "anthropic", "anthropic", false, &no_glm), "zincirde kotası olan motor yok");
        // Zincirin sonundan çıkış: sırada kimse yok, asıl sağlayıcı (Claude) sayılır.
        assert_eq!(none_reason(&c, "local", "anthropic", false, &no_glm), "zincirde kullanılabilir hedef yok");
        mark_exhausted(&c, "anthropic", Some(later), "kota").unwrap();
        assert_eq!(none_reason(&c, "local", "anthropic", false, &no_glm), "zincirde kotası olan motor yok");
    }

    #[test]
    fn pick_skips_exhausted_and_tokenless() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        mark_exhausted(&c, "muse", Some(now_ms() + 3_600_000), "kota").unwrap();
        // Tükenmiş Muse ve jetonsuz GLM atlanır, Local'e geçilir.
        let next = pick_next(&c, "anthropic", &|p| p == "local").unwrap();
        assert_eq!(next.as_deref(), Some("local"));
        // Zincir geri sarmaz: GLM'den sonra jetonsuz local atlanır, baştaki Claude'a dönülmez.
        assert_eq!(pick_next(&c, "glm", &|_| false).unwrap(), None);
        // Süresi dolan kayıt kalkar.
        mark_exhausted(&c, "anthropic", Some(now_ms() - 1), "eski").unwrap();
        let next = pick_next(&c, "eski-hedef", &|_| false).unwrap();
        assert_eq!(next.as_deref(), Some("anthropic"), "süresi dolan + jetonsuz değil (anthropic)");
        // Hepsi tükenirse bekleme.
        mark_exhausted(&c, "anthropic", Some(now_ms() + 3_600_000), "k").unwrap();
        mark_exhausted(&c, "glm", Some(now_ms() + 3_600_000), "k").unwrap();
        mark_exhausted(&c, "local", Some(now_ms() + 3_600_000), "k").unwrap();
        assert_eq!(pick_next(&c, "muse", &|_| true).unwrap(), None);
        assert_eq!(pick_next(&c, "muse", &|_| false).unwrap(), None);
        // Varsayılan süre 5 sa.
        mark_exhausted(&c, "x", None, "k").unwrap();
        let e: i64 = c.query_row("SELECT reset_at FROM provider_exhausted WHERE provider = 'x'", [], |r| r.get(0)).unwrap();
        assert!((e - (now_ms() + DEFAULT_EXHAUST_MS)).abs() < 5_000);
    }
}
