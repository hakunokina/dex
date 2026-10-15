//! Görev #289 — yapılandırılabilir GPU broker'ı (`GET /v1/models` türleri: t2i, bg-removal,
//! llm …). Adres ve token yalnız dexd'de: adresler `settings` satırında (`gpu_broker`), token sır
//! deposunda (`secrets`, S4). Hiçbir ajan yanıtında adres/token geçmez ([`scrub`]).
//!
//! Akış: lider `gpu_request` (tür kümesi, worker, gerekçe, bütçe) → patron kartı
//! (`gpu-requests-changed`) → patron `gpu_request_decide` RPC'si → izin (`gpu_grants`) → lider
//! `delegate(gpuGrant)` → izin o devretmeye bağlanır; worker araçları (`gpu_recipes`, `gpu_submit`,
//! `gpu_job`, `gpu_fetch`) yalnız o devretme sürerken (queued/running) listede görünür. İzni
//! yalnız patron RPC'si oluşturur; MCP'de izin yaratan araç yoktur.
//!
//! Bütçe = üretilen görsel adedi; istek gönderilmeden ayrılır, hata ya da `cached: true` geri verir.
//! Bütçe yetmezken broker'a POST gitmez: aynı istek iznin erişiminde daha önce gönderilmiş ve
//! broker'da `done` ise o iş GET ile döner (bütçeden düşmez); değilse bütçe hatası.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db::with;
use crate::delegate::Caller;
use crate::Core;

pub const DEFAULT_PRIMARY: &str = "";
pub const DEFAULT_FALLBACK: &str = "";
pub const DEFAULT_BUDGET: i64 = 24;
/// Broker'ın günlük sınırı henüz okunmadıysa tek izin için geri dönüş (src/gpu/view.ts GPU_BUDGET_MAX ile aynı).
const MAX_BUDGET: i64 = 240;
/// Takım başına patron onayında bekleyen istek.
const MAX_PENDING: i64 = 5;
const CACHE: Duration = Duration::from_secs(30);
const MAX_OUTPUT: u64 = 64 << 20;
const SECRET: (&str, &str) = ("dex-gpu", "token");
const CONFIG: &str = "gpu_broker";
/// Son bakışın özeti (bağlı mı, ajan türleri): lider kuralları spawn'da ağa çıkmadan bunu okur.
const SEEN: &str = "gpu_seen";
const LIMITS: &str = "gpu_limits";

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_image_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pending_jobs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count_per_request: Option<i64>,
}

impl Limits {
    fn validate(&self) -> Result<(), String> {
        if self.daily_image_limit.is_some_and(|n| n < 1) || self.max_pending_jobs.is_some_and(|n| n < 1)
            || self.max_count_per_request.is_some_and(|n| !(1..=4).contains(&n)) {
            return Err("limitler pozitif tam sayı olmalı; istek başına en çok 4".into());
        }
        if self == &Self::default() {
            return Err("en az bir limit gerekli".into());
        }
        Ok(())
    }
}

fn parse_limits(v: &Value) -> Limits {
    let positive = |v: Option<&Value>| v.and_then(Value::as_i64).filter(|n| *n > 0);
    Limits {
        daily_image_limit: positive(v.pointer("/daily/limit")).or_else(|| positive(v.pointer("/limits/daily_image_limit"))),
        max_pending_jobs: positive(v.pointer("/limits/max_pending_jobs")),
        max_count_per_request: positive(v.pointer("/limits/max_count_per_request")).filter(|n| *n <= 4),
    }
}

// Broker sözleşmesi: PATCH /admin/limits, Bearer yönetici şifresi; düz snake_case
// alanlar + reason. `null` alanı config varsayılanına döndürür (Dex göndermez).
fn admin_limits(b: &Broker, token: &str, body: &Value) -> Result<(), String> {
    let admin = Broker { addrs: b.addrs, token: Some(token), last: b.last };
    admin.json_coded("PATCH", "/admin/limits", None, Some(body))
        .map(|_| ())
        .map_err(|(code, err)| {
            if matches!(code, Some(401 | 403)) {
                "yönetici şifresi yanlış".into()
            } else {
                b.scrub_str(&err.replace(token, "<gizli>"))
            }
        })
}

fn now() -> i64 {
    crate::model_news::now_ms()
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

// ---- ayar ----

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Addrs {
    pub primary: String,
    pub fallback: String,
}

impl Default for Addrs {
    fn default() -> Self {
        Addrs { primary: DEFAULT_PRIMARY.into(), fallback: DEFAULT_FALLBACK.into() }
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    pub primary: String,
    pub fallback: String,
    /// Token hiçbir zaman geri dönmez; yalnız kayıtlı mı.
    pub token_set: bool,
}

/// `gpu_config_set`: verilmeyen alan aynen kalır; `token: ""` token'ı, `fallback: ""` yedek adresi siler.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigPatch {
    pub primary: Option<String>,
    pub fallback: Option<String>,
    pub token: Option<String>,
}

fn setting<T: serde::de::DeserializeOwned>(conn: &Connection, key: &str) -> Option<T> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
}

fn put_setting(conn: &Connection, key: &str, v: &impl Serialize) -> Result<(), String> {
    let v = serde_json::to_string(v).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, v],
    )
    .map_err(e)?;
    Ok(())
}

pub fn addrs(conn: &Connection) -> Addrs {
    setting(conn, CONFIG).unwrap_or_default()
}

fn check_url(u: &str) -> Result<String, String> {
    let u = u.trim().trim_end_matches('/').to_string();
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("https://")).unwrap_or("");
    if rest.is_empty() || rest.contains(['/', ' ', '?', '#', '@']) {
        return Err("adres http(s)://host:port biçiminde olmalı (yol yok)".into());
    }
    Ok(u)
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Seen {
    pub connected: bool,
    pub types: Vec<String>,
    pub at: i64,
}

pub fn seen(conn: &Connection) -> Seen {
    setting(conn, SEEN).unwrap_or_default()
}

/// Lider kurallarına giden bölüm (yalnız bağlı ve ajan türü varken). Adres/token yok.
pub fn leader_rules(conn: &Connection) -> Option<String> {
    let s = seen(conn);
    (s.connected && !s.types.is_empty()).then(|| leader_text(&s.types.join(", ")))
}

/// [`leader_rules`]' text for `types`; the prompt-part list (Plan #114) shows it with a placeholder.
pub(crate) fn leader_text(types: &str) -> String {
    format!(
        "## GPU kaynağı\n\
         - Dex'e bağlı GPU kaynağı var; kaynak (ajana açık türler): {types}. Adres ve token sende yok; doğrudan kullanmak yasak.\n\
         - `gpu_status` diskteki, servis edilen (patronun) ve ajana açık modelleri ayrı gösterir. Ajana kapalı bir model işe yarayacaksa patrona açılmasını önerebilirsin.\n\
         - Bir iş gerektiriyorsa `gpu_request` (types, workerId, reason, budget?) ile patrondan iste; durum `gpu_status`. \
         Patron onaylarsa sonuçla gelen izin id'sini `delegate`'e `gpuGrant` olarak ekle; araçlar yalnız o işin worker'ına açılır."
    )
}

// ---- durum (Core'da) ----

#[derive(Default)]
pub struct State {
    /// `None` = sır deposundan henüz okunmadı.
    token: Mutex<Option<Option<String>>>,
    /// En son yanıt veren adres (0 birincil, 1 yedek): önce o denenir.
    last: AtomicUsize,
    overview: Mutex<Option<(Instant, Overview)>>,
    recipes: Mutex<Option<(Instant, Value)>>,
}

// ---- HTTP (ureq, quota.rs gibi) ----

pub struct Broker<'a> {
    pub addrs: &'a Addrs,
    pub token: Option<&'a str>,
    pub last: &'a AtomicUsize,
}

const LABEL: [&str; 2] = ["primary", "fallback"];

impl Broker<'_> {
    /// Önce son yanıt veren adres; ulaşılamazsa öteki. HTTP hata kodu da bir yanıttır (geçilmez).
    /// Hata metninde adres yok (ureq'in taşıma hatası URL içerir, kullanılmaz).
    fn send(&self, method: &str, path: &str, task: Option<&str>, body: Option<&Value>, timeout: Duration) -> Result<(usize, ureq::Response), String> {
        let first = self.last.load(Ordering::Relaxed).min(1);
        let bases = [&self.addrs.primary, &self.addrs.fallback];
        for i in [first, 1 - first] {
            if bases[i].is_empty() {
                continue; // yedek yok
            }
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(timeout).build();
            let mut req = agent.request(method, &format!("{}{path}", bases[i]));
            if let Some(t) = self.token.filter(|t| !t.is_empty()) {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            if let Some(t) = task {
                req = req.set("X-Dex-Task", t);
            }
            let res = match body {
                Some(b) => req.send_json(b.clone()),
                None => req.call(),
            };
            match res {
                Ok(r) | Err(ureq::Error::Status(_, r)) => {
                    self.last.store(i, Ordering::Relaxed);
                    return Ok((i, r));
                }
                Err(ureq::Error::Transport(_)) => continue,
            }
        }
        Err("GPU broker'a ulaşılamadı (iki adres de yanıt vermedi)".into())
    }

    /// JSON yanıt; 2xx değilse broker'ın `message`'ı aynen hata olur.
    fn json(&self, method: &str, path: &str, task: Option<&str>, body: Option<&Value>) -> Result<(usize, u16, Value), String> {
        self.json_coded(method, path, task, body).map_err(|(_, err)| err)
    }

    /// [`Self::json`] gibi; hatada HTTP kodu da döner (`None` = taşıma hatası / zaman aşımı).
    fn json_coded(&self, method: &str, path: &str, task: Option<&str>, body: Option<&Value>) -> Result<(usize, u16, Value), (Option<u16>, String)> {
        let (i, r) = self.send(method, path, task, body, Duration::from_secs(10)).map_err(|err| (None, err))?;
        let code = r.status();
        let body = r.into_string();
        if (200..300).contains(&code) {
            // #307: 2xx'te yarım/bozuk gövde Null'a yutulursa "bağlı, model yok" görünür; servis hatası olsun.
            // #309: boş gövde yalnız gövdesiz kodlarda (204/205) kabul; boş/boşluk 200 de ayrıştırılamaz sayılır.
            let parsed = body.map_err(|e| e.to_string()).and_then(|s| {
                if matches!(code, 204 | 205) && s.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&s).map_err(|e| e.to_string()) }
            });
            return match parsed {
                Ok(v) => Ok((i, code, v)),
                Err(e) => Err((Some(code), self.scrub_str(&format!("GPU broker yanıtı okunamadı (HTTP {code}): {e}")))),
            };
        }
        let v: Value = body.ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        Err((Some(code), self.error(code, &v)))
    }

    fn error(&self, code: u16, v: &Value) -> String {
        if code == 401 {
            return "GPU broker token'ı yok ya da geçersiz (patron Ayarlar'dan girmeli)".into();
        }
        let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
        let kind = v.get("error").and_then(Value::as_str).unwrap_or("hata");
        let text = if msg.is_empty() { format!("GPU broker HTTP {code} ({kind})") } else { format!("{kind}: {msg}") };
        self.scrub_str(&text)
    }

    fn secrets(&self) -> Vec<String> {
        let mut out = Vec::new();
        for a in [&self.addrs.primary, &self.addrs.fallback] {
            out.push(a.clone());
            let host = a.split("://").nth(1).unwrap_or(a);
            out.push(host.to_string());
            out.push(host.split(':').next().unwrap_or(host).to_string());
        }
        if let Some(t) = self.token {
            out.push(t.to_string());
        }
        out.retain(|s| s.len() >= 3);
        out.sort_by_key(|s| std::cmp::Reverse(s.len()));
        out
    }

    fn scrub_str(&self, s: &str) -> String {
        self.secrets().iter().fold(s.to_string(), |acc, x| acc.replace(x.as_str(), "<gizli>"))
    }

    /// Ajana giden her yanıt: `url` alanları düşer, adres/host/token geçen metinler maskelenir.
    pub fn scrub(&self, v: Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(m.into_iter().filter(|(k, _)| k != "url").map(|(k, v)| (k, self.scrub(v))).collect()),
            Value::Array(a) => Value::Array(a.into_iter().map(|v| self.scrub(v)).collect()),
            Value::String(s) => Value::String(self.scrub_str(&s)),
            v => v,
        }
    }
}

// ---- genel bakış ----

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub name: String,
    /// Servis edilende görevi (`t2i`, `llm`…), yalnız diskte olanda disk kategorisi (`image`, `tts`…).
    #[serde(rename = "type")]
    pub kind: String,
    pub engine: Option<String>,
    /// `/opt/ai-models` altındaki göreli yol.
    pub path: Option<String>,
    /// Ajan bir tarifle kullanabilir; izin ve tarif yalnız bunlar için.
    pub agent: bool,
    /// Makinede servis ediliyor (agent değilse patronun kullanımında).
    pub serve: bool,
    /// Dosyaları diskte.
    pub disk: bool,
    pub recipes: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatusSummary {
    /// GPU'nun o anki sahibi (`llm`, `broker`, …; broker'ın `gpu.owner`'ı aynen).
    pub owner: Option<String>,
    pub llm_model: Option<String>,
    pub queued: i64,
    pub running_job: Option<String>,
    pub daily_used: i64,
    pub daily_limit: i64,
}

/// `gpu_overview` (yalnız patron UI'sı; ajana `url` gitmez).
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub token_set: bool,
    /// `/health` yanıt verdi.
    pub connected: bool,
    /// Token'la `/v1/models` okundu.
    pub authorized: bool,
    /// Yanıt veren adres: "primary" | "fallback".
    pub via: Option<String>,
    pub url: Option<String>,
    pub models: Vec<Model>,
    pub status: Option<StatusSummary>,
    pub error: Option<String>,
    /// `error`'ın türü: "unreachable" (`/health` yok), "noToken", "auth" (401), "service"
    /// (broker yanıt verdi ama model/durum okunamadı: 5xx, zaman aşımı…).
    pub error_kind: Option<String>,
    pub checked_at: i64,
}

impl Overview {
    fn fail(&mut self, kind: &str, err: String) {
        self.error = Some(err);
        self.error_kind = Some(kind.into());
    }

    /// Ajan erişimi açık türler (sıralı, tekil).
    pub fn agent_types(&self) -> Vec<String> {
        let mut t: Vec<String> = self.models.iter().filter(|m| m.agent).map(|m| m.kind.clone()).collect();
        t.sort();
        t.dedup();
        t
    }
}

