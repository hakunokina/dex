//! #911: Tauri wrappers over `dex_core::proc`'s process panel (the WS methods of the same name
//! are dexd's). Sampling sleeps, so both run off the UI thread.

use std::collections::BTreeMap;

use dex_core::proc::AgentProcs;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub async fn agent_processes(core: State<'_, CoreState>, agent: Option<String>) -> Result<BTreeMap<String, AgentProcs>, String> {
    crate::providers::blocking(&core, move |c| dex_core::proc::agent_processes(c, agent.as_deref())).await
}

#[tauri::command]
pub async fn kill_agent_process(core: State<'_, CoreState>, agent: String, pid: u32) -> Result<(), String> {
    crate::providers::blocking(&core, move |c| dex_core::proc::kill_agent_process(c, &agent, pid)).await
}
