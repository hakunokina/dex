//! Tauri wrappers over `dex_core::layout`.

use serde_json::Value;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn layout_load(core: State<CoreState>) -> Result<Option<Value>, String> {
    core.layout_load()
}

#[tauri::command]
pub fn layout_save(core: State<CoreState>, layout: Value) -> Result<(), String> {
    core.layout_save(&layout)
}