/// Eski şema (`agent_access`, #289) da okunur; eksik bayrak false.
fn parse_models(v: &Value) -> Vec<Model> {
    let s = |m: &Value, k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let o = |m: &Value, k: &str| m.get(k).and_then(Value::as_str).map(String::from);
    let f = |m: &Value, k: &str| m.get(k).and_then(Value::as_bool);
    v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|m| Model {
                    name: s(m, "name"),
                    kind: s(m, "type"),
                    engine: o(m, "engine"),
                    path: o(m, "path"),
                    agent: f(m, "agent").or_else(|| f(m, "agent_access")).unwrap_or(false),
                    serve: f(m, "serve").unwrap_or(false),
                    disk: f(m, "disk").unwrap_or(false),
                    recipes: m.get("recipes").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `gpu_status` grubunda dönen en çok örnek ad.
const STATUS_SAMPLE: usize = 8;

/// Kısa özet: diskteki onlarca kayıt lider bağlamını şişirmesin. `count` tam; adlardan en çok
/// [`STATUS_SAMPLE`] örnek, kalanı "+N daha". Tam liste yalnız `detail` ile.
fn status_group(models: &[Model], p: fn(&Model) -> bool) -> Value {
    let all: Vec<&Model> = models.iter().filter(|m| p(m)).collect();
    let mut ms: Vec<String> = all.iter().take(STATUS_SAMPLE).map(|m| format!("{} ({})", m.name, m.kind)).collect();
    if all.len() > STATUS_SAMPLE {
        ms.push(format!("+{} daha", all.len() - STATUS_SAMPLE));
    }
    json!({ "count": all.len(), "models": ms })
}

fn parse_status(v: &Value) -> StatusSummary {
    let st = |p: &str| v.pointer(p).and_then(Value::as_str).map(String::from);
    let n = |p: &str| v.pointer(p).and_then(Value::as_i64).unwrap_or(0);
    StatusSummary {
        owner: st("/gpu/owner"),
        llm_model: st("/gpu/llm_model"),
        queued: n("/queue/queued"),
        running_job: st("/broker/running_job"),
        daily_used: n("/daily/used"),
        daily_limit: n("/daily/limit"),
    }
}

pub fn fetch_overview(b: &Broker) -> Overview {
    let mut o = Overview { token_set: b.token.is_some_and(|t| !t.is_empty()), checked_at: now(), ..Default::default() };
    match b.json("GET", "/health", None, None) {
        Ok((i, _, _)) => {
            o.connected = true;
            o.via = Some(LABEL[i].into());
            o.url = Some([&b.addrs.primary, &b.addrs.fallback][i].clone());
        }
        Err(err) => {
            o.fail("unreachable", err);
            return o;
        }
    }
    if !o.token_set {
        o.fail("noToken", "token girilmedi".into());
        return o;
    }
    match b.json_coded("GET", "/v1/models", None, None) {
        Ok((_, _, v)) => {
            o.authorized = true;
            o.models = parse_models(&v);
        }
        Err((code, err)) => {
            o.fail(if code == Some(401) { "auth" } else { "service" }, err);
            return o;
        }
    }
    match b.json_coded("GET", "/v1/status", None, None) {
        Ok((_, _, v)) => o.status = Some(parse_status(&v)),
        Err((code, err)) => o.fail(if code == Some(401) { "auth" } else { "service" }, err),
    }
    o
}

// ---- istek / izin (patron kararı) ----

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: i64,
    pub team_id: String,
    pub leader: String,
    pub leader_name: String,
    pub worker: String,
    pub worker_name: String,
    pub types: Vec<String>,
    pub reason: String,
    /// Liderin istediği; onaylanınca patronun verdiği.
    pub budget: i64,
    /// pending | approved | rejected
    pub status: String,
    pub grant_id: Option<i64>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

const REQ_COLS: &str = "r.id, r.team_id, r.leader, IFNULL(l.name, r.leader), r.worker, IFNULL(w.name, r.worker), r.types, r.reason,
    r.budget, r.status, (SELECT g.id FROM gpu_grants g WHERE g.request_id = r.id), r.created_at, r.decided_at";
const REQ_FROM: &str = "gpu_requests r LEFT JOIN agents l ON l.id = r.leader LEFT JOIN agents w ON w.id = r.worker";

fn req_row(r: &rusqlite::Row) -> rusqlite::Result<Request> {
    let types: String = r.get(6)?;
    Ok(Request {
        id: r.get(0)?,
        team_id: r.get(1)?,
        leader: r.get(2)?,
        leader_name: r.get(3)?,
        worker: r.get(4)?,
        worker_name: r.get(5)?,
        types: serde_json::from_str(&types).unwrap_or_default(),
        reason: r.get(7)?,
        budget: r.get(8)?,
        status: r.get(9)?,
        grant_id: r.get(10)?,
        created_at: r.get(11)?,
        decided_at: r.get(12)?,
    })
}

pub fn get_request(conn: &Connection, id: i64) -> Result<Request, String> {
    conn.query_row(&format!("SELECT {REQ_COLS} FROM {REQ_FROM} WHERE r.id = ?1"), [id], req_row)
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("GPU isteği yok: #{id}"))
}

pub fn pending_requests(conn: &Connection) -> Result<Vec<Request>, String> {
    let mut st = conn.prepare(&format!("SELECT {REQ_COLS} FROM {REQ_FROM} WHERE r.status = 'pending' ORDER BY r.id")).map_err(e)?;
    let rows = st.query_map([], req_row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

fn check_budget(conn: &Connection, b: i64) -> Result<i64, String> {
    let max = setting::<Limits>(conn, LIMITS).and_then(|l| l.daily_image_limit).filter(|n| *n > 0).unwrap_or(MAX_BUDGET);
    if (1..=max).contains(&b) {
        Ok(b)
    } else {
        Err(format!("bütçe 1–{max} görsel olmalı"))
    }
}

/// Lider isteği. `available` = şu an ajan erişimi açık türler (bağlı değilse boş). `project`: MCP'nin
/// çözüp doğruladığı bağlam (#899) — tek-proje legacy takımda da kendi id'si; NULL yalnız projesiz takım.
pub fn request(conn: &Connection, lead: &Caller, args: &Value, available: &[String], project: Option<&str>) -> Result<Request, String> {
    if available.is_empty() {
        return Err("GPU kaynağı şu an bağlı değil ya da ajan türü yok; patrona sor (escalate)".into());
    }
    let mut types: Vec<String> = args
        .get("types")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    types.sort();
    types.dedup();
    if types.is_empty() {
        return Err(format!("types gerekli (liste); ajan türleri: {}", available.join(", ")));
    }
    if let Some(t) = types.iter().find(|t| !available.contains(t)) {
        return Err(format!("tür ajan erişimine açık değil: {t}. Açık türler: {}", available.join(", ")));
    }
    let key = args.get("workerId").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).ok_or("workerId gerekli")?;
    let worker: String = conn
        .query_row(
            "SELECT id FROM agents WHERE team_id = ?1 AND kind = 'worker' AND (id = ?2 OR lower(name) = lower(?2)) ORDER BY id = ?2 DESC LIMIT 1",
            params![lead.team_id, key],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("takımında böyle bir worker yok: {key}"))?;
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).ok_or("reason gerekli (patron bunu görecek)")?;
    let budget = check_budget(conn, args.get("budget").and_then(Value::as_i64).unwrap_or(DEFAULT_BUDGET))?;
    let pending: i64 = conn
        .query_row("SELECT COUNT(*) FROM gpu_requests WHERE team_id = ?1 AND status = 'pending'", [&lead.team_id], |r| r.get(0))
        .map_err(e)?;
    if pending >= MAX_PENDING {
        return Err(format!("takımın {pending} GPU isteği zaten patron onayında; önce onlar karara bağlansın"));
    }
    conn.execute(
        "INSERT INTO gpu_requests (team_id, leader, worker, types, reason, budget, created_at, project_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![lead.team_id, lead.id, worker, serde_json::to_string(&types).unwrap(), reason, budget, now(), project],
    )
    .map_err(e)?;
    get_request(conn, conn.last_insert_rowid())
}

/// Yalnız patron RPC'si çağırır: onay → izin satırı; her iki yolda lidere not.
pub fn decide(conn: &Connection, id: i64, approve: bool, budget: Option<i64>) -> Result<Request, String> {
    let r = get_request(conn, id)?;
    if r.status != "pending" {
        return Err(format!("GPU isteği #{id} zaten karara bağlandı ({})", r.status));
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    let ts = now();
    let text = if approve {
        let budget = check_budget(conn, budget.unwrap_or(r.budget))?;
        tx.execute("UPDATE gpu_requests SET status = 'approved', budget = ?2, decided_at = ?3 WHERE id = ?1", params![id, budget, ts]).map_err(e)?;
        // İzin isteğin projesinde doğar (#899); açılış backfill'inden sonra hâlâ NULL eski istek NULL kalır.
        tx.execute(
            "INSERT INTO gpu_grants (request_id, team_id, worker, types, budget, created_at, project_id)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, project_id FROM gpu_requests WHERE id = ?1",
            params![id, r.team_id, r.worker, serde_json::to_string(&r.types).unwrap(), budget, ts],
        )
        .map_err(e)?;
        format!(
            "GPU isteğin #{id} ONAYLANDI: izin #{} ({} için, türler {}, bütçe {budget} görsel). İşi `delegate` ile bu worker'a verirken `gpuGrant: {}` ekle; izin o işe bağlanır, iş bitince kapanır.",
            tx.last_insert_rowid(), r.worker_name, r.types.join(", "), tx.last_insert_rowid()
        )
    } else {
        tx.execute("UPDATE gpu_requests SET status = 'rejected', decided_at = ?2 WHERE id = ?1", params![id, ts]).map_err(e)?;
        format!("GPU isteğin #{id} ({}) patron tarafından REDDEDİLDİ; GPU kullanma, işi onsuz yürüt.", r.types.join(", "))
    };
    crate::plan::note(&tx, &r.leader, &text)?;
    tx.commit().map_err(e)?;
    get_request(conn, id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub id: i64,
    pub worker: String,
    pub types: Vec<String>,
    pub budget: i64,
    pub used: i64,
    pub delegation_id: Option<i64>,
}

fn grant(conn: &Connection, id: i64) -> Result<Option<(String, Grant)>, String> {
    conn.query_row("SELECT id, worker, types, budget, used, delegation_id, team_id FROM gpu_grants WHERE id = ?1", [id], |r| {
        let types: String = r.get(2)?;
        Ok((
            r.get::<_, String>(6)?,
            Grant { id: r.get(0)?, worker: r.get(1)?, types: serde_json::from_str(&types).unwrap_or_default(), budget: r.get(3)?, used: r.get(4)?, delegation_id: r.get(5)? },
        ))
    })
    .optional()
    .map_err(e)
}

/// İzin bağlanabilir: hiç bağlanmamış ya da bağlandığı iş onu hiç kullanmadan (used=0) düştü
/// (failed / raporsuz) — #316: sağlayıcı hatasıyla düşen işin izni boşa gitmez.
const BINDABLE: &str = "(delegation_id IS NULL OR (used = 0 AND delegation_id IN      (SELECT id FROM delegations WHERE status IN ('failed','done_no_report'))))";

/// `delegate(gpuGrant)` öncesi: patronun onayladığı, bu takımın, bu worker'ın, bağlanabilir izni.
pub fn check_grant(conn: &Connection, team: &str, id: i64, to: &str) -> Result<(), String> {
    let (g_team, g) = grant(conn, id)?.ok_or_else(|| format!("GPU izni yok: #{id} (izni yalnız patron onayı oluşturur; `gpu_request`)"))?;
    if g_team != team {
        return Err(format!("GPU izni #{id} senin takımının değil"));
    }
    if g.worker != to {
        return Err(format!("GPU izni #{id} başka bir worker için ({}); işi o worker'a ver ya da yeni izin iste", g.worker));
    }
    let bindable: bool =
        conn.query_row(&format!("SELECT {BINDABLE} FROM gpu_grants WHERE id = ?1"), [id], |r| r.get(0)).map_err(e)?;
    if let (false, Some(d)) = (bindable, g.delegation_id) {
        return Err(format!(
            "GPU izni #{id} zaten #{d} işine bağlandı; yeni iş için yeni izin iste (yalnız o iş izni hiç kullanmadan düştüyse — failed/raporsuz — aynı worker'ın yeni işine yeniden bağlanır)"
        ));
    }
    Ok(())
}

/// #899: izin ve iş aynı projede olmalı; farklı dolu proje `project_mismatch` (yeni patron izni
/// gerekir). NULL eski izin bağlandığı işin projesini alır — legacy NULL penceresi bağda kapanır.
const SAME_PROJECT: &str = "(project_id IS NULL OR project_id IS (SELECT project_id FROM delegations WHERE id = ?2))";
const TAKE_PROJECT: &str = "project_id = COALESCE(project_id, (SELECT project_id FROM delegations WHERE id = ?2))";

fn project_mismatch() -> String {
    crate::projects::ProjectError::Mismatch.to_string()
}

pub fn bind(conn: &Connection, id: i64, delegation: i64) -> Result<(), String> {
    let same: bool =
        conn.query_row(&format!("SELECT {SAME_PROJECT} FROM gpu_grants WHERE id = ?1"), params![id, delegation], |r| r.get(0)).map_err(e)?;
    if !same {
        return Err(project_mismatch());
    }
    conn.execute(&format!("UPDATE gpu_grants SET delegation_id = ?2, {TAKE_PROJECT} WHERE id = ?1 AND {BINDABLE}"), params![id, delegation])
        .map_err(e)?;
    Ok(())
}

/// `fixOf`: düzeltme işi aynı worker'daysa düzeltilen işin izni (kalan bütçesiyle) ona geçer —
/// yalnız aynı projede (#899).
pub fn inherit(conn: &Connection, fix_of: i64, delegation: i64, to: &str) -> Result<bool, String> {
    let foreign: bool = conn
        .query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM gpu_grants WHERE delegation_id = ?1 AND worker = ?3 AND NOT {SAME_PROJECT})"),
            params![fix_of, delegation, to],
            |r| r.get(0),
        )
        .map_err(e)?;
    if foreign {
        return Err(project_mismatch());
    }
    let n = conn
        .execute(&format!("UPDATE gpu_grants SET delegation_id = ?2, {TAKE_PROJECT} WHERE delegation_id = ?1 AND worker = ?3"), params![fix_of, delegation, to])
        .map_err(e)?;
    Ok(n > 0)
}

/// Worker'ın yetkili işi ve ona bağlı izin. İş bitince (done/failed…) izin kendiliğinden kapanır.
/// #899: iş, spawn anahtarının güvenilir launch bağından (`job`, sıradayken de); kuyruktaki sonraki
/// iş yetki vermez. Bağsız eski anahtar (`None`; G `key_for_job`'u bağlayınca yalnız idari yol)
/// yalnız çalışan işi görür.
pub fn active_grant(conn: &Connection, agent: &str, job: Option<i64>) -> Result<Option<(i64, Grant)>, String> {
    let current: Option<i64> = match job {
        Some(id) => conn.query_row(
            "SELECT id FROM delegations WHERE id = ?2 AND to_agent = ?1 AND status IN ('running','queued')",
            params![agent, id],
            |r| r.get(0),
        ),
        None => conn.query_row("SELECT id FROM delegations WHERE to_agent = ?1 AND status = 'running' ORDER BY id LIMIT 1", [agent], |r| r.get(0)),
    }
    .optional()
    .map_err(e)?;
    let Some(d) = current else { return Ok(None) };
    let id: Option<i64> = conn
        .query_row("SELECT id FROM gpu_grants WHERE delegation_id = ?1 AND worker = ?2", params![d, agent], |r| r.get(0))
        .optional()
        .map_err(e)?;
    match id {
        Some(id) => Ok(grant(conn, id)?.map(|(_, g)| (d, g))),
        None => Ok(None),
    }
}

/// İzin hâlâ bu işe bağlı ve iş sürüyor (running/queued). Çağrı izni `active_grant` ile aldıktan
/// sonra ağ beklerken iş bitip izin başka işe geçebilir (#320); bütçe ve POST bu kontrolün ardında.
fn still_bound(conn: &Connection, g: &Grant, delegation: i64) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM gpu_grants WHERE id = ?1 AND delegation_id = ?2)
            AND EXISTS (SELECT 1 FROM delegations WHERE id = ?2 AND status IN ('running','queued'))",
        params![g.id, delegation],
        |r| r.get(0),
    )
    .map_err(e)
}

/// Bütçeden ayır; yetmezse hata (lidere sor).
fn reserve(conn: &Connection, g: &Grant, n: i64) -> Result<(), String> {
    let ok = conn
        .execute("UPDATE gpu_grants SET used = used + ?2 WHERE id = ?1 AND used + ?2 <= budget", params![g.id, n])
        .map_err(e)?;
    if ok == 0 {
        return Err(budget_err(conn, g, n));
    }
    Ok(())
}

fn budget_err(conn: &Connection, g: &Grant, n: i64) -> String {
    match conn.query_row("SELECT budget, used FROM gpu_grants WHERE id = ?1", [g.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
        Ok((budget, used)) => format!("GPU bütçesi bitti ({used}/{budget} görsel, bu istek {n}); devam için lidere sor (ask_leader)"),
        Err(err) => err.to_string(),
    }
}

fn refund(conn: &Connection, g: &Grant, n: i64) {
    let _ = conn.execute("UPDATE gpu_grants SET used = MAX(0, used - ?2) WHERE id = ?1", params![g.id, n]);
}

/// Erişim (job_id, izin) ilişkisinden: broker önbelleği aynı job_id'yi başka izne de döndürebilir.
fn owns_job(conn: &Connection, g: &Grant, job: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM gpu_job_grants WHERE job_id = ?1 AND grant_id = ?2)", params![job, g.id], |r| r.get(0)).map_err(e)
}

