//! Tauri wrappers over `dex_core::providers` (DEX-13).

use dex_core::providers::{ImportRow, LocalStatus, ProviderInfo};
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn provider_list(core: State<CoreState>) -> Result<Vec<ProviderInfo>, String> {
    core.provider_list()
}
#[tauri::command]
pub async fn gateway_auth_start(core: State<'_,CoreState>,account_id: Option<String>) -> Result<dex_core::gateway::auth::Started,String> {
    blocking(&core,move |c|c.gateway_auth_start(account_id.as_deref())).await
}
#[tauri::command]
pub async fn gateway_auth_status(core: State<'_,CoreState>) -> Result<dex_core::gateway::auth::Status,String> {
    blocking(&core,|c|c.gateway_auth_status()).await
}
#[tauri::command]
pub async fn gateway_auth_logout(core: State<'_,CoreState>,account_id: Option<String>) -> Result<dex_core::gateway::auth::LoggedOut,String> {
    blocking(&core,move |c|c.gateway_auth_logout(account_id.as_deref())).await
}
#[tauri::command]
pub async fn gateway_models(core: State<'_,CoreState>) -> Result<Vec<dex_core::gateway::auth::Model>,String> {
    blocking(&core,|c|c.gateway_models()).await
}

#[tauri::command]
pub fn provider_import_routes(core: State<CoreState>) -> Result<Vec<ImportRow>, String> {
    core.provider_import_routes()
}

#[tauri::command]
pub fn provider_set_token(core: State<CoreState>, id: String, token: String) -> Result<(), String> {
    core.provider_set_token(&id, &token)
}

#[tauri::command]
pub fn provider_local_set_base_url(core: State<CoreState>, url: String) -> Result<(), String> {
    core.provider_local_set_base_url(&url)
}

// DEX-25: ağa çıkan uçlar async + blocking havuzda — senkron komut ana iş parçacığını (UI) dondurur.
pub(crate) async fn blocking<T: Send + 'static>(
    core: &CoreState,
    f: impl FnOnce(&dex_core::Core) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let core = core.clone();
    tauri::async_runtime::spawn_blocking(move || f(&core)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn provider_local_status(core: State<'_, CoreState>) -> Result<LocalStatus, String> {
    blocking(&core, |c| c.provider_local_status()).await
}

#[tauri::command]
pub async fn provider_local_add(core: State<'_, CoreState>, name: String) -> Result<LocalStatus, String> {
    blocking(&core, move |c| c.provider_local_add(&name)).await
}

#[tauri::command]
pub async fn provider_local_delete(core: State<'_, CoreState>, name: String) -> Result<LocalStatus, String> {
    blocking(&core, move |c| c.provider_local_delete(&name)).await
}

/// Görev #120: açılış kontrolü — sağlayıcılara sorar, ağ beklemesi UI'ı dondurmasın.
#[tauri::command]
pub async fn model_news(core: State<'_, CoreState>) -> Result<Vec<dex_core::model_news::NewModel>, String> {
    blocking(&core, |c| c.model_news()).await
}

#[tauri::command]
pub fn model_news_dismiss(core: State<CoreState>, keys: Vec<String>) -> Result<(), String> {
    core.model_news_dismiss(keys)
}
