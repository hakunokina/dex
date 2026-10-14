//! Tauri wrappers over `dex_core::pty`: the webview's `Channel`s become the PTY's subscriber.

use dex_core::pty::Spawn;
use dex_core::PtySink;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::host::CoreState;

struct ChannelSink {
    data: Channel<InvokeResponseBody>,
    exit: Channel<()>,
}

impl PtySink for ChannelSink {
    fn data(&mut self, bytes: &[u8]) -> bool {
        self.data.send(InvokeResponseBody::Raw(bytes.to_vec())).is_ok()
    }

    fn exit(&mut self) {
        let _ = self.exit.send(());
    }

    /// #62: on the data channel (a channel keeps its order), as JSON next to the raw bytes.
    fn resized(&mut self, cols: u16, rows: u16) {
        let size = serde_json::json!({ "cols": cols, "rows": rows }).to_string();
        let _ = self.data.send(InvokeResponseBody::Json(size));
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn pty_spawn(
    core: State<CoreState>,
    id: String,
    cmd: String,
    args: Vec<String>,
    cwd: Option<String>,
    memory_dir: Option<String>,
    provider: Option<String>,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
    on_exit: Channel<()>,
) -> Result<(), String> {
    let req = Spawn { id, cmd, args, cwd, memory_dir, provider, cols, rows, env: Vec::new() };
    core.pty_spawn(req, ChannelSink { data: on_data, exit: on_exit })
}

#[tauri::command]
pub fn pty_list(core: State<CoreState>) -> Vec<dex_core::pty::PtyInfo> {
    core.ptys.list()
}

#[tauri::command]
pub fn pty_attach(
    core: State<CoreState>,
    id: String,
    on_data: Channel<InvokeResponseBody>,
    on_exit: Channel<()>,
) -> Result<dex_core::pty::Attachment, String> {
    core.ptys.attach(&id, ChannelSink { data: on_data, exit: on_exit })
}

#[tauri::command]
pub fn pty_detach(core: State<CoreState>, id: String, subscriber: u64) -> bool {
    core.ptys.detach(&id, subscriber)
}

#[tauri::command]
pub fn pty_write(core: State<CoreState>, id: String, data: String) -> Result<(), String> {
    core.ptys.write(&id, data.as_bytes())
}

/// #62: this window is the size client `local` (a dexd connection is `ws<n>`).
#[tauri::command]
pub fn pty_resize(core: State<CoreState>, id: String, cols: u16, rows: u16, claim: Option<bool>) -> Result<(), String> {
    core.ptys.resize(&id, "local", cols, rows, claim.unwrap_or(false))
}

#[tauri::command]
pub fn pty_kill(core: State<CoreState>, id: String) {
    dex_core::diagnostics::event("pty_kill", serde_json::json!({"agent": id, "reason": "user"}));
    core.agent_stop(&id);
}
