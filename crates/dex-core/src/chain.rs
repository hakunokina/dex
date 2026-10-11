//! DEX-17 — motor zinciri: kredi/limit bitince otomatik sağlayıcı geçişi + resume.
//!
//! Akış event-driven: birincil sinyal Claude Code'un `StopFailure` hook'u. Geçici hatalarda
//! (5xx, ağ, timeout) zincir ÇALIŞMAZ; yalnız kalıcı kota/kredi bitiminde çalışır. Codex motorunda
//! StopFailure yok → Codex ajanları kapsam dışı (UI'da belirtilir).
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

/// Varsayılan zincir: Claude sağlayıcıları, son çare olarak yeni bir Codex oturumu.
pub const DEFAULT_CHAIN: &[&str] = &["anthropic", "muse", "glm", "local", "codex"];
// Gateway is opt-in through set_order; existing chains retain their order.

const CHAIN_ORDER_KEY: &str = "chain_order";
const CHAIN_DISABLED_KEY: &str = "chain_disabled";

/// Sağlayıcı başına varsayılan tükenme süresi (sıfırlama saati ayrıştırılamazsa): 5 saat.
pub const DEFAULT_EXHAUST_MS: i64 = 5 * 3_600_000;

/// Geçici sağlayıcı yanıt vermezse (ağ/timeout) "unreachable" sayıldığı süre: 15 dk, sabit.
pub const UNREACHABLE_EXHAUST_MS: i64 = 15 * 60_000;

/// Hata sınıfı: zincir yalnız kalıcıda çalışır.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    /// Kota/kredi bitti → zincir çalışır.
    Permanent,
    /// 5xx, ağ, timeout → zincir çalışmaz, normal retry/StopFailure akışı.
    Transient,
    /// Jeton reddedildi → zincir çalışmaz (başka sağlayıcının jetonu sorunu çözmez değil ama
    /// yanlış jetonla sessizce başka hesaba geçmek istenmez; kullanıcı jetonu yeniler).
    Auth,
    /// Bilinmeyen → güvenli tarafta kal, zincir çalışmaz.
    Unknown,
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
pub fn classify(error: &str, message: &str) -> Kind {
    let m = clean(message).to_ascii_lowercase();
    // Kalıcı: faturalandırma/kota sinyalleri.
    if error == "billing_error"
        || m.contains("[1113]")
        || m.contains("insufficient balance")
        || m.contains("credit balance")
        || m.contains("balance is too low")
        || m.contains("usage limit")
        || m.contains("quota exhaust")
        || m.contains("quota_exhaust")
        || m.contains("quota exceeded")
        || m.contains("[1308]")
        || m.contains("[1310]")
        || m.contains("limit will reset")
        || m.contains("limit reset")
    {
        return Kind::Permanent;
    }
    // Auth: jeton sorunu.
    if error == "authentication_failed"
        || m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("authentication failed")
        || m.contains("incorrect api key")
        || m.contains("unauthorized")
        || m.contains(" 401")
        || m.contains("(401)")
    {
        return Kind::Auth;
    }
    // Geçici: sunucu/ağ/timeout.
    if ["overloaded", "server_error", "timeout", "network_error", "api_connection", "api_timeout"]
        .contains(&error)
        || m.contains("overloaded")
        || m.contains("server error")
        || m.contains(" 500")
        || m.contains(" 502")
        || m.contains(" 503")
        || m.contains("(500)")
        || m.contains("(502)")
        || m.contains("(503)")
        || m.contains("timeout")
        || m.contains("timed out")
        || m.contains("econnreset")
        || m.contains("enotfound")
        || m.contains("network")
    {
        return Kind::Transient;
    }
    // `rate_limit` kota sinyali taşımıyorsa hız sınırıdır (geçici); kalan her şey bilinmeyen.
    if error == "rate_limit" {
        return Kind::Transient;
    }
    Kind::Unknown
}

