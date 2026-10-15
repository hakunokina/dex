//! #2019 — ikinci Claude (OAuth abonelik) hesabı: zincirde ayrı halka (`anthropic-2`, "Claude 2").
//!
//! Proxy yok: hesap kendi `CLAUDE_CONFIG_DIR`'ında (`<data_dir>/claude-accounts/2`) çalışır. Orada
//! yalnız hesaba ait olan durur: kimlik (`.credentials.json`, CLI yazar/yeniler) ve hesap durumu
//! (`.claude.json`: oauthAccount, onboarding, klasör güveni, önbellekler).
//!
//! Kullanıcıya ait olan asıl yapılandırma dizininden (`$CLAUDE_CONFIG_DIR` ya da `~/.claude`) bağlanır,
//! kopyalanmaz ([`prepare`]):
//! - Dizinler bağ (Windows'ta junction): `projects` (transcript'ler — Claude ↔ Claude 2 ↔ Muse
//!   geçişinde `--resume` aynı oturumu bulur; Dex'in transcript okuyan kodu aynı dosyaları görür),
//!   oturum yanı durum (`todos`, `file-history`, `plans`) ve kullanıcı eklentileri (`plugins`,
//!   `skills`, `agents`, `commands`, `output-styles`).
//! - Dosyalar sabit bağ: `settings.json` (env, izinler, kullanıcı hook'ları, eklentiler), `CLAUDE.md`.
//!   CLI dosyayı yerine yazarsa bağ kopar; her hazırlıkta içerik farklıysa yeniden bağlanır.
//! - `.claude.json` paylaşılamaz (oauthAccount hesaba ait): asıl dosyadan yalnız onboarding bitti
//!   bayrağı ve güvenilen klasörler aktarılır. Yoksa worker'ın PTY'si güven/onboarding diyaloğunda
//!   bekler. Başka alan (MCP sunucuları, hesap bilgisi) okunup yazılmaz.
//!
//! Dex'in worker ayarları (`--settings` hook/izin, `--mcp-config`, `--plugin-dir`) bayrakla gelir;
//! dizinden bağımsızdır. Jeton hiçbir yere kopyalanmaz/loglanmaz: Dex yalnız var mı diye bakar ve
//! kota ölçümünde bellekte okur.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Zincirdeki Claude abonelik hesapları (sağlayıcı kimlikleri), kota ölçüm sırasıyla.
pub const ACCOUNTS: [&str; 2] = ["anthropic", "anthropic-2"];

/// Ek hesap → `<data_dir>/claude-accounts/` altındaki dizin adı. Asıl hesap (`anthropic`) burada yok.
const EXTRA: &[(&str, &str)] = &[("anthropic-2", "2")];

/// Giriş PTY'sinin komutu (`pty_spawn` `cmd`); `provider` hesabı seçer. Ajan değildir: hook/MCP yok.
pub const LOGIN_CMD: &str = "claude-login";

const SHARED_DIRS: &[&str] =
    &["projects", "todos", "file-history", "plans", "plugins", "skills", "agents", "commands", "output-styles"];
const SHARED_FILES: &[&str] = &["settings.json", "CLAUDE.md"];
const CREDENTIALS: &str = ".credentials.json";
const STATE: &str = ".claude.json";

/// Sağlayıcı bir Claude abonelik hesabı mı (asıl ya da ek).
pub fn is_claude(provider: &str) -> bool {
    ACCOUNTS.contains(&provider)
}

/// Ek hesabın `CLAUDE_CONFIG_DIR`'ı; asıl hesap ve başka sağlayıcı `None`.
pub fn config_dir(data_dir: &Path, provider: &str) -> Option<PathBuf> {
    EXTRA.iter().find(|(p, _)| *p == provider).map(|(_, n)| data_dir.join("claude-accounts").join(n))
}

/// Asıl hesabın yapılandırma dizini: Dex sürecinin `CLAUDE_CONFIG_DIR`'ı, yoksa `~/.claude`.
pub fn main_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| home().map(|h| h.join(".claude")))
}

/// Asıl hesabın durum dosyası: `$CLAUDE_CONFIG_DIR/.claude.json`, yoksa `~/.claude.json`.
#[cfg_attr(test, allow(dead_code))]
fn main_state() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(home).map(|d| d.join(STATE))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

#[cfg(test)]
thread_local! {
    /// Testlerde asıl dizin (yapılandırma dizini, durum dosyası): gerçek `~/.claude`'a bağ kurulmaz.
    pub(crate) static TEST_MAIN: std::cell::RefCell<Option<(PathBuf, PathBuf)>> = const { std::cell::RefCell::new(None) };
}

