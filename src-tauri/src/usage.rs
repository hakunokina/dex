//! #54: Dex kullanım şeridinin verisi (tok/s, bugünün token + $, sağlayıcı kotaları).
//! Aşama 1'de kaynak Token Monitor'ün hub'ı: `GET 127.0.0.1:17321/api/stats/stream` (SSE; ilk
//! olay `snapshot` = `/api/stats`'ın tamamı, sonrası her değişimde `stats`). Poll yok; kopunca
//! üstel geri çekilmeyle yeniden bağlanır, TM kapalıysa sessizce `offline` kalır.
//! UI yalnız [`UsageSnapshot`]'ı görür (komut `usage_get` + olay `usage_snapshot`), kaynağı bilmez:
//! Aşama 2'de Dex'in kendi modülü aynı tipi üretecek.
//! Yerel-yalnız: hub bu makinenin TM'si; dexd'de karşılığı yok (CLIENT_ONLY).

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{plugin::TauriPlugin, AppHandle, Emitter, Manager, Runtime, State};

const HUB: &str = "127.0.0.1:17321";
const EVENT: &str = "usage_snapshot";
/// TM host-mode'da hub'ı hep bir anahtarla açar; anahtar OS keyring'inde (servis/hesap).
const SECRET_SERVICE: &str = "dex-tm-hub";
const SECRET_ACCOUNT: &str = "hub";
/// Hub 30 sn'de bir `: hb` atar; bundan uzun sessizlik yarı açık bağlantıdır.
const READ_TIMEOUT: Duration = Duration::from_secs(75);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

// ── Sağlayıcı-bağımsız tip ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageStatus {
    Connecting,
    Live,
    /// Kaynak yok (TM kapalı ya da hub açık değil).
    Offline,
    /// Hub anahtar istiyor ya da kayıtlı anahtar yanlış.
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    /// Sağlayıcı kimliği (`claude`, `codex`, `zai`, `muse`…); etiket/renk UI'ın işi.
    pub id: String,
    /// Birincil pencerede kalan yüzde; `None` = sağlayıcı şu an bir şey bildirmiyor ("--").
    pub remaining_percent: Option<f64>,
    /// Pencere türü: `session` (5 saat), `daily`, `weekly`, `billing`.
    pub window: Option<String>,
    /// ISO-8601.
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub source: &'static str,
    pub status: UsageStatus,
    /// Son çıktı hızı (çıktı token/sn), `rate_at` anındaki örnek; eskiyince UI söndürür.
    pub tok_per_sec: Option<f64>,
    /// Epoch ms.
    pub rate_at: Option<u64>,
    pub today_tokens: Option<u64>,
    pub today_cost_usd: Option<f64>,
    pub providers: Vec<ProviderUsage>,
    /// Kaynak bir anahtarla korunuyor ve Dex'te kayıtlı bir anahtar var.
    pub has_secret: bool,
    /// Epoch ms; son veri anı.
    pub updated_at: Option<u64>,
}