/// Ajan ZİNCİR GEÇİCİ sağlayıcıdayken (asıl sağlayıcısı değil) bu sinyal gelirse sağlayıcı
/// kısa süreyle "unreachable" sayılır ve zincir ilerler. Yalnız ağ/timeout/connection
/// sınıfı — 5xx/aşırı yük/zaman sınırı (rate_limit) dahil DEĞİL: onlar normal retry akışında
/// kalır. Ölçüm/poll/health check yok, yalnız mevcut StopFailure sinyali.
pub fn unreachable_signal(error: &str, message: &str) -> bool {
    if ["timeout", "network_error", "api_connection", "api_timeout"].contains(&error) {
        return true;
    }
    let m = clean(message).to_ascii_lowercase();
    ["timed out", "timeout", "econnreset", "enotfound", "econnrefused", "network", "connection refused", "connection reset", "failed to connect"]
        .iter()
        .any(|s| m.contains(s))
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

// ── Ölçülen kota: Codex'in kendi kaydı ───────────────────────────────────────

/// En çok bu kadar yeni rollout dosyasına bakılır (en yeni gün klasörlerinden).
const CODEX_ROLLOUT_FILES: usize = 4;
/// Her dosyanın yalnız sonu okunur.
const CODEX_ROLLOUT_TAIL: u64 = 256 * 1024;

/// Codex kotası BİLİNİYOR ve bitmişse sıfırlanma anı (epoch ms). Kaynak: Codex CLI'nin her turda
/// rollout JSONL'ına (`<sessions>/<Y>/<M>/<G>/rollout-*.jsonl`) yazdığı `token_count` olayının
/// `rate_limits`'i — resmî sözleşme değil, biçim değişirse bilinmiyor'a düşer. Yalnız en yeni
/// [`CODEX_ROLLOUT_FILES`] dosyanın sonundan geriye ilk `limit_id = "codex"` satırı okunur; o
/// satırda birincil ya da ikincil pencere %100 ve sıfırlanması gelecekteyse tükenmiş sayılır.
/// Satır yok / pencere `null` / sıfırlanma geçmiş / ayrıştırma hatası → `None` (bilinmiyor).
pub fn codex_exhausted_until(sessions: &std::path::Path, now: i64) -> Option<i64> {
    for file in newest_rollouts(sessions) {
        if let Some(limits) = last_codex_limits(&file) {
            return exhausted_window(&limits, now);
        }
    }
    None
}

/// Tek rollout dosyasında (çalışan ajanın kendi oturumu) Codex kotası bitmişse sıfırlanma anı;
/// okuma kuralları [`codex_exhausted_until`] ile aynı.
pub fn codex_file_exhausted_until(file: &std::path::Path, now: i64) -> Option<i64> {
    exhausted_window(&last_codex_limits(file)?, now)
}

/// #1027: Codex'te StopFailure hook'u yok; kota bitince tur rollout'a
/// `event_msg/task_complete` + `error.codex_error_info = "usage_limit_exceeded"` yazılarak biter
/// (codex-cli 0.154 olay sözleşmesi; hemen önceki `token_count` satırı `rate_limits`'i %100 taşır).
/// `Some(true)` = kota hatasıyla biten tur, `Some(false)` = başka biten tur, `None` = tur sonu değil.
pub fn codex_turn_limit(v: &serde_json::Value) -> Option<bool> {
    if v["type"] != "event_msg" || v["payload"]["type"] != "task_complete" {
        return None;
    }
    // CLI dize gönderir; ileride nesneye dönerse (`{"usage_limit_exceeded": {…}}`) anahtar da sayılır.
    let info = &v["payload"]["error"]["codex_error_info"];
    Some(info == "usage_limit_exceeded" || info.get("usage_limit_exceeded").is_some())
}

/// #1033: izleyici kurulurken dosyada zaten olan, [`CODEX_RECENT_MS`]'den yeni ve son tur sonu olan
/// kota hatası (taze oturumun izleyiciden önce biten ilk turu). Eski oturumun (resume) bayat hatası
/// ya da arkasından yeni tur başlamış hata sayılmaz.
pub fn codex_recent_limit(rollout: &[u8], now: i64) -> bool {
    let mut last = None;
    for line in rollout.split(|b| *b == b'\n') {
        // Hızlı ön süzgeç: yalnız tur başı/sonu satırları ayrıştırılır.
        if !line.windows(5).any(|w| w == b"task_") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { continue };
        if v["type"] == "event_msg" && v["payload"]["type"] == "task_started" {
            last = None;
        } else if let Some(limit) = codex_turn_limit(&v) {
            last = Some((limit, v["timestamp"].as_str().and_then(parse_rfc3339)));
        }
    }
    matches!(last, Some((true, Some(at))) if now - at <= CODEX_RECENT_MS)
}

/// [`codex_recent_limit`] tazelik sınırı: izleyici kurulmadan hemen önce biten turu yakalar.
pub const CODEX_RECENT_MS: i64 = 60_000;

/// Son günlerin rollout'ları, en son yazılan önce.
fn newest_rollouts(sessions: &std::path::Path) -> Vec<std::path::PathBuf> {
    // Klasör adları sayısal (YYYY/MM/DD): ters ad sırası = en yeni önce.
    let desc = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        v.sort();
        v.reverse();
        v
    };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    let mut days = 0;
    'walk: for y in desc(sessions).iter().filter(|p| p.is_dir()) {
        for m in desc(y).iter().filter(|p| p.is_dir()) {
            for d in desc(m).iter().filter(|p| p.is_dir()) {
                for f in desc(d) {
                    let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        let at = std::fs::metadata(&f).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                        files.push((at, f));
                    }
                }
                days += 1;
                // Gece yarısını aşan oturum dünün klasöründe yazmaya devam eder: iki gün (boş
                // günlerde en çok bir hafta geriye).
                if (days >= 2 && !files.is_empty()) || days >= 7 {
                    break 'walk;
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().take(CODEX_ROLLOUT_FILES).map(|(_, f)| f).collect()
}

/// Dosyanın sonundan geriye ilk `limit_id = "codex"` `rate_limits` nesnesi.
fn last_codex_limits(file: &std::path::Path) -> Option<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(CODEX_ROLLOUT_TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("\"rate_limits\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        let limits = v.pointer("/payload/rate_limits")?;
        (limits.get("limit_id").and_then(|x| x.as_str()) == Some("codex")).then(|| limits.clone())
    })
}

/// `primary`/`secondary` pencerelerinden dolu (%100) ve sıfırlanması gelecekte olanın en geç sıfırlanması.
fn exhausted_window(limits: &serde_json::Value, now: i64) -> Option<i64> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let w = limits.get(k).filter(|w| w.is_object())?;
            let used = w.get("used_percent")?.as_f64()?;
            let reset = w.get("resets_at")?.as_i64()?.checked_mul(1000)?;
            (used >= 100.0 && reset > now).then_some(reset)
        })
        .max()
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
    id == "codex" || crate::providers::get(id).is_some()
}

