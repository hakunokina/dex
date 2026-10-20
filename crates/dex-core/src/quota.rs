//! Görev #250 — Claude aboneliği için proaktif kota eşiği. 5 saatlik pencere %85'i geçince
//! Claude'daki WORKER'lar zincirde sıradakine geçer (haftalık pencere yalnız ölçülür/gösterilir;
//! geçiş eşiği değildir) (mevcut devir yolu:
//! `Core::chain_execute`), eşik üstündeyken açılan worker doğrudan sıradakinde açılır. Liderler
//! etkilenmez. Eşikler sabit; yapılandırma ayarı yoktur.
//!
//! Kaynak: Claude Code'un `/usage`'ının da çağırdığı `GET api.anthropic.com/api/oauth/usage`
//! (hesap geneli; CLI'nın Dex dışındaki kullanımı dahil). Jeton `model_news::claude_token` ile, yalnız
//! bellekte, yenilenmez (CLI yeniler). Jeton ve yanıt gövdesi loglanmaz. Ölçüm aralığı kullanıma göre
//! kademeli (#253, [`tier_delay`]): iki pencereden yüksek olanı %50 altı → 10 dk, %50–70 → 3 dk,
//! %70+ → 60 sn. Hata (429 dahil) aralığı ikiye katlar (taban 60 sn, en çok 10 dk); sonraki başarılı
//! ölçüm yeniden kademeye döner. #2247: hata yanıtında `Retry-After` (saniye ya da HTTP tarihi) varsa
//! bir sonraki deneme en az o kadar ertelenir (tavan [`RETRY_AFTER_CAP_MS`]); başlık yoksa katlama
//! sürer. Son başarılı ölçüm 15 dk'dan eskiyse eşik "bilinmiyor" sayılır → kimse yönlendirilmez
//! (eski davranış: zincir yalnız kota bitince).

use serde::Serialize;

pub const FIVE_HOUR_LIMIT: f64 = 85.0;
/// En kısa aralık: %70+ kademesi ve geri çekilmenin tabanı (ölçüm yok/bayat iken de buradan başlanır).
pub const MIN_POLL_MS: u64 = 60_000;
pub const MID_POLL_MS: u64 = 180_000;
/// En uzun aralık: %50 altı kademesi ve geri çekilme tavanı.
pub const MAX_POLL_MS: u64 = 600_000;
/// Bayatlık sınırı 15 dk kalır: en uzun normal aralık (10 dk) + istek zaman aşımı (13 sn) bunun
/// altında, yani 10 dk kademesinde ölçümler arası eşik "bilinmiyor"a düşmez. Ayrıca eşikler (%80/%85)
/// yalnız 60 sn kademesinde aşılabilir; orada 15 dk ≈ art arda birkaç başarısız deneme demek.
pub const STALE_MS: i64 = 15 * 60_000;
const _: () = assert!(STALE_MS > (MAX_POLL_MS + 13_000) as i64);
/// #2247: ucun `Retry-After`'ına en çok bu kadar uyulur (bozuk/aşırı değer ölçümü saatlerce susturmasın).
pub const RETRY_AFTER_CAP_MS: u64 = 30 * 60_000;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Pencere doluluğu (yüzde, 0-100) ve uç noktanın bildirdiği sıfırlanma saati (RFC 3339, aynen).
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub five_hour: f64,
    pub seven_day: f64,
    pub five_hour_resets_at: Option<String>,
    pub seven_day_resets_at: Option<String>,
}

/// Aşılan pencere: geçiş bildirimindeki "Claude %85 (5 sa)" kısmı.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Breach {
    pub window: &'static str,
    pub percent: f64,
    pub resets_at: Option<String>,
}

impl Breach {
    pub fn label(&self) -> String {
        self.label_for("Claude")
    }

    /// #2019: hesabın etiketiyle ("Claude 2 %85 (5 sa)").
    pub fn label_for(&self, who: &str) -> String {
        format!("{who} %{:.0} ({})", self.percent.floor(), self.window)
    }
}

/// Eşik kontrolü: yalnız 5 saatlik pencere; haftalık pencere geçişi tetiklemez.
pub fn breach(u: &Usage) -> Option<Breach> {
    (u.five_hour >= FIVE_HOUR_LIMIT)
        .then(|| Breach { window: "5 sa", percent: u.five_hour, resets_at: u.five_hour_resets_at.clone() })
}

