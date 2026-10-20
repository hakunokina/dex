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
//!
//! #2252: aynı jetonla birden çok Dex süreci (üretim + dev + `dexd serve`) ayrı ayrı ölçünce uç 429
//! veriyordu. Artık hesap başına makine geneli bir durum dosyası var (`usage_share.rs`): her tick
//! önce dosyaya bakar; ölçüm kademesinden tazeyse ya da `nextAllowedAt` gelmemişse uca gitmez,
//! dosyadaki değeri kullanır. Gerekiyorsa OS kilidini alır, dosyayı yeniden okur, hâlâ gerekiyorsa
//! ölçer ve atomik yazar. Kilidi alamayan beklemez. Döngünün uykusu da dosyanın `nextAllowedAt`'i.

use serde::{Deserialize, Serialize};

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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
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

/// #2019: Claude hesabı başına ([`crate::claude_accounts::ACCOUNTS`]) son ölçüm, kaynağı ve
/// (#2252) süreçler arası ortak dosyası.
pub struct Accounts(Vec<Account>);

struct Account {
    id: &'static str,
    state: std::sync::Mutex<State>,
    source: std::sync::Mutex<std::sync::Arc<dyn UsageSource>>,
    share: std::sync::Mutex<crate::usage_share::Store>,
}

impl Accounts {
    /// Testte kaynaklar ağa çıkmaz ([`NoUsage`]) ve ortak dosya veri dizini altındadır; gerçekte
    /// her hesap kendi jetonuyla, dosya anahtarı jetonun config dizini.
    pub fn new(data_dir: &std::path::Path) -> Self {
        let root = crate::usage_share::root(data_dir);
        Self(
            crate::claude_accounts::ACCOUNTS
                .iter()
                .map(|&id| {
                    let dir = crate::claude_accounts::config_dir(data_dir, id);
                    let source: std::sync::Arc<dyn UsageSource> =
                        if cfg!(test) { std::sync::Arc::new(NoUsage) } else { std::sync::Arc::new(OAuthUsage::new(dir.clone())) };
                    Account {
                        id,
                        state: std::sync::Mutex::default(),
                        source: std::sync::Mutex::new(source),
                        share: std::sync::Mutex::new(Self::store(&root, dir)),
                    }
                })
                .collect(),
        )
    }

    /// Ek hesabın anahtarı kendi dizini; asıl hesabınki `claude_accounts::main_dir()` (ev dizini
    /// bile yoksa sabit bir ad: tek dosya, jeton zaten kasadan).
    fn store(root: &std::path::Path, dir: Option<std::path::PathBuf>) -> crate::usage_share::Store {
        let key = dir
            .or_else(crate::claude_accounts::main_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("claude-main"));
        crate::usage_share::Store::new(root.to_path_buf(), &key)
    }

    fn account(&self, provider: &str) -> Option<&Account> {
        self.0.iter().find(|a| a.id == provider)
    }

    /// Hesabın ölçüm durumu; Claude hesabı değilse `None`.
    pub fn of(&self, provider: &str) -> Option<&std::sync::Mutex<State>> {
        self.account(provider).map(|a| &a.state)
    }

    fn source(&self, provider: &str) -> Option<std::sync::Arc<dyn UsageSource>> {
        self.account(provider).map(|a| a.source.lock().unwrap().clone())
    }

    /// #2252: hesabın ortak dosyası; Claude hesabı değilse `None`.
    pub(crate) fn share(&self, provider: &str) -> Option<crate::usage_share::Store> {
        self.account(provider).map(|a| a.share.lock().unwrap().clone())
    }

    #[cfg(test)]
    fn set_source(&self, provider: &str, source: std::sync::Arc<dyn UsageSource>) {
        if let Some(a) = self.account(provider) {
            *a.source.lock().unwrap() = source;
        }
    }