/// Sıradaki hedefi seç: jetonu/oturumu olmayan, kapalı ya da tükenmiş sağlayıcı atlanır.
/// `available`: sağlayıcıda kullanılabilir jeton var mı; `codex` için CLI/giriş kullanılabilir mi.
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
/// `home` (kapalı olsa da dönülebilir). `skipped` = seçimde erişimi olmadığı için atlanan vardı
/// (Codex hazır değil / modeli yok); `available` = jeton/oturum.
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

/// Codex kataloğu öncelik sırasındadır: ilk güçlü, son hafif, orta katman ortadaki model.
/// Katalog canlıdır; burada model adı hard-code edilmez.
pub fn map_codex_model(source_model: &str, models: &[String]) -> Option<String> {
    if models.is_empty() {
        return None;
    }
    let light = is_light(source_model);
    let middle = source_model == "sonnet" || source_model.starts_with("claude-sonnet") || source_model.starts_with("muse-");
    let index = if light { models.len() - 1 } else if middle { models.len() / 2 } else { 0 };
    models.get(index).cloned()
}

/// Zincir geçişinde PTY'ye verilen kısa devam turu. `[Dex] ` önekiyle başlar: tur Dex turu
/// sayılır (delegate::on_prompt), böylece sonraki iş patron oturumuna değil yeni oturuma gider.
pub fn continue_prompt(from_label: &str, to_label: &str) -> String {
    format!(
        "[Dex] Kaldığın yerden devam et: az önce {from_label} sağlayıcısının kotası dolduğu için {to_label} üzerinden devam ediyorsun. \
Yarım kalan işin neyse onu sürdür; bitmiş işi baştan yapma, kullanıcıya quoting yapmadan tek cümleyle geçişi belirt."
    )
}