/// Ölçüm kaynağı; testler sahtesini verir (`Core::quota_set_source`).
pub trait UsageSource: Send + Sync {
    fn read(&self) -> Result<Usage, String>;

    /// #2247: son başarısız `read`'in `Retry-After` ipucu (ms); yoksa `None`. Yalnız hata sonrası okunur.
    fn retry_after_ms(&self) -> Option<u64> {
        None
    }
}

/// `Retry-After` başlığı: saniye (`120`) ya da HTTP tarihi (`Wed, 21 Oct 2015 07:28:00 GMT`) →
/// şimdiden itibaren ms. Okunamayan/geçmiş = `None` (çağrıcı mevcut geri çekilmeyi sürdürür).
pub fn parse_retry_after(value: &str, now_ms: i64) -> Option<u64> {
    let v = value.trim();
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse::<u64>().ok()?.checked_mul(1000);
    }
    let at = chrono::DateTime::parse_from_rfc2822(v).ok()?.timestamp_millis();
    (at > now_ms).then(|| (at - now_ms) as u64)
}

/// `/api/oauth/usage` gövdesi: `{"five_hour":{"utilization":42.0,"resets_at":"…"},"seven_day":{…}}`.
/// Pencere yoksa (`null`) doluluk 0 sayılır; ikisi de yoksa gövde geçersiz.
pub fn parse_usage(v: &serde_json::Value) -> Result<Usage, String> {
    let window = |k: &str| {
        let w = v.get(k).filter(|w| w.is_object());
        let pct = w.and_then(|w| w.get("utilization")).and_then(serde_json::Value::as_f64);
        let reset = w.and_then(|w| w.get("resets_at")).and_then(serde_json::Value::as_str).map(str::to_string);
        (pct, reset)
    };
    let (five, five_reset) = window("five_hour");
    let (week, week_reset) = window("seven_day");
    if five.is_none() && week.is_none() {
        return Err("kullanım yanıtında pencere yok".into());
    }
    Ok(Usage {
        five_hour: five.unwrap_or(0.0),
        seven_day: week.unwrap_or(0.0),
        five_hour_resets_at: five_reset,
        seven_day_resets_at: week_reset,
    })
}

/// Gerçek kaynak: abonelik OAuth jetonuyla kullanım uç noktası. `dir` `None` = asıl hesap
/// (`model_news::claude_token`); `Some` = #2019 ek hesabın config dizini.
pub struct OAuthUsage {
    dir: Option<std::path::PathBuf>,
    /// #2247: son hata yanıtının `Retry-After`'ı (ms); başarılı okuma ya da başlıksız hata siler.
    retry_after: std::sync::Mutex<Option<u64>>,
}

impl OAuthUsage {
    pub fn new(dir: Option<std::path::PathBuf>) -> Self {
        Self { dir, retry_after: std::sync::Mutex::default() }
    }
}

impl UsageSource for OAuthUsage {
    fn retry_after_ms(&self) -> Option<u64> {
        *self.retry_after.lock().unwrap()
    }

    fn read(&self) -> Result<Usage, String> {
        *self.retry_after.lock().unwrap() = None;
        let token = match &self.dir {
            None => crate::model_news::claude_token(),
            Some(dir) if !crate::claude_accounts::connected(dir) => return Err("hesap bağlı değil".into()),
            Some(dir) => crate::model_news::claude_token_in(dir),
        };
        let token = token.ok_or("Claude OAuth jetonu yok ya da süresi geçmiş")?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(10))
            .build();
        let resp = agent
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-beta", "oauth-2025-04-20")
            .call();
        match resp {
            Ok(r) => parse_usage(&r.into_json().map_err(|_| "kullanım yanıtı JSON değil".to_string())?),
            // Gövde/başlık loglanmaz: yalnız durum kodu. `Retry-After` yalnız bir sonraki
            // denemenin zamanlaması için bellekte tutulur.
            Err(ureq::Error::Status(code, r)) => {
                *self.retry_after.lock().unwrap() =
                    r.header("Retry-After").and_then(|v| parse_retry_after(v, crate::model_news::now_ms()));
                Err(format!("kullanım uç noktası HTTP {code}"))
            }
            Err(ureq::Error::Transport(_)) => Err("kullanım uç noktasına ulaşılamadı".into()),
        }
    }
}

