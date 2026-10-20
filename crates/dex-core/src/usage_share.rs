//! #2252 — Claude kullanım ölçümünde süreçler arası tek uçuş (single-flight).
//!
//! Asıl Claude hesabının jetonu ortak `~/.claude/.credentials.json`'dan okunur; üretim uygulaması,
//! dev uygulaması ve elle açılan `dexd serve` her biri kendi `quota_watch`'ıyla aynı jetonla
//! `oauth/usage`'a gidince uç 429 verir (#2247'de bulunan çarpan). Çözüm: hesap başına makine
//! geneli bir durum dosyası + OS düzeyi danışma kilidi; "asıl süreç"/bayrak kavramı yok.
//!
//! - Dosya: `<home>/.dex/usage/<anahtar>.json`; anahtar jetonun geldiği config dizininin özeti
//!   (asıl hesap için `claude_accounts::main_dir()`, ek hesap için kendi `CLAUDE_CONFIG_DIR`'ı).
//!   Farklı `CLAUDE_CONFIG_DIR` farklı dosya. İçerik ([`Shared`]): son başarılı ölçüm + anı, son
//!   deneme anı, `nextAllowedAt` (kademe/geri çekilme/`Retry-After`), son hatanın kısa nedeni.
//!   Jeton ve yanıt gövdesi asla yazılmaz (yalnız ayrıştırılmış yüzdeler ve durum kodu metni).
//! - Kilit: `<anahtar>.lock` üstünde `File::try_lock` (Windows `LockFileEx`, Unix `flock`; aynı
//!   mekanizma `instance.rs`'te). Sahibi çökerse OS bırakır; dosya varlığına dayanan kalıp yok.
//!   Kilidi alamayan beklemez, dosyadaki değeri kullanır ([`crate::Core::quota_tick_for`]).
//! - Yazma atomik: geçici dosya + rename. Bozuk/okunamayan dosya "yok" sayılır ve ölçümle üstüne
//!   yazılır; saçma `nextAllowedAt` (30 dk'dan ilerisi) yok sayılır.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// Kilit başkasındayken ya da dosya henüz yokken bir sonraki bakış (ölçüm değil, dosya okuması).
pub const RETRY_MS: u64 = 5_000;

