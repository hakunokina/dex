//! `inproc-core` only: the in-app core's tailnet listener (dexd::tailnet) behind Tauri commands.
//! In the default build `dexd --local` owns it and answers these over its endpoint.

use crate::host::CoreState;
use dexd::auth::Device;
use dexd::tailnet::{TailnetState, View};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub fn tailnet_get(tailnet: State<Arc<TailnetState>>) -> View {
    tailnet.view()
}

#[tauri::command]
pub async fn tailnet_set(
    core: State<'_, CoreState>,
    tailnet: State<'_, Arc<TailnetState>>,
    enabled: bool,
    port: u16,
) -> Result<View, String> {
    tailnet.set(core.inner().clone(), enabled, port).await
}

#[tauri::command]
pub fn tailnet_pair_code(tailnet: State<Arc<TailnetState>>) -> Result<String, String> {
    tailnet.pair_code()
}

#[tauri::command]
pub fn tailnet_devices(tailnet: State<Arc<TailnetState>>) -> Result<Vec<Device>, String> {
    tailnet.devices()
}

#[tauri::command]
pub fn tailnet_device_revoke(tailnet: State<Arc<TailnetState>>, id: String) -> Result<bool, String> {
    tailnet.revoke(&id)
}