/// Testlerde varsayılan: ağa çıkmaz, hep "okunamadı".
pub struct NoUsage;

impl UsageSource for NoUsage {
    fn read(&self) -> Result<Usage, String> {
        Err("kaynak yok".into())
    }
}

/// Son ölçüm durumu (Core'da bir Mutex arkasında).
#[derive(Default)]
pub struct State {
    pub last: Option<(i64, Usage)>,
    /// Zincirde hedef bulunamadı uyarısı bu eşik bölümünde basıldı mı (her ölçümde tekrar etmesin).
    pub warned: bool,
    /// #1033: aynı uyarı gerçek %100 tükenme bölümü için ayrı — 5 sa eşik notu haftalık
    /// tükenmenin notunu susturmaz (ve tersi).
    pub warned_full: bool,
}

impl State {
    /// #1033: "hedef yok" notu bu bölümde ilk kez mi — `breach` %100 ise tükenme bölümü, değilse
    /// eşik bölümü sayılır; `true` = bas (bayrak kalkar).
    pub fn warn_once(&mut self, breach: &Breach) -> bool {
        let flag = if breach.percent >= 100.0 { &mut self.warned_full } else { &mut self.warned };
        !std::mem::replace(flag, true)
    }

    /// Taze ölçümde bir pencere %100 mü (sıfırlanma saati okunmasa da) — tükenme bölümü sürüyor.
    fn full_at(&self, now: i64) -> bool {
        self.last.as_ref().is_some_and(|(at, u)| now - at <= STALE_MS && (u.five_hour >= 100.0 || u.seven_day >= 100.0))
    }

    /// Taze ölçüm eşiği aşıyorsa `Some`. Bayat (ya da hiç) ölçüm = bilinmiyor = `None`.
    pub fn breach_at(&self, now: i64) -> Option<Breach> {
        let (at, u) = self.last.as_ref()?;
        if now - at > STALE_MS {
            return None;
        }
        breach(u)
    }

    /// Taze ölçümde bir pencere %100 ve sıfırlanması gelecekteyse Claude BİLİNEN tükenmiştir:
    /// en geç sıfırlanma (epoch ms). Bayat ölçüm / okunamayan saat = bilinmiyor = `None`.
    pub fn exhausted_until(&self, now: i64) -> Option<i64> {
        self.exhausted_window(now).map(|(_, reset)| reset)
    }

    /// [`Self::exhausted_until`] ve o pencere (`Claude %100 (hafta)`): haftalık EŞİK yok ama
    /// gerçek %100 tükenme taze spawn'ı da yönlendirir (#1027, bilinen-bitmiş Claude'da boşa tur yok).
    pub fn exhausted_window(&self, now: i64) -> Option<(Breach, i64)> {
        let (at, u) = self.last.as_ref()?;
        if now - at > STALE_MS {
            return None;
        }
        [("5 sa", u.five_hour, &u.five_hour_resets_at), ("hafta", u.seven_day, &u.seven_day_resets_at)]
            .into_iter()
            .filter(|(_, pct, _)| *pct >= 100.0)
            .filter_map(|(window, percent, reset)| {
                let at = crate::chain::parse_rfc3339(reset.as_deref()?)?;
                (at > now).then(|| (Breach { window, percent, resets_at: reset.clone() }, at))
            })
            .max_by_key(|(_, at)| *at)
    }
}

/// #2019: Claude hesabı başına ([`crate::claude_accounts::ACCOUNTS`]) son ölçüm ve kaynağı.
pub struct Accounts(Vec<Account>);

