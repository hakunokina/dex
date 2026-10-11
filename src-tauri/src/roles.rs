//! Tauri wrapper over `dex_core::roles`.

use dex_core::roles::Role;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn role_list(core: State<CoreState>) -> Result<Vec<Role>, String> {
    core.role_list()
}
