//! Tauri wrappers over `dex_core::teams` (every mutation emits `teams-changed`).

use dex_core::teams::{Agent, AgentPatch, NewAgent, NewTeam, Team, TeamPatch};
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn team_viewed(core: State<CoreState>, window: tauri::Window, team_id: String, viewed: bool) -> Result<(), String> {
    core.team_viewed_for(&team_id, &format!("window:{}", window.label()), viewed)
}

#[tauri::command]
pub fn team_list(core: State<CoreState>) -> Result<Vec<Team>, String> {
    core.team_list()
}

#[tauri::command]
pub async fn team_create(core: State<'_, CoreState>, team: NewTeam) -> Result<Team, String> {
    crate::providers::blocking(&core, move |c| c.team_create(team)).await
}

#[tauri::command]
pub async fn team_folder_preview(core: State<'_, CoreState>, name: String) -> Result<dex_core::teams::TeamFolderPreview, String> {
    crate::providers::blocking(&core, move |c| c.team_folder_preview(&name)).await
}

#[tauri::command]
pub fn team_clone_template(core: State<CoreState>, team_id: String) -> Result<dex_core::teams::TeamCloneTemplate, String> {
    core.team_clone_template(&team_id)
}

#[tauri::command]
pub fn team_update(core: State<CoreState>, id: String, patch: TeamPatch) -> Result<Team, String> {
    core.team_update(&id, patch)
}

#[tauri::command]
pub fn team_delete(core: State<CoreState>, id: String, stop_agents: Option<bool>) -> Result<(), String> {
    core.team_delete(&id, stop_agents.unwrap_or(false))
}

/// Görev #43: running members, open jobs and active plans the delete would stop.
#[tauri::command]
pub fn team_delete_info(core: State<CoreState>, id: String) -> Result<dex_core::teams::DeleteInfo, String> {
    core.team_delete_info(&id)
}

#[tauri::command]
pub fn agent_list(core: State<CoreState>) -> Result<Vec<Agent>, String> {
    core.agent_list()
}

#[tauri::command]
pub fn agent_create(core: State<CoreState>, agent: NewAgent) -> Result<Agent, String> {
    core.agent_create(agent)
}

#[tauri::command]
pub fn agent_update(core: State<CoreState>, id: String, patch: AgentPatch) -> Result<Agent, String> {
    core.agent_update(&id, patch)
}

#[tauri::command]
pub fn agent_delete(core: State<CoreState>, id: String) -> Result<(), String> {
    core.agent_delete(&id)
}

/// DEX-30: all of a team's workers → one engine/provider/model (running ones: pending).
/// #2443: `ids` narrows it to the chosen workers.
#[tauri::command]
pub fn team_set_workers_model(
    core: State<CoreState>,
    team_id: String,
    target: dex_core::teams::PendingModel,
    ids: Option<Vec<String>>,
) -> Result<dex_core::teams::WorkersOutcome, String> {
    core.team_set_workers_model(&team_id, target, ids)
}

/// DEX-30: lands a pending change now (no-op while the agent's session runs).
#[tauri::command]
pub fn agent_apply_pending(core: State<CoreState>, id: String) -> Result<bool, String> {
    core.agent_apply_pending(&id)
}

