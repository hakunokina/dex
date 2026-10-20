//! Tauri wrappers over `dex_core::catchup` (Plan #124, `inproc-core` only; the default shell
//! reaches the same calls on dexd — see crates/dexd/src/methods.rs).

use dex_core::catchup::Catchup;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn catchup(core: State<CoreState>, team_id: String, since: Option<i64>) -> Result<Catchup, String> {
    core.catchup(&team_id, since)
}

#[tauri::command]
pub fn catchup_ack(core: State<CoreState>, team_id: String, at: i64) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "seenAt": core.catchup_ack(&team_id, at)? }))
}