/// Worker'ın gönderdiği tarif+parametrenin anahtarı (anahtarlar sıralı). Aynı ham istek broker'da
/// aynı normalize isteğe gider; tersi şart değil (yalnız ihtiyatlı tarafta yanılır).
fn req_key(recipe: &str, p: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut ks: Vec<&String> = m.keys().collect();
                ks.sort();
                Value::Array(ks.into_iter().map(|k| json!([k, canon(&m[k])])).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            v => v.clone(),
        }
    }
    sha256_hex(json!([recipe, canon(p)]).to_string().as_bytes())
}

/// Bütçe: önce ayır (`Ok(None)`); yetmezse aynı istek bu iznin erişimindeki bir işse onun
/// job_id'si (`Ok(Some)`) — broker'a gönderilmez, çağıran GET ile done mu bakar. Tek kilitte;
/// izin artık bu işin değilse ikisi de yok.
fn reserve_or_known(conn: &Connection, g: &Grant, delegation: i64, n: i64, key: &str) -> Result<Option<String>, String> {
    if !still_bound(conn, g, delegation)? {
        return Err(format!("GPU izni artık #{delegation} işinin değil (iş bitti ya da izin başka işe geçti); broker'a gönderilmedi"));
    }
    match reserve(conn, g, n) {
        Ok(()) => Ok(None),
        Err(err) => {
            let known: Option<String> = conn
                .query_row(
                    "SELECT k.job_id FROM gpu_request_keys k JOIN gpu_job_grants a ON a.job_id = k.job_id AND a.grant_id = ?2 WHERE k.req_key = ?1",
                    params![key, g.id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(e)?;
            known.map(Some).ok_or(err)
        }
    }
}

fn job_id(v: Option<&Value>) -> Result<String, String> {
    let id = v.and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("jobId geçersiz".into());
    }
    Ok(id.to_string())
}

/// Worker'ın yazabildiği kök (#899): işin bağlamı — canlı worktree (W), yoksa işin proje kökü
/// snapshot'ı (P); klasörsüz/servis işinde bağlamın çalışma alanı. Takım cwd'sine düşmez; bağ
/// çözülemezse (kayıp kök, yabancı iş) indirme yok.
fn work_root(conn: &Connection, data_dir: &Path, c: &Caller, delegation: i64) -> Result<PathBuf, String> {
    use crate::project_context::{resolve, ContextMode, ResolveRequest};
    let request =
        ResolveRequest { team_id: &c.team_id, agent_id: Some(&c.id), delegation_id: Some(delegation), project_id: None, mode: ContextMode::Selection };
    resolve(conn, data_dir, request).map(|ctx| ctx.cwd).map_err(|err| format!("işin çalışma klasörü çözülemedi ({err}); çıktı indirilemez"))
}

/// Symlink, junction ya da başka bir reparse point.
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

const LINK: &str = "path bir bağlantıdan (symlink/junction) geçemez";

/// Çıktı yolu. Metin kontrolü yetmez: ara klasörler kökten tek tek açılır, hiçbiri bağlantı
/// olamaz; ebeveynin gerçek yolu kökün gerçek yolu içinde kalmalı; hedef varsa bağlantı olamaz.
fn target(root: &Path, p: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let p = p.trim();
    if p.is_empty() {
        return Err("path gerekli (çalışma klasörüne göre, ör. assets/icon.png)".into());
    }
    let outside = || "path çalışma klasörünün içinde bir dosya olmalı".to_string();
    let full = if Path::new(p).is_absolute() { PathBuf::from(p) } else { root.join(p) };
    if !crate::delegate::path_within(&full, root) || full == root {
        return Err(outside());
    }
    let real_root = root.canonicalize().map_err(|err| format!("çalışma klasörü okunamadı: {err}"))?;
    let rel = full.strip_prefix(root).map_err(|_| outside())?;
    let parts: Vec<_> = rel.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    let Some((Component::Normal(file), dirs)) = parts.split_last() else { return Err(outside()) };
    let mut dir = root.to_path_buf();
    for c in dirs {
        let Component::Normal(c) = c else { return Err(outside()) };
        dir.push(c);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if is_link(&m) => return Err(LINK.into()),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(outside()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&dir).map_err(|err| format!("klasör açılamadı: {err}"))?,
            Err(err) => return Err(format!("klasör okunamadı: {err}")),
        }
    }
    if !dir.canonicalize().map_err(|err| format!("klasör okunamadı: {err}"))?.starts_with(&real_root) {
        return Err(outside());
    }
    let path = dir.join(file);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if is_link(&m) => Err(LINK.into()),
        Ok(m) if m.is_dir() => Err(outside()),
        _ => Ok(path),
    }
}

/// Hedefle aynı klasörde benzersiz geçici dosya (`create_new`: var olanı — bağlantı dahil — açmaz),
/// sonra yerine taşıma. Hata olursa yalnız kendi geçici dosyası silinir.
fn write_output(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = path.parent().ok_or("path bir dosya olmalı")?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}-{}-{}.dex-part", std::process::id(), now(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|err| format!("yazılamadı: {err}"))?;
    let res = f.write_all(bytes).and_then(|_| f.sync_all());
    drop(f);
    let res = res.map_err(|err| format!("yazılamadı: {err}")).and_then(|_| match std::fs::symlink_metadata(path) {
        Ok(m) if is_link(&m) => Err(LINK.to_string()),
        _ => std::fs::rename(&tmp, path).map_err(|err| format!("yazılamadı: {err}")),
    });
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

// ---- MCP araçları ----

fn lead_tools() -> Vec<Value> {
    vec![
        json!({ "name": "gpu_status", "description": "YALNIZ LİDER. Dex'e bağlı GPU kaynağı: modeller üç grupta (ajana açık / servis edilen / yalnız diskte) tam sayı ve en çok 8 örnek adla, kuyruk ve günlük kullanım. detail: true tam model listesini verir. Doğrudan kullanım yasak; iş gerekiyorsa gpu_request.",
          "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
              "detail": { "type": "boolean", "description": "Tam model listesi (ad, tür, motor, yol, tarifler, bayraklar)" } } } }),
        json!({ "name": "gpu_request", "description": "YALNIZ LİDER. Bir worker'ın işi için patrondan GPU izni iste (tür kümesi + bütçe = üretilecek görsel adedi, varsayılan 24). Karar bağlamına not olarak gelir; onaylanırsa izin id'sini delegate'e gpuGrant olarak ekle.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["types", "workerId", "reason"], "properties": {
              "types": { "type": "array", "items": { "type": "string" }, "description": "gpu_status'taki ajan türleri, ör. [\"t2i\",\"bg-removal\"]" },
              "workerId": { "type": "string", "description": "İşi yapacak worker (id ya da ad)" },
              "reason": { "type": "string", "description": "Patronun göreceği kısa gerekçe" },
              "budget": { "type": "integer", "description": "Görsel adedi (en çok broker günlük limiti)" } } } }),
    ]
}

fn worker_tools() -> Vec<Value> {
    vec![
        json!({ "name": "gpu_recipes", "description": "Bu işin GPU izninin türlerindeki tarifler, parametreleri, boyutlar ve kalan bütçe.",
          "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} } }),
        json!({ "name": "gpu_submit", "description": "GPU işi gönder (tarif + parametre). Yanıttaki message ve poll_after_s'e uy; waiting_owner'da yeniden gönderme. Bütçe üretilen görsel adedidir; önbellekten dönen (cached) düşmez.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["recipe", "params"], "properties": {
              "recipe": { "type": "string" }, "params": { "type": "object" } } } }),
        json!({ "name": "gpu_job", "description": "Gönderdiğin GPU işinin durumu (poll_after_s'ten sık sorma).",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["jobId"], "properties": { "jobId": { "type": "string" } } } }),
        json!({ "name": "gpu_fetch", "description": "Tamamlanan işin çıktısını çalışma klasörüne indirir (sha256 doğrulanır).",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["jobId", "index", "path"], "properties": {
              "jobId": { "type": "string" }, "index": { "type": "integer" },
              "path": { "type": "string", "description": "Çalışma klasörüne göre dosya yolu, ör. assets/icon.png" } } } }),
    ]
}

/// `tools/list`'e eklenecekler: lidere istek araçları, worker'a yalnız izinli işi sürerken.
pub fn tools_for(conn: &Connection, c: &Caller, job: Option<i64>) -> Vec<Value> {
    if c.is_leader() {
        return lead_tools();
    }
    match active_grant(conn, &c.id, job) {
        Ok(Some(_)) => worker_tools(),
        _ => Vec::new(),
    }
}

impl Core {
    fn gpu_token(&self) -> Option<String> {
        let mut t = self.gpu.token.lock().unwrap();
        if t.is_none() {
            // Testte sır deposuna (anahtarlık) gidilmez.
            *t = Some(if cfg!(test) { None } else { crate::secrets::get(SECRET.0, SECRET.1).ok().flatten() });
        }
        t.clone().flatten()
    }

    fn gpu_addrs(&self) -> Result<Addrs, String> {
        with(&self.db, |c| Ok(addrs(c)))
    }

    pub fn gpu_config_get(&self) -> Result<ConfigView, String> {
        let a = self.gpu_addrs()?;
        Ok(ConfigView { primary: a.primary, fallback: a.fallback, token_set: self.gpu_token().is_some() })
    }

    pub fn gpu_config_set(&self, p: ConfigPatch) -> Result<ConfigView, String> {
        let mut a = self.gpu_addrs()?;
        if let Some(u) = &p.primary {
            a.primary = check_url(u)?;
        }
        if let Some(u) = &p.fallback {
            a.fallback = if u.trim().is_empty() { String::new() } else { check_url(u)? };
        }
        if let Some(t) = &p.token {
            let t = t.trim();
            if t.contains(char::is_whitespace) {
                return Err("token boşluk içeremez".into());
            }
            if !cfg!(test) {
                if t.is_empty() { crate::secrets::delete(SECRET.0, SECRET.1)? } else { crate::secrets::set(SECRET.0, SECRET.1, t)? }
            }
            *self.gpu.token.lock().unwrap() = Some(Some(t.to_string()).filter(|t| !t.is_empty()));
        }
        with(&self.db, |c| {
            put_setting(c, CONFIG, &a)?;
            // Bağlantı değişti: lider kuralları bir sonraki bakışa kadar "bağlı" demesin.
            put_setting(c, SEEN, &Seen::default())?;
            put_setting(c, LIMITS, &Limits::default())
        })?;
        *self.gpu.overview.lock().unwrap() = None;
        *self.gpu.recipes.lock().unwrap() = None;
        self.gpu_refresh_later();
        self.gpu_config_get()
    }

    /// Önbellekli (30 sn); `refresh` önbelleği atlar. Sonuç lider kuralları için kaydedilir.
    pub fn gpu_overview(&self, refresh: bool) -> Result<Overview, String> {
        if !refresh {
            if let Some((at, o)) = self.gpu.overview.lock().unwrap().as_ref() {
                if at.elapsed() < CACHE {
                    return Ok(o.clone());
                }
            }
        }
        let a = self.gpu_addrs()?;
        let token = self.gpu_token();
        let o = fetch_overview(&Broker { addrs: &a, token: token.as_deref(), last: &self.gpu.last });
        let seen = Seen { connected: o.authorized, types: o.agent_types(), at: o.checked_at };
        with(&self.db, |c| {
            put_setting(c, SEEN, &seen)?;
            if let Some(status) = &o.status {
                if status.daily_limit > 0 {
                    let mut limits = setting::<Limits>(c, LIMITS).unwrap_or_default();
                    limits.daily_image_limit = Some(status.daily_limit);
                    put_setting(c, LIMITS, &limits)?;
                }
            }
            Ok(())
        })?;
        *self.gpu.overview.lock().unwrap() = Some((Instant::now(), o.clone()));
        Ok(o)
    }

    /// Yalnız arayüz RPC'si. Okuma normal broker token'ıyla; yönetici şifresi gerekmez.
    pub fn gpu_limits_get(&self) -> Result<Limits, String> {
        let a = self.gpu_addrs()?;
        let token = self.gpu_token();
        let b = Broker { addrs: &a, token: token.as_deref(), last: &self.gpu.last };
        let (_, _, v) = b.json("GET", "/v1/status", None, None)?;
        let limits = parse_limits(&v);
        with(&self.db, |c| put_setting(c, LIMITS, &limits))?;
        Ok(limits)
    }

    /// Şifre yalnız bu isteğin ömrü boyunca borç alınır; önbelleğe/DB'ye/olaya girmez.
    pub fn gpu_limits_set(&self, admin_token: &str, limits: Limits, reason: Option<&str>) -> Result<Limits, String> {
        if !(1..10).contains(&admin_token.chars().count()) || admin_token.contains(|c: char| c.is_whitespace() || c.is_control()) {
            return Err("yönetici şifresi 1–9 karakter olmalı, boşluk içeremez".into());
        }
        limits.validate()?;
        let a = self.gpu_addrs()?;
        let token = self.gpu_token();
        let b = Broker { addrs: &a, token: token.as_deref(), last: &self.gpu.last };
        let mut body = serde_json::to_value(&limits).map_err(|_| "limitler hazırlanamadı")?;
        if let Some(reason) = reason.filter(|s| !s.trim().is_empty()) {
            body["reason"] = json!(reason);
        }
        admin_limits(&b, admin_token, &body)?;
        *self.gpu.overview.lock().unwrap() = None;
        *self.gpu.recipes.lock().unwrap() = None;
        let limits = self.gpu_limits_get().map_err(|err| format!("Limitler kaydedildi; güncel değerler okunamadı: {}", err.replace(admin_token, "<gizli>")))?;
        let _ = self.events.emit("gpu-limits-changed", &limits);
        Ok(limits)
    }

    /// Açılışta ve ayar değişince bir bakış (UI'yı ve başlangıcı bloklamaz). Token yoksa ağa çıkmaz.
    pub(crate) fn gpu_refresh_later(&self) {
        if cfg!(test) || self.gpu_token().is_none() {
            return;
        }
        let weak = self.weak_self.clone();
        std::thread::spawn(move || {
            if let Some(core) = weak.upgrade() {
                let _ = core.gpu_overview(true);
            }
        });
    }

    pub fn gpu_request_list(&self) -> Result<Vec<Request>, String> {
        with(&self.db, pending_requests)
    }

    /// Patron kartı. İzni oluşturan TEK yol.
    pub fn gpu_request_decide(&self, id: i64, approve: bool, budget: Option<i64>) -> Result<Request, String> {
        let r = with(&self.db, |c| decide(c, id, approve, budget))?;
        let _ = self.events.emit("gpu-requests-changed", &r);
        self.leader_notes_wake(&r.leader);
        Ok(r)
    }

    fn gpu_recipes_cached(&self, b: &Broker) -> Result<Value, String> {
        if let Some((at, v)) = self.gpu.recipes.lock().unwrap().as_ref() {
            if at.elapsed() < CACHE {
                return Ok(v.clone());
            }
        }
        let (_, _, v) = b.json("GET", "/v1/recipes", None, None)?;
        *self.gpu.recipes.lock().unwrap() = Some((Instant::now(), v.clone()));
        Ok(v)
    }

    /// `gpu_request` için ajan erişimi açık türler. Broker'a ağ çağrısı olabilir: çağıran ne başlatma
    /// kapısını ne DB kilidini tutar (#882).
    pub(crate) fn gpu_agent_types(&self) -> Result<Vec<String>, String> {
        let o = self.gpu_overview(false)?;
        Ok(if o.authorized { o.agent_types() } else { Vec::new() })
    }

    /// Kaydedilmiş isteğin patron kartı olayı ve lidere yanıtı.
    pub(crate) fn gpu_request_created(&self, r: &Request) -> Value {
        let _ = self.events.emit("gpu-requests-changed", r);
        json!({ "requestId": r.id, "status": r.status, "types": r.types, "worker": r.worker, "budget": r.budget,
            "note": "Patron onayında; karar bağlamına not olarak gelir. Yoklama yapma." })
    }

