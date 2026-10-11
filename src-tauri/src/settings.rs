//! Tauri wrappers over `dex_core::settings` (DEX-31).

use dex_core::proc::Limits;
use dex_core::settings::LimitsView;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn core_status(core: State<CoreState>) -> Result<serde_json::Value, String> {
    core.core_status()
}

#[tauri::command]
pub fn limits_get(core: State<CoreState>) -> Result<LimitsView, String> {
    core.limits_get()
}

#[tauri::command]
pub fn limits_set(core: State<CoreState>, limits: Limits) -> Result<LimitsView, String> {
    core.limits_set(limits)
}
