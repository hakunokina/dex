//! Tauri wrappers over `dex_core::notes` (DEX-41).

use dex_core::notes::Note;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn note_get(core: State<CoreState>, scope: String) -> Result<Note, String> {
    core.note_get(&scope)
}

#[tauri::command]
pub fn note_set(core: State<CoreState>, scope: String, body: String) -> Result<Note, String> {
    core.note_set(&scope, &body)
}
