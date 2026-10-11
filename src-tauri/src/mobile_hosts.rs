//! S5-A (mobile): the same three device-token commands as desktop `hosts.rs`, backed by the Android
//! Keystore instead of the OS keyring (the keyring crate has no Android backend). The Kotlin side
//! (`HostTokensPlugin`, living in the app module under `src-tauri/gen/android` so no extra Gradle
//! module is needed) wraps each token with a Keystore-held AES-256-GCM master key via
//! `EncryptedSharedPreferences` and stores only ciphertext in the app's private dir. Same command
//! names, so `src/api/hosts.ts` is unchanged.
//!
//! Wiring: `init()` builds a real Tauri plugin named `dex-host-tokens` whose `setup` registers the
//! Kotlin class through `PluginApi::register_android_plugin` and manages the resulting
//! `PluginHandle`. The commands below are async (frontend calls `invoke`), while
//! `PluginHandle::run_mobile_plugin` blocks on a JNI round-trip — hence `spawn_blocking`.
//!
//! (The previous revision called a nonexistent `AppHandle::run_mobile_plugin`; it never compiled for
//! the Android target because the mobile cfg was never `cargo check`ed. This is the
//! `tauri-plugin-notification/src/mobile.rs` pattern.)

use tauri::{
    plugin::{Builder, PluginApi, PluginHandle, TauriPlugin},
    AppHandle, Manager, Runtime,
};

const PLUGIN_IDENTIFIER: &str = "com.hakunokina.dex.hosttokens";
const PLUGIN_CLASS: &str = "HostTokensPlugin";

pub struct HostTokens<R: Runtime>(PluginHandle<R>);

fn check(id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then_some(()).ok_or_else(|| format!("geçersiz sunucu kimliği: {id}"))
}

/// Initializes the plugin. Call once from `lib.rs` inside `#[cfg(mobile)]`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("dex-host-tokens")
        .setup(|app, api: PluginApi<R, ()>| {
            // A missing Kotlin class must not take the whole app down at launch: without the bridge
            // only pairing fails (with a readable error from `call`), the rest of the client works.
            match api.register_android_plugin(PLUGIN_IDENTIFIER, PLUGIN_CLASS) {
                Ok(handle) => {
                    app.manage(HostTokens::<R>(handle));
                }
                Err(e) => eprintln!("dex-host-tokens: Kotlin köprüsü yüklenemedi: {e}"),
            }
            Ok(())
        })
        .build()
}

#[derive(serde::Deserialize)]
struct TokenReply {
    value: Option<String>,
}

async fn call<R: Runtime>(
    app: &AppHandle<R>,
    method: &str,
    id: &str,
    token: Option<&str>,
) -> Result<Option<String>, String> {
    let handle = app
        .try_state::<HostTokens<R>>()
        .ok_or("cihaz jetonu köprüsü yüklenemedi (Android Keystore eklentisi yok)")?
        .inner()
        .0
        .clone();
    let mut payload = serde_json::json!({ "id": id });
    if let Some(t) = token {
        payload["token"] = serde_json::json!(t);
    }
    let method = method.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .run_mobile_plugin::<TokenReply>(&method, payload)
            .map(|r| r.value)
            .map_err(|e| format!("cihaz jetonu işlemi başarısız ({method}): {e}"))
    })
    .await
    .map_err(|e| format!("cihaz jetonu köprüsü koptu: {e}"))?
}

#[tauri::command]
pub async fn host_token_get<R: Runtime>(app: AppHandle<R>, id: String) -> Result<Option<String>, String> {
    check(&id)?;
    call(&app, "get", &id, None).await
}

#[tauri::command]
pub async fn host_token_set<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    token: String,
) -> Result<(), String> {
    check(&id)?;
    let t = token.trim().to_string();
    call(&app, "set", &id, Some(&t)).await.map(|_| ())
}

#[tauri::command]
pub async fn host_token_delete<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    check(&id)?;
    call(&app, "delete", &id, None).await.map(|_| ())
}
