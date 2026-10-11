//! S5-B: the agent's session transcript as a chat (crates/dex-core/src/chat.rs).

use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn agent_context(core: State<CoreState>, agent_id: String) -> Result<dex_core::cost::AgentContext,String> {
    core.agent_context(&agent_id)
}

#[tauri::command]
pub fn team_context(core: State<CoreState>, team_id: String) -> Result<std::collections::BTreeMap<String,dex_core::cost::AgentContext>,String> {
    core.team_context(&team_id)
}

#[tauri::command]
pub fn usage_summary(core: State<CoreState>, team_id: String, since: Option<i64>) -> Result<dex_core::cost::Summary,String> {
    core.usage_summary(&team_id,since)
}

#[tauri::command]
pub fn usage_delegation(core: State<CoreState>, id:i64) -> Result<dex_core::cost::Summary,String> {
    core.usage_delegation(id)
}

#[tauri::command]
pub fn settings_usage_prices(core: State<CoreState>) -> Result<Vec<dex_core::cost::Price>,String> {
    core.settings_usage_prices()
}

#[tauri::command]
pub fn settings_set_usage_prices(core: State<CoreState>, prices:Vec<dex_core::cost::Price>) -> Result<Vec<dex_core::cost::Price>,String> {
    core.settings_set_usage_prices(prices)
}

#[tauri::command]
pub fn chat_history(
    core: State<CoreState>,
    agent_id: String,
    before: Option<u64>,
    limit: Option<u32>,
) -> Result<dex_core::chat::ChatPage, String> {
    core.chat_history(&agent_id, before, limit)
}

/// Sends patron text: offline → starts the agent with it, idle → pasted, busy → queued.
#[tauri::command]
pub fn chat_send(core: State<CoreState>, agent_id: String, text: String) -> Result<&'static str, String> {
    core.chat_send(&agent_id, &text)
}

/// The PTY's visible screen as text — the waiting question shown in the chat card.
#[tauri::command]
pub fn chat_screen(core: State<CoreState>, agent_id: String) -> Result<String, String> {
    core.chat_screen(&agent_id)
}
