//! S4: device tokens of the `dexd` hosts this window talks to. OS keyring (Credential Manager /
//! Keychain / Secret Service; service `dex-host`, account = host id), never `localStorage`.
//! Desktop-only: a server has no hosts of its own, so dexd has no counterpart.

const SERVICE: &str = "dex-host";

fn check(id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then_some(()).ok_or_else(|| format!("geçersiz sunucu kimliği: {id}"))
}

#[tauri::command]
pub fn host_token_get(id: String) -> Result<Option<String>, String> {
    check(&id)?;
    crate::secrets::get(SERVICE, &id)
}

#[tauri::command]
pub fn host_token_set(id: String, token: String) -> Result<(), String> {
    check(&id)?;
    crate::secrets::set(SERVICE, &id, token.trim()).map_err(|e| format!("cihaz jetonu anahtarlığa yazılamadı: {e}"))
}

#[tauri::command]
pub fn host_token_delete(id: String) -> Result<(), String> {
    check(&id)?;
    crate::secrets::delete(SERVICE, &id)
}

/// `inproc-core` only (the default build's core is `dexd --local`, which answers itself).
/// The in-app core's build, same fields as dexd's `server_info` (crates/dexd/src/server.rs): the
/// client compares a remote host's `commitTime` with this one. `methods: null` — the local core is
/// this very build, it has every command the UI calls.
#[cfg(feature = "inproc-core")]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    version: &'static str,
    commit: &'static str,
    commit_time: i64,
    methods: Option<Vec<String>>,
}

#[cfg(feature = "inproc-core")]
#[tauri::command]
pub fn server_info() -> ServerInfo {
    ServerInfo {
        version: env!("CARGO_PKG_VERSION"),
        commit: env!("DEX_COMMIT"),
        commit_time: env!("DEX_COMMIT_TIME").parse().unwrap_or(0),
        methods: None,
    }
}