    /// `gpu_*` MCP araçları (`gpu_request` hariç: MCP adaptörü proje bağlamıyla yazar). Yanıtlar
    /// [`Broker::scrub`]'tan geçer. `job`: spawn anahtarının güvenilir launch bağı.
    pub(crate) fn gpu_mcp(&self, c: &Caller, name: &str, args: &Value, job: Option<i64>) -> Result<Value, String> {
        let a = self.gpu_addrs()?;
        let token = self.gpu_token();
        let b = Broker { addrs: &a, token: token.as_deref(), last: &self.gpu.last };
        match name {
            "gpu_status" if !c.is_leader() => Err(format!("yetki yok: {name} yalnız takım liderine açık ({} bir worker)", c.name)),
            "gpu_status" => {
                let o = self.gpu_overview(false)?;
                let group = |p: fn(&Model) -> bool| status_group(&o.models, p);
                let mut v = json!({
                    "available": o.authorized,
                    "error": o.error,
                    "agentTypes": o.agent_types(),
                    "agent": group(|m| m.agent),
                    "served": group(|m| m.serve && !m.agent),
                    "diskOnly": group(|m| !m.serve && !m.agent),
                    "status": o.status,
                    "note": "Ajan yalnız agent grubundaki türleri kullanır; iş gerekiyorsa gpu_request ile patrondan iste. \
                             Servis edilen ya da yalnız diskteki bir model işe yarayacaksa patrona ajana açılmasını öner."
                });
                if args.get("detail").and_then(Value::as_bool) == Some(true) {
                    v["models"] = json!(o.models);
                }
                Ok(b.scrub(v))
            }
            "gpu_recipes" | "gpu_submit" | "gpu_job" | "gpu_fetch" => {
                let (delegation, g) = with(&self.db, |conn| active_grant(conn, &c.id, job))?
                    .ok_or_else(|| format!("bilinmeyen araç: {name}"))?;
                let out = self.gpu_worker(&b, c, delegation, &g, name, args);
                if let Ok(v) = &out {
                    self.gpu_waiting_notice(c, v);
                }
                out.map(|v| b.scrub(v)).map_err(|err| b.scrub_str(&err))
            }
            _ => Err(format!("bilinmeyen araç: {name}")),
        }
    }

    /// İş patronun LLM'i yüzünden bekliyorsa UI'ya haber (toast; aynı iş için UI bir kez gösterir).
    fn gpu_waiting_notice(&self, c: &Caller, v: &Value) {
        if v.get("status").and_then(Value::as_str) != Some("waiting_owner") {
            return;
        }
        let Some(job) = v.get("job_id").and_then(Value::as_str) else { return };
        let _ = self.events.emit("gpu-waiting", &json!({
            "agent": c.id, "agentName": c.name, "teamId": c.team_id, "jobId": job,
            "waitReason": v.get("wait_reason"), "etaS": v.get("eta_s").and_then(Value::as_i64),
        }));
    }

