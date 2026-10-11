//! Kademe 1 Hafıza Haritası'nın salt okuma RPC'leri (crates/dex-core/src/memory.rs).

use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn memory_graph(core: State<CoreState>, team_id: String) -> Result<dex_core::memory::MemGraph, String> {
    core.memory_graph(&team_id)
}

#[tauri::command]
pub fn memory_read(
    core: State<CoreState>,
    team_id: String,
    id: String,
) -> Result<dex_core::memory::MemDoc, String> {
    core.memory_read(&team_id, &id)
}

/// #838 G2: one project's map — team shared, private and that project's shared (`team/<tid>/…`,
/// `private/<tid>/agents/…`, `project/<pid>/…` ids); never every project in one graph.
#[tauri::command]
pub fn project_memory_graph(
    core: State<CoreState>,
    team_id: String,
    project_id: String,
) -> Result<dex_core::memory::MemGraph, String> {
    core.project_memory_graph(&team_id, &project_id)
}

#[tauri::command]
pub fn project_memory_read(
    core: State<CoreState>,
    team_id: String,
    project_id: String,
    id: String,
) -> Result<dex_core::memory::MemDoc, String> {
    core.project_memory_read(&team_id, &project_id, &id)
}

/// #838 G2: the team scope alone (team shared + private), no project.
#[tauri::command]
pub fn team_memory_graph(core: State<CoreState>, team_id: String) -> Result<dex_core::memory::MemGraph, String> {
    core.team_memory_graph(&team_id)
}

#[tauri::command]
pub fn team_memory_read(core: State<CoreState>, team_id: String, id: String) -> Result<dex_core::memory::MemDoc, String> {
    core.team_memory_read(&team_id, &id)
}