/// Görev #250: proaktif eşik geçişinin devam turu (`why` = "Claude %85 (5 sa)").
pub fn continue_prompt_threshold(why: &str, to_label: &str) -> String {
    format!(
        "[Dex] Kaldığın yerden devam et: {why} kullanım eşiğini geçtiği için {to_label} üzerinden devam ediyorsun. Yarım kalan işin neyse onu sürdür; bitmiş işi baştan yapma, kullanıcıya quoting yapmadan tek cümleyle geçişi belirt."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Görev #95: geçiş turu "[Dex]" ile başlamazsa patron turu sayılır ve sonraki iş dolu oturuma yapışır.
    #[test]
    fn continue_prompt_is_a_dex_turn() {
        assert!(continue_prompt("GLM", "Muse").starts_with("[Dex]"));
    }

    #[test]
    fn classify_permanent_transient_auth() {
        // Sentetik kota yanıtı; hata kodu ve mesaj biçimi korunur.
        let glm = "API Error: Request rejected (429) · [1308][Usage limit reached for 5 hour. Your limit will reset at 2002-01-01 17:42:19][example-request]";
        assert_eq!(classify("rate_limit", glm), Kind::Permanent);
        // `⎿` önekli satır da yakalanmalı.
        assert_eq!(classify("rate_limit", &format!("⎿  {glm}")), Kind::Permanent);
        assert_eq!(classify("rate_limit", "● [1310][Usage limit reached]"), Kind::Permanent);
        assert_eq!(classify("billing_error", "Credit balance is too low"), Kind::Permanent);
        assert_eq!(classify("rate_limit", "API Error 429 [1113][Insufficient balance]"), Kind::Permanent);
        assert_eq!(classify("rate_limit", "429 quota exhausted, limit reset soon"), Kind::Permanent);
        // Geçiciler zinciri ÇALIŞTIRMAZ.
        assert_eq!(classify("overloaded", "Overloaded"), Kind::Transient);
        assert_eq!(classify("server_error", "API Error: 503 Service Unavailable"), Kind::Transient);
        assert_eq!(classify("rate_limit", "429 Too Many Requests — slow down"), Kind::Transient);
        assert_eq!(classify("timeout", "request timed out"), Kind::Transient);
        // Auth zinciri ÇALIŞTIRMAZ.
        assert_eq!(classify("authentication_failed", "401 Unauthorized"), Kind::Auth);
        assert_eq!(classify("", "Invalid API key"), Kind::Auth);
        assert_eq!(classify("model_not_found", "model not found"), Kind::Unknown);
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
        let codex = ["gpt-strong".into(), "gpt-mid".into(), "gpt-light".into()];
        assert_eq!(map_codex_model("opus", &codex).as_deref(), Some("gpt-strong"));
        assert_eq!(map_codex_model("sonnet", &codex).as_deref(), Some("gpt-mid"));
        assert_eq!(map_codex_model("GLM-5.3-Flash", &codex).as_deref(), Some("gpt-light"));
        assert_eq!(map_codex_model("claude-sonnet-5-5", &codex).as_deref(), Some("gpt-mid"));
        assert_eq!(map_codex_model("claude-haiku-4-5-20251001", &codex).as_deref(), Some("gpt-light"));
        assert_eq!(map_codex_model("opus", &[]), None);
    }

    #[test]
    fn order_defaults_validates_and_dedups() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(order(&c), vec!["anthropic", "muse", "glm", "local", "codex"]);
        assert_eq!(set_order(&c, &["glm".into(), "glm".into(), "anthropic".into(), "codex".into()]).unwrap(),
            vec!["glm", "anthropic", "codex", "muse", "local"]);
        assert!(set_order(&c, &[]).is_err());
        assert!(set_order(&c, &["bogus".into()]).is_err());
        assert_eq!(order(&c), vec!["glm", "anthropic", "codex", "muse", "local"], "reddedilen yazım kaydı değiştirmez");
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
        assert_eq!(next.as_deref(), Some("codex"), "kapalı local atlanır");
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

    /// Sentetik Codex `token_count` satırı; olay biçimi CLI sözleşmesini korur.
    fn limits_line(id: &str, primary: &str) -> String {
        format!(
            r#"{{"timestamp":"2002-01-01T12:00:20Z","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"{id}","primary":{primary},"secondary":null,"credits":{{"has_credits":false}}}}}}}}"#
        )
    }

    fn window(used: f64, reset_s: i64) -> String {
        format!(r#"{{"used_percent":{used},"window_minutes":10080,"resets_at":{reset_s}}}"#)
    }

    #[test]
    fn codex_rollout_quota_known_only_when_full_and_future() {
        let root = std::env::temp_dir().join(format!("dex-codex-rl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let day = root.join("2002").join("01").join("01");
        std::fs::create_dir_all(&day).unwrap();
        let now = 1_009_886_430_000; // Sentetik UTC: 2002-01-01T12:00:30Z.
        let future = now / 1000 + 3_600;
        assert_eq!(codex_exhausted_until(&root.join("yok"), now), None, "klasör yok = bilinmiyor");
        // Dolu codex satırı, ardından daha yeni "premium" satırı (yok sayılır) ve bozuk satır.
        let file = day.join("rollout-2002-01-01T12-00-20-a.jsonl");
        let body = [
            r#"{"type":"session_meta","payload":{}}"#.to_string(),
            limits_line("codex", &window(40.0, future)),
            limits_line("codex", &window(100.0, future)),
            limits_line("premium", "null"),
            r#"{"rate_limits": bozuk"#.to_string(),
        ]
        .join("\n");
        std::fs::write(&file, body).unwrap();
        assert_eq!(codex_exhausted_until(&root, now), Some(future * 1000), "son codex satırı %100");
        // Sıfırlanması geçmiş = bilinmiyor.
        std::fs::write(&file, limits_line("codex", &window(100.0, now / 1000 - 60))).unwrap();
        assert_eq!(codex_exhausted_until(&root, now), None);
        // Pencere null = bilinmiyor; kotası olan = aday.
        std::fs::write(&file, limits_line("codex", "null")).unwrap();
        assert_eq!(codex_exhausted_until(&root, now), None);
        std::fs::write(&file, limits_line("codex", &window(97.0, future))).unwrap();
        assert_eq!(codex_exhausted_until(&root, now), None);
        // En son yazılan dosya karar verir (eski günün dolu satırı geçersiz).
        let old = root.join("2001").join("12").join("31");
        std::fs::create_dir_all(&old).unwrap();
        let old_line = limits_line("codex", &window(100.0, future)).replace("2002-01-01T12:00:20Z", "2001-12-31T12:00:20Z");
        std::fs::write(old.join("rollout-2001-12-31T12-00-20-b.jsonl"), old_line).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, limits_line("codex", &window(10.0, future))).unwrap();
        assert_eq!(codex_exhausted_until(&root, now), None, "en yeni ölçüm kotası var diyor");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_usage_limit_turn_end_is_the_signal() {
        // Sentetik UTC takvimi, codex-cli 0.154 olay biçimi: kota sinyali task_complete'ta;
        // önceki token_count tek başına tur sonu hatasını temsil etmez.
        let hit: serde_json::Value = serde_json::from_str(
            r#"{"timestamp":"2002-01-01T12:00:30.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t","last_agent_message":null,"error":{"message":"You’ve hit your usage limit. … try again at Jan 7th, 2002 1:38:54 PM UTC.","codex_error_info":"usage_limit_exceeded"}}}"#,
        )
        .unwrap();
        assert_eq!(codex_turn_limit(&hit), Some(true));
        let obj = serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","error":{"codex_error_info":{"usage_limit_exceeded":{}}}}});
        assert_eq!(codex_turn_limit(&obj), Some(true));
        let ok = serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"bitti"}});
        assert_eq!(codex_turn_limit(&ok), Some(false));
        let other = serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","error":{"codex_error_info":"internal_server_error"}}});
        assert_eq!(codex_turn_limit(&other), Some(false), "kota dışı hata zinciri tetiklemez");
        assert_eq!(codex_turn_limit(&serde_json::json!({"type":"event_msg","payload":{"type":"token_count"}})), None);
    }

    #[test]
    fn codex_recent_limit_only_for_a_fresh_last_turn() {
        let now = 1_009_886_430_000; // 2002-01-01T12:00:30Z (synthetic UTC)
        let started = r#"{"timestamp":"2002-01-01T12:00:20.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}"#;
        let limit = |ts: &str| format!(r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"t","error":{{"codex_error_info":"usage_limit_exceeded"}}}}}}"#);
        let ok = r#"{"timestamp":"2002-01-01T12:00:29.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"u","last_agent_message":"bitti"}}"#;
        let fresh = limit("2002-01-01T12:00:28.000Z");
        assert!(codex_recent_limit(format!("{started}\n{fresh}\n").as_bytes(), now));
        assert!(codex_recent_limit(format!("{started}\n{fresh}").as_bytes(), now), "son satır yarım kalmış newline'sız");
        assert!(!codex_recent_limit(format!("{started}\n{}\n", limit("2002-01-01T11:58:00.000Z")).as_bytes(), now), "bayat");
        assert!(!codex_recent_limit(format!("{fresh}\n{started}\n").as_bytes(), now), "arkasından yeni tur başladı");
        assert!(!codex_recent_limit(format!("{fresh}\n{ok}\n").as_bytes(), now), "son tur başarılı");
        assert!(!codex_recent_limit(format!("{}\n", limit("yok")).as_bytes(), now), "damgasız/okunamayan saat");
        assert!(!codex_recent_limit(b"", now));
    }

    #[test]
    fn codex_own_rollout_gives_the_reset() {
        let dir = std::env::temp_dir().join(format!("dex-codex-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-2002-01-01T12-00-20-x.jsonl");
        let now = 1_009_886_430_000; // Sentetik UTC: 2002-01-01T12:00:30Z; reset 2002-01-07T13:38:54Z.
        std::fs::write(&file, format!("{}\n{}\n", limits_line("codex", &window(100.0, 1_010_410_734)), limits_line("premium", "null"))).unwrap();
        assert_eq!(codex_file_exhausted_until(&file, now), Some(1_010_410_734_000));
        std::fs::write(&file, limits_line("codex", &window(40.0, 1_010_410_734))).unwrap();
        assert_eq!(codex_file_exhausted_until(&file, now), None);
        assert_eq!(codex_file_exhausted_until(&dir.join("yok.jsonl"), now), None);
        let _ = std::fs::remove_dir_all(&dir);
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
        mark_exhausted(&c, "codex", Some(now + 7_200_000), "kota").unwrap();
        mark_exhausted(&c, "glm", Some(now + 3_600_000), "kota").unwrap();
        assert_eq!(earliest_reset(&c).unwrap(), Some(now + 3_600_000));
        set_disabled(&c, &["glm".into()]).unwrap();
        assert_eq!(earliest_reset(&c).unwrap(), Some(now + 7_200_000), "kapalı motor sayılmaz");
    }

    #[test]
    fn none_reason_tells_quota_from_access() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        set_disabled(&c, &["muse".into(), "local".into()]).unwrap();
        let later = now_ms() + 3_600_000;
        // Claude'dan sonra açık: GLM, Codex.
        let no_glm = |p: &str| p != "glm";
        // Codex hazır değil (seçimde atlandı), GLM jetonsuz: kota değil erişim.
        assert_eq!(none_reason(&c, "anthropic", "anthropic", true, &no_glm), "zincirde jetonu/erişimi olan motor yok");
        mark_exhausted(&c, "codex", Some(later), "kota").unwrap();
        assert_eq!(none_reason(&c, "anthropic", "anthropic", false, &no_glm), "zincirde kotası olan motor yok, kalanların jetonu/erişimi yok");
        set_disabled(&c, &["muse".into(), "local".into(), "glm".into()]).unwrap();
        assert_eq!(none_reason(&c, "anthropic", "anthropic", false, &no_glm), "zincirde kotası olan motor yok");
        // Codex'ten çıkış: sırada kimse yok, asıl sağlayıcı (Claude) sayılır.
        assert_eq!(none_reason(&c, "codex", "anthropic", false, &no_glm), "zincirde kullanılabilir hedef yok");
        mark_exhausted(&c, "anthropic", Some(later), "kota").unwrap();
        assert_eq!(none_reason(&c, "codex", "anthropic", false, &no_glm), "zincirde kotası olan motor yok");
    }

    #[test]
    fn pick_skips_exhausted_and_tokenless() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        mark_exhausted(&c, "muse", Some(now_ms() + 3_600_000), "kota").unwrap();
        // Zincir geri sarmaz: GLM'den sonra token'sız local atlanır, Codex'e geçilir.
        let next = pick_next(&c, "glm", &|p| p == "codex").unwrap();
        assert_eq!(next.as_deref(), Some("codex"));
        // Süresi dolan kayıt kalkar.
        mark_exhausted(&c, "anthropic", Some(now_ms() - 1), "eski").unwrap();
        let next = pick_next(&c, "eski-hedef", &|_| false).unwrap();
        assert_eq!(next.as_deref(), Some("anthropic"), "süresi dolan + jetonsuz değil (anthropic)");
        // Hepsi tükenirse bekleme.
        mark_exhausted(&c, "anthropic", Some(now_ms() + 3_600_000), "k").unwrap();
        mark_exhausted(&c, "glm", Some(now_ms() + 3_600_000), "k").unwrap();
        mark_exhausted(&c, "local", Some(now_ms() + 3_600_000), "k").unwrap();
        assert_eq!(pick_next(&c, "muse", &|p| p == "codex").unwrap().as_deref(), Some("codex"));
        assert_eq!(pick_next(&c, "muse", &|_| false).unwrap(), None);
        // Varsayılan süre 5 sa.
        mark_exhausted(&c, "x", None, "k").unwrap();
        let e: i64 = c.query_row("SELECT reset_at FROM provider_exhausted WHERE provider = 'x'", [], |r| r.get(0)).unwrap();
        assert!((e - (now_ms() + DEFAULT_EXHAUST_MS)).abs() < 5_000);
    }
}