    fn gpu_worker(&self, b: &Broker, c: &Caller, delegation: i64, g: &Grant, name: &str, args: &Value) -> Result<Value, String> {
        let budget = |used: i64| json!({ "total": g.budget, "used": used, "left": g.budget - used });
        match name {
            "gpu_recipes" => {
                let v = self.gpu_recipes_cached(b)?;
                let recipes: serde_json::Map<String, Value> = v
                    .get("recipes")
                    .and_then(Value::as_object)
                    .map(|m| m.iter().filter(|(_, r)| r.get("type").and_then(Value::as_str).is_some_and(|t| g.types.iter().any(|x| x == t))).map(|(k, r)| (k.clone(), r.clone())).collect())
                    .unwrap_or_default();
                Ok(json!({ "types": g.types, "recipes": recipes, "sizes": v.get("sizes"), "limits": v.get("limits"), "budget": budget(g.used) }))
            }
            "gpu_submit" => {
                let recipe = args.get("recipe").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).ok_or("recipe gerekli")?;
                let p = args.get("params").cloned().unwrap_or(json!({}));
                let pm = p.as_object().ok_or("params bir nesne olmalı")?;
                let info = self.gpu_recipes_cached(b)?;
                let r = info.pointer(&format!("/recipes/{recipe}")).ok_or_else(|| format!("tarif yok: {recipe} (gpu_recipes)"))?;
                let kind = r.get("type").and_then(Value::as_str).unwrap_or("");
                if !g.types.iter().any(|t| t == kind) {
                    return Err(format!("'{recipe}' tarifi ({kind}) bu işin GPU izninde değil (izinli türler: {})", g.types.join(", ")));
                }
                for (k, v) in pm.iter().filter(|(k, _)| k.ends_with("_job_id")) {
                    let src = job_id(Some(v)).map_err(|_| format!("{k} geçersiz"))?;
                    if !with(&self.db, |conn| owns_job(conn, g, &src))? {
                        return Err(format!("{k}: kaynak iş bu GPU izniyle üretilmiş olmalı ({src} değil)"));
                    }
                }
                let takes_count = r.pointer("/params/count").is_some();
                let max = info.pointer("/limits/max_count_per_request").and_then(Value::as_i64).unwrap_or(4);
                let n = if takes_count { pm.get("count").and_then(Value::as_i64).unwrap_or(max).clamp(1, max.max(1)) } else { 1 };
                let key = req_key(recipe, &p);
                if let Some(known) = with(&self.db, |conn| reserve_or_known(conn, g, delegation, n, &key))? {
                    // Bütçe yetmiyor: broker'a POST yok (yeni üretim başlatabilir). Yalnız bitmiş bilinen iş döner.
                    let done = b.json("GET", &format!("/v1/jobs/{known}"), None, None).ok().map(|(_, _, v)| v).filter(|v| v.get("status").and_then(Value::as_str) == Some("done"));
                    let Some(mut v) = done else { return Err(with(&self.db, |conn| Ok(budget_err(conn, g, n)))?) };
                    let used = with(&self.db, |conn| conn.query_row("SELECT used FROM gpu_grants WHERE id = ?1", [g.id], |r| r.get::<_, i64>(0)).map_err(e))?;
                    v["cached"] = json!(true);
                    v["budget"] = budget(used);
                    return Ok(v);
                }
                let task = format!("{}/{delegation}", c.id);
                let res = b.json("POST", "/v1/jobs", Some(&task), Some(&json!({ "recipe": recipe, "params": p })));
                let (_, _, v) = match res {
                    Ok(x) => x,
                    Err(err) => {
                        with(&self.db, |conn| {
                            refund(conn, g, n);
                            Ok(())
                        })?;
                        return Err(err);
                    }
                };
                let cached = v.get("cached").and_then(Value::as_bool).unwrap_or(false);
                let job = job_id(v.get("job_id")).map_err(|_| "broker iş kimliği dönmedi".to_string())?;
                let used = with(&self.db, |conn| {
                    if cached {
                        refund(conn, g, n);
                    }
                    conn.execute(
                        "INSERT OR IGNORE INTO gpu_jobs (job_id, grant_id, delegation_id, recipe, images, cached, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![job, g.id, delegation, recipe, if cached { 0 } else { n }, cached, now()],
                    )
                    .map_err(e)?;
                    // Erişim yalnız bu iznin başarılı submit yanıtıyla.
                    conn.execute("INSERT OR IGNORE INTO gpu_job_grants (job_id, grant_id) VALUES (?1, ?2)", params![job, g.id]).map_err(e)?;
                    conn.execute(
                        "INSERT INTO gpu_request_keys (req_key, job_id, created_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(req_key) DO UPDATE SET job_id = excluded.job_id, created_at = excluded.created_at",
                        params![key, job, now()],
                    )
                    .map_err(e)?;
                    conn.query_row("SELECT used FROM gpu_grants WHERE id = ?1", [g.id], |r| r.get::<_, i64>(0)).map_err(e)
                })?;
                let mut v = v;
                v["budget"] = budget(used);
                Ok(v)
            }
            "gpu_job" => {
                let job = job_id(args.get("jobId"))?;
                if !with(&self.db, |conn| owns_job(conn, g, &job))? {
                    return Err(format!("{job} bu işin GPU izniyle gönderilmedi"));
                }
                Ok(b.json("GET", &format!("/v1/jobs/{job}"), None, None)?.2)
            }
            "gpu_fetch" => {
                let job = job_id(args.get("jobId"))?;
                let index = args.get("index").and_then(Value::as_i64).filter(|i| (0..64).contains(i)).ok_or("index gerekli (0..)")?;
                let (owned, root) = with(&self.db, |conn| Ok((owns_job(conn, g, &job)?, work_root(conn, &self.paths.data_dir, c, delegation)?)))?;
                if !owned {
                    return Err(format!("{job} bu işin GPU izniyle gönderilmedi"));
                }
                let (_, _, v) = b.json("GET", &format!("/v1/jobs/{job}"), None, None)?;
                if v.get("status").and_then(Value::as_str) != Some("done") {
                    return Err(format!("iş henüz bitmedi ({}); gpu_job ile poll_after_s aralığında bak", v.get("status").and_then(Value::as_str).unwrap_or("?")));
                }
                let out = v
                    .get("outputs")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find(|o| o.get("index").and_then(Value::as_i64) == Some(index)))
                    .ok_or_else(|| format!("çıktı yok: {index}"))?;
                let want = out.get("sha256").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
                let (_, r) = b.send("GET", &format!("/v1/jobs/{job}/outputs/{index}"), None, None, Duration::from_secs(60))?;
                if r.status() != 200 {
                    let code = r.status();
                    return Err(b.error(code, &r.into_json().unwrap_or(Value::Null)));
                }
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), MAX_OUTPUT), &mut bytes).map_err(|_| "çıktı okunamadı".to_string())?;
                let got = sha256_hex(&bytes);
                if want.is_empty() || got != want {
                    return Err("çıktının sha256'sı tutmadı; dosya yazılmadı, yeniden dene".into());
                }
                // Ağ beklemesi bitti: yol kontrolü, geçici dosya ve taşıma ardışık (arada indirme yok).
                let path = target(&root, args.get("path").and_then(Value::as_str).unwrap_or(""))?;
                write_output(&path, &bytes)?;
                Ok(json!({ "path": path.to_string_lossy(), "bytes": bytes.len(), "sha256": got, "width": out.get("width"), "height": out.get("height"), "mode": out.get("mode") }))
            }
            _ => Err(format!("bilinmeyen araç: {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Events, NullSink};
    use crate::paths::Paths;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    const TOKEN: &str = "sekret-tok-123";
    const PNG: &[u8] = b"\x89PNG-fake-bytes";

    /// Sahte broker: API.md/app.py biçimleri, adres `message` ve `url`'de geçer (maskelenmeli).
    /// Önbellek app.py gibi global: aynı tarif+parametre → aynı job_id, `cached: true` (izinden bağımsız).
    #[derive(Default)]
    struct Fake {
        admin: Mutex<Vec<(String, Value)>>,
        limits: Mutex<Limits>,
        hits: Mutex<Vec<String>>,
        jobs: Mutex<std::collections::HashMap<String, String>>,
        /// Broker'da düşmüş işler (yeniden kullanılmaz).
        failed: Mutex<std::collections::HashSet<String>>,
        /// Çıktı gövdesi gönderilmeden hemen önce (indirme sürerken) çalışır.
        on_output: Mutex<Option<Box<dyn Fn() + Send>>>,
        /// Tarif yanıtı gönderilmeden hemen önce çalışır (yanıtı bekletmek için).
        on_recipes: Mutex<Option<Box<dyn Fn() + Send>>>,
        /// Model listesi yanıtı gönderilmeden hemen önce çalışır (çağıranın kilitlerini yoklamak için).
        on_models: Mutex<Option<Box<dyn Fn() + Send>>>,
        /// Bu yollar (yetkiyle bile) 500 döner.
        broken: Mutex<Vec<String>>,
        /// Bu yol (yetkiyle) 200 + verilen gövdeyi döner; başlıktaki uzunluk gövdeden büyükse yarım kalır.
        mangled: Mutex<Option<(String, Vec<u8>, usize)>>,
        /// İşler patronun LLM'i yüzünden bekler (`waiting_owner`).
        waiting: std::sync::atomic::AtomicBool,
    }

    fn fake() -> (String, Arc<Fake>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let f: Arc<Fake> = Arc::default();
        let (b, h) = (base.clone(), f.clone());
        std::thread::spawn(move || {
            let mut n = 0;
            for s in l.incoming().flatten() {
                n += 1;
                serve(s, &b, &h, n);
            }
        });
        (base, f)
    }

    fn serve(mut s: std::net::TcpStream, base: &str, f: &Fake, n: i32) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let k = s.read(&mut chunk).unwrap_or(0);
            if k == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..k]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let line = lines.next().unwrap_or("").to_string();
        let (mut len, mut auth, mut task) = (0usize, String::new(), String::new());
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => len = v.trim().parse().unwrap_or(0),
                    "authorization" => auth = v.trim().into(),
                    "x-dex-task" => task = v.trim().into(),
                    _ => {}
                }
            }
        }
        let mut body = buf[head_end..].to_vec();
        while body.len() < len {
            let k = s.read(&mut chunk).unwrap_or(0);
            if k == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..k]);
        }
        let mut parts = line.split(' ');
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        f.hits.lock().unwrap().push(format!("{method} {path} task={task}"));
        let authed = auth == format!("Bearer {TOKEN}");
        if path == "/admin/limits" {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            f.admin.lock().unwrap().push((auth.clone(), v.clone()));
            let (code, out) = if auth == "Bearer 1234567" {
                *f.limits.lock().unwrap() = serde_json::from_value(json!({
                    "daily_image_limit": v["daily_image_limit"], "max_pending_jobs": v["max_pending_jobs"],
                    "max_count_per_request": v["max_count_per_request"]
                })).unwrap();
                (200, json!({"message": auth}))
            } else if auth == "Bearer noauth" {
                (401, json!({"message": auth}))
            } else if auth == "Bearer x" {
                (400, json!({"message": "invalid x", "error": "invalid"}))
            } else {
                (403, json!({"message": auth}))
            };
            return reply(&mut s, code, "application/json", out.to_string().as_bytes());
        }
        if let Some((_, out, declared)) = f.mangled.lock().unwrap().as_ref().filter(|m| authed && m.0 == path) {
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
            let _ = s.write_all(out);
            return;
        }
        if (method, path) == ("GET", "/v1/models") {
            if let Some(hook) = f.on_models.lock().unwrap().as_ref() {
                hook();
            }
        }
        let (code, ctype, out): (u16, &str, Vec<u8>) = match (method, path) {
            ("GET", "/health") => (200, "application/json", br#"{"status":"ok"}"#.to_vec()),
            ("GET", p) if f.broken.lock().unwrap().iter().any(|b| b == p) => (500, "application/json", br#"{"error":"internal","message":"vllm yanit vermedi"}"#.to_vec()),
            _ if !authed => (401, "application/json", br#"{"detail":"gecersiz ya da eksik token"}"#.to_vec()),
            ("GET", "/v1/models") => (200, "application/json", json!({ "models": [
                { "name": "FLUX.2-dev", "type": "t2i", "engine": "comfyui", "path": "image/flux2-dev", "recipes": ["flux_draft", "flux_final"], "agent": true, "serve": true, "disk": true },
                { "name": "RMBG-2.0", "type": "bg-removal", "engine": "comfyui", "path": "image/rmbg-2", "recipes": ["rmbg"], "agent": true, "serve": true, "disk": true },
                { "name": "qwen3-coder-next", "type": "llm", "engine": "vllm", "path": "llm/Qwen3-Coder-Next", "recipes": [], "agent": false, "serve": true, "disk": true },
                { "name": "Qwen-Image", "type": "image", "engine": null, "path": "image/Qwen-Image", "recipes": [], "agent": false, "serve": false, "disk": true } ] }).to_string().into_bytes()),
            ("GET", "/v1/status") => {
                let limits = f.limits.lock().unwrap().clone();
                (200, "application/json", json!({ "gpu": { "owner": "llm", "llm_model": "qwen3-coder-next" },
                "broker": { "running_job": null }, "queue": { "queued": 2, "pending": [] },
                "daily": { "used": 7, "limit": limits.daily_image_limit.unwrap_or(120) },
                "limits": limits }).to_string().into_bytes())
            },
            ("GET", "/v1/recipes") => {
                if let Some(hook) = f.on_recipes.lock().unwrap().as_ref() {
                    hook();
                }
                (200, "application/json", json!({ "recipes": {
                "flux_draft": { "params": { "prompt": "str", "count": "1..4" }, "model": "FLUX.2-dev", "type": "t2i" },
                "flux_final": { "params": { "draft_job_id": "str", "index": "int" }, "model": "FLUX.2-dev", "type": "t2i" },
                "rmbg": { "params": { "source_job_id": "str", "index": "int" }, "model": "RMBG-2.0", "type": "bg-removal" } },
                "sizes": { "square": [1024, 1024] }, "limits": { "max_count_per_request": 4, "daily_image_limit": 120 } }).to_string().into_bytes())
            }
            ("POST", "/v1/jobs") => {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if v.pointer("/params/prompt").and_then(Value::as_str) == Some("bozuk") {
                    (400, "application/json", br#"{"error":"invalid_params","message":"'count' 1..4 araliginda olmali"}"#.to_vec())
                } else {
                    let key = v.to_string();
                    let mut jobs = f.jobs.lock().unwrap();
                    let reuse = jobs.get(&key).filter(|j| !f.failed.lock().unwrap().contains(*j)).cloned();
                    let cached = reuse.is_some();
                    let id = reuse.unwrap_or_else(|| format!("j_{n}"));
                    jobs.insert(key, id.clone());
                    if f.waiting.load(Ordering::SeqCst) {
                        return reply(&mut s, 202, "application/json", &waiting_owner(&id));
                    }
                    (if cached { 200 } else { 202 }, "application/json", json!({ "job_id": id, "status": "queued", "cached": cached, "poll_after_s": 30,
                        "message": format!("Sirada 1. ({base} uzerinden)") }).to_string().into_bytes())
                }
            }
            ("DELETE", p) if p.starts_with("/v1/jobs/") => (200, "application/json", json!({ "job_id": &p["/v1/jobs/".len()..], "status": "canceled" }).to_string().into_bytes()),
            ("GET", p) if p.starts_with("/v1/jobs/") && p.ends_with("/outputs/0") => {
                if let Some(hook) = f.on_output.lock().unwrap().as_ref() {
                    hook();
                }
                (200, "image/png", PNG.to_vec())
            }
            ("GET", p) if p.starts_with("/v1/jobs/") && f.waiting.load(Ordering::SeqCst) => (200, "application/json", waiting_owner(&p["/v1/jobs/".len()..])),
            ("GET", p) if p.starts_with("/v1/jobs/") => {
                let id = &p["/v1/jobs/".len()..];
                let status = if f.failed.lock().unwrap().contains(id) { "failed" } else { "done" };
                (200, "application/json", json!({ "job_id": id, "status": status, "poll_after_s": 5, "message": "Tamamlandi.",
                    "outputs": [{ "index": 0, "url": format!("{base}/v1/jobs/{id}/outputs/0"), "sha256": sha256_hex(PNG), "width": 1024, "height": 1024, "mode": "RGB", "bytes": PNG.len() }] }).to_string().into_bytes())
            }
            _ => (404, "application/json", br#"{"error":"not_found","message":"yok"}"#.to_vec()),
        };
        reply(&mut s, code, ctype, &out);
    }

    fn reply(s: &mut std::net::TcpStream, code: u16, ctype: &str, out: &[u8]) {
        let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
        let _ = s.write_all(out);
    }

    /// API.md'deki örnek `waiting_owner` yanıtı.
    fn waiting_owner(id: &str) -> Vec<u8> {
        json!({ "job_id": id, "status": "waiting_owner", "wait_reason": "owner_llm_active", "queue_position": 1, "eta_s": 365, "poll_after_s": 60,
            "gpu": { "owner": "llm", "llm_model": "qwen3-coder-next", "llm_idle_s": 40 }, "message": "GPU su an patronun LLM oturumunda." })
        .to_string()
        .into_bytes()
    }

    /// Kapalı port: bağlantı hemen reddedilir (birincil düştü).
    fn dead() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        a
    }

    fn core(tag: &str) -> (Arc<Core>, PathBuf) {
        core_with(tag, Events::new(NullSink))
    }

    /// Yayınlanan olayları toplar.
    struct Got(Arc<Mutex<Vec<(String, Value)>>>);
    impl crate::EventSink for Got {
        fn emit(&self, event: &str, data: Value) -> Result<(), String> {
            self.0.lock().unwrap().push((event.into(), data));
            Ok(())
        }
    }

    fn core_with(tag: &str, events: Events) -> (Arc<Core>, PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!(
            "dex-gpu-{tag}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = PathBuf::from(crate::teams::normalize_dir(root.to_str().unwrap()).unwrap());
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, events).unwrap();
        std::fs::create_dir_all(root.join("proj")).unwrap();
        // Isolate Git discovery from the enclosing checkout, just like projects::tests::Fixture.
        assert!(std::process::Command::new("git").arg("-C").arg(root.join("proj"))
            .args(["init", "--quiet", "--initial-branch=fixture-main"]).output().unwrap().status.success());
        // #899: işin W/P kökü projeden gelir; ham takım cwd'si legacy projeyi klasörsüz bırakırdı.
        let proj = crate::teams::normalize_dir(root.join("proj").to_str().unwrap()).unwrap();
        with(&core.db, |c| {
            c.execute("UPDATE teams SET cwd = ?1, worktrees = 0 WHERE id = 'default'", [&proj]).map_err(e)?;
            c.execute("UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'", params![proj, crate::projects::path_key(&proj)]).map_err(e)
        })
        .unwrap();
        (core, root)
    }

    fn configure(core: &Core, primary: &str, fallback: &str) {
        core.gpu_config_set(ConfigPatch { primary: Some(primary.into()), fallback: Some(fallback.into()), token: Some(TOKEN.into()) }).unwrap();
    }

    fn mcp(core: &Core, agent: &str, tool: &str, args: Value) -> Value {
        crate::mcp::call_with_args_for_test(core, agent, tool, args)
    }

    fn text(v: &Value) -> String {
        v["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    fn listed(core: &Core, agent: &str) -> Vec<String> {
        crate::mcp::tools_list_for_test(core, agent).iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()
    }

    /// Lider ister → patron onaylar → delegate(gpuGrant) → worker araçları; ayrıntılar alt adımlarda.
    fn approved_job(core: &Core, budget: Option<i64>) -> (i64, i64) {
        approved_job_for(core, "ayla", budget)
    }

    fn approved_job_for(core: &Core, worker: &str, budget: Option<i64>) -> (i64, i64) {
        let r = mcp(core, "timur", "gpu_request", json!({ "types": ["t2i"], "workerId": worker, "reason": "ikon taslakları" }));
        assert!(r.get("isError").is_none(), "{r}");
        let id = r["structuredContent"]["requestId"].as_i64().unwrap();
        let g = core.gpu_request_decide(id, true, budget).unwrap().grant_id.unwrap();
        let d = with(&core.db, |c| {
            let from = crate::delegate::caller(c, "timur")?;
            check_grant(c, "default", g, worker)?;
            let d = crate::delegate::insert(c, crate::delegate::NewDelegation { from: &from, to_agent: worker.into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None })?;
            bind(c, g, d.id)?;
            // Bağsız (eski) anahtarla `mcp` yalnız çalışan işi görür (#899).
            crate::delegate::set_status(c, d.id, "running")?;
            Ok(d.id)
        })
        .unwrap();
        (g, d)
    }

    #[test]
    fn gpu_request_requires_the_project_repository_root() {
        let (core, root) = core("repo-root");
        let (base, broker) = fake();
        configure(&core, &base, &dead());
        let request = request_in(&core, "ayla", None).unwrap();
        let grant = core.gpu_request_decide(request, true, Some(4)).unwrap().grant_id.unwrap();
        let job = job_in(&core, "ayla", "legacy-default", true);
        with(&core.db, |c| bind(c, grant, job)).unwrap();
        assert_eq!(grant_project(&core, grant), (Some("legacy-default".into()), Some(job)));

        // An existing repository's subdirectory is not a project root. The rejection must
        // precede both the broker call and any request/grant insertion.
        let nested = root.join("proj/nested");
        std::fs::create_dir(&nested).unwrap();
        let nested = nested.to_string_lossy().into_owned();
        with(&core.db, |c| {
            c.execute("UPDATE projects SET root_path = ?1, root_key = ?2 WHERE id = 'legacy-default'",
                params![nested, crate::projects::path_key(&nested)]).map_err(e)
        }).unwrap();
        let hits = broker.hits.lock().unwrap().len();
        assert_eq!(request_in(&core, "ayla", None).unwrap_err(), "project_mismatch");
        assert_eq!(broker.hits.lock().unwrap().len(), hits);
        let counts: (i64, i64) = with(&core.db, |c| c.query_row(
            "SELECT (SELECT COUNT(*) FROM gpu_requests), (SELECT COUNT(*) FROM gpu_grants)",
            [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)).unwrap();
        assert_eq!(counts, (1, 1));
        assert_eq!(grant_project(&core, grant), (Some("legacy-default".into()), Some(job)));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unconfigured_broker_has_no_addresses_even_with_token() {
        let (core, root) = core("unconfigured");
        let v = core.gpu_config_get().unwrap();
        assert_eq!((v.primary.as_str(), v.fallback.as_str()), ("", ""));
        core.gpu_config_set(ConfigPatch { token: Some(TOKEN.into()), ..Default::default() }).unwrap();
        let o = core.gpu_overview(true).unwrap();
        assert!(!o.connected && !o.authorized && o.url.is_none());
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saved_broker_survives_two_database_reopens() {
        let (base, fake) = fake();
        let (core, root) = core("saved-broker");
        configure(&core, &base, "http://fallback.example.invalid:8100");
        let stored = with(&core.db, |c| c.query_row("SELECT value FROM settings WHERE key = ?1", [CONFIG], |r| r.get::<_, String>(0)).map_err(e)).unwrap();
        let counts = |c: &Connection| c.query_row(
            "SELECT (SELECT COUNT(*) FROM settings), (SELECT COUNT(*) FROM teams), (SELECT COUNT(*) FROM agents)",
            [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        ).unwrap();
        let before = with(&core.db, |c| Ok(counts(c))).unwrap();
        core.shutdown();
        drop(core);
        for _ in 0..2 {
            let conn = crate::db::open(&root.join("data/dex.db")).unwrap();
            assert_eq!(counts(&conn), before);
            let value: String = conn.query_row("SELECT value FROM settings WHERE key = ?1", [CONFIG], |r| r.get(0)).unwrap();
            assert_eq!(value, stored);
            let a = addrs(&conn);
            assert_eq!(a.primary, base);
            assert_eq!(a.fallback, "http://fallback.example.invalid:8100");
            let overview = fetch_overview(&Broker { addrs: &a, token: Some(TOKEN), last: &AtomicUsize::new(0) });
            assert!(overview.connected && overview.authorized);
        }
        assert_eq!(fake.hits.lock().unwrap().iter().filter(|p| p.starts_with("GET /v1/models task=")).count(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn config_hides_token_and_validates_urls() {
        let (core, root) = core("cfg");
        let v = core.gpu_config_get().unwrap();
        assert_eq!((v.primary.as_str(), v.fallback.as_str(), v.token_set), (DEFAULT_PRIMARY, DEFAULT_FALLBACK, false));
        let v = core.gpu_config_set(ConfigPatch { token: Some(TOKEN.into()), ..Default::default() }).unwrap();
        assert!(v.token_set && !serde_json::to_string(&v).unwrap().contains(TOKEN));
        assert!(core.gpu_config_set(ConfigPatch { primary: Some("ftp://x".into()), ..Default::default() }).is_err());
        assert!(core.gpu_config_set(ConfigPatch { primary: Some("http://h:1/yol".into()), ..Default::default() }).is_err());
        let v = core.gpu_config_set(ConfigPatch { primary: Some("http://h:1/".into()), token: Some(String::new()), ..Default::default() }).unwrap();
        assert_eq!((v.primary.as_str(), v.token_set), ("http://h:1", false));
        let stored: String = with(&core.db, |c| c.query_row("SELECT group_concat(value) FROM settings", [], |r| r.get(0)).map_err(e)).unwrap();
        assert!(!stored.contains(TOKEN), "token DB'ye yazılmaz");
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_groups_keep_the_count_and_cut_names_to_eight() {
        // 59 kayıt: 3 ajana açık, 10 servis, 46 yalnız disk.
        let raw: Vec<Value> = (0..59)
            .map(|i| json!({ "name": format!("m{i:02}"), "type": "image", "agent": i < 3, "serve": i < 13, "disk": true }))
            .collect();
        let ms = parse_models(&json!({ "models": raw }));
        assert_eq!(ms.len(), 59);
        let agent = status_group(&ms, |m| m.agent);
        assert_eq!(agent, json!({ "count": 3, "models": ["m00 (image)", "m01 (image)", "m02 (image)"] }));
        let served = status_group(&ms, |m| m.serve && !m.agent);
        assert_eq!(served["count"], 10);
        let sm = served["models"].as_array().unwrap();
        assert_eq!((sm.len(), sm[0].as_str(), sm[8].as_str()), (9, Some("m03 (image)"), Some("+2 daha")));
        let disk = status_group(&ms, |m| !m.serve && !m.agent);
        assert_eq!(disk["count"], 46);
        let dm = disk["models"].as_array().unwrap();
        assert_eq!((dm.len(), dm[7].as_str(), dm[8].as_str()), (9, Some("m20 (image)"), Some("+38 daha")));
        // Tam sınırda kısaltma yok.
        let eight = status_group(&ms[..8], |_| true);
        assert_eq!((eight["count"].as_u64(), eight["models"].as_array().map(Vec::len)), (Some(8), Some(8)));
    }

    #[test]
    fn parse_accepts_the_old_agent_access_schema() {
        let ms = parse_models(&json!({ "models": [
            { "name": "FLUX.2-dev", "type": "t2i", "engine": "comfyui", "recipes": ["flux_draft"], "agent_access": true },
            { "name": "qwen3", "type": "llm", "engine": "vllm", "recipes": [], "agent_access": false },
            { "name": "x", "type": "t2i" },
            { "name": "y", "type": "t2i", "agent": false, "agent_access": true } ] }));
        let flags: Vec<_> = ms.iter().map(|m| (m.name.as_str(), m.agent, m.serve, m.disk, m.path.clone())).collect();
        assert_eq!(flags, vec![("FLUX.2-dev", true, false, false, None), ("qwen3", false, false, false, None), ("x", false, false, false, None), ("y", false, false, false, None)]);
        assert_eq!(ms[0].engine.as_deref(), Some("comfyui"));
    }

    #[test]
    fn overview_falls_back_to_the_second_address_and_feeds_leader_rules() {
        let (core, root) = core("ov");
        let (base, _) = fake();
        configure(&core, &dead(), &base);
        let o = core.gpu_overview(true).unwrap();
        assert!(o.connected && o.authorized, "{o:?}");
        assert_eq!((o.via.as_deref(), o.url.as_deref()), (Some("fallback"), Some(base.as_str())));
        assert_eq!(o.agent_types(), vec!["bg-removal".to_string(), "t2i".into()]);
        assert_eq!(o.status.as_ref().map(|s| (s.owner.as_deref(), s.queued, s.daily_used, s.daily_limit)), Some((Some("llm"), 2, 7, 120)));
        let rules = with(&core.db, |c| Ok(crate::rules::system_prompt(c, "timur", None))).unwrap();
        assert!(rules.contains("## GPU kaynağı") && rules.contains("bg-removal, t2i") && rules.contains("`gpu_request`"), "{rules}");
        assert!(!rules.contains(&base) && !rules.contains("127.0.0.1"), "{rules}");
        let gr = with(&core.db, |c| Ok(leader_rules(c))).unwrap().unwrap();
        assert!(gr.contains("patrona açılmasını önerebilirsin") && !gr.contains("llm") && !gr.contains("image"), "{gr}");
        // UI: her modelde ad, tür, motor, yol ve üç bayrak.
        let qi = o.models.iter().find(|m| m.name == "Qwen-Image").unwrap();
        assert_eq!((qi.kind.as_str(), qi.engine.as_deref(), qi.path.as_deref(), qi.agent, qi.serve, qi.disk), ("image", None, Some("image/Qwen-Image"), false, false, true));
        let ui = serde_json::to_value(&o.models[0]).unwrap();
        assert_eq!(ui, json!({ "name": "FLUX.2-dev", "type": "t2i", "engine": "comfyui", "path": "image/flux2-dev", "agent": true, "serve": true, "disk": true, "recipes": ["flux_draft", "flux_final"] }));
        // Lider: üç grup kısa; tam liste yalnız detail ile; adres yok.
        let s = mcp(&core, "timur", "gpu_status", json!({}));
        let sc = &s["structuredContent"];
        assert_eq!(sc["agent"], json!({ "count": 2, "models": ["FLUX.2-dev (t2i)", "RMBG-2.0 (bg-removal)"] }), "{s}");
        assert_eq!(sc["served"], json!({ "count": 1, "models": ["qwen3-coder-next (llm)"] }));
        assert_eq!(sc["diskOnly"], json!({ "count": 1, "models": ["Qwen-Image (image)"] }));
        assert_eq!(sc["agentTypes"], json!(["bg-removal", "t2i"]));
        assert!(sc.get("models").is_none() && !s.to_string().contains("image/flux2-dev"), "{s}");
        let d = mcp(&core, "timur", "gpu_status", json!({ "detail": true }));
        assert_eq!(d["structuredContent"]["models"].as_array().map(Vec::len), Some(4), "{d}");
        assert!(!s.to_string().contains(&base) && !d.to_string().contains(&base) && !d.to_string().contains(TOKEN));
        // Ajana kapalı tür istenemez.
        assert!(text(&mcp(&core, "timur", "gpu_request", json!({ "types": ["image"], "workerId": "ayla", "reason": "x" }))).contains("ajan erişimine açık değil"));
        let worker = with(&core.db, |c| Ok(crate::rules::system_prompt(c, "ayla", None))).unwrap();
        assert!(!worker.contains("GPU") && !worker.contains("gpu_"), "worker kurallarında GPU geçmez\n{worker}");

        // Token yok → /health var ama yetkisiz; lider bölümü düşer.
        core.gpu_config_set(ConfigPatch { token: Some(String::new()), ..Default::default() }).unwrap();
        let o = core.gpu_overview(true).unwrap();
        assert!(o.connected && !o.authorized && !o.token_set);
        assert!(!with(&core.db, |c| Ok(crate::rules::system_prompt(c, "timur", None))).unwrap().contains("GPU kaynağı"));
        // Yanlış token → 401 metni, adres yok.
        core.gpu_config_set(ConfigPatch { token: Some("yanlis".into()), ..Default::default() }).unwrap();
        let o = core.gpu_overview(true).unwrap();
        assert!(!o.authorized && o.error.as_deref().unwrap().contains("token"), "{o:?}");
        assert_eq!(o.error_kind.as_deref(), Some("auth"));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_the_patron_creates_a_grant_and_tools_follow_it() {
        let (core, root) = core("grant");
        let (base, _) = fake();
        configure(&core, &base, &dead());
        // Lider araçları liderde; worker'da GPU aracı yok.
        assert!(listed(&core, "timur").contains(&"gpu_request".to_string()));
        assert!(listed(&core, "ayla").iter().all(|n| !n.starts_with("gpu_")));
        // Worker istek yapamaz; lider izin yaratamaz (karar aracı MCP'de yok, uydurma izin reddedilir).
        assert!(text(&mcp(&core, "ayla", "gpu_request", json!({ "types": ["t2i"], "workerId": "ayla", "reason": "x" }))).contains("yalnız takım liderine"));
        assert!(text(&mcp(&core, "timur", "gpu_request_decide", json!({ "id": 1, "approve": true }))).contains("bilinmeyen araç"));
        assert!(text(&mcp(&core, "timur", "gpu_request", json!({ "types": ["llm"], "workerId": "ayla", "reason": "x" }))).contains("ajan erişimine açık değil"));
        let d = mcp(&core, "timur", "delegate", json!({ "agentId": "ayla", "prompt": "x", "gpuGrant": 1 }));
        assert!(text(&d).contains("GPU izni yok"), "{d}");

        let r = mcp(&core, "timur", "gpu_request", json!({ "types": ["t2i"], "workerId": "Ayla", "reason": "ikon" }));
        let id = r["structuredContent"]["requestId"].as_i64().unwrap();
        assert_eq!(core.gpu_request_list().unwrap()[0].types, vec!["t2i".to_string()]);
        let req = core.gpu_request_decide(id, true, Some(8)).unwrap();
        assert_eq!((req.status.as_str(), req.budget), ("approved", 8));
        assert!(core.gpu_request_decide(id, false, None).is_err(), "bir kez karar");
        let note = with(&core.db, |c| crate::plan::take_notes(c, "timur")).unwrap().unwrap();
        assert!(note.contains("ONAYLANDI") && note.contains(&format!("gpuGrant: {}", req.grant_id.unwrap())), "{note}");
        // Başka worker'a bağlanamaz.
        let g = req.grant_id.unwrap();
        assert!(with(&core.db, |c| check_grant(c, "default", g, "kaan")).unwrap_err().contains("başka bir worker"));

        // İzin bağlanınca araçlar yalnız o işin worker'ında; iş bitince kaybolur.
        let d = with(&core.db, |c| {
            let from = crate::delegate::caller(c, "timur")?;
            crate::delegate::insert(c, crate::delegate::NewDelegation { from: &from, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None })
        })
        .unwrap();
        assert!(listed(&core, "ayla").iter().all(|n| !n.starts_with("gpu_")), "bağlanmadan araç yok");
        with(&core.db, |c| bind(c, g, d.id)).unwrap();
        assert!(with(&core.db, |c| check_grant(c, "default", g, "ayla")).unwrap_err().contains("zaten"));
        with(&core.db, |c| crate::delegate::set_status(c, d.id, "running")).unwrap();
        let names = listed(&core, "ayla");
        assert!(["gpu_recipes", "gpu_submit", "gpu_job", "gpu_fetch"].iter().all(|t| names.contains(&t.to_string())), "{names:?}");
        assert!(listed(&core, "kaan").iter().all(|n| !n.starts_with("gpu_")));
        with(&core.db, |c| crate::delegate::set_status(c, d.id, "done")).unwrap();
        assert!(listed(&core, "ayla").iter().all(|n| !n.starts_with("gpu_")), "iş bitti → izin kapandı");
        assert!(text(&mcp(&core, "ayla", "gpu_recipes", json!({}))).contains("bilinmeyen araç"));
        // fixOf: kalan bütçe aynı worker'ın düzeltme işine geçer.
        let fix = with(&core.db, |c| {
            let from = crate::delegate::caller(c, "timur")?;
            let f = crate::delegate::insert(c, crate::delegate::NewDelegation { from: &from, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "fix".into(), task_id: None, patron_words: None, effort: None })?;
            assert!(!inherit(c, d.id, f.id, "kaan")?);
            assert!(inherit(c, d.id, f.id, "ayla")?);
            active_grant(c, "ayla", Some(f.id))
        })
        .unwrap();
        assert_eq!(fix.map(|(_, g)| g.id), Some(g));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// #316: bağlandığı iş hiç kullanmadan (used=0) failed / raporsuz biterse izin aynı worker'ın
    /// yeni işine yeniden bağlanır; patrona yeni onay kartı çıkmaz. Kullanılmışsa ya da iş
    /// sürüyorsa/bittiyse (done) bağlanmaz.
    #[test]
    fn unused_grant_of_a_dropped_job_rebinds_to_the_same_worker() {
        let (core, root) = core("rebind");
        let (base, _) = fake();
        configure(&core, &base, &dead());
        let (g, d) = approved_job(&core, Some(4));
        let new_job = |c: &Connection| {
            let from = crate::delegate::caller(c, "timur")?;
            crate::delegate::insert(c, crate::delegate::NewDelegation { from: &from, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "y".into(), task_id: None, patron_words: None, effort: None })
        };
        // Sürüyor → bağlanmaz.
        with(&core.db, |c| crate::delegate::set_status(c, d, "running")).unwrap();
        assert!(with(&core.db, |c| check_grant(c, "default", g, "ayla")).unwrap_err().contains("zaten"));
        for dropped in ["failed", "done_no_report"] {
            let cur = with(&core.db, |c| Ok(grant(c, g)?.unwrap().1.delegation_id.unwrap())).unwrap();
            with(&core.db, |c| crate::delegate::set_status(c, cur, dropped)).unwrap();
            assert!(with(&core.db, |c| check_grant(c, "default", g, "kaan")).unwrap_err().contains("başka bir worker"));
            let next = with(&core.db, |c| {
                check_grant(c, "default", g, "ayla")?;
                let n = new_job(c)?;
                bind(c, g, n.id)?;
                Ok(n.id)
            })
            .unwrap();
            assert_eq!(with(&core.db, |c| Ok(grant(c, g)?.unwrap().1.delegation_id)).unwrap(), Some(next), "{dropped}");
        }
        // Kullanılmış izin düşen işten devralınmaz.
        let cur = with(&core.db, |c| Ok(grant(c, g)?.unwrap().1.delegation_id.unwrap())).unwrap();
        with(&core.db, |c| {
            c.execute("UPDATE gpu_grants SET used = 1 WHERE id = ?1", [g]).map_err(e)?;
            crate::delegate::set_status(c, cur, "failed")
        })
        .unwrap();
        assert!(with(&core.db, |c| check_grant(c, "default", g, "ayla")).unwrap_err().contains("zaten"));
        let n = with(&core.db, new_job).unwrap();
        with(&core.db, |c| bind(c, g, n.id)).unwrap();
        assert_eq!(with(&core.db, |c| Ok(grant(c, g)?.unwrap().1.delegation_id)).unwrap(), Some(cur), "bind de korur");
        // Başarıyla biten (done) işin kullanılmamış izni de devralınmaz.
        let (g2, d2) = approved_job(&core, Some(4));
        with(&core.db, |c| crate::delegate::set_status(c, d2, "done")).unwrap();
        assert!(with(&core.db, |c| check_grant(c, "default", g2, "ayla")).unwrap_err().contains("zaten"));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// #320: biten işin süren `gpu_submit` çağrısı, izin yeni işe geçtikten sonra yeni işin
    /// bütçesinden ayıramaz ve broker'a POST atamaz.
    #[test]
    fn a_stale_submit_cannot_spend_a_rebound_grant() {
        let (core, root) = core("stale");
        let (base, f) = fake();
        configure(&core, &base, &dead());
        let (g, d) = approved_job(&core, Some(4));
        with(&core.db, |c| crate::delegate::set_status(c, d, "running")).unwrap();
        let (arrived, wait_arrived) = std::sync::mpsc::channel::<()>();
        let (release, wait_release) = std::sync::mpsc::channel::<()>();
        let (arrived, wait_release) = (Mutex::new(arrived), Mutex::new(wait_release));
        *f.on_recipes.lock().unwrap() = Some(Box::new(move || {
            let _ = arrived.lock().unwrap().send(());
            wait_release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }));
        let c2 = core.clone();
        let old = std::thread::spawn(move || mcp(&c2, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "kedi", "count": 2 } })));
        wait_arrived.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // Tarif yanıtı beklerken iş raporsuz biter, izin aynı worker'ın yeni işine geçer.
        let next = with(&core.db, |c| {
            crate::delegate::set_status(c, d, "done_no_report")?;
            check_grant(c, "default", g, "ayla")?;
            let from = crate::delegate::caller(c, "timur")?;
            let n = crate::delegate::insert(c, crate::delegate::NewDelegation { from: &from, to_agent: "ayla".into(), team_id: "default".into(), service_team_id: None, prompt: "y".into(), task_id: None, patron_words: None, effort: None })?;
            bind(c, g, n.id)?;
            crate::delegate::set_status(c, n.id, "running")?;
            Ok(n.id)
        })
        .unwrap();
        release.send(()).unwrap();
        let r = old.join().unwrap();
        assert_eq!(r["isError"], json!(true), "{r}");
        assert!(text(&r).contains("broker'a gönderilmedi"), "{r}");
        assert!(!f.hits.lock().unwrap().iter().any(|h| h.starts_with("POST")), "POST yok");
        let after = with(&core.db, |c| Ok(grant(c, g)?.unwrap().1)).unwrap();
        assert_eq!((after.used, after.delegation_id), (0, Some(next)), "bütçe değişmez, izin yeni işte");
        // Yeni iş aynı izni tam bütçeyle kullanır.
        let r = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "kedi", "count": 2 } }));
        assert!(r.get("isError").is_none(), "{r}");
        assert_eq!(r["structuredContent"]["budget"]["used"], json!(2), "{r}");
        assert!(f.hits.lock().unwrap().iter().any(|h| h.starts_with("POST /v1/jobs") && h.ends_with(&format!("/{next}"))), "POST yeni işin kimliğiyle");
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worker_tools_enforce_types_sources_budget_and_hide_the_address() {
        let (core, root) = core("work");
        let (base, f) = fake();
        configure(&core, &base, &dead());
        let (g, d) = approved_job(&core, Some(5));
        let ok = |v: &Value| {
            assert!(v.get("isError").is_none(), "{v}");
            let s = v.to_string();
            assert!(!s.contains(&base) && !s.contains("127.0.0.1") && !s.contains(TOKEN), "adres/token sızdı: {s}");
            v["structuredContent"].clone()
        };
        let r = ok(&mcp(&core, "ayla", "gpu_recipes", json!({})));
        assert!(r["recipes"].get("flux_draft").is_some() && r["recipes"].get("rmbg").is_none(), "{r}");
        assert_eq!(r["budget"]["left"], 5);
        // Tür dışı tarif.
        let e1 = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "rmbg", "params": { "source_job_id": "j_1", "index": 0 } }));
        assert!(text(&e1).contains("GPU izninde değil"), "{e1}");
        // Başka iznin/işin kaynağı.
        let e2 = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_final", "params": { "draft_job_id": "j_999", "index": 0 } }));
        assert!(text(&e2).contains("bu GPU izniyle üretilmiş olmalı"), "{e2}");
        // 4 görsel → bütçe 4/5; broker mesajı ve poll_after_s aynen, adres maskeli.
        let s = ok(&mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "ikon" } })));
        assert_eq!((s["poll_after_s"].as_i64(), s["budget"]["used"].as_i64()), (Some(30), Some(4)));
        assert!(s["message"].as_str().unwrap().contains("<gizli>"), "{s}");
        assert!(f.hits.lock().unwrap().iter().any(|h| h.starts_with("POST /v1/jobs") && h.ends_with(&format!("task=ayla/{d}"))), "{:?}", f.hits.lock().unwrap());
        let job = s["job_id"].as_str().unwrap().to_string();
        // Önbellekten dönen düşmez; broker 400'ü aynen gelir ve ayrılan geri verilir.
        let c = ok(&mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "ikon" } })));
        assert_eq!((c["cached"].as_bool(), c["job_id"].as_str(), c["budget"]["used"].as_i64()), (Some(true), Some(job.as_str()), Some(4)));
        assert!(text(&mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "bozuk", "count": 1 } }))).contains("'count' 1..4 araliginda olmali"));
        // Bütçe: 4 + 2 > 5.
        let e3 = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "yeni", "count": 2 } }));
        assert!(text(&e3).contains("bütçesi bitti") && text(&e3).contains("lidere sor"), "{e3}");
        // Kendi işinin kaynağı geçer (1 görsel → 5/5).
        let f = ok(&mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_final", "params": { "draft_job_id": job, "index": 0 } })));
        assert_eq!(f["budget"]["left"], 0);
        // Durum: url düşer; başka iş sorulamaz.
        let j = ok(&mcp(&core, "ayla", "gpu_job", json!({ "jobId": job })));
        assert!(j["outputs"][0].get("url").is_none() && j["outputs"][0]["sha256"].is_string(), "{j}");
        assert!(text(&mcp(&core, "ayla", "gpu_job", json!({ "jobId": "j_999" }))).contains("gönderilmedi"));
        // İndirme: çalışma klasörüne, sha256 doğrulanır; dışarı yazılmaz.
        let out = ok(&mcp(&core, "ayla", "gpu_fetch", json!({ "jobId": job, "index": 0, "path": "assets/ikon.png" })));
        assert_eq!(std::fs::read(root.join("proj/assets/ikon.png")).unwrap(), PNG);
        assert_eq!(out["sha256"], sha256_hex(PNG));
        let outside = root.join("disari.png").to_string_lossy().into_owned();
        for bad in ["../disari.png", outside.as_str(), "/tmp/disari.png", ""] {
            let v = mcp(&core, "ayla", "gpu_fetch", json!({ "jobId": job, "index": 0, "path": bad }));
            assert!(v.get("isError").is_some(), "{bad}: {v}");
        }
        #[cfg(windows)]
        assert!(mcp(&core, "ayla", "gpu_fetch", json!({ "jobId": job, "index": 0, "path": "C:/disari.png" })).get("isError").is_some());
        assert!(!root.join("disari.png").exists());
        let used: i64 = with(&core.db, |c| c.query_row("SELECT used FROM gpu_grants WHERE id = ?1", [g], |r| r.get(0)).map_err(e)).unwrap();
        assert_eq!(used, 5);
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// #292: broker önbelleği izinden bağımsız aynı job_id'yi döndürür — erişim (job_id, izin)
    /// ilişkisi; önbellekten dönen düşmez. #300: bütçe yetmezken broker'a POST hiç gitmez; yalnız
    /// bu iznin erişimindeki bitmiş iş GET ile döner, `used` bütçeyi aşmaz.
    #[test]
    fn cached_jobs_need_no_budget_and_grant_access_to_each_grant() {
        let (core, root) = core("cache");
        let (base, f) = fake();
        configure(&core, &base, &dead());
        let (ga, _) = approved_job_for(&core, "ayla", Some(4));
        let (gb, _) = approved_job_for(&core, "kaan", Some(4));
        let (gc, _) = approved_job_for(&core, "bora", Some(1));
        let sub = |agent: &str, prompt: &str| mcp(&core, agent, "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": prompt } }));
        let posts = || f.hits.lock().unwrap().iter().filter(|h| h.starts_with("POST")).count();
        let a = sub("ayla", "ikon")["structuredContent"].clone();
        let job = a["job_id"].as_str().unwrap().to_string();
        assert_eq!((a["cached"].as_bool(), a["budget"]["left"].as_i64()), (Some(false), Some(0)));
        // A'nın bütçesi sıfır: aynı istek kendi bitmiş işinden GET ile döner, POST yok, düşmez.
        let before = posts();
        let a2 = sub("ayla", "ikon");
        assert!(a2.get("isError").is_none(), "{a2}");
        let a2 = &a2["structuredContent"];
        assert_eq!((a2["cached"].as_bool(), a2["job_id"].as_str(), a2["status"].as_str(), a2["budget"]["used"].as_i64()), (Some(true), Some(job.as_str()), Some("done"), Some(4)));
        assert_eq!(posts(), before, "bütçesiz broker'a POST gitmez");
        // B (bütçe yeterli) aynı isteği ister: broker aynı job_id'yi cached döndürür; B'ye erişim eklenir, düşmez.
        let b = sub("kaan", "ikon");
        assert!(b.get("isError").is_none(), "{b}");
        assert_eq!((b["structuredContent"]["job_id"].as_str(), b["structuredContent"]["budget"]["used"].as_i64()), (Some(job.as_str()), Some(0)));
        for agent in ["ayla", "kaan"] {
            let j = mcp(&core, agent, "gpu_job", json!({ "jobId": job }));
            assert!(j.get("isError").is_none(), "{agent}: {j}");
        }
        let access: i64 = with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM gpu_job_grants WHERE job_id = ?1 AND grant_id IN (?2, ?3)", params![job, ga, gb], |r| r.get(0)).map_err(e)).unwrap();
        assert_eq!(access, 2);
        let before = posts();
        // C (bütçe 1 < 4): istek yerelde bilinse de C'nin erişiminde değil → ret, erişim eklenmez.
        assert!(text(&sub("bora", "ikon")).contains("bütçesi bitti"));
        assert!(text(&mcp(&core, "bora", "gpu_job", json!({ "jobId": job }))).contains("gönderilmedi"));
        // Bilinmeyen istek bütçesiz geçmez.
        assert!(text(&sub("ayla", "yeni")).contains("bütçesi bitti"));
        // Broker'da düşmüş (done olmayan) iş: bütçesizken ret.
        f.failed.lock().unwrap().insert(job.clone());
        assert!(text(&sub("ayla", "ikon")).contains("lidere sor"));
        assert_eq!(posts(), before, "bütçesiz broker'a POST gitmez");
        assert!(!f.hits.lock().unwrap().iter().any(|h| h.starts_with("DELETE")), "iptal yolu yok");
        let used: Vec<(i64, i64)> = with(&core.db, |c| {
            let mut st = c.prepare("SELECT used, budget FROM gpu_grants WHERE id IN (?1, ?2, ?3) ORDER BY id").map_err(e)?;
            let rows = st.query_map(params![ga, gb, gc], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
            rows.collect::<Result<_, _>>().map_err(e)
        })
        .unwrap();
        assert_eq!(used, vec![(4, 4), (0, 4), (0, 1)]);
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// #292: geçici dosya benzersiz (`create_new`); var olan dosyaya dokunulmaz. Bağlantıdan
    /// (junction/symlink) geçen yol reddedilir, kök dışına yazılmaz.
    #[test]
    fn fetch_uses_a_private_temp_file_and_refuses_links() {
        let (core, root) = core("fetch");
        let (base, fk) = fake();
        configure(&core, &base, &dead());
        approved_job(&core, Some(4));
        let s = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "ikon" } }));
        let job = s["structuredContent"]["job_id"].as_str().unwrap().to_string();
        let fetch = |path: &str| mcp(&core, "ayla", "gpu_fetch", json!({ "jobId": job, "index": 0, "path": path }));
        let proj = root.join("proj");
        std::fs::create_dir_all(proj.join("assets")).unwrap();
        std::fs::write(proj.join("assets/icon.dex-part"), b"benim").unwrap();
        let v = fetch("assets/icon.png");
        assert!(v.get("isError").is_none(), "{v}");
        assert_eq!(std::fs::read(proj.join("assets/icon.png")).unwrap(), PNG);
        assert_eq!(std::fs::read(proj.join("assets/icon.dex-part")).unwrap(), b"benim", "başkasının dosyası ezilmez");
        let left: Vec<_> = std::fs::read_dir(proj.join("assets")).unwrap().flatten().map(|d| d.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), 2, "geçici dosya kalmaz: {left:?}");

        // Kök dışına giden klasör bağlantısı (Windows'ta junction: yönetici gerektirmez).
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        #[cfg(windows)]
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(proj.join("linked")).arg(&out).output().unwrap().status.success();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&out, proj.join("linked")).is_ok();
        assert!(made, "klasör bağlantısı kurulamadı");
        for p in ["linked/x.png", "linked/alt/x.png"] {
            let v = fetch(p);
            assert!(text(&v).contains("bağlantı"), "{p}: {v}");
        }
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "kök dışına yazılmadı");

        // Hedef dosya bağlantısı (Windows'ta dosya symlink'i ayrıcalık ister; yoksa bu adım atlanır).
        std::fs::write(out.join("kurban.txt"), b"dokunma").unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(out.join("kurban.txt"), proj.join("assets/evil.png")).is_ok();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(out.join("kurban.txt"), proj.join("assets/evil.png")).is_ok();
        if made {
            assert!(text(&fetch("assets/evil.png")).contains("bağlantı"));
            assert_eq!(std::fs::read(out.join("kurban.txt")).unwrap(), b"dokunma");
        }

        // #300: indirme sürerken assets kök dışına bağlantıyla değiştirilir — yol kontrolü indirmeden
        // sonra, yazmadan hemen önce çalıştığı için reddedilir.
        let swap = (proj.join("assets"), proj.join("assets-eski"), out.join("yeni"));
        std::fs::create_dir_all(&swap.2).unwrap();
        *fk.on_output.lock().unwrap() = Some(Box::new(move || {
            std::fs::rename(&swap.0, &swap.1).unwrap();
            #[cfg(windows)]
            let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&swap.0).arg(&swap.2).output().unwrap().status.success();
            #[cfg(unix)]
            let made = std::os::unix::fs::symlink(&swap.2, &swap.0).is_ok();
            assert!(made, "klasör bağlantısı kurulamadı");
        }));
        let v = fetch("assets/icon.png");
        assert!(text(&v).contains("bağlantı"), "{v}");
        assert_eq!(std::fs::read_dir(out.join("yeni")).unwrap().count(), 0, "indirme sırasında değişen bağlantıdan dışarı yazılmadı");
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_fallback_clears_it_and_saves_the_rest() {
        // #305: UI boş yedek adresi "" gönderir; reddedilirse aynı kayıttaki token/birincil de kaybolurdu.
        let (core, root) = core("nofb");
        let (base, _) = fake();
        let v = core.gpu_config_set(ConfigPatch { primary: Some(base.clone()), fallback: Some("  ".into()), token: Some(TOKEN.into()) }).unwrap();
        assert_eq!((v.primary.as_str(), v.fallback.as_str(), v.token_set), (base.as_str(), "", true));
        assert_eq!(core.gpu_config_get().unwrap().fallback, "");
        // Yedeksiz de çalışır: tek adres denenir.
        let o = core.gpu_overview(true).unwrap();
        assert!(o.connected && o.authorized && o.via.as_deref() == Some("primary"), "{o:?}");
        // Birincil düşükse "ulaşılamadı", boş yedek denenmez.
        core.gpu_config_set(ConfigPatch { primary: Some(dead()), ..Default::default() }).unwrap();
        let o = core.gpu_overview(true).unwrap();
        assert_eq!((o.connected, o.error_kind.as_deref()), (false, Some("unreachable")));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overview_tells_a_broken_service_from_a_wrong_token() {
        // #305: /health var, token doğru ama /v1/models 500 → "token yanlış" değil, servis hatası.
        let (core, root) = core("svc");
        let (base, f) = fake();
        configure(&core, &base, "");
        f.broken.lock().unwrap().push("/v1/models".into());
        let o = core.gpu_overview(true).unwrap();
        assert!(o.connected && !o.authorized, "{o:?}");
        assert_eq!(o.error_kind.as_deref(), Some("service"));
        assert!(o.error.as_deref().unwrap().contains("vllm yanit vermedi"), "{o:?}");
        // Modeller gelir, durum 500: yetkili kalır, hata görünür.
        f.broken.lock().unwrap().clear();
        f.broken.lock().unwrap().push("/v1/status".into());
        let o = core.gpu_overview(true).unwrap();
        assert!(o.authorized && o.status.is_none() && o.models.len() == 4, "{o:?}");
        assert_eq!(o.error_kind.as_deref(), Some("service"));
        let ui = serde_json::to_value(&o).unwrap();
        assert_eq!(ui["errorKind"], "service");
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overview_reports_a_truncated_or_garbled_model_list_as_a_service_error() {
        // #307: 200 başlığı + yarım ya da bozuk JSON gövde → "bağlı, model yok" değil, servis hatası.
        let (core, root) = core("mangled");
        let (base, f) = fake();
        configure(&core, &base, "");
        let full = br#"{"models":[{"name":"FLUX.2-dev","type":"t2i"}]}"#.to_vec();
        // #309: boş ve yalnız boşluk 200 gövde de "model yok" değil, servis hatası.
        let cases = [(full[..20].to_vec(), full.len()), (b"<html>bakimda</html>".to_vec(), 20), (Vec::new(), 0), (b" 
	 ".to_vec(), 5)];
        for (out, declared) in cases {
            *f.mangled.lock().unwrap() = Some(("/v1/models".into(), out, declared));
            let o = core.gpu_overview(true).unwrap();
            assert!(o.connected && !o.authorized && o.models.is_empty(), "{o:?}");
            assert_eq!(o.error_kind.as_deref(), Some("service"), "{o:?}");
            let err = o.error.as_deref().unwrap();
            assert!(err.contains("okunamadı") && !err.contains("127.0.0.1"), "{err}");
        }
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn errors_never_carry_the_address() {
        let a = Addrs { primary: dead(), fallback: dead() };
        let last = AtomicUsize::new(0);
        let b = Broker { addrs: &a, token: Some(TOKEN), last: &last };
        let err = b.json("GET", "/health", None, None).unwrap_err();
        assert!(!err.contains("127.0.0.1") && !err.contains(TOKEN), "{err}");
        let v = b.scrub(json!({ "m": format!("x {} y {TOKEN}", a.primary), "url": "/v1/jobs/j/outputs/0" }));
        assert_eq!(v, json!({ "m": "x <gizli> y <gizli>" }));
    }

    #[test]
    fn budget_uses_last_broker_daily_limit() {
        let (core, root) = core("daily-limit");
        with(&core.db, |c| put_setting(c, "gpu_limits", &json!({"daily_image_limit": 300}))).unwrap();
        with(&core.db, |c| {
            let lead = crate::delegate::caller(c, "timur")?;
            assert!(request(c, &lead, &json!({"workerId":"ayla", "types":["t2i"], "reason":"test", "budget":301}), &["t2i".into()], None).is_err());
            let r = request(c, &lead, &json!({"workerId":"ayla", "types":["t2i"], "reason":"test", "budget":250}), &["t2i".into()], None)?;
            assert!(decide(c, r.id, true, Some(301)).is_err());
            assert_eq!(decide(c, r.id, true, Some(300))?.budget, 300);
            Ok(())
        }).unwrap();
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn admin_limits_contract_and_no_persistent_token() {
        let (core, root) = core("admin-limits");
        let (base, f) = fake();
        configure(&core, &base, "");
        let limits = Limits { daily_image_limit: Some(300), max_pending_jobs: Some(8), max_count_per_request: Some(2) };
        assert_eq!(core.gpu_limits_get().unwrap().daily_image_limit, Some(120));
        assert_eq!(core.gpu_limits_set("1234567", limits.clone(), Some("deneme")).unwrap(), limits);
        assert_eq!(*f.admin.lock().unwrap(), vec![("Bearer 1234567".into(), json!({"daily_image_limit":300,"max_pending_jobs":8,"max_count_per_request":2,"reason":"deneme"}))]);
        assert_eq!(core.gpu_limits_set("wrong", limits.clone(), None).unwrap_err(), "yönetici şifresi yanlış");
        assert_eq!(core.gpu_limits_set("noauth", limits.clone(), None).unwrap_err(), "yönetici şifresi yanlış");
        assert_eq!(core.gpu_limits_set("x", limits.clone(), None).unwrap_err(), "invalid: invalid <gizli>");
        let sent = f.admin.lock().unwrap().len();
        for token in ["", "0123456789", "a\r\nb"] {
            assert!(core.gpu_limits_set(token, limits.clone(), None).is_err());
        }
        for limits in [Limits::default(), Limits { daily_image_limit: Some(0), ..Default::default() }, Limits { max_count_per_request: Some(5), ..Default::default() }] {
            assert!(core.gpu_limits_set("1234567", limits, None).is_err());
        }
        assert_eq!(f.admin.lock().unwrap().len(), sent, "geçersiz girdi broker'a gönderilmedi");
        with(&core.db, |c| {
            let mut st = c.prepare("SELECT value FROM settings").map_err(e)?;
            let values = st.query_map([], |r| r.get::<_, String>(0)).map_err(e)?;
            for v in values { assert!(!v.map_err(e)?.contains("1234567")); }
            assert_eq!(addrs(c).primary, base);
            for agent in ["timur", "ayla"] {
                let caller = crate::delegate::caller(c, agent)?;
                assert!(!tools_for(c, &caller, None).iter().any(|t| t["name"].as_str().is_some_and(|n| n.starts_with("gpu_limits"))));
            }
            Ok(())
        }).unwrap();
        assert_eq!(core.gpu_token().as_deref(), Some(TOKEN));
        for agent in ["timur", "ayla"] {
            for method in ["gpu_limits_get", "gpu_limits_set"] {
                assert!(text(&mcp(&core, agent, method, json!({}))).contains("bilinmeyen araç"));
            }
        }
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn waiting_owner_emits_gpu_waiting_for_the_ui() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let (core, root) = core_with("waiting", Events::new(Got(got.clone())));
        let (base, f) = fake();
        configure(&core, &base, "");
        approved_job(&core, Some(8));
        let waits = || got.lock().unwrap().iter().filter(|(e, _)| e == "gpu-waiting").map(|(_, v)| v.clone()).collect::<Vec<_>>();
        // Sırada (queued) olay yok.
        let q = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "a", "count": 1 } }));
        assert!(q.get("isError").is_none(), "{q}");
        assert!(waits().is_empty());
        f.waiting.store(true, Ordering::SeqCst);
        let w = mcp(&core, "ayla", "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "b", "count": 1 } }));
        let job = w["structuredContent"]["job_id"].as_str().unwrap().to_string();
        assert_eq!(w["structuredContent"]["status"], "waiting_owner");
        let j = mcp(&core, "ayla", "gpu_job", json!({ "jobId": job }));
        assert!(j.get("isError").is_none(), "{j}");
        let ws = waits();
        assert_eq!(ws.len(), 2, "submit + job yanıtı; tekilleştirme UI'da (key = jobId)");
        assert_eq!(ws[0], json!({ "agent": "ayla", "agentName": ws[0]["agentName"], "teamId": "default", "jobId": job, "waitReason": "owner_llm_active", "etaS": 365 }));
        assert!(!ws[0].to_string().contains("127.0.0.1"));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- #899: proje katmanı F ----

    /// Takımın ikinci bağlı projesi (gerçek git kökü, çalışma dizininin `.dex/tmp`'si altında).
    fn second_project(core: &Core) -> (crate::projects::tests::Fixture, String, PathBuf) {
        let f = crate::projects::tests::Fixture::new();
        let dir = f.dir("p2");
        let ready = crate::projects::prepare_root(dir.to_str().unwrap(), None).unwrap();
        let p = with(&core.db, |c| {
            crate::projects::create(c, "default", crate::projects::NewProject { name: "P2".into(), worktrees: None }, &ready).map_err(|e| e.to_string())
        })
        .unwrap();
        let root = PathBuf::from(p.root_path.unwrap());
        (f, p.id, root)
    }

    /// Lider `project` için bağlı iş açar (MCP'nin çözdüğü bağlamla); `running` ise çalışır hâlde.
    fn job_in(core: &Core, worker: &str, project: &str, running: bool) -> i64 {
        with(&core.db, |c| {
            let from = crate::delegate::caller(c, "timur")?;
            let ctx = crate::delegate::resolve_context(c, Path::new(""), &from, Some(project), None)?;
            let d = crate::delegate::insert_bound(c, crate::delegate::NewDelegation { from: &from, to_agent: worker.into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }, &ctx)?;
            if running {
                crate::delegate::set_status(c, d.id, "running")?;
            }
            Ok(d.id)
        })
        .unwrap()
    }

    fn request_in(core: &Core, worker: &str, project: Option<&str>) -> Result<i64, String> {
        let mut args = json!({ "types": ["t2i"], "workerId": worker, "reason": "ikon" });
        if let Some(p) = project {
            args["project"] = json!(p);
        }
        let r = mcp(core, "timur", "gpu_request", args);
        r["structuredContent"]["requestId"].as_i64().ok_or_else(|| text(&r))
    }

    fn grant_project(core: &Core, g: i64) -> (Option<String>, Option<i64>) {
        with(&core.db, |c| c.query_row("SELECT project_id, delegation_id FROM gpu_grants WHERE id = ?1", [g], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)).unwrap()
    }

    /// İstek seçilen projeyi, izin isteğin projesini taşır; tek-proje legacy de kendi id'siyle (NULL
    /// penceresi kapalı). Bind / used=0 rebind / fixOf yalnız aynı projede; farklı P `project_mismatch`
    /// ve hiçbir şey değişmez. Eski NULL izin bağlandığı işin projesini alır.
    #[test]
    fn request_grant_and_every_bind_stay_in_one_project() {
        let (core, root) = core("scope");
        let (base, _) = fake();
        configure(&core, &base, &dead());
        let req_project = |id: i64| -> Option<String> {
            with(&core.db, |c| c.query_row("SELECT project_id FROM gpu_requests WHERE id = ?1", [id], |r| r.get(0)).map_err(e)).unwrap()
        };
        // Tek bağlı proje: alan yokken de legacy id yazılır; açık legacy id de kabul.
        let implicit = request_in(&core, "ayla", None).unwrap();
        assert_eq!(req_project(implicit).as_deref(), Some("legacy-default"));
        let g1 = core.gpu_request_decide(implicit, true, Some(4)).unwrap().grant_id.unwrap();
        assert_eq!(grant_project(&core, g1).0.as_deref(), Some("legacy-default"));
        let explicit = request_in(&core, "ayla", Some("legacy-default")).unwrap();
        assert_eq!(req_project(explicit).as_deref(), Some("legacy-default"));

        let (_f, p2, _) = second_project(&core);
        // Çok projede alan zorunlu; yabancı id yok sayılmaz.
        assert!(request_in(&core, "ayla", None).unwrap_err().contains("project_required"));
        assert!(request_in(&core, "ayla", Some("p-yok")).is_err());
        let r2 = request_in(&core, "ayla", Some(&p2)).unwrap();
        let g2 = core.gpu_request_decide(r2, true, Some(4)).unwrap().grant_id.unwrap();
        assert_eq!(grant_project(&core, g2).0, Some(p2.clone()));

        // Bind: P2 izni P1 işine bağlanmaz; P2 işine bağlanır.
        let j1 = job_in(&core, "ayla", "legacy-default", false);
        assert_eq!(with(&core.db, |c| bind(c, g2, j1)).unwrap_err(), "project_mismatch");
        assert_eq!(grant_project(&core, g2), (Some(p2.clone()), None));
        let j2 = job_in(&core, "ayla", &p2, false);
        with(&core.db, |c| bind(c, g2, j2)).unwrap();
        assert_eq!(grant_project(&core, g2).1, Some(j2));

        // used=0 rebind: düşen işin izni başka projeye geçmez, aynı projenin yeni işine geçer.
        with(&core.db, |c| crate::delegate::set_status(c, j2, "failed")).unwrap();
        let other = job_in(&core, "ayla", "legacy-default", false);
        with(&core.db, |c| check_grant(c, "default", g2, "ayla")).unwrap();
        assert_eq!(with(&core.db, |c| bind(c, g2, other)).unwrap_err(), "project_mismatch");
        let again = job_in(&core, "ayla", &p2, false);
        with(&core.db, |c| bind(c, g2, again)).unwrap();
        assert_eq!(grant_project(&core, g2), (Some(p2.clone()), Some(again)));

        // fixOf: izin yalnız aynı projenin düzeltme işine geçer.
        let fix_other = job_in(&core, "ayla", "legacy-default", false);
        assert_eq!(with(&core.db, |c| inherit(c, again, fix_other, "ayla")).unwrap_err(), "project_mismatch");
        assert_eq!(grant_project(&core, g2).1, Some(again));
        let fix = job_in(&core, "ayla", &p2, false);
        assert!(with(&core.db, |c| inherit(c, again, fix, "ayla")).unwrap());
        assert_eq!(grant_project(&core, g2), (Some(p2.clone()), Some(fix)));

        // Açılış backfill'inden kalmış NULL izin: bağlandığı işin projesini alır.
        with(&core.db, |c| {
            c.execute_batch("INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,status,created_at) VALUES(90,'default','timur','ayla','[\"t2i\"]','eski',1,'approved',1);
                INSERT INTO gpu_grants(id,request_id,team_id,worker,types,budget,created_at) VALUES(90,90,'default','ayla','[\"t2i\"]',1,1);").map_err(e)
        })
        .unwrap();
        let legacy_job = job_in(&core, "ayla", "legacy-default", false);
        with(&core.db, |c| bind(c, 90, legacy_job)).unwrap();
        assert_eq!(grant_project(&core, 90), (Some("legacy-default".into()), Some(legacy_job)));
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Worker GPU yetkisi spawn anahtarının launch işinden: kuyruktaki sonraki iş yetki vermez.
    /// Bağsız eski anahtar (None) yalnız çalışan işi görür; iş bitince erişim biter.
    #[test]
    fn worker_access_follows_the_launch_job_and_ends_with_it() {
        let (core, root) = core("launch");
        let (base, _) = fake();
        configure(&core, &base, &dead());
        let r = request_in(&core, "ayla", None).unwrap();
        let g = core.gpu_request_decide(r, true, Some(4)).unwrap().grant_id.unwrap();
        let queued = job_in(&core, "ayla", "legacy-default", false);
        with(&core.db, |c| bind(c, g, queued)).unwrap();
        let ayla = with(&core.db, |c| crate::delegate::caller(c, "ayla")).unwrap();
        let has_tools = |job: Option<i64>| with(&core.db, |c| Ok(!tools_for(c, &ayla, job).is_empty())).unwrap();
        let recipes = |job: Option<i64>| core.gpu_mcp(&ayla, "gpu_recipes", &json!({}), job);
        // Yalnız izinli iş sırada: bağsız anahtar sıradaki işi yetki saymaz.
        assert!(!has_tools(None) && recipes(None).is_err());
        let running = job_in(&core, "ayla", "legacy-default", true);
        let kaan_job = job_in(&core, "kaan", "legacy-default", true);

        // Çalışan iş izinsiz, izinli iş sırada: bağsız anahtar da çalışan işin anahtarı da göremez.
        assert!(!has_tools(None) && !has_tools(Some(running)));
        assert!(recipes(None).unwrap_err().contains("bilinmeyen araç"));
        assert!(recipes(Some(running)).is_err());
        // Yabancı iş kimliği yetki vermez.
        assert!(!has_tools(Some(kaan_job)) && recipes(Some(kaan_job)).is_err());
        // Launch anahtarı sıradaki kendi işine bağlıysa (queued dahil) görür.
        assert!(has_tools(Some(queued)));
        assert_eq!(recipes(Some(queued)).unwrap()["budget"]["left"], 4);

        // Çalışan iş bitip izinli iş başlayınca bağsız anahtar da görür.
        with(&core.db, |c| {
            crate::delegate::set_status(c, running, "done")?;
            crate::delegate::set_status(c, queued, "running")
        })
        .unwrap();
        assert!(has_tools(None) && recipes(None).is_ok());
        // İş bitti: hiçbir anahtar görmez.
        with(&core.db, |c| crate::delegate::set_status(c, queued, "done")).unwrap();
        assert!(!has_tools(None) && !has_tools(Some(queued)));
        assert!(recipes(Some(queued)).is_err() && recipes(None).is_err());
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// `gpu_fetch` işin W/P kökünü kullanır: canlı worktree, yoksa işin proje kökü snapshot'ı;
    /// takım cwd'si değişse de oraya yazmaz. Broker önbelleği global (aynı job_id) kalır; erişim
    /// (job_id, izin) ile — başka projenin izni kendi submit'i olmadan işi göremez, indiremez.
    #[test]
    fn fetch_lands_in_the_job_project_and_the_global_cache_keeps_grant_ownership() {
        let (core, root) = core("wp");
        let (base, _) = fake();
        configure(&core, &base, &dead());
        let (_f, p2, p2_root) = second_project(&core);
        let mut grants = Vec::new();
        for (worker, project) in [("ayla", "legacy-default"), ("kaan", p2.as_str())] {
            let r = request_in(&core, worker, Some(project)).unwrap();
            let g = core.gpu_request_decide(r, true, Some(4)).unwrap().grant_id.unwrap();
            let j = job_in(&core, worker, project, true);
            with(&core.db, |c| bind(c, g, j)).unwrap();
            grants.push((g, j));
        }
        // Takım cwd'si başka yere taşınır: işler kendi snapshot'ında kalır.
        let moved = root.join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        with(&core.db, |c| c.execute("UPDATE teams SET cwd = ?1 WHERE id = 'default'", [moved.to_string_lossy()]).map_err(e)).unwrap();
        let sub = |agent: &str| mcp(&core, agent, "gpu_submit", json!({ "recipe": "flux_draft", "params": { "prompt": "ikon", "count": 1 } }));
        let fetch = |agent: &str, job: &str, path: &str| mcp(&core, agent, "gpu_fetch", json!({ "jobId": job, "index": 0, "path": path }));
        let a = sub("ayla");
        let job = a["structuredContent"]["job_id"].as_str().unwrap().to_string();
        assert_eq!(a["structuredContent"]["cached"], json!(false));
        // P2 izni bu işi kendi submit'i olmadan sorgulayamaz / indiremez.
        assert!(text(&mcp(&core, "kaan", "gpu_job", json!({ "jobId": job }))).contains("gönderilmedi"));
        assert!(text(&fetch("kaan", &job, "assets/x.png")).contains("gönderilmedi"));
        assert!(!p2_root.join("assets/x.png").exists());
        // Aynı istek: broker aynı job_id'yi cached döndürür; P2 iznine erişim eklenir, bütçe düşmez.
        let b = sub("kaan");
        assert_eq!((b["structuredContent"]["job_id"].as_str(), b["structuredContent"]["cached"].as_bool()), (Some(job.as_str()), Some(true)));
        assert_eq!(b["structuredContent"]["budget"]["used"], json!(0));
        let owners: Vec<i64> = with(&core.db, |c| {
            let mut st = c.prepare("SELECT grant_id FROM gpu_job_grants WHERE job_id = ?1 ORDER BY grant_id").map_err(e)?;
            let rows = st.query_map([&job], |r| r.get(0)).map_err(e)?;
            rows.collect::<Result<_, _>>().map_err(e)
        })
        .unwrap();
        assert_eq!(owners, vec![grants[0].0, grants[1].0]);
        // Her iş kendi P kökünü kullanır; takım cwd'sine yazılmaz.
        assert!(fetch("ayla", &job, "assets/a.png").get("isError").is_none());
        assert!(fetch("kaan", &job, "assets/b.png").get("isError").is_none());
        assert_eq!(std::fs::read(root.join("proj/assets/a.png")).unwrap(), PNG);
        assert_eq!(std::fs::read(p2_root.join("assets/b.png")).unwrap(), PNG);
        assert!(!moved.join("assets").exists());
        // Canlı worktree (W) varsa çıktı oraya; kaldırılınca P'ye döner.
        let wt = root.join("wt-ayla");
        std::fs::create_dir_all(&wt).unwrap();
        with(&core.db, |c| {
            c.execute(
                "INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(?1,'ayla',?2,?3,'job','base',1)",
                params![grants[0].1, root.join("proj").to_string_lossy(), wt.to_string_lossy()],
            )
            .map_err(e)
        })
        .unwrap();
        assert!(fetch("ayla", &job, "assets/w.png").get("isError").is_none());
        assert_eq!(std::fs::read(wt.join("assets/w.png")).unwrap(), PNG);
        assert!(!root.join("proj/assets/w.png").exists());
        with(&core.db, |c| c.execute("UPDATE delegation_worktrees SET removed_at = 1 WHERE delegation_id = ?1", [grants[0].1]).map_err(e)).unwrap();
        assert!(fetch("ayla", &job, "assets/p.png").get("isError").is_none());
        assert!(root.join("proj/assets/p.png").exists());
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// #882: `gpu_request`'in broker çağrısı (ağ) sırasında ne `agent_start_gate` ne DB kilidi tutulur;
    /// istek satırı yine seçilen projeyle yazılır. Broker'ı kilit içine geri taşıyan değişiklik burada kırılır.
    #[test]
    fn gpu_request_holds_no_gate_or_db_lock_during_the_broker_call() {
        let (core, root) = core("gate");
        let (base, f) = fake();
        configure(&core, &base, &dead());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (weak, out) = (Arc::downgrade(&core), seen.clone());
        *f.on_models.lock().unwrap() = Some(Box::new(move || {
            if let Some(core) = weak.upgrade() {
                let gate = core.agent_start_gate.try_lock().is_ok();
                let db = core.db.0.try_lock().is_ok();
                out.lock().unwrap().push((gate, db));
            }
        }));
        let id = request_in(&core, "ayla", None).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(true, true)], "broker çağrısında kilit serbest");
        let project: Option<String> = with(&core.db, |c| c.query_row("SELECT project_id FROM gpu_requests WHERE id = ?1", [id], |r| r.get(0)).map_err(e)).unwrap();
        assert_eq!(project.as_deref(), Some("legacy-default"));
        *f.on_models.lock().unwrap() = None;
        core.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }
}
