//! #838 G2: Tauri wrappers over the project layer (`dex_core::projects`); dexd serves the same
//! calls under the same names. Errors are the stable codes (`project_busy`, …). Mutations wait
//! for the agent start gate and create/update/verify run Git: off the UI thread.

use std::collections::BTreeMap;

use dex_core::project_context::ProjectContext;
use dex_core::projects::{NewProject, Project, ProjectPatch, SessionProject};
use tauri::State;

use crate::host::CoreState;
use crate::providers::blocking;

#[tauri::command]
pub fn project_list(core: State<CoreState>, team_id: String, include_archived: Option<bool>) -> Result<Vec<Project>, String> {
    core.project_list(&team_id, include_archived.unwrap_or(false))
}

#[tauri::command]
pub async fn project_create(
    core: State<'_, CoreState>,
    team_id: String,
    project: NewProject,
    root: Option<String>,
    branch: Option<String>,
    create_folder: Option<bool>,
) -> Result<Project, String> {
    blocking(&core, move |c| {
        if create_folder.unwrap_or(false) {
            if root.is_some() {
                return Err("createFolder ile root birlikte verilemez".into());
            }
            c.project_create_folder(&team_id, project, branch.as_deref())
        } else {
            c.project_create(&team_id, project, &root.ok_or("root gerekli")?, branch.as_deref())
        }
    })
    .await
}

#[tauri::command]
pub async fn project_update(
    core: State<'_, CoreState>,
    team_id: String,
    id: String,
    patch: Option<ProjectPatch>,
    root: Option<String>,
    branch: Option<String>,
) -> Result<Project, String> {
    blocking(&core, move |c| {
        c.project_update(&team_id, &id, patch.unwrap_or_default(), root.as_deref(), branch.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn project_archive(core: State<'_, CoreState>, team_id: String, id: String) -> Result<Project, String> {
    blocking(&core, move |c| c.project_archive(&team_id, &id)).await
}

#[tauri::command]
pub async fn project_reopen(core: State<'_, CoreState>, team_id: String, id: String) -> Result<Project, String> {
    blocking(&core, move |c| c.project_reopen(&team_id, &id)).await
}

#[tauri::command]
pub async fn project_set_default(core: State<'_, CoreState>, team_id: String, id: Option<String>) -> Result<(), String> {
    blocking(&core, move |c| c.project_set_default(&team_id, id.as_deref())).await
}

#[tauri::command]
pub fn project_context(core: State<CoreState>, team_id: String, project_id: Option<String>) -> Result<ProjectContext, String> {
    core.project_context(&team_id, project_id.as_deref())
}

#[tauri::command]
pub async fn project_verify(core: State<'_, CoreState>, team_id: String, id: String) -> Result<Project, String> {
    blocking(&core, move |c| c.project_verify(&team_id, &id)).await
}

#[tauri::command]
pub fn session_projects(core: State<CoreState>, team_id: Option<String>) -> Result<BTreeMap<String, SessionProject>, String> {
    core.session_projects(team_id.as_deref())
}
