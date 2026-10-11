use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn agent_start(
    core: State<CoreState>,
    id: String,
    options: Option<dex_core::agents::StartOptions>,
) -> Result<dex_core::pty::PtyInfo, String> {
    core.agent_start(&id, options.unwrap_or_default())
}

#[tauri::command]
pub fn agent_status_list(core: State<CoreState>) -> Vec<dex_core::agents::PtyState> {
    core.agent_status_list()
}

// Görev #51: the leader's clean session — begin (hand-over note first) / force / cancel / get.
#[tauri::command]
pub fn leader_reset(
    core: State<CoreState>,
    id: String,
    action: String,
) -> Result<Option<dex_core::leader_reset::View>, String> {
    core.leader_reset(&id, &action)
}

#[tauri::command]
pub fn agent_input(core: State<CoreState>, id: String, draft: bool) {
    core.agent_input(&id, draft);
}

// FIX-3: retry writes Esc, waits, then Enter — off the UI thread.
#[tauri::command]
pub async fn agent_first_reply_retry(core: State<'_, CoreState>, id: String) -> Result<(), String> {
    crate::providers::blocking(&core, move |c| c.agent_first_reply_retry(&id)).await
}

#[tauri::command]
pub fn agent_first_reply_cancel(core: State<CoreState>, id: String) -> Result<(), String> {
    core.agent_first_reply_cancel(&id)
}

#[tauri::command]
pub fn first_reply_get(core: State<CoreState>) -> Result<std::collections::BTreeMap<String, u32>, String> {
    core.first_reply_get()
}

#[tauri::command]
pub fn first_reply_set(core: State<CoreState>, provider: String, secs: u32) -> Result<std::collections::BTreeMap<String, u32>, String> {
    core.first_reply_set(&provider, secs)
}

/// DEX-17: motor zinciri — sıra ve tükenen sağlayıcılar (Ayarlar paneli).
#[tauri::command]
pub fn chain_get(core: State<CoreState>) -> Result<dex_core::chain::ChainView, String> {
    core.chain_get()
}

#[tauri::command]
pub fn chain_set_order(core: State<CoreState>, order: Vec<String>) -> Result<dex_core::chain::ChainView, String> {
    core.chain_set_order(order)
}

#[tauri::command]
pub fn chain_set_disabled(core: State<CoreState>, ids: Vec<String>) -> Result<dex_core::chain::ChainView, String> {
    core.chain_set_disabled(ids)
}

#[tauri::command]
pub fn chain_set_agent(core: State<CoreState>, id: String, off: bool) -> Result<dex_core::teams::Agent, String> {
    core.chain_set_agent(&id, off)
}

/// DEX-17: "geri dön" — tükenen asıl sağlayıcı kullanılabilirse geçiciyi kaldırır.
#[tauri::command]
pub fn chain_back(core: State<CoreState>, id: String) -> Result<(), String> {
    core.chain_back(&id)
}