/// [`prepare`]'in kaynakları: asıl yapılandırma dizini ve durum dosyası.
fn sources() -> (Option<PathBuf>, Option<PathBuf>) {
    #[cfg(test)]
    return TEST_MAIN.with(|m| m.borrow().clone()).map_or((None, None), |(d, s)| (Some(d), Some(s)));
    #[cfg(not(test))]
    (main_dir(), main_state())
}

/// Hesap dizinini kurar/tazeler: paylaşılan içerik bağlanır, güven/onboarding aktarılır. Her bağ
/// en iyi çaba: kurulamayan Dex log'una düşer, oturum yine açılır (yalnız o paylaşım eksik kalır).
pub fn prepare(dir: &Path) -> Result<(), String> {
    crate::private_fs::create_dir(dir).map_err(|e| format!("Claude hesap dizini açılamadı: {e}"))?;
    let (main, state) = sources();
    if let Some(main) = main {
        for problem in link_shared(&main, dir) {
            eprintln!("dex: Claude hesap dizini {}: {problem}", dir.display());
        }
    }
    if let Some(state) = state {
        if let Err(e) = mirror_state(&state, &dir.join(STATE)) {
            eprintln!("dex: Claude hesap dizini {}: güven/onboarding aktarılamadı: {e}", dir.display());
        }
    }
    Ok(())
}

/// Bağlanamayanların kısa nedenleri (yol + neden; içerik yok).
fn link_shared(main: &Path, dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for name in SHARED_DIRS {
        let (src, dst) = (main.join(name), dir.join(name));
        // Transcript'ler hep paylaşılır: kaynak yoksa açılır (CLI de açardı).
        if *name == "projects" && !src.exists() {
            let _ = std::fs::create_dir_all(&src);
        }
        if !src.is_dir() || crate::worktree::is_link(&dst) {
            continue;
        }
        if dst.is_dir() {
            // CLI'ın daha önce açtığı boş klasör bağla değişir; doluysa dokunulmaz (veri silinmez).
            if std::fs::remove_dir(&dst).is_err() {
                problems.push(format!("{name} ayrı bir klasör (dolu), paylaşılmıyor"));
                continue;
            }
        }
        if !crate::worktree::link_dir(&src, &dst) {
            problems.push(format!("{name} bağlanamadı"));
        }
    }
    for name in SHARED_FILES {
        let (src, dst) = (main.join(name), dir.join(name));
        if !src.is_file() {
            continue;
        }
        if std::fs::read(&src).ok() == std::fs::read(&dst).ok() {
            continue;
        }
        let _ = std::fs::remove_file(&dst);
        if let Err(e) = std::fs::hard_link(&src, &dst) {
            problems.push(format!("{name} bağlanamadı: {e}"));
        }
    }
    problems
}

/// Asıl durumdan hesabın durumuna: `hasCompletedOnboarding` ve güvenilen klasörler
/// (`projects[yol].hasTrustDialogAccepted`). Yalnız eksik olan eklenir; değişiklik yoksa yazılmaz.
/// Hesap dosyası okunamıyorsa (CLI yazarken) dokunulmaz.
fn mirror_state(main: &Path, account: &Path) -> Result<bool, String> {
    let main: Value = match std::fs::read_to_string(main) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|_| "asıl durum dosyası JSON değil".to_string())?,
        Err(_) => return Ok(false),
    };
    let mut acct: Value = match std::fs::read_to_string(account) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|_| "hesap durum dosyası JSON değil".to_string())?,
        Err(_) => json!({}),
    };
    let Some(obj) = acct.as_object_mut() else { return Err("hesap durum dosyası nesne değil".into()) };
    let mut changed = false;
    if main["hasCompletedOnboarding"] == Value::Bool(true) && obj.get("hasCompletedOnboarding") != Some(&Value::Bool(true)) {
        obj.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        changed = true;
    }
    let trusted = main["projects"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, p)| p["hasTrustDialogAccepted"] == Value::Bool(true))
        .map(|(k, _)| k.clone());
    let projects = obj.entry("projects").or_insert_with(|| json!({}));
    let Some(projects) = projects.as_object_mut() else { return Err("hesap durumunda projects nesne değil".into()) };
    for key in trusted {
        let entry = projects.entry(key).or_insert_with(|| json!({}));
        if let Some(e) = entry.as_object_mut() {
            if e.get("hasTrustDialogAccepted") != Some(&Value::Bool(true)) {
                e.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                changed = true;
            }
        }
    }
    if changed {
        let text = serde_json::to_string_pretty(&acct).map_err(|e| e.to_string())?;
        let tmp = account.with_extension("json.dex-tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, account).map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

/// Hesap bağlı mı: CLI'ın kimlik dosyasında OAuth jetonu var (süresi geçmiş olabilir; CLI yeniler).
/// Değer okunur ama tutulmaz.
pub fn connected(dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(dir.join(CREDENTIALS)) else { return false };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return false };
    let o = &v["claudeAiOauth"];
    ["refreshToken", "accessToken"].iter().any(|k| o[*k].as_str().is_some_and(|t| !t.is_empty()))
}

/// Hesabın e-postası (CLI'ın `.claude.json` `oauthAccount.emailAddress`'i); yoksa `None`.
pub fn email(dir: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(STATE)).ok()?).ok()?;
    v["oauthAccount"]["emailAddress"].as_str().filter(|e| !e.is_empty()).map(str::to_string)
}