/// Dosya içeriği. Alanlar camelCase; `usage` [`crate::quota::Usage`]'ın kendisi.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    /// Anahtarın kaynağı (okunabilirlik için; sır değil).
    #[serde(default)]
    pub config_dir: String,
    /// Son başarılı ölçüm ve anı (epoch ms); hiç başarı yoksa `None`.
    #[serde(default)]
    pub measured_at: Option<i64>,
    #[serde(default)]
    pub usage: Option<crate::quota::Usage>,
    /// Son deneme (başarı ya da hata) anı.
    #[serde(default)]
    pub attempted_at: i64,
    /// Bundan önce uca gidilmez (kademe, geri çekilme ya da `Retry-After`).
    #[serde(default)]
    pub next_allowed_at: i64,
    /// Son denemenin hatası (`kullanım uç noktası HTTP 429`); son deneme başarılıysa `None`.
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Shared {
    /// Ölçüm, kendi kademesinin aralığından yeni mi (ileri tarihli = bozuk = taze değil).
    pub fn fresh(&self, now: i64) -> bool {
        match (self.measured_at, &self.usage) {
            (Some(at), Some(u)) => (0..crate::quota::tier_delay(u) as i64).contains(&(now - at)),
            _ => false,
        }
    }

    /// Uca gitmek gerekiyor mu: ölçüm taze değil ve `nextAllowedAt` gelmiş (ya da saçma ileride).
    pub fn due(&self, now: i64) -> bool {
        !self.fresh(now) && !self.holds(now)
    }

    /// `now < nextAllowedAt` ve makul (30 dk tavan) → henüz gidilmez.
    fn holds(&self, now: i64) -> bool {
        now < self.next_allowed_at && self.next_allowed_at - now <= crate::quota::RETRY_AFTER_CAP_MS as i64
    }

    /// Döngünün uykusu: `nextAllowedAt`'e kadar, en az [`RETRY_MS`] (geçmişse kısa sürede yeniden
    /// bak: başka süreç ölçmüş olabilir), en çok [`crate::quota::RETRY_AFTER_CAP_MS`].
    pub fn wait_ms(&self, now: i64) -> u64 {
        let until = self.next_allowed_at.saturating_sub(now).max(0) as u64;
        until.clamp(RETRY_MS, crate::quota::RETRY_AFTER_CAP_MS)
    }

    /// Son deneme başarılı mı (değer var, hata yok).
    pub fn ok(&self) -> bool {
        self.last_error.is_none() && self.usage.is_some()
    }

    /// Bir denemenin ardından yeni içerik. Başarı: değer + `tier_delay`. Hata: önceki değer durur,
    /// aralık önceki aralıktan katlanır (`next_delay_hinted`; ilk hatada 2 dk), `Retry-After` ipucu
    /// en azı belirler.
    pub fn after(prev: Option<&Shared>, read: Result<crate::quota::Usage, String>, retry_after_ms: Option<u64>, now: i64) -> Shared {
        let prev_interval = prev.map_or(0, |p| (p.next_allowed_at - p.attempted_at).max(0) as u64);
        let delay = crate::quota::next_delay_hinted(prev_interval, read.as_ref().ok(), retry_after_ms);
        let base = prev.cloned().unwrap_or_default();
        match read {
            Ok(u) => Shared {
                measured_at: Some(now),
                usage: Some(u),
                attempted_at: now,
                next_allowed_at: now + delay as i64,
                last_error: None,
                ..base
            },
            Err(e) => Shared { attempted_at: now, next_allowed_at: now + delay as i64, last_error: Some(e), ..base },
        }
    }
}

/// Bir hesabın dosyası ve kilidi.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    key: String,
    config_dir: String,
}

/// Kilit tutulduğu sürece yaşar; düşünce bırakılır (Unix'te fork edilmiş çocuk tanıtıcısı
/// kilidi uzatmasın diye açıkça `unlock`, `instance.rs` ile aynı gerekçe).
pub struct Guard(File);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Makine geneli kök: `<home>/.dex/usage`; ev dizini yoksa `<data_dir>/usage-share` (tek süreç).
/// Testlerde hep veri dizini altı (gerçek `~/.dex`'e yazılmaz).
pub fn root(data_dir: &Path) -> PathBuf {
    let home = if cfg!(test) { None } else { dirs::home_dir() };
    home.map(|h| h.join(".dex").join("usage")).unwrap_or_else(|| data_dir.join("usage-share"))
}

