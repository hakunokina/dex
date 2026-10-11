//! Tauri wrappers over the core's task store (`dex_core::db::tasks`).

use dex_core::db::tasks;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn task_list(core: State<CoreState>, team_id: Option<String>) -> Result<Vec<tasks::Task>, String> {
    core.task_list(team_id.as_deref())
}

#[tauri::command]
pub fn task_create(core: State<CoreState>, task: tasks::NewTask) -> Result<tasks::Task, String> {
    core.task_create(task)
}

#[tauri::command]
pub fn task_update(core: State<CoreState>, id: i64, patch: tasks::TaskPatch) -> Result<tasks::Task, String> {
    core.task_update(id, patch)
}

#[tauri::command]
pub fn task_delete(core: State<CoreState>, id: i64) -> Result<(), String> {
    core.task_delete(id)
}