/// Bağlantıyı kaldırır: kimlik dosyası ve hesap durumu silinir. Paylaşılan bağlar kalır (onları
/// silmek asıl dizine dokunmaz ama gerekmez); bir sonraki girişte durum yeniden aktarılır.
/// Geri alma: yeniden "hesabı bağla".
pub fn remove(dir: &Path) -> Result<(), String> {
    for name in [CREDENTIALS, STATE] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{name} silinemedi: {e}")),
        }
    }
    Ok(())
}

/// Ayarlar kartı: bağlı mı, e-posta.
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub connected: bool,
    pub email: Option<String>,
}

pub fn account(dir: &Path) -> Account {
    let connected = connected(dir);
    Account { connected, email: if connected { email(dir) } else { None } }
}

/// Giriş PTY'si: gerçek `claude auth login --claudeai`, hesabın dizininde. Hesap dizini önce
/// hazırlanır (onboarding/güven aktarılmış olur, ilk worker diyalogda beklemez).
pub fn login_command(data_dir: &Path, provider: Option<&str>) -> Result<(String, Vec<String>, PathBuf), String> {
    let provider = provider.ok_or("giriş için Claude hesabı seçilmeli")?;
    let dir = config_dir(data_dir, provider).ok_or_else(|| format!("bilinmeyen Claude hesabı: {provider}"))?;
    prepare(&dir)?;
    let cmd = crate::providers::resolve_claude("claude");
    Ok((cmd, vec!["auth".into(), "login".into(), "--claudeai".into()], dir))
}

/// Giriş PTY'sinin kimliği (UI bununla açar; çıkışta sağlayıcı listesi tazelenir).
pub fn login_pty_id(provider: &str) -> String {
    format!("claude-login-{provider}")
}