/// Config dizininden dosya anahtarı: mutlak yol (Windows'ta küçük harf, `\`), sha256'nın ilk 16 hex'i.
fn key_of(config_dir: &Path) -> (String, String) {
    use sha2::Digest;
    let abs = std::path::absolute(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let mut s = abs.to_string_lossy().into_owned();
    if cfg!(windows) {
        s = s.replace('/', "\\").to_lowercase();
    }
    let s = s.trim_end_matches(['\\', '/']).to_string();
    let digest = sha2::Sha256::digest(s.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    (hex, s)
}

impl Store {
    pub fn new(root: PathBuf, config_dir: &Path) -> Self {
        let (key, config_dir) = key_of(config_dir);
        Self { root, key, config_dir }
    }

    fn path(&self) -> PathBuf {
        self.root.join(format!("{}.json", self.key))
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(format!("{}.lock", self.key))
    }

    /// Dosyadaki durum; yok/bozuk/okunamıyor = `None`.
    pub fn read(&self) -> Option<Shared> {
        let raw = std::fs::read_to_string(self.path()).ok()?;
        if raw.len() > 64 * 1024 {
            return None;
        }
        serde_json::from_str(&raw).ok()
    }

    /// Kilidi dener: `Some` = bizde, `None` = başka süreçte. Kilit dosyası açılamıyorsa (yetki,
    /// disk) `Err`: çağrıcı kilitsiz ölçer (eski davranışa düşer; sessizce atlanmaz).
    pub fn try_lock(&self) -> Result<Option<Guard>, String> {
        crate::private_fs::create_dir(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        let path = self.lock_path();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Guard(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Atomik yazım (geçici dosya + rename). Anahtarın kaynağı her yazımda damgalanır.
    pub fn write(&self, shared: &Shared) -> Result<(), String> {
        crate::private_fs::create_dir(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        let mut shared = shared.clone();
        shared.config_dir = self.config_dir.clone();
        let text = serde_json::to_string(&shared).map_err(|e| e.to_string())?;
        let path = self.path();
        let tmp = self.root.join(format!("{}.json.{}.tmp", self.key, std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
    }

    /// Dosyayı siler (hesap bağlantısı kalkınca/değişince eski hesabın ölçümü kalmasın). Yoksa sessiz.
    pub fn clear(&self) {
        let _ = std::fs::remove_file(self.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{Usage, MAX_POLL_MS, MIN_POLL_MS, RETRY_AFTER_CAP_MS};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-usage-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn u(five: f64) -> Usage {
        Usage { five_hour: five, seven_day: 10.0, ..Default::default() }
    }

    #[test]
    fn key_is_stable_per_config_dir() {
        let a = Store::new("r".into(), Path::new("C:/w/acct/.claude"));
        let b = Store::new("r".into(), Path::new("C:/w/acct/.claude/"));
        let c = Store::new("r".into(), Path::new("C:/w/acct/.claude-2"));
        assert_eq!(a.key, b.key, "sondaki ayraç fark değil");
        assert_ne!(a.key, c.key, "farklı CLAUDE_CONFIG_DIR farklı dosya");
        assert_eq!(a.key.len(), 16);
        #[cfg(windows)]
        assert_eq!(a.key, Store::new("r".into(), Path::new("c:\\W\\ACCT\\.claude")).key, "Windows'ta büyük/küçük harf aynı");
        assert!(a.path().to_string_lossy().ends_with(".json") && a.lock_path().to_string_lossy().ends_with(".lock"));
    }

    #[test]
    fn freshness_due_and_wait_follow_tier_and_backoff() {
        let now = 1_000_000_000;
        let ok = Shared::after(None, Ok(u(20.0)), None, now);
        assert_eq!((ok.measured_at, ok.next_allowed_at, ok.last_error.as_deref()), (Some(now), now + MAX_POLL_MS as i64, None));
        assert!(ok.fresh(now) && ok.fresh(now + MAX_POLL_MS as i64 - 1) && !ok.fresh(now + MAX_POLL_MS as i64));
        assert!(!ok.fresh(now - 1), "ileri tarihli ölçüm taze değil");
        assert!(!ok.due(now + 1) && ok.due(now + MAX_POLL_MS as i64));
        assert_eq!(ok.wait_ms(now), MAX_POLL_MS);
        assert_eq!(ok.wait_ms(now + MAX_POLL_MS as i64 + 5), RETRY_MS, "geçmiş = kısa sürede yeniden bak");
        // %75: 60 sn kademesi.
        let hot = Shared::after(Some(&ok), Ok(u(75.0)), None, now);
        assert_eq!(hot.next_allowed_at, now + MIN_POLL_MS as i64);
        // Hata: değer durur, aralık katlanır (ilk hatada 2 dk), neden yazılır.
        let err = Shared::after(Some(&hot), Err("kullanım uç noktası HTTP 429".into()), None, now + 10);
        assert_eq!((err.measured_at, err.usage.as_ref().map(|u| u.five_hour)), (Some(now), Some(75.0)));
        assert_eq!(err.next_allowed_at, now + 10 + 2 * MIN_POLL_MS as i64, "60 sn aralığından katlanır");
        assert_eq!(err.last_error.as_deref(), Some("kullanım uç noktası HTTP 429"));
        assert!(!err.ok() && hot.ok());
        assert!(!err.due(now + 20) && err.due(now + 10 + 2 * MIN_POLL_MS as i64), "nextAllowedAt'e saygı");
        let err2 = Shared::after(Some(&err), Err("x".into()), None, err.next_allowed_at);
        assert_eq!(err2.next_allowed_at - err2.attempted_at, 4 * MIN_POLL_MS as i64, "katlama sürer");
        let first_err = Shared::after(None, Err("x".into()), None, now);
        assert_eq!(first_err.next_allowed_at - now, 2 * MIN_POLL_MS as i64);
        // Retry-After: en azı belirler, tavan 30 dk.
        let ra = Shared::after(Some(&hot), Err("x".into()), Some(300_000), now);
        assert_eq!(ra.next_allowed_at - now, 300_000);
        let ra = Shared::after(Some(&hot), Err("x".into()), Some(3_600_000), now);
        assert_eq!(ra.next_allowed_at - now, RETRY_AFTER_CAP_MS as i64);
        assert_eq!(ra.wait_ms(now), RETRY_AFTER_CAP_MS);
        // Saçma ileri tarih (bozuk dosya) tutmaz: ölçüm gerekir.
        let bad = Shared { next_allowed_at: now + 2 * RETRY_AFTER_CAP_MS as i64, ..Default::default() };
        assert!(bad.due(now));
        assert!(Shared::default().due(now));
    }

    #[test]
    fn round_trip_atomic_write_corrupt_ignored_no_secret() {
        let root = tmp("rw");
        let store = Store::new(root.clone(), Path::new("C:/w/acct/.claude"));
        assert_eq!(store.read(), None, "dosya yok");
        let s = Shared::after(None, Ok(Usage { five_hour: 42.0, seven_day: 7.0, five_hour_resets_at: Some("2026-10-20T00:00:00Z".into()), ..Default::default() }), None, 5_000);
        store.write(&s).unwrap();
        let got = store.read().unwrap();
        assert_eq!((got.measured_at, got.usage.as_ref().map(|u| u.five_hour), got.next_allowed_at), (Some(5_000), Some(42.0), s.next_allowed_at));
        assert!(got.config_dir.to_lowercase().contains(".claude"), "anahtarın kaynağı damgalanır");
        let raw = std::fs::read_to_string(store.path()).unwrap();
        for word in ["accessToken", "refreshToken", "Bearer", "utilization"] {
            assert!(!raw.contains(word), "jeton/gövde yazılmaz: {word}");
        }
        assert!(std::fs::read_dir(&root).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")), "geçici dosya kalmaz");
        store.write(&s).unwrap();
        assert_eq!(store.read(), Some(got), "idempotent");
        std::fs::write(store.path(), "{bozuk").unwrap();
        assert_eq!(store.read(), None, "bozuk dosya yok sayılır");
        std::fs::write(store.path(), r#"{"nextAllowedAt":"yarın"}"#).unwrap();
        assert_eq!(store.read(), None);
        store.clear();
        store.clear();
        assert!(!store.path().exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let root = tmp("lock");
        let a = Store::new(root.clone(), Path::new("/x/.claude"));
        let b = a.clone();
        let g = a.try_lock().unwrap().expect("ilk kilit");
        assert!(b.try_lock().unwrap().is_none(), "ikinci süreç beklemez, alamaz");
        drop(g);
        assert!(b.try_lock().unwrap().is_some(), "düşünce bırakılır");
        let _ = std::fs::remove_dir_all(&root);
    }
}