    /// Testte: iki çekirdek aynı "ev dizinini" paylaşsın (anahtarlar aynı kalır).
    #[cfg(test)]
    fn set_share_root(&self, data_dir: &std::path::Path, root: &std::path::Path) {
        for a in &self.0 {
            *a.share.lock().unwrap() = Self::store(root, crate::claude_accounts::config_dir(data_dir, a.id));
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
    /// [`Self::quota_tick_for`]. #2252: uyku ortak dosyanın `nextAllowedAt`'i (kademe, geri çekilme
    /// ve `Retry-After` orada); dosya yoksa (kilit başkasında) kısa sürede yeniden bakılır.
    /// Testte iş parçacığı yok: kısa ömürlü çekirdekler ortak dosyaya "kaynak yok" yazmasın.
    pub(crate) fn quota_watch(&self) {
        if cfg!(test) {
            return;
        }
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
                    let shared = core.quota_tick_shared(provider);
                    delay = shared.map_or(crate::usage_share::RETRY_MS, |s| s.wait_ms(crate::model_news::now_ms()));
                }
            });
        }
    }

    /// Asıl Claude hesabının zorunlu ölçümü: ortak dosya silinir, kaynak okunur
    /// ([`Self::quota_tick_for`]; testler her tick'te yeni değer bekler).
    #[cfg(test)]
    pub(crate) fn quota_tick(&self) -> bool {
        if let Some(share) = self.quota.share("anthropic") {
            share.clear();
        }
        self.quota_tick_for("anthropic")
    }

    /// Bir hesabın ölçümü + eşik kararı. `false` = son deneme (bu ya da başka süreçte) okunamadı
    /// (son ölçüm durur, bayatlarsa eşik bilinmiyor) ya da kilit başkasındaydı ve dosya yoktu.
    /// Hata nedeni `usage_record`'da yalnız değişince loglanır.
    pub(crate) fn quota_tick_for(&self, provider: &'static str) -> bool {
        self.quota_tick_shared(provider).is_some_and(|s| s.ok())
    }

    /// #2252: tek uçuş. Dosya tazeyse ya da `nextAllowedAt` gelmemişse ağa çıkmadan dosyadaki durum;
    /// değilse kilit → yeniden oku → hâlâ gerekiyorsa ölç → yaz. Kilit başkasındaysa dosyadaki durum
    /// (yoksa `None`: bu tick'te uygulanacak bir şey yok). Kilit dosyası açılamıyorsa kilitsiz ölçülür
    /// (tek süreç gibi; yazılamazsa durum yalnız bellekte kalır).
    fn quota_tick_shared(&self, provider: &'static str) -> Option<crate::usage_share::Shared> {
        let (Some(state), Some(source), Some(share)) =
            (self.quota.of(provider), self.quota.source(provider), self.quota.share(provider))
        else {
            return None;
        };
        let now = crate::model_news::now_ms();
        let mut shared = share.read();
        if shared.as_ref().is_none_or(|s| s.due(now)) {
            let (guard, measure) = match share.try_lock() {
                Ok(Some(guard)) => (Some(guard), true),
                // Başka süreç ölçüyor: beklenmez, dosyadaki (varsa) kullanılır.
                Ok(None) => (None, false),
                Err(e) => {
                    eprintln!("dex: {provider} kullanım kilidi açılamadı, kilitsiz ölçülüyor: {e}");
                    (None, true)
                }
            };
            if measure {
                // Kilit bizde: başka süreç az önce ölçmüş olabilir, yeniden oku.
                shared = share.read();
                if shared.as_ref().is_none_or(|s| s.due(now)) {
                    let read = source.read();
                    let now = crate::model_news::now_ms();
                    let hint = if read.is_err() { source.retry_after_ms() } else { None };
                    let next = crate::usage_share::Shared::after(shared.as_ref(), read, hint, now);
                    if let Err(e) = share.write(&next) {
                        eprintln!("dex: {provider} ortak kullanım dosyası yazılamadı: {e}");
                    }
                    shared = Some(next);
                }
            }
            drop(guard);
        }
        let shared = shared?;
        // #1861: aynı ölçüm kullanım şeridine de gider (ikinci sorgu yok); ölçüm anı dosyadaki.
        let last = shared.measured_at.zip(shared.usage.as_ref());
        self.usage_apply(provider, last.map(|(at, u)| (at, crate::usage::Limits::from(u))), shared.last_error.clone());
        if let Some((at, u)) = last {
            state.lock().unwrap().last = Some((at, u.clone()));
        }
        self.quota_decide(provider, state, now);
        Some(shared)
    }

    /// Taze ölçüme göre eşik kararı ve uyarı bayrakları.
    fn quota_decide(&self, provider: &'static str, state: &std::sync::Mutex<State>, now: i64) {
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

    /// #2252 testleri: ortak dosyanın kökü (iki çekirdek aynı "ev dizini"ni paylaşsın).
    #[cfg(test)]
    pub(crate) fn quota_set_share_root(&self, root: &std::path::Path) {
        self.quota.set_share_root(&self.paths.data_dir, root);
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

    // ── #2252: süreçler arası tek ölçüm ──────────────────────────────────────

    /// Sahte kaynak: kaç kez ağa "çıkıldığını" sayar.
    struct Counting {
        reads: std::sync::atomic::AtomicUsize,
        value: std::sync::Mutex<Result<Usage, String>>,
        hint: std::sync::Mutex<Option<u64>>,
    }

    impl Counting {
        fn new(value: Result<Usage, String>) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self { reads: Default::default(), value: std::sync::Mutex::new(value), hint: Default::default() })
        }
        fn reads(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn set(&self, value: Result<Usage, String>) {
            *self.value.lock().unwrap() = value;
        }
    }

    impl UsageSource for Counting {
        fn read(&self) -> Result<Usage, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.value.lock().unwrap().clone()
        }
        fn retry_after_ms(&self) -> Option<u64> {
            *self.hint.lock().unwrap()
        }
    }

    fn core(tag: &str, share_root: &std::path::Path) -> (std::sync::Arc<crate::Core>, std::sync::Arc<Counting>) {
        let root = share_root.join(tag);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        core.quota_set_share_root(&share_root.join("home"));
        let src = Counting::new(Ok(u(20.0, 10.0)));
        core.quota_set_source(src.clone());
        (core, src)
    }

    fn share_root(tag: &str) -> std::path::PathBuf {
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/quota-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn strip(core: &crate::Core) -> crate::usage::ProviderUsage {
        core.usage_get().providers.into_iter().find(|p| p.id == "anthropic").unwrap()
    }

    /// Taze dosya varken ağa gidilmez: değer dosyadan, eşik ve şerit her zamanki gibi. Dosyada
    /// jeton/gövde yok. Bağlantı unutulunca dosya silinir.
    #[test]
    fn fresh_shared_file_skips_network() {
        let root = share_root("fresh");
        let (core, src) = core("a", &root);
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 1);
        for _ in 0..3 {
            assert!(core.quota_tick_for("anthropic"));
        }
        assert_eq!(src.reads(), 1, "10 dk kademesinde ölçüm taze: uca gidilmez");
        let share = core.quota.share("anthropic").unwrap();
        let s = share.read().unwrap();
        assert_eq!((s.usage.as_ref().map(|u| u.five_hour), s.last_error), (Some(20.0), None));
        let at = s.measured_at.unwrap();
        assert_eq!(core.quota.of("anthropic").unwrap().lock().unwrap().last.as_ref().map(|(a, _)| *a), Some(at));
        let p = strip(&core);
        assert_eq!((p.measured_at, p.note, p.limits.five_hour.map(|w| w.remaining_percent)), (Some(at), None, Some(80.0)));
        // Kaynak değişse de dosya tazeyken eski değer: eşik dosyadaki değere göre.
        src.set(Ok(u(90.0, 10.0)));
        assert!(core.quota_tick_for("anthropic"));
        assert!(!core.quota_breached("anthropic"));
        assert_eq!(src.reads(), 1);
        // Zorunlu ölçüm (test yardımcısı dosyayı siler) → yeni değer, eşik.
        assert!(core.quota_tick());
        assert_eq!(src.reads(), 2);
        assert!(core.quota_breached("anthropic"));
        // %90 → 60 sn kademesi: 61 sn önce ölçülmüş gibi dosya → uca gidilir.
        let mut s = share.read().unwrap();
        s.measured_at = Some(s.measured_at.unwrap() - MIN_POLL_MS as i64 - 1_000);
        s.next_allowed_at = s.measured_at.unwrap() + MIN_POLL_MS as i64;
        share.write(&s).unwrap();
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 3);
        // Dosyada jeton/gövde yok; anahtarın kaynağı var.
        let dir = root.join("home");
        let json = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == "json")).unwrap();
        let raw = std::fs::read_to_string(&json).unwrap();
        assert!(!raw.contains("accessToken") && !raw.contains("Bearer") && !raw.contains("utilization"), "{raw}");
        assert!(raw.contains("\"configDir\""));
        core.usage_forget("anthropic");
        assert_eq!(share.read(), None, "bağlantı kalkınca dosya silinir");
        assert!(strip(&core).measured_at.is_none());
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Hatada `nextAllowedAt` (Retry-After dahil) gelene dek uca gidilmez; önceki değer ve neden
    /// şeritte durur. Süre dolunca yeniden denenir.
    #[test]
    fn next_allowed_at_is_honoured_across_ticks() {
        let root = share_root("hold");
        let (core, src) = core("a", &root);
        assert!(core.quota_tick_for("anthropic"));
        let share = core.quota.share("anthropic").unwrap();
        let good_at = share.read().unwrap().measured_at.unwrap();
        // Ölçüm bayatlamış olsun (şerit bayatlığı da geçmiş); uç 429 + Retry-After 300 sn desin.
        let mut s = share.read().unwrap();
        s.measured_at = Some(good_at - STALE_MS - 1_000);
        s.next_allowed_at = s.measured_at.unwrap();
        share.write(&s).unwrap();
        src.set(Err("kullanım uç noktası HTTP 429".into()));
        *src.hint.lock().unwrap() = Some(300_000);
        assert!(!core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 2);
        let s = share.read().unwrap();
        assert_eq!(s.last_error.as_deref(), Some("kullanım uç noktası HTTP 429"));
        assert!(s.next_allowed_at - s.attempted_at == 300_000, "Retry-After kadar");
        assert_eq!(s.usage.as_ref().map(|u| u.five_hour), Some(20.0), "önceki değer durur");
        for _ in 0..3 {
            assert!(!core.quota_tick_for("anthropic"));
        }
        assert_eq!(src.reads(), 2, "nextAllowedAt gelmeden uca gidilmez");
        let p = strip(&core);
        assert_eq!((p.note.as_deref(), p.limits.five_hour.map(|w| w.remaining_percent)), (Some("kullanım uç noktası HTTP 429"), Some(80.0)));
        assert!(s.wait_ms(crate::model_news::now_ms()) > 290_000, "döngü o kadar uyur");
        // Süre doldu (dosyada geçmişe çekilir): yeniden denenir, başarı notu siler.
        let mut s = share.read().unwrap();
        s.next_allowed_at = crate::model_news::now_ms() - 1;
        share.write(&s).unwrap();
        src.set(Ok(u(30.0, 10.0)));
        *src.hint.lock().unwrap() = None;
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 3);
        let p = strip(&core);
        assert_eq!((p.note, p.limits.five_hour.map(|w| w.remaining_percent)), (None, Some(70.0)));
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Aynı ev dizinindeki iki çekirdek: biri ölçer, öteki dosyadan okur (ağa çıkmaz). Kilit
    /// başkasındayken beklenmez, dosyadaki (bayat da olsa) değer kullanılır; kilit düşünce ölçülür.
    #[test]
    fn two_cores_share_one_measurement() {
        let root = share_root("two");
        let (a, src_a) = core("a", &root);
        let (b, src_b) = core("b", &root);
        assert!(a.quota_tick_for("anthropic"));
        assert!(b.quota_tick_for("anthropic"));
        assert_eq!((src_a.reads(), src_b.reads()), (1, 0), "tek ölçüm");
        let at_a = a.quota.of("anthropic").unwrap().lock().unwrap().last.clone().unwrap();
        let at_b = b.quota.of("anthropic").unwrap().lock().unwrap().last.clone().unwrap();
        assert_eq!(at_a, at_b, "B, A'nın ölçümünü aynı anıyla kullanır");
        assert_eq!(strip(&b).limits.five_hour.map(|w| w.remaining_percent), Some(80.0));
        // Dosya bayat ve A kilidi tutuyor (ölçüm sürüyor): B beklemez, dosyadakini kullanır.
        let share = a.quota.share("anthropic").unwrap();
        let mut s = share.read().unwrap();
        s.measured_at = Some(s.measured_at.unwrap() - MAX_POLL_MS as i64 - 1_000);
        s.next_allowed_at = s.measured_at.unwrap();
        share.write(&s).unwrap();
        let guard = share.try_lock().unwrap().expect("A kilidi alır");
        src_b.set(Ok(u(50.0, 10.0)));
        assert!(b.quota_tick_for("anthropic"), "dosyadaki değer uygulanır");
        assert_eq!(src_b.reads(), 0, "kilit başkasında: ağa çıkılmaz");
        assert_eq!(share.read().unwrap().wait_ms(crate::model_news::now_ms()), crate::usage_share::RETRY_MS, "kısa sürede yeniden bakar");
        drop(guard);
        assert!(b.quota_tick_for("anthropic"));
        assert_eq!(src_b.reads(), 1, "kilit düşünce B ölçer");
        assert_eq!(share.read().unwrap().usage.as_ref().map(|u| u.five_hour), Some(50.0));
        // A artık B'nin taze ölçümünü kullanır.
        assert!(a.quota_tick_for("anthropic"));
        assert_eq!(src_a.reads(), 1);
        assert_eq!(a.quota.of("anthropic").unwrap().lock().unwrap().last.as_ref().map(|(_, u)| u.five_hour), Some(50.0));
        // Kilit başkasında ve dosya yok: bu tick'te uygulanacak şey yok, durum dokunulmaz.
        share.clear();
        let guard = share.try_lock().unwrap().unwrap();
        assert!(!b.quota_tick_for("anthropic"));
        assert_eq!(src_b.reads(), 1);
        assert_eq!(b.quota.of("anthropic").unwrap().lock().unwrap().last.as_ref().map(|(_, u)| u.five_hour), Some(50.0));
        drop(guard);
        a.shutdown();
        b.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Bozuk ya da çok eski dosya güvenle yok sayılır: ölçülür ve üstüne yazılır.
    #[test]
    fn corrupt_or_stale_file_is_replaced() {
        let root = share_root("corrupt");
        let (core, src) = core("a", &root);
        let share = core.quota.share("anthropic").unwrap();
        assert!(core.quota_tick_for("anthropic"));
        let path = {
            let dir = root.join("home");
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == "json")).unwrap()
        };
        std::fs::write(&path, "{yarım").unwrap();
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 2, "bozuk dosya = yok");
        assert!(share.read().is_some(), "üstüne yazıldı");
        std::fs::write(&path, r#"{"measuredAt":"dün","usage":{"fiveHour":"çok"}}"#).unwrap();
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 3);
        // Çok eski ölçüm (bayat, nextAllowedAt geçmiş): ölçülür.
        let mut s = share.read().unwrap();
        s.measured_at = Some(1_000);
        s.attempted_at = 1_000;
        s.next_allowed_at = 1_000 + MAX_POLL_MS as i64;
        share.write(&s).unwrap();
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 4);
        assert!(share.read().unwrap().fresh(crate::model_news::now_ms()));
        // Saçma ileri tarihli nextAllowedAt (bozuk): tutmaz, ölçülür.
        let mut s = share.read().unwrap();
        s.measured_at = Some(1_000);
        s.next_allowed_at = crate::model_news::now_ms() + 10 * RETRY_AFTER_CAP_MS as i64;
        share.write(&s).unwrap();
        assert!(core.quota_tick_for("anthropic"));
        assert_eq!(src.reads(), 5);
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
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