type Account = (&'static str, std::sync::Mutex<State>, std::sync::Mutex<std::sync::Arc<dyn UsageSource>>);

impl Accounts {
    /// Testte kaynaklar ağa çıkmaz ([`NoUsage`]); gerçekte her hesap kendi jetonuyla.
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self(
            crate::claude_accounts::ACCOUNTS
                .iter()
                .map(|&id| {
                    let source: std::sync::Arc<dyn UsageSource> = if cfg!(test) {
                        std::sync::Arc::new(NoUsage)
                    } else {
                        std::sync::Arc::new(OAuthUsage::new(crate::claude_accounts::config_dir(data_dir, id)))
                    };
                    (id, std::sync::Mutex::default(), std::sync::Mutex::new(source))
                })
                .collect(),
        )
    }

    /// Hesabın ölçüm durumu; Claude hesabı değilse `None`.
    pub fn of(&self, provider: &str) -> Option<&std::sync::Mutex<State>> {
        self.0.iter().find(|(id, ..)| *id == provider).map(|(_, state, _)| state)
    }

    fn source(&self, provider: &str) -> Option<std::sync::Arc<dyn UsageSource>> {
        self.0.iter().find(|(id, ..)| *id == provider).map(|(.., source)| source.lock().unwrap().clone())
    }

    #[cfg(test)]
    fn set_source(&self, provider: &str, source: std::sync::Arc<dyn UsageSource>) {
        if let Some((.., s)) = self.0.iter().find(|(id, ..)| *id == provider) {
            *s.lock().unwrap() = source;
        }
    }
}

/// Kademe: iki pencereden yüksek doluluk %50 altı → 10 dk, %50–70 → 3 dk, %70+ → 60 sn.
pub fn tier_delay(u: &Usage) -> u64 {
    let pct = u.five_hour.max(u.seven_day);
    if pct >= 70.0 {
        MIN_POLL_MS
    } else if pct >= 50.0 {
        MID_POLL_MS
    } else {
        MAX_POLL_MS
    }
}

/// Başarılı ölçüm (`Some`) → kademe; hata (`None`) → aralık katlanır (taban [`MIN_POLL_MS`], en çok
/// [`MAX_POLL_MS`]).
pub fn next_delay(prev: u64, read: Option<&Usage>) -> u64 {
    match read {
        Some(u) => tier_delay(u),
        None => (prev.max(MIN_POLL_MS) * 2).min(MAX_POLL_MS),
    }
}

/// #2247: [`next_delay`] + hatada ucun `Retry-After` ipucu: bekleme en az o kadar (tavan
/// [`RETRY_AFTER_CAP_MS`]); ipucu yoksa ya da ölçüm başarılıysa aynen [`next_delay`].
pub fn next_delay_hinted(prev: u64, read: Option<&Usage>, retry_after_ms: Option<u64>) -> u64 {
    let base = next_delay(prev, read);
    match (read, retry_after_ms) {
        (None, Some(hint)) => base.max(hint.min(RETRY_AFTER_CAP_MS)),
        _ => base,
    }
}

/// İlk ölçüm açılıştan biraz sonra (kısa ömürlü test çekirdekleri uca istek atmasın).
const FIRST_POLL_MS: u64 = 5_000;

