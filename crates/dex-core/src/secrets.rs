//! S4 — where secrets (provider tokens, the desktop's host device tokens) are kept.
//!
//! Default: the OS keyring (Credential Manager / Keychain / Secret Service) — the desktop never
//! changes this. A headless Linux server has no Secret Service (no D-Bus session), so `dexd`
//! can switch the process to [`Store::Files`]: one file per secret, `<dir>/<service>.<account>`,
//! the directory 0700 and each file 0600 (owner only).
//!
//! systemd: a secret handed in with `LoadCredential=<service>.<account>:/path` is read from
//! `$CREDENTIALS_DIRECTORY` first and is read-only here (the unit file owns it).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub enum Store {
    Keyring,
    Files(PathBuf),
}

static STORE: OnceLock<Store> = OnceLock::new();

/// Once per process, before the first secret is read (dexd at startup). Later calls are refused.
pub fn use_store(store: Store) -> Result<(), String> {
    STORE.set(store).map_err(|_| "sır deposu zaten seçildi".to_string())
}

fn store() -> &'static Store {
    STORE.get_or_init(|| Store::Keyring)
}

/// Where secrets go in this process, for a startup line (never a secret).
pub fn describe() -> String {
    match store() {
        Store::Keyring => "işletim sistemi anahtarlığı".into(),
        Store::Files(dir) => format!("{} (0600 dosyalar)", dir.display()),
    }
}

/// Does the OS keyring answer here? A missing entry is an answer; a dead D-Bus / no Secret
/// Service is not.
pub fn keyring_works() -> bool {
    match keyring::Entry::new("dex-probe", "probe") {
        Ok(e) => matches!(e.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
        Err(_) => false,
    }
}

fn entry(service: &str, account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, account).map_err(|e| format!("kimlik kasası açılamadı: {e}"))
}

/// `Ok(None)` = no such secret (a keyring read error counts as none, as before S4).
pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    match store() {
        Store::Keyring => Ok(entry(service, account)?.get_password().ok()),
        Store::Files(dir) => {
            if let Some(v) = credential(service, account)? {
                return Ok(Some(v));
            }
            read(&dir.join(file_name(service, account)?))
        }
    }
}

pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
    match store() {
        Store::Keyring => entry(service, account)?.set_password(value).map_err(|e| e.to_string()),
        Store::Files(dir) => {
            if credential(service, account)?.is_some() {
                return Err(format!("{service}.{account} systemd LoadCredential ile veriliyor; birim dosyasından değiştirin"));
            }
            write_private(dir, &file_name(service, account)?, value)
        }
    }
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    match store() {
        Store::Keyring => match entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
        Store::Files(dir) => match std::fs::remove_file(dir.join(file_name(service, account)?)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// Both parts become one file name: nothing that could leave the directory.
fn file_name(service: &str, account: &str) -> Result<String, String> {
    let ok = |s: &str| !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if ok(service) && ok(account) {
        Ok(format!("{service}.{account}"))
    } else {
        Err(format!("geçersiz sır adı: {service}.{account}"))
    }
}

fn credential(service: &str, account: &str) -> Result<Option<String>, String> {
    match std::env::var_os("CREDENTIALS_DIRECTORY") {
        Some(d) => read(&Path::new(&d).join(file_name(service, account)?)),
        None => Ok(None),
    }
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim_end_matches(['\r', '\n']).to_string()).filter(|s| !s.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("sır okunamadı ({}): {e}", path.display())),
    }
}

/// Written to a 0600 temp file in the same 0700 directory, then renamed over the old one: a reader
/// never sees half a token, and the secret is never on disk with wider permissions.
fn write_private(dir: &Path, name: &str, value: &str) -> Result<(), String> {
    use std::io::Write;
    create_private_dir(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp).map_err(|e| format!("sır yazılamadı: {e}"))?;
    f.write_all(value.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("sır yazılamadı: {e}"))?;
    drop(f);
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| format!("sır yazılamadı: {e}"))
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).map_err(|e| format!("sır dizini açılamadı: {e}"))?;
    // An existing directory keeps its mode; a secrets dir readable by others is corrected.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The process-wide store stays Keyring in tests (other tests never touch it); the file
    // functions are exercised directly.
    #[test]
    fn file_store_round_trip_and_names_stay_in_the_directory() {
        let dir = std::env::temp_dir().join(format!("dex-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let name = file_name("dex-provider", "glm").unwrap();
        write_private(&dir, &name, "tok-1").unwrap();
        write_private(&dir, &name, "tok-2\n").unwrap();
        assert_eq!(read(&dir.join(&name)).unwrap().as_deref(), Some("tok-2"));
        assert_eq!(read(&dir.join("dex-provider.none")).unwrap(), None);
        assert!(!dir.join(format!(".{name}.tmp")).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(&name)).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        for bad in ["../x", "a/b", "a\\b", "", ".hidden"] {
            assert!(file_name("dex-provider", bad).is_err(), "{bad}");
        }
        assert_eq!(file_name("dex-provider", "local.base_url").unwrap(), "dex-provider.local.base_url");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
