//! Tauri wrappers over `dex_core::plan` / `dex_core::proposals` (DEX-22).

use dex_core::plan::Plan;
use dex_core::proposals::Proposal;
use tauri::State;

use crate::host::CoreState;

/// The frontend is up (panes can take `dex-delegate-start`): continue the leaders' plans.
#[tauri::command]
pub fn plan_resume(core: State<CoreState>) -> Result<(), String> {
    core.plan_resume()
}

#[tauri::command]
pub fn plan_list(core: State<CoreState>) -> Result<Vec<Plan>, String> {
    core.plan_list()
}

#[tauri::command]
pub fn proposal_list(core: State<CoreState>) -> Result<Vec<Proposal>, String> {
    core.proposal_list()
}

/// The patron's answer to a leader's `propose_member`.
#[tauri::command]
pub fn proposal_decide(core: State<CoreState>, id: i64, approve: bool, name: Option<String>) -> Result<Proposal, String> {
    core.proposal_decide(id, approve, name.as_deref())
}
