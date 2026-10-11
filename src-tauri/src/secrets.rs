//! #150: the shell's own secrets in the OS keyring — the same entries `dex_core::secrets` wrote
//! with its default store, so tokens saved by an earlier build are still found. The shell no
//! longer links dex-core (the core is `dexd --local`).

fn entry(service: &str, account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, account).map_err(|e| format!("kimlik kasası açılamadı: {e}"))
}

/// `Ok(None)` = no such secret (a keyring read error counts as none, as in dex-core).
pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    Ok(entry(service, account)?.get_password().ok())
}

pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
    entry(service, account)?.set_password(value).map_err(|e| e.to_string())
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    match entry(service, account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