impl UsageSnapshot {
    fn empty(status: UsageStatus, has_secret: bool) -> Self {
        Self {
            source: "token-monitor",
            status,
            tok_per_sec: None,
            rate_at: None,
            today_tokens: None,
            today_cost_usd: None,
            providers: Vec::new(),
            has_secret,
            updated_at: None,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ── TM istatistiği → UsageSnapshot ───────────────────────────────────────────

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

/// Canlı hız: TM'nin kendi yöntemi. Bir cihazın bugünkü `timedOutputTokens`/`timedDurationMs`
/// sayaçları kümülatif; iki itme arasındaki fark o aralığın gerçek hızı. Geri giden sayaç
/// (gece yarısı, yeniden yapılandırma) yeni taban sayılır.
#[derive(Default)]
struct RateTracker {
    base: HashMap<String, (f64, f64)>,
    /// cihaz → (tok/s, epoch ms)
    samples: HashMap<String, (f64, u64)>,
}

impl RateTracker {
    fn observe(&mut self, stats: &Value, now: u64) -> Option<(f64, u64)> {
        let devices = stats["devices"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let mut seen = Vec::new();
        for d in devices.iter().filter(|d| d["stale"] != Value::Bool(true)) {
            let Some(id) = d["deviceId"].as_str() else { continue };
            let today = &d["periods"]["today"];
            seen.push(id.to_string());
            let (Some(out), Some(dur)) = (num(&today["timedOutputTokens"]), num(&today["timedDurationMs"])) else {
                self.base.remove(id);
                continue;
            };
            if let Some((b_out, b_dur)) = self.base.insert(id.to_string(), (out, dur)) {
                let (d_out, d_dur) = (out - b_out, dur - b_dur);
                if d_out < 0.0 || d_dur < 0.0 {
                    self.samples.remove(id);
                } else if d_dur > 0.0 {
                    self.samples.insert(id.to_string(), (d_out * 1000.0 / d_dur, now));
                }
            }
        }
        self.base.retain(|k, _| seen.contains(k));
        self.samples.retain(|k, _| seen.contains(k));
        if self.samples.is_empty() {
            return None;
        }
        let rate = self.samples.values().map(|s| s.0).sum::<f64>();
        let at = self.samples.values().map(|s| s.1).max().unwrap_or(now);
        Some((rate, at))
    }
}

const WINDOW_ORDER: [&str; 4] = ["session", "daily", "weekly", "billing"];

/// Pencerede kalan yüzde; kredi pencerelerinde ikisi de yok → None (uydurma %0 değil).
fn remaining(w: &Value) -> Option<f64> {
    num(&w["remainingPercent"]).or_else(|| num(&w["usedPercent"]).map(|u| 100.0 - u)).map(|r| r.clamp(0.0, 100.0))
}

/// TM'nin `compactLimitSelection`'ı: session > daily > weekly > billing; aynı türden birden çok
/// havuz varsa etiketsiz (kanonik) olan, yoksa en dar olan.
fn primary_window(provider: &Value) -> Option<&Value> {
    let id = provider["provider"].as_str().unwrap_or("");
    let windows = provider["windows"].as_array()?;
    for kind in WINDOW_ORDER {
        let pool: Vec<&Value> = windows
            .iter()
            .filter(|w| w["kind"] == kind && w["showMeter"] != Value::Bool(false))
            .filter(|w| !(id == "codex" && w["additional"] == Value::Bool(true)))
            .filter(|w| remaining(w).is_some())
            .collect();
        let canonical = |w: &&&Value| {
            let label = w["label"].as_str().unwrap_or("").trim().to_lowercase();
            label.is_empty() || (kind == "weekly" && label == "weekly") || (kind == "billing" && label == "total")
        };
        let pick = match pool.len() {
            0 => continue,
            1 => pool[0],
            _ => pool.iter().find(canonical).copied().unwrap_or_else(|| {
                pool.iter().copied().min_by(|a, b| remaining(a).partial_cmp(&remaining(b)).unwrap()).unwrap()
            }),
        };
        return Some(pick);
    }
    None
}

/// Bayat olmayan bir cihazda TM'nin "active" saydığı istemciler (yerelde kullanımı görülen araçlar).
fn active_clients(stats: &Value) -> Vec<String> {
    let devices = stats["devices"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut out = Vec::new();
    for d in devices.iter().filter(|d| d["stale"] != Value::Bool(true)) {
        for (id, st) in d["clientStatus"].as_object().into_iter().flatten() {
            let (id, st) = (id.to_lowercase(), st.as_str().or_else(|| st["status"].as_str()));
            if st == Some("active") && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

fn providers(stats: &Value) -> Vec<ProviderUsage> {
    let active = active_clients(stats);
    let mut out: Vec<ProviderUsage> = Vec::new();
    for p in stats["limits"]["providers"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(id) = p["provider"].as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) else { continue };
        let ok = p["status"] == "ok";
        // Penceresiz hata satırı (notConfigured/unauthorized) yalnız araç bu makinede kullanılıyorsa
        // "--" olarak kalır (TM dock'u gibi: Muse'un limit kaynağı yok ama kullanımı var); TM'nin
        // seçili ama hiç kurulmamış sağlayıcıları şeride yer kaplamaz. Kimlik bilinmese de elenmez.
        if !ok && p["windows"].as_array().is_none_or(|w| w.is_empty()) && !active.contains(&id) {
            continue;
        }
        let fresh = ok && p["stale"] != Value::Bool(true);
        let w = if fresh { primary_window(p) } else { None };
        let row = ProviderUsage {
            id: id.clone(),
            remaining_percent: w.and_then(remaining),
            window: w.and_then(|w| w["kind"].as_str()).map(str::to_string),
            resets_at: w.and_then(|w| w["resetsAt"].as_str()).map(str::to_string),
        };
        // Bir sağlayıcının birden çok hesabı: en dar olanı göster.
        match out.iter_mut().find(|r| r.id == id) {
            Some(prev) if prev.remaining_percent.is_none()
                || row.remaining_percent.is_some_and(|r| r < prev.remaining_percent.unwrap()) => *prev = row,
            Some(_) => {}
            None => out.push(row),
        }
    }
    out
}

fn apply_stats(snap: &mut UsageSnapshot, rate: &mut RateTracker, stats: &Value, now: u64) {
    let today = &stats["periods"]["today"];
    snap.status = UsageStatus::Live;
    snap.today_tokens = num(&today["totalTokens"]).map(|n| n.max(0.0) as u64);
    snap.today_cost_usd = num(&today["costUsd"]);
    snap.providers = providers(stats);
    if let Some((r, at)) = rate.observe(stats, now) {
        snap.tok_per_sec = Some(r);
        snap.rate_at = Some(at);
    }
    snap.updated_at = Some(now);
}

// ── SSE + HTTP/1.1 ───────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct SseEvent {
    event: String,
    data: String,
}

/// Satır satır SSE ayrıştırıcı (WHATWG): `event:`/`data:` alanları, boş satır olayı bitirir,
/// `:` ile başlayan satır yorumdur (hub'ın kalp atışı).
#[derive(Default)]
struct SseParser {
    event: String,
    data: Vec<String>,
}

impl SseParser {
    fn line(&mut self, line: &str) -> Option<SseEvent> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if self.data.is_empty() {
                self.event.clear();
                return None;
            }
            let ev = SseEvent {
                event: if self.event.is_empty() { "message".into() } else { std::mem::take(&mut self.event) },
                data: std::mem::take(&mut self.data).join("\n"),
            };
            self.event.clear();
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = value.to_string(),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }
}

/// `Transfer-Encoding: chunked` gövdesini düz akışa çevirir (Node SSE'yi böyle yollar).
struct Chunked<R: BufRead> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut size = String::new();
            if self.inner.read_line(&mut size)? == 0 {
                return Ok(0);
            }
            let hex = size.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(hex, 16).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bozuk chunk"))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let want = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n;
        if self.left == 0 {
            let mut crlf = String::new();
            self.inner.read_line(&mut crlf)?;
        }
        Ok(n)
    }
}

#[derive(Debug, PartialEq)]
enum Ended {
    /// Bağlanılamadı ya da akış koptu.
    Down,
    /// 401: anahtar yok/yanlış.
    Unauthorized,
}

/// Bir bağlantı ömrü: istek, başlık, sonra kopana dek her olay `on_stats`'a. `stream` kopyası
/// `conn`'a konur ki anahtar değişince dışarıdan kapatılabilsin.
fn run_stream(
    addr: &str,
    secret: Option<&str>,
    conn: &Mutex<Option<TcpStream>>,
    mut on_stats: impl FnMut(&Value),
) -> Ended {
    let Ok(sock) = addr.parse::<SocketAddr>() else { return Ended::Down };
    let Ok(mut stream) = TcpStream::connect_timeout(&sock, Duration::from_secs(2)) else { return Ended::Down };
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    *conn.lock().unwrap() = stream.try_clone().ok();
    let auth = secret.map(|s| format!("Authorization: Bearer {s}\r\n")).unwrap_or_default();
    let req = format!(
        "GET /api/stats/stream HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\n{auth}\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return Ended::Down;
    }
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    if reader.read_line(&mut status).is_err() {
        return Ended::Down;
    }
    let code = status.split_whitespace().nth(1).unwrap_or("");
    let mut chunked = false;
    loop {
        let mut h = String::new();
        match reader.read_line(&mut h) {
            Ok(0) | Err(_) => return Ended::Down,
            Ok(_) if h.trim().is_empty() => break,
            Ok(_) => {
                let h = h.to_ascii_lowercase();
                chunked |= h.starts_with("transfer-encoding:") && h.contains("chunked");
            }
        }
    }
    match code {
        "200" => {}
        "401" => return Ended::Unauthorized,
        _ => return Ended::Down,
    }
    let body: Box<dyn BufRead> = if chunked {
        Box::new(BufReader::new(Chunked { inner: reader, left: 0, done: false }))
    } else {
        Box::new(reader)
    };
    let mut parser = SseParser::default();
    for line in body.lines() {
        let Ok(line) = line else { break };
        let Some(ev) = parser.line(&line) else { continue };
        if ev.event != "snapshot" && ev.event != "stats" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
            if v["stats"].is_object() {
                on_stats(&v["stats"]);
            }
        }
    }
    Ended::Down
}

// ── Köprü ────────────────────────────────────────────────────────────────────

pub struct UsageBridge {
    snap: Mutex<UsageSnapshot>,
    conn: Mutex<Option<TcpStream>>,
    kick: Mutex<Sender<()>>,
}

impl UsageBridge {
    pub fn snapshot(&self) -> UsageSnapshot {
        self.snap.lock().unwrap().clone()
    }

    /// Bekleyen geri çekilmeyi keser, açık bağlantıyı kapatır: döngü hemen yeniden dener.
    fn reconnect(&self) {
        let _ = self.kick.lock().unwrap().send(());
        if let Some(s) = self.conn.lock().unwrap().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

fn read_secret() -> Option<String> {
    crate::secrets::get(SECRET_SERVICE, SECRET_ACCOUNT).ok().flatten().filter(|s| !s.is_empty())
}

/// Geri çekilme: her başarısız denemede iki katı, BACKOFF_MAX'ta tavan; veri gelince sıfırlanır.
fn next_backoff(cur: Duration) -> Duration {
    (cur * 2).min(BACKOFF_MAX)
}

/// Köprü döngüsü. `publish` her durum/veri değişiminde çağrılır (Tauri'de olay atar).
fn spawn_loop(
    addr: String,
    secret: impl Fn() -> Option<String> + Send + 'static,
    publish: impl Fn(&UsageSnapshot) + Send + 'static,
) -> Arc<UsageBridge> {
    let (tx, rx) = mpsc::channel();
    let bridge = Arc::new(UsageBridge {
        snap: Mutex::new(UsageSnapshot::empty(UsageStatus::Connecting, false)),
        conn: Mutex::new(None),
        kick: Mutex::new(tx),
    });
    let b = bridge.clone();
    std::thread::Builder::new()
        .name("dex-usage".into())
        .spawn(move || {
            let mut backoff = BACKOFF_MIN;
            loop {
                let key = secret();
                let mut rate = RateTracker::default();
                let mut got = false;
                let ended = run_stream(&addr, key.as_deref(), &b.conn, |stats| {
                    got = true;
                    let mut s = b.snap.lock().unwrap();
                    s.has_secret = key.is_some();
                    apply_stats(&mut s, &mut rate, stats, now_ms());
                    publish(&s);
                });
                b.conn.lock().unwrap().take();
                if got {
                    backoff = BACKOFF_MIN;
                }
                let status = if ended == Ended::Unauthorized { UsageStatus::Unauthorized } else { UsageStatus::Offline };
                {
                    let mut s = b.snap.lock().unwrap();
                    // Durum değişmediyse olay da yok: TM kapalıyken her denemede UI'a gürültü gitmez.
                    if s.status != status || s.has_secret != key.is_some() || s.updated_at.is_some() {
                        *s = UsageSnapshot::empty(status, key.is_some());
                        publish(&s);
                    }
                }
                match rx.recv_timeout(backoff) {
                    Ok(()) => backoff = BACKOFF_MIN,
                    Err(RecvTimeoutError::Timeout) => backoff = next_backoff(backoff),
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("dex-usage iş parçacığı başlatılamadı");
    bridge
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("dex-usage")
        .setup(|app, _| {
            let handle: AppHandle<R> = app.clone();
            let bridge = spawn_loop(HUB.into(), read_secret, move |s| {
                let _ = handle.emit(EVENT, s);
            });
            app.manage(bridge);
            Ok(())
        })
        .build()
}

#[tauri::command]
pub fn usage_get(bridge: State<Arc<UsageBridge>>) -> UsageSnapshot {
    bridge.snapshot()
}

/// TM hub anahtarı (TM → Ayarlar → Multi-device Sync → Host). Boş = sil. Yalnız OS keyring'e.
#[tauri::command]
pub fn usage_hub_secret_set(bridge: State<Arc<UsageBridge>>, secret: String) -> Result<(), String> {
    let secret = secret.trim();
    if secret.is_empty() {
        crate::secrets::delete(SECRET_SERVICE, SECRET_ACCOUNT)?;
    } else {
        crate::secrets::set(SECRET_SERVICE, SECRET_ACCOUNT, secret)
            .map_err(|e| format!("TM anahtarı anahtarlığa yazılamadı: {e}"))?;
    }
    bridge.reconnect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;

    fn parse_all(text: &str) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        text.split_inclusive('\n').filter_map(|l| p.line(l)).collect()
    }

    #[test]
    fn sse_events_comments_and_multiline_data() {
        let evs = parse_all(": hb\n\nevent: snapshot\ndata: {\"a\":\ndata: 1}\n\nevent: stats\r\ndata:{}\r\n\r\ndata: x\n\n");
        assert_eq!(
            evs,
            vec![
                SseEvent { event: "snapshot".into(), data: "{\"a\":\n1}".into() },
                SseEvent { event: "stats".into(), data: "{}".into() },
                SseEvent { event: "message".into(), data: "x".into() },
            ]
        );
    }

    #[test]
    fn chunked_body_is_unwrapped() {
        let raw = b"5\r\nhello\r\n7;ext=1\r\n world\n\r\n0\r\n\r\n";
        let mut out = String::new();
        Chunked { inner: &raw[..], left: 0, done: false }.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello world\n");
    }

    fn stats(out: f64, dur: f64) -> Value {
        json!({
            "periods": {"today": {"totalTokens": 221_700_000, "costUsd": 94.04}},
            "devices": [
                {"deviceId": "pc", "periods": {"today": {"timedOutputTokens": out, "timedDurationMs": dur}}},
                {"deviceId": "old", "stale": true, "periods": {"today": {"timedOutputTokens": 9e9, "timedDurationMs": 1.0}}}
            ],
            "limits": {"providers": [
                {"provider": "claude", "status": "ok", "windows": [
                    {"kind": "weekly", "usedPercent": 50},
                    {"kind": "session", "remainingPercent": 66.4, "resetsAt": "2026-10-02T23:10:00.000Z"}
                ]},
                {"provider": "codex", "status": "ok", "windows": [
                    {"kind": "session", "label": "", "usedPercent": 3},
                    {"kind": "session", "label": "spark", "additional": true, "usedPercent": 90}
                ]},
                {"provider": "claude", "status": "ok", "windows": [{"kind": "session", "remainingPercent": 20}]},
                {"provider": "muse", "status": "error", "windows": [{"kind": "session", "usedPercent": 10}]},
                {"provider": "cursor", "status": "error", "windows": []},
                {"provider": "zai", "status": "ok", "stale": true, "windows": [{"kind": "session", "usedPercent": 0}]}
            ]}
        })
    }

    #[test]
    fn stats_map_to_snapshot() {
        let mut snap = UsageSnapshot::empty(UsageStatus::Connecting, false);
        let mut rate = RateTracker::default();
        apply_stats(&mut snap, &mut rate, &stats(1000.0, 10_000.0), 5);
        assert_eq!(snap.status, UsageStatus::Live);
        assert_eq!(snap.today_tokens, Some(221_700_000));
        assert_eq!(snap.today_cost_usd, Some(94.04));
        assert_eq!(snap.tok_per_sec, None, "ilk itme yalnız taban");
        let ids: Vec<_> = snap.providers.iter().map(|p| (p.id.as_str(), p.remaining_percent)).collect();
        // claude: iki hesap → en dar (20); codex: ek havuz sayılmaz; muse hata → "--";
        // cursor penceresiz → yok; zai bayat → "--".
        assert_eq!(ids, vec![("claude", Some(20.0)), ("codex", Some(97.0)), ("muse", None), ("zai", None)]);
        assert_eq!(snap.providers[1].window.as_deref(), Some("session"));

        apply_stats(&mut snap, &mut rate, &stats(1000.0 + 1130.0, 20_000.0), 9);
        assert_eq!(snap.tok_per_sec, Some(113.0));
        assert_eq!(snap.rate_at, Some(9));
        // Aynı sayaçlar (yalnız limit itmesi) son örneği korur; geri giden sayaç örneği düşürür.
        apply_stats(&mut snap, &mut rate, &stats(2130.0, 20_000.0), 12);
        assert_eq!(snap.tok_per_sec, Some(113.0));
        apply_stats(&mut snap, &mut rate, &stats(10.0, 100.0), 15);
        assert_eq!(rate.samples.len(), 0);
    }

    /// Canlı hub'ın biçimi: Muse'un limit kaynağı `notConfigured` (penceresiz) ama istemcisi active;
    /// seçili ama kullanılmayan sağlayıcılar (waiting) düşer; bilinmeyen kimlik aynı kurala tabi.
    #[test]
    fn unconfigured_provider_stays_when_its_client_is_in_use() {
        let s = json!({
            "devices": [
                {"deviceId": "pc", "clientStatus": {"claude": "active", "Muse": "active", "acme-code": {"status": "active"}, "cursor": "waiting"}},
                {"deviceId": "old", "stale": true, "clientStatus": {"copilot": "active"}}
            ],
            "limits": {"providers": [
                {"provider": "antigravity", "status": "notConfigured", "windows": []},
                {"provider": "claude", "status": "ok", "windows": [{"kind": "session", "usedPercent": 34}]},
                {"provider": "copilot", "status": "notConfigured", "windows": []},
                {"provider": "cursor", "status": "unauthorized", "windows": []},
                {"provider": "muse", "status": "notConfigured", "windows": []},
                {"provider": "acme-code", "status": "error"},
                {"provider": "newvendor", "status": "ok", "windows": [{"kind": "daily", "remainingPercent": 40}]}
            ]}
        });
        let ids: Vec<_> = providers(&s).into_iter().map(|p| (p.id, p.remaining_percent)).collect();
        assert_eq!(
            ids,
            vec![("claude".into(), Some(66.0)), ("muse".into(), None), ("acme-code".into(), None), ("newvendor".into(), Some(40.0))]
        );
    }

    #[test]
    fn window_preference_follows_tm() {
        let p = json!({"provider": "claude", "windows": [
            {"kind": "weekly", "label": "fable", "usedPercent": 90},
            {"kind": "weekly", "label": "", "usedPercent": 10},
            {"kind": "billing", "remainingPercent": null, "usedPercent": null}
        ]});
        assert_eq!(remaining(primary_window(&p).unwrap()), Some(90.0));
        let credits = json!({"provider": "x", "windows": [{"kind": "billing"}]});
        assert!(primary_window(&credits).is_none());
    }

    #[test]
    fn backoff_doubles_to_cap() {
        let mut d = BACKOFF_MIN;
        let seq: Vec<u64> = (0..8).map(|_| { d = next_backoff(d); d.as_secs() }).collect();
        assert_eq!(seq, vec![2, 4, 8, 16, 32, 60, 60, 60]);
    }

    /// Sahte hub: her bağlantıya `script(n)` cevabını yazar ve kapatır.
    fn fake_hub(script: impl Fn(usize) -> Vec<u8> + Send + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for (n, s) in l.incoming().enumerate() {
                let mut s = s.unwrap();
                let mut req = [0u8; 2048];
                let got = s.read(&mut req).unwrap_or(0);
                let req = String::from_utf8_lossy(&req[..got]).to_string();
                let mut body = script(n);
                if req.contains("Authorization: Bearer good") {
                    body = body.replace_text("401 Unauthorized", "200 OK");
                }
                let _ = s.write_all(&body);
            }
        });
        addr
    }

    trait ReplaceText {
        fn replace_text(self, from: &str, to: &str) -> Vec<u8>;
    }
    impl ReplaceText for Vec<u8> {
        fn replace_text(self, from: &str, to: &str) -> Vec<u8> {
            String::from_utf8(self).unwrap().replace(from, to).into_bytes()
        }
    }

    fn sse_response(status: &str, stats: &Value) -> Vec<u8> {
        let chunk = |t: &str| format!("{:x}\r\n{t}\r\n", t.len());
        let ev = format!("event: snapshot\ndata: {}\n\n", json!({"type": "stats", "stats": stats}));
        let (hb, ev) = (chunk(": hb\n\n"), chunk(&ev));
        format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{hb}{ev}0\r\n\r\n").into_bytes()
    }

    /// Köprünün yayınladığı her anlık görüntü; durum geçişleri yarışsız izlenir.
    type Log = Arc<Mutex<Vec<UsageSnapshot>>>;

    fn spawn_logged(addr: String, secret: impl Fn() -> Option<String> + Send + 'static) -> (Arc<UsageBridge>, Log) {
        let log: Log = Arc::default();
        let l = log.clone();
        (spawn_loop(addr, secret, move |s| l.lock().unwrap().push(s.clone())), log)
    }

    /// `log`'da `from`'dan sonra koşulu sağlayan ilk kayıt; indeksiyle.
    fn wait_for(log: &Log, from: usize, f: impl Fn(&UsageSnapshot) -> bool) -> (usize, UsageSnapshot) {
        for _ in 0..250 {
            if let Some((i, s)) = log.lock().unwrap().iter().enumerate().skip(from).find(|(_, s)| f(s)) {
                return (i, s.clone());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("beklenen durum gelmedi: {:?}", log.lock().unwrap());
    }

    #[test]
    fn no_hub_is_offline_and_quiet() {
        // Dinlemeyen port: bağlanma reddedilir.
        let addr = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        let (bridge, log) = spawn_logged(addr, || None);
        wait_for(&log, 0, |s| s.status == UsageStatus::Offline);
        bridge.reconnect();
        bridge.reconnect();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(log.lock().unwrap().len(), 1, "tekrar denemeler olay atmamalı");
        assert_eq!(bridge.snapshot().status, UsageStatus::Offline);
    }

    #[test]
    fn streams_then_reconnects_after_drop() {
        let addr = fake_hub(|_| sse_response("200 OK", &stats(1.0, 1.0)));
        let (bridge, log) = spawn_logged(addr, || None);
        // Hub her bağlantıyı ilk olaydan sonra kapatıyor: veri gelir, sonra offline'a düşer.
        let (i, s) = wait_for(&log, 0, |s| s.status == UsageStatus::Live);
        assert_eq!(s.today_tokens, Some(221_700_000));
        let (j, _) = wait_for(&log, i, |s| s.status == UsageStatus::Offline);
        // Veri geldiği için geri çekilme 1 sn'ye döndü; dürtünce beklemeden yeniden bağlanır.
        bridge.reconnect();
        wait_for(&log, j, |s| s.status == UsageStatus::Live);
    }

    #[test]
    fn unauthorized_until_secret_matches() {
        let addr = fake_hub(|_| sse_response("401 Unauthorized", &stats(1.0, 1.0)));
        let key = Arc::new(Mutex::new(None::<String>));
        let k = key.clone();
        let (bridge, log) = spawn_logged(addr, move || k.lock().unwrap().clone());
        let (i, s) = wait_for(&log, 0, |s| s.status == UsageStatus::Unauthorized);
        assert!(!s.has_secret);
        *key.lock().unwrap() = Some("good".into());
        bridge.reconnect();
        let (_, s) = wait_for(&log, i, |s| s.status == UsageStatus::Live);
        assert!(s.has_secret);
    }
    /// Salt okuma: bu makinenin canlı hub'ından ilk anlık görüntünün sağlayıcı satırları (anahtar
    /// keyring'den; yazdırılmaz). `DEX_TM_EXPECT=muse,claude` verilirse hepsi şeritte olmalı.
    #[test]
    #[ignore = "canlı TM hub'ı (127.0.0.1:17321) ve keyring'de dex-tm-hub anahtarı ister"]
    fn live_hub_snapshot_providers() {
        let (bridge, log) = spawn_logged(HUB.into(), read_secret);
        let (_, s) = wait_for(&log, 0, |s| s.status != UsageStatus::Connecting);
        assert_eq!(s.status, UsageStatus::Live, "hub'a bağlanılamadı");
        let rows: Vec<_> = s.providers.iter().map(|p| format!("{}={:?}", p.id, p.remaining_percent)).collect();
        println!("şerit: {}", rows.join(" "));
        for id in std::env::var("DEX_TM_EXPECT").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            assert!(s.providers.iter().any(|p| p.id == id), "{id} şeritte yok");
        }
        drop(bridge);
    }

    /// Gerçek TM hub'ına karşı (Node `src/hub/server.js`): snapshot olayı, sonra ingest'in
    /// tetiklediği `stats` itmesi chunked SSE'den okunur.
    #[test]
    #[ignore = "canlı TM hub'ı ister: DEX_TM_HUB_TEST=127.0.0.1:<port>, DEX_TM_SECRET_TEST=<anahtar>"]
    fn live_tm_hub() {
        let addr = std::env::var("DEX_TM_HUB_TEST").unwrap();
        let secret = std::env::var("DEX_TM_SECRET_TEST").ok();
        let ingest = |tokens: u64| {
            let body = json!({"deviceId": "dex-live-test", "periods": {"today": {"totalTokens": tokens, "costUsd": 1.5}},
                "limits": {"providers": [{"provider": "claude", "status": "ok",
                    "windows": [{"kind": "session", "usedPercent": 34, "resetsAt": "2099-01-01T00:00:00.000Z"}]}]}})
            .to_string();
            let auth = secret.as_deref().map(|s| format!("Authorization: Bearer {s}\r\n")).unwrap_or_default();
            let mut s = TcpStream::connect(&addr).unwrap();
            let head = "Content-Type: application/json\r\nConnection: close\r\n";
            write!(s, "POST /api/ingest HTTP/1.1\r\nHost: {addr}\r\n{head}Content-Length: {}\r\n{auth}\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        };
        ingest(1000);
        let key = secret.clone();
        let (_bridge, log) = spawn_logged(addr.clone(), move || key.clone());
        let (i, s) = wait_for(&log, 0, |s| s.status == UsageStatus::Live);
        assert_eq!(s.providers[0].id, "claude");
        assert_eq!(s.providers[0].remaining_percent, Some(66.0));
        ingest(2000);
        let (_, s) = wait_for(&log, i + 1, |s| s.today_tokens.is_some_and(|t| t >= 2000));
        assert_eq!(s.status, UsageStatus::Live);
    }
}