/// `id` bir giriş PTY'si ise hesabı.
pub fn login_provider(id: &str) -> Option<&'static str> {
    EXTRA.iter().map(|(p, _)| *p).find(|p| login_pty_id(p) == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-claude-acct-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_extra_accounts_have_their_own_dir() {
        let data = Path::new("data");
        assert_eq!(config_dir(data, "anthropic-2"), Some(data.join("claude-accounts").join("2")));
        assert_eq!(config_dir(data, "anthropic"), None, "asıl hesap kendi dizininde");
        assert_eq!(config_dir(data, "muse"), None);
        assert!(is_claude("anthropic") && is_claude("anthropic-2") && !is_claude("glm"));
        assert_eq!(login_provider(&login_pty_id("anthropic-2")), Some("anthropic-2"));
        assert_eq!(login_provider("claude-login-anthropic"), None);
        assert!(login_command(data, Some("anthropic")).is_err(), "asıl hesabın girişi Dex'te değil");
        assert!(login_command(data, None).is_err());
    }

    #[test]
    fn shares_user_content_by_link_not_copy() {
        let root = tmp("link");
        let (main, acct) = (root.join("main"), root.join("acct"));
        std::fs::create_dir_all(main.join("projects/p1")).unwrap();
        std::fs::write(main.join("projects/p1/s1.jsonl"), "{}\n").unwrap();
        std::fs::create_dir_all(main.join("skills")).unwrap();
        std::fs::write(main.join("settings.json"), r#"{"outputStyle":"Concise"}"#).unwrap();
        // CLI'ın önceden açtığı boş klasör bağa döner.
        std::fs::create_dir_all(acct.join("projects")).unwrap();
        assert!(link_shared(&main, &acct).is_empty());
        assert!(crate::worktree::is_link(&acct.join("projects")) && crate::worktree::is_link(&acct.join("skills")));
        assert!(!acct.join("plugins").exists(), "kaynağı olmayan dizin açılmaz");
        // Hesap dizininden yazılan transcript asıl dizinde görünür (resume iki yönde).
        std::fs::write(acct.join("projects/p1/s2.jsonl"), "{}\n").unwrap();
        assert!(main.join("projects/p1/s2.jsonl").is_file());
        assert_eq!(std::fs::read_to_string(acct.join("settings.json")).unwrap(), r#"{"outputStyle":"Concise"}"#);
        // CLI yerine yazarsa bağ kopar; sonraki hazırlık yeniden bağlar.
        std::fs::remove_file(acct.join("settings.json")).unwrap();
        std::fs::write(acct.join("settings.json"), "{}").unwrap();
        assert!(link_shared(&main, &acct).is_empty(), "idempotent");
        assert_eq!(std::fs::read_to_string(acct.join("settings.json")).unwrap(), r#"{"outputStyle":"Concise"}"#);
        // Dolu ayrı klasöre dokunulmaz, nedeni söylenir.
        std::fs::create_dir_all(main.join("agents")).unwrap();
        std::fs::create_dir_all(acct.join("agents")).unwrap();
        std::fs::write(acct.join("agents/x.md"), "x").unwrap();
        let problems = link_shared(&main, &acct);
        assert_eq!(problems, vec!["agents ayrı bir klasör (dolu), paylaşılmıyor".to_string()]);
        assert!(acct.join("agents/x.md").is_file(), "veri silinmez");
        // Bağlar kaldırılırken asıl içerik yerinde kalır.
        #[cfg(windows)]
        std::fs::remove_dir(acct.join("projects")).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(acct.join("projects")).unwrap();
        assert!(main.join("projects/p1/s1.jsonl").is_file());
        let _ = std::fs::remove_dir(acct.join("skills"));
        let _ = std::fs::remove_file(acct.join("skills"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mirrors_only_onboarding_and_trust() {
        let root = tmp("mirror");
        let (main, acct) = (root.join("main.json"), root.join("acct.json"));
        std::fs::write(&main, r#"{"hasCompletedOnboarding":true,"oauthAccount":{"emailAddress":"a@example.com"},
            "mcpServers":{"x":{"env":{"TOKEN":"sentinel"}}},
            "projects":{"C:/w/a":{"hasTrustDialogAccepted":true,"allowedTools":["x"]},"C:/w/b":{"hasTrustDialogAccepted":false}}}"#).unwrap();
        std::fs::write(&acct, r#"{"oauthAccount":{"emailAddress":"b@example.com"},"projects":{"C:/w/c":{"hasTrustDialogAccepted":true}}}"#).unwrap();
        assert!(mirror_state(&main, &acct).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&acct).unwrap()).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], true);
        assert_eq!(v["oauthAccount"]["emailAddress"], "b@example.com", "hesap kimliği korunur");
        assert_eq!(v["projects"]["C:/w/a"], json!({"hasTrustDialogAccepted": true}), "yalnız güven bayrağı");
        assert_eq!(v["projects"]["C:/w/c"]["hasTrustDialogAccepted"], true);
        assert!(v["projects"].get("C:/w/b").is_none());
        assert!(!std::fs::read_to_string(&acct).unwrap().contains("sentinel"), "MCP/env aktarılmaz");
        assert!(!mirror_state(&main, &acct).unwrap(), "ikinci koşu yazmaz");
        // Okunamayan hesap dosyasına dokunulmaz.
        std::fs::write(&acct, "{yarım").unwrap();
        assert!(mirror_state(&main, &acct).is_err());
        assert_eq!(std::fs::read_to_string(&acct).unwrap(), "{yarım");
        // Hesap dosyası yoksa açılır.
        std::fs::remove_file(&acct).unwrap();
        assert!(mirror_state(&main, &acct).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn connection_email_and_remove() {
        let dir = tmp("status");
        assert_eq!(account(&dir), Account { connected: false, email: None });
        std::fs::write(dir.join(STATE), r#"{"oauthAccount":{"emailAddress":"b@example.com"}}"#).unwrap();
        assert_eq!(account(&dir), Account { connected: false, email: None }, "kimlik yoksa e-posta gösterilmez");
        std::fs::write(dir.join(CREDENTIALS), r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"synthetic-refresh"}}"#).unwrap();
        assert_eq!(account(&dir), Account { connected: true, email: Some("b@example.com".into()) });
        std::fs::write(dir.join(CREDENTIALS), "{}").unwrap();
        assert!(!connected(&dir), "boş kimlik bağlı sayılmaz");
        std::fs::write(dir.join(CREDENTIALS), r#"{"claudeAiOauth":{"accessToken":"synthetic-access"}}"#).unwrap();
        remove(&dir).unwrap();
        assert!(!dir.join(CREDENTIALS).exists() && !dir.join(STATE).exists());
        remove(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