impl crate::Core {
    /// Arka plan ölçümü: Claude hesabı başına bir iş parçacığı; çekirdek yaşadıkça
    /// [`Self::quota_tick_for`], aralık [`next_delay`] ile.
    pub(crate) fn quota_watch(&self) {
        for provider in crate::claude_accounts::ACCOUNTS {
            let weak = self.weak_self.clone();
            std::thread::spawn(move || {
                let mut delay = FIRST_POLL_MS;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    let Some(core) = weak.upgrade() else { return };
                    if core.instance.lock().unwrap().is_none() {
                        return; // shutdown
                    }
                    let read = core
                        .quota_tick_for(provider)
                        .then(|| core.quota.of(provider).and_then(|q| q.lock().unwrap().last.clone()))
                        .flatten();
                    // #2247: 429 gibi hatada ucun söylediği kadar bekle (katlamadan kısa değilse).
                    let hint = if read.is_none() { core.quota.source(provider).and_then(|s| s.retry_after_ms()) } else { None };
                    delay = next_delay_hinted(delay, read.as_ref().map(|(_, u)| u), hint);
                }
            });
        }
    }

    /// Asıl Claude hesabının ölçümü ([`Self::quota_tick_for`]).
    #[cfg(test)]
    pub(crate) fn quota_tick(&self) -> bool {
        self.quota_tick_for("anthropic")
    }

    /// Bir hesabın ölçümü + eşik kararı. `false` = okunamadı (son ölçüm durur, bayatlarsa eşik
    /// bilinmiyor). Hata nedeni `usage_record`'da yalnız değişince loglanır.
    pub(crate) fn quota_tick_for(&self, provider: &'static str) -> bool {
        let (Some(state), Some(source)) = (self.quota.of(provider), self.quota.source(provider)) else { return false };
        let read = source.read();
        // #1861: aynı ölçüm kullanım şeridine de gider (ikinci sorgu yok).
        self.usage_record(provider, read.as_ref().map(crate::usage::Limits::from).map_err(Clone::clone));
        let now = crate::model_news::now_ms();
        let ok = match read {
            Ok(u) => {
                state.lock().unwrap().last = Some((now, u));
                true
            }
            Err(_) => false,
        };
        let breach = state.lock().unwrap().breach_at(now);
        match breach {
            Some(b) => self.quota_switch(provider, &b),
            None => state.lock().unwrap().warned = false,
        }
        // #1033: %100 tükenme ayrı bölüm: sürdükçe "hedef yok" notu ölçüm başına tekrarlanmaz.
        let mut q = state.lock().unwrap();
        if !q.full_at(now) {
            q.warned_full = false;
        }
        ok
    }

    /// #2019: hesabın taze ölçümü eşiği aşıyor mu (zincir seçimi bu hesaba worker taşımaz).
    pub(crate) fn quota_breached(&self, provider: &str) -> bool {
        self.quota.of(provider).is_some_and(|q| q.lock().unwrap().breach_at(crate::model_news::now_ms()).is_some())
    }

    #[cfg(test)]
    pub(crate) fn quota_set_source(&self, source: std::sync::Arc<dyn UsageSource>) {
        self.quota.set_source("anthropic", source);
    }

    #[cfg(test)]
    pub(crate) fn quota_set_source_for(&self, provider: &str, source: std::sync::Arc<dyn UsageSource>) {
        self.quota.set_source(provider, source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(five: f64, week: f64) -> Usage {
        Usage { five_hour: five, seven_day: week, ..Default::default() }
    }

    #[test]
    fn thresholds() {
        assert_eq!(breach(&u(84.9, 79.9)), None);
        assert_eq!(breach(&u(85.0, 10.0)).unwrap().window, "5 sa");
        // Weekly window no longer moves workers off Claude.
        assert_eq!(breach(&u(10.0, 80.0)), None);
        assert_eq!(breach(&u(0.0, 99.0)), None);
        assert_eq!(breach(&u(90.0, 95.0)).unwrap().label(), "Claude %90 (5 sa)");
    }

    #[test]
    fn parses_usage_body() {
        let v = serde_json::json!({
            "five_hour": {"utilization": 42.5, "resets_at": "2026-10-06T15:00:00+00:00"},
            "seven_day": {"utilization": 81.0, "resets_at": "2026-10-09T00:00:00+00:00"},
            "seven_day_opus": null,
        });
        let got = parse_usage(&v).unwrap();
        assert_eq!(got.five_hour, 42.5);
        assert_eq!(got.seven_day, 81.0);
        assert_eq!(got.five_hour_resets_at.as_deref(), Some("2026-10-06T15:00:00+00:00"));
        let partial = parse_usage(&serde_json::json!({"five_hour": null, "seven_day": {"utilization": 3}})).unwrap();
        assert_eq!((partial.five_hour, partial.seven_day), (0.0, 3.0));
        assert!(parse_usage(&serde_json::json!({"error": "x"})).is_err());
    }

    #[test]
    fn stale_reading_is_unknown() {
        let s = State { last: Some((1_000, u(99.0, 99.0))), ..Default::default() };
        assert!(s.breach_at(1_000 + STALE_MS).is_some());
        assert!(s.breach_at(1_001 + STALE_MS).is_none());
        assert!(State::default().breach_at(0).is_none());
    }

    /// #1018: yalnız taze, %100 ve saati okunan gelecekteki pencere "bilinen tükenmiş".
    #[test]
    fn exhausted_only_when_full_fresh_and_reset_known() {
        let now = 1_790_346_821_000; // 2026-09-25T14:33:41Z
        let at = |five: f64, week: f64, five_r: Option<&str>, week_r: Option<&str>| State {
            last: Some((now, Usage {
                five_hour: five,
                seven_day: week,
                five_hour_resets_at: five_r.map(str::to_string),
                seven_day_resets_at: week_r.map(str::to_string),
            })),
            ..Default::default()
        };
        let later = Some("2026-09-25T16:00:00+00:00");
        let week = Some("2026-09-28T00:00:00.5+00:00");
        assert_eq!(at(99.9, 50.0, later, week).exhausted_until(now), None, "%85 eşiği tükenme değil");
        assert_eq!(at(100.0, 50.0, later, week).exhausted_until(now), crate::chain::parse_rfc3339(later.unwrap()));
        assert_eq!(at(10.0, 100.0, later, week).exhausted_until(now), crate::chain::parse_rfc3339(week.unwrap()), "haftalık dolu");
        assert_eq!(at(100.0, 100.0, later, week).exhausted_until(now), crate::chain::parse_rfc3339(week.unwrap()), "en geç");
        assert_eq!(at(100.0, 50.0, None, week).exhausted_until(now), None, "saat yok = bilinmiyor");
        assert_eq!(at(100.0, 50.0, Some("yarın"), week).exhausted_until(now), None);
        assert_eq!(at(100.0, 50.0, Some("2026-09-25T14:00:00Z"), week).exhausted_until(now), None, "geçmiş");
        assert_eq!(at(100.0, 50.0, later, week).exhausted_until(now + STALE_MS + 1), None, "bayat");
        // #1027: hangi pencerenin bittiği bildirim etiketine taşınır.
        let (b, _) = at(10.0, 100.0, later, week).exhausted_window(now).unwrap();
        assert_eq!((b.label(), b.resets_at.as_deref()), ("Claude %100 (hafta)".to_string(), week));
    }

    /// #1033: 5 sa eşik notu ile %100 tükenme notu ayrı bölümler; her biri bölüm başına bir kez.
    #[test]
    fn threshold_and_full_warn_separately() {
        let mut s = State::default();
        let five = Breach { window: "5 sa", percent: 90.0, resets_at: None };
        let week = Breach { window: "hafta", percent: 100.0, resets_at: None };
        assert!(s.warn_once(&five));
        assert!(!s.warn_once(&five), "eşik bölümünde tek not");
        assert!(s.warn_once(&week), "eşik notu tükenme notunu susturmaz");
        assert!(!s.warn_once(&week));
        let now = 1_000_000;
        s.last = Some((now, u(10.0, 100.0)));
        assert!(s.full_at(now) && !s.full_at(now + STALE_MS + 1), "bayat ölçüm bölümü bitirir");
        s.last = Some((now, u(100.0, 10.0)));
        assert!(s.full_at(now));
        s.last = Some((now, u(99.9, 99.9)));
        assert!(!s.full_at(now));
    }

    #[test]
    fn stale_limit_covers_longest_interval() {
        // 10 dk kademesinde bir sonraki ölçüm (+ istek süresi) gelene dek ölçüm taze kalır.
        let s = State { last: Some((0, u(99.0, 99.0))), ..Default::default() };
        assert!(s.breach_at(MAX_POLL_MS as i64 + 13_000).is_some());
    }

    #[test]
    fn tier_boundaries() {
        assert_eq!(tier_delay(&u(49.9, 0.0)), MAX_POLL_MS);
        assert_eq!(tier_delay(&u(50.0, 0.0)), MID_POLL_MS);
        assert_eq!(tier_delay(&u(69.9, 0.0)), MID_POLL_MS);
        assert_eq!(tier_delay(&u(70.0, 0.0)), MIN_POLL_MS);
        assert_eq!(tier_delay(&u(0.0, 0.0)), MAX_POLL_MS);
        assert_eq!(tier_delay(&u(100.0, 0.0)), MIN_POLL_MS);
    }

    #[test]
    fn tier_uses_higher_window() {
        assert_eq!(tier_delay(&u(10.0, 72.0)), MIN_POLL_MS);
        assert_eq!(tier_delay(&u(72.0, 10.0)), MIN_POLL_MS);
        assert_eq!(tier_delay(&u(49.9, 55.0)), MID_POLL_MS);
        assert_eq!(tier_delay(&u(55.0, 49.9)), MID_POLL_MS);
    }

    #[test]
    fn first_poll_without_reading_starts_at_60s() {
        // Açılışta ölçüm yok: ilk deneme okunamazsa geri çekilme 60 sn tabanından katlanır.
        assert_eq!(next_delay(FIRST_POLL_MS, None), 2 * MIN_POLL_MS);
        assert_eq!(next_delay(FIRST_POLL_MS, Some(&u(20.0, 20.0))), MAX_POLL_MS);
    }

    #[test]
    fn backoff_doubles_caps_and_returns_to_tier() {
        assert_eq!(next_delay(MIN_POLL_MS, None), 120_000);
        assert_eq!(next_delay(120_000, None), 240_000);
        assert_eq!(next_delay(480_000, None), MAX_POLL_MS);
        assert_eq!(next_delay(MAX_POLL_MS, None), MAX_POLL_MS);
        // Geri çekilme biter bitmez (başarılı ölçüm) kademe kuralı.
        assert_eq!(next_delay(MAX_POLL_MS, Some(&u(75.0, 0.0))), MIN_POLL_MS);
        assert_eq!(next_delay(MAX_POLL_MS, Some(&u(0.0, 60.0))), MID_POLL_MS);
        assert_eq!(next_delay(240_000, Some(&u(10.0, 10.0))), MAX_POLL_MS);
    }

    /// #2247: `Retry-After` saniye ya da HTTP tarihi; okunamayan/geçmiş = ipucu yok.
    #[test]
    fn retry_after_parses_seconds_and_http_date() {
        let now = 1_445_412_480_000; // 2015-10-21T07:28:00Z
        assert_eq!(parse_retry_after("120", now), Some(120_000));
        assert_eq!(parse_retry_after(" 7 ", now), Some(7_000));
        assert_eq!(parse_retry_after("0", now), Some(0));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:30:00 GMT", now), Some(120_000));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now), None, "geçmiş/şimdi = ipucu yok");
        assert_eq!(parse_retry_after("yarın", now), None);
        assert_eq!(parse_retry_after("", now), None);
        assert_eq!(parse_retry_after("-5", now), None);
    }

    /// #2247: hatada ipucu beklemeyi uzatır (katlamadan kısa değilse), tavanı aşmaz; başarıda yok sayılır.
    #[test]
    fn retry_after_extends_backoff_with_cap() {
        // İlk denemede 429 + Retry-After 300 sn: katlama 120 sn derdi, uç 300 sn dedi.
        assert_eq!(next_delay_hinted(FIRST_POLL_MS, None, Some(300_000)), 300_000);
        // Uç katlamadan kısa süre söylerse katlama kalır (daha sık vurulmaz).
        assert_eq!(next_delay_hinted(MIN_POLL_MS, None, Some(5_000)), 120_000);
        // Tavan: bir saat söylese bile 30 dk.
        assert_eq!(next_delay_hinted(MIN_POLL_MS, None, Some(3_600_000)), RETRY_AFTER_CAP_MS);
        // İpucu yok → eski geri çekilme aynen.
        assert_eq!(next_delay_hinted(120_000, None, None), next_delay(120_000, None));
        // Başarılı ölçümde bayat ipucu kademeyi değiştirmez.
        assert_eq!(next_delay_hinted(MAX_POLL_MS, Some(&u(75.0, 0.0)), Some(900_000)), MIN_POLL_MS);
    }

    /// #2247: gerçek kaynak hata yanıtının başlığını ipucu olarak saklar, başarı/başlıksız hata siler.
    #[test]
    fn oauth_source_keeps_retry_hint_only_from_last_error() {
        let s = OAuthUsage::new(None);
        assert_eq!(s.retry_after_ms(), None);
        *s.retry_after.lock().unwrap() = Some(42);
        assert_eq!(s.retry_after_ms(), Some(42));
        // Bağlı olmayan ek hesap: ağa çıkmadan hata, ipucu temizlenir.
        let s2 = OAuthUsage::new(Some(std::env::temp_dir().join("dex-no-such-claude-account")));
        *s2.retry_after.lock().unwrap() = Some(42);
        assert!(s2.read().is_err());
        assert_eq!(s2.retry_after_ms(), None);
    }
}
