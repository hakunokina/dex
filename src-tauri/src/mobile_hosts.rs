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

const SHARE_IDENTIFIER: &str = "com.hakunokina.dex.hosttokens";
const SHARE_CLASS: &str = "ShareTargetPlugin";

/// Android "Paylaş" hedefi (`ShareTargetPlugin.kt`): bekleyen paylaşımı verir.
pub struct ShareTarget<R: Runtime>(PluginHandle<R>);

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

/// Paylaş hedefi için AYRI Tauri eklentisi: Kotlin `PluginManager` eklentileri Tauri eklenti adıyla
/// anahtarlar; ikinci kayıt `dex-host-tokens` altına yapılırsa token köprüsünü ezer. Yoksa yalnız
/// paylaşım alınamaz (isteğe bağlı).
pub fn init_share<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("dex-share-target")
        .setup(|app, api: PluginApi<R, ()>| {
            match api.register_android_plugin(SHARE_IDENTIFIER, SHARE_CLASS) {
                Ok(handle) => {
                    app.manage(ShareTarget::<R>(handle));
                }
                Err(e) => eprintln!("dex-share-target: paylaş köprüsü yüklenemedi: {e}"),
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
    url: Option<&str>,
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
    // #1859: the host's address next to its token, for the native morning job (MotivationWorker).
    if let Some(u) = url {
        payload["url"] = serde_json::json!(u);
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
pub async fn host_token_get<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    url: Option<String>,
) -> Result<Option<String>, String> {
    check(&id)?;
    call(&app, "get", &id, None, url.as_deref()).await
}

#[tauri::command]
pub async fn host_token_set<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    token: String,
    url: Option<String>,
) -> Result<(), String> {
    check(&id)?;
    let t = token.trim().to_string();
    call(&app, "set", &id, Some(&t), url.as_deref()).await.map(|_| ())
}

#[tauri::command]
pub async fn host_token_delete<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    check(&id)?;
    call(&app, "delete", &id, None, None).await.map(|_| ())
}

/// Bekleyen Android paylaşımı (`{ text?, subject?, image?: { mime, base64 }, error? }`); yoksa `{}`.
/// Bir kez verir, sonra temizler. İçerik loglanmaz.
#[tauri::command]
pub async fn share_take<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    let handle = app
        .try_state::<ShareTarget<R>>()
        .ok_or("paylaş köprüsü yüklenemedi (Android eklentisi yok)")?
        .inner()
        .0
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .run_mobile_plugin::<serde_json::Value>("take", ())
            .map_err(|e| format!("paylaşım alınamadı: {e}"))
    })
    .await
    .map_err(|e| format!("paylaş köprüsü koptu: {e}"))?
}

/// #1762 geri düşüş: hızlı gönderim olmayıp gönderim ekranı açılacaksa pencere + WebView
/// opaklığa döner (CSS'e güvenilmez). Dex önde kalır, bu komut kapatmaz/arkaya almaz.
#[tauri::command]
pub async fn share_opaque<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let handle = app
        .try_state::<ShareTarget<R>>()
        .ok_or("paylaş köprüsü yüklenemedi (Android eklentisi yok)")?
        .inner()
        .0
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .run_mobile_plugin::<serde_json::Value>("opaque", serde_json::json!({}))
            .map(|_| ())
            .map_err(|e| format!("paylaşım opaklaştırılamadı: {e}"))
    })
    .await
    .map_err(|e| format!("paylaş köprüsü koptu: {e}"))?
}

/// #1717 hızlı gönderim bitti: Kotlin kısa bir Toast gösterir (`message`, en fazla 80 karakter; kişisel
/// içerik taşımaz) ve Dex'i kapatır/arkaya alır — kullanıcı paylaştığı uygulamaya döner. Soğuk paylaşım
/// açılışında görev tamamen kaldırılır (son kullanılanlarda kopya kalmaz); uygulama zaten açıksa görev
/// arkaya alınır (#1762: önce opaklığa dönülür). Gönderim ekranı açılırsa çağrılmaz, Dex önde kalır.
#[tauri::command]
pub async fn share_done<R: Runtime>(app: AppHandle<R>, message: String) -> Result<(), String> {
    let handle = app
        .try_state::<ShareTarget<R>>()
        .ok_or("paylaş köprüsü yüklenemedi (Android eklentisi yok)")?
        .inner()
        .0
        .clone();
    let message: String = message.chars().take(80).collect();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .run_mobile_plugin::<serde_json::Value>("done", serde_json::json!({ "message": message }))
            .map(|_| ())
            .map_err(|e| format!("paylaşım kapatılamadı: {e}"))
    })
    .await
    .map_err(|e| format!("paylaş köprüsü koptu: {e}"))?
}

const PUSH_IDENTIFIER: &str = "com.hakunokina.dex.hosttokens";
const PUSH_CLASS: &str = "PushPlugin";

/// #1719 bildirim köprüsü (`PushPlugin.kt`): FCM jetonu, bildirimden açılış, sistem tarayıcısı.
pub struct Push<R: Runtime>(PluginHandle<R>);

/// Ayrı Tauri eklentisi (Kotlin eklentileri Tauri eklenti adıyla anahtarlanır, bkz. `init_share`).
/// Yoksa yalnız bildirim/bağlantı açma çalışmaz.
pub fn init_push<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("dex-push")
        .setup(|app, api: PluginApi<R, ()>| {
            match api.register_android_plugin(PUSH_IDENTIFIER, PUSH_CLASS) {
                Ok(handle) => {
                    app.manage(Push::<R>(handle));
                }
                Err(e) => eprintln!("dex-push: bildirim köprüsü yüklenemedi: {e}"),
            }
            Ok(())
        })
        .build()
}

async fn push_call<R: Runtime>(
    app: &AppHandle<R>,
    method: &'static str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = app
        .try_state::<Push<R>>()
        .ok_or("bildirim köprüsü yüklenemedi (Android eklentisi yok)")?
        .inner()
        .0
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .run_mobile_plugin::<serde_json::Value>(method, payload)
            .map_err(|e| format!("bildirim köprüsü ({method}): {e}"))
    })
    .await
    .map_err(|e| format!("bildirim köprüsü koptu: {e}"))?
}

/// FCM jetonu: `{ token? }` (Firebase'siz APK: `{}`). Jeton loglanmaz.
#[tauri::command]
pub async fn push_token<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    push_call(&app, "token", serde_json::json!({})).await
}

/// Bildirime dokunulduysa bekleyen `dex://reply/<id>`: `{ uri? }`, bir kez.
#[tauri::command]
pub async fn push_take<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    push_call(&app, "take", serde_json::json!({})).await
}

/// Cevaptaki bağlantıyı sistem tarayıcısında açar; yalnız http/https (Kotlin de denetler).
#[tauri::command]
pub async fn push_open_url<R: Runtime>(app: AppHandle<R>, url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) || url.len() > 4096 {
        return Err("yalnız http/https bağlantı açılır".into());
    }
    push_call(&app, "openUrl", serde_json::json!({ "url": url })).await.map(|_| ())
}
