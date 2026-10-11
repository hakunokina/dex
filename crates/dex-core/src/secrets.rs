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

// OAuth records exceed Windows Credential Manager's single-entry limit. Two slots:
// write the inactive slot, commit its small head last, then clear the old slot.
// Caller serializes updates. Existing provider entries keep their old representation.
pub(crate) fn record_get(service: &str, account: &str) -> Result<Option<String>, String> {
    record_get_with(account, &|key| protected_get(service,key))
}
fn protected_get(service: &str, key: &str) -> Result<Option<String>, String> {
    match store() {
        Store::Keyring => match entry(service, key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("gateway_secret_unavailable".into()),
        },
        Store::Files(_) => get(service, key),
    }
}
pub(crate) fn record_set(service: &str, account: &str, value: &str) -> Result<(), String> {
    record_set_with(account, value, &|key| protected_get(service,key), &|key,v| set(service,key,v), &|key| delete(service,key))
}
type RecordGet<'a> = &'a dyn Fn(&str) -> Result<Option<String>, String>;
type RecordSet<'a> = &'a dyn Fn(&str, &str) -> Result<(), String>;
type RecordDelete<'a> = &'a dyn Fn(&str) -> Result<(), String>;
fn record_head(account: &str, get: RecordGet) -> Result<Option<(usize, usize)>, String> {
    let Some(head) = get(account)? else { return Ok(None); };
    let (slot, count) = head.split_once(':').ok_or("gateway_secret_invalid")?;
    let slot: usize = slot.parse().map_err(|_| "gateway_secret_invalid")?;
    let count: usize = count.parse().map_err(|_| "gateway_secret_invalid")?;
    if slot > 1 || !(1..=256).contains(&count) { return Err("gateway_secret_invalid".into()); }
    Ok(Some((slot,count)))
}
fn record_get_with(account: &str, get: RecordGet) -> Result<Option<String>, String> {
    use base64::Engine;
    let Some((slot,count)) = record_head(account,get)? else { return Ok(None); };
    let mut bytes = String::new();
    for i in 0..count {
        let chunk = get(&format!("{account}.slot{slot}.{i}"))?.ok_or("gateway_secret_invalid")?;
        if chunk.len() > 900 { return Err("gateway_secret_invalid".into()); }
        bytes.push_str(&chunk);
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(bytes).map_err(|_| "gateway_secret_invalid")?;
    String::from_utf8(bytes).map(Some).map_err(|_| "gateway_secret_invalid".into())
}
fn record_set_with(account: &str, value: &str, get: RecordGet, set: RecordSet, delete: RecordDelete) -> Result<(), String> {
    use base64::Engine;
    let old = record_head(account,get)?;
    let slot = old.map(|(slot,_)| 1-slot).unwrap_or(0);
    let encoded = base64::engine::general_purpose::STANDARD.encode(value);
    let chunks: Vec<_> = encoded.as_bytes().chunks(900).collect();
    if chunks.is_empty() || chunks.len()>256 { return Err("gateway_secret_limit".into()); }
    let count_key=format!("{account}.slot{slot}.count");
    let previous=get(&count_key)?.map(|s|s.parse::<usize>().map_err(|_|"gateway_secret_invalid")).transpose()?.unwrap_or(0);
    if previous>256 {return Err("gateway_secret_invalid".into());}
    // Record the whole write extent first so even interrupted/failed writes are cleaned later.
    set(&count_key,&previous.max(chunks.len()).to_string())?;
    for (i,chunk) in chunks.iter().enumerate() {
        set(&format!("{account}.slot{slot}.{i}"), std::str::from_utf8(chunk).unwrap())?;
    }
    set(account,&format!("{slot}:{}",chunks.len()))?;
    for i in chunks.len()..previous {delete(&format!("{account}.slot{slot}.{i}"))?;}
    set(&count_key,&chunks.len().to_string())?;
    if let Some((old,count)) = old {
        for i in 0..count { delete(&format!("{account}.slot{old}.{i}"))?; }
        delete(&format!("{account}.slot{old}.count"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_record_rotation_is_atomic_across_keyring_sized_chunks() {
        let rows = std::sync::Mutex::new(std::collections::HashMap::<String,String>::new());
        let get = |key: &str| Ok(rows.lock().unwrap().get(key).cloned());
        let set = |key: &str, value: &str| { assert!(value.len() <= 900); rows.lock().unwrap().insert(key.into(), value.into()); Ok(()) };
        let delete = |key: &str| { rows.lock().unwrap().remove(key); Ok(()) };
        let old = "a".repeat(8000);
        record_set_with("oauth", &old, &get, &set, &delete).unwrap();
        assert_eq!(record_get_with("oauth", &get).unwrap(), Some(old.clone()));
        let failed = |key: &str, value: &str| { if key.ends_with(".3") { Err("synthetic failure".into()) } else { set(key, value) } };
        assert!(record_set_with("oauth", &"b".repeat(8000), &get, &failed, &delete).is_err());
        assert_eq!(record_get_with("oauth", &get).unwrap(), Some(old));
        record_set_with("oauth", "new", &get, &set, &delete).unwrap();
        assert_eq!(record_get_with("oauth", &get).unwrap().as_deref(), Some("new"));
        assert!(rows.lock().unwrap().keys().all(|k|k=="oauth" || k=="oauth.slot1.0" || k=="oauth.slot1.count"));
    }

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
