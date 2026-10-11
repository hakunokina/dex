//! Tauri wrappers over `dex_core::delegate`.

use dex_core::delegate::Delegation;
use dex_core::questions::Question;
use tauri::State;

use crate::host::CoreState;

#[tauri::command]
pub fn delegation_list(core: State<CoreState>) -> Result<Vec<Delegation>, String> {
    core.delegation_list()
}

/// The frontend answers `dex-delegate-start`: started (→ running), busy (stays queued; the next
/// Stop delivers it) or failed.
#[tauri::command]
pub fn delegation_ack(core: State<CoreState>, id: i64, error: Option<String>, queued: bool) -> Result<(), String> {
    core.delegation_ack(id, error, queued)
}

#[tauri::command]
pub fn delegation_session_exited(core: State<CoreState>, agent_id: String) -> Result<(), String> {
    core.delegation_session_exited(&agent_id)
}

#[tauri::command]
pub fn delegation_seen(core: State<CoreState>, agent_id: String) -> Result<(), String> {
    core.delegation_seen(&agent_id)
}

/// DEX-24: worker questions (open, escalated, recently answered).
#[tauri::command]
pub fn question_list(core: State<CoreState>) -> Result<Vec<Question>, String> {
    core.question_list()
}

/// DEX-24: the patron answers an escalated question.
#[tauri::command]
pub fn question_answer(core: State<CoreState>, id: i64, text: String) -> Result<Question, String> {
    core.question_answer(id, &text)
}

/// DEX-28: the stall watchdog fired; the worker's leader (if it is on a job) gets a note → its id.
#[tauri::command]
pub fn delegation_stalled(core: State<CoreState>, agent_id: String, minutes: u32) -> Result<Option<String>, String> {
    core.delegation_stalled(&agent_id, minutes)
}
