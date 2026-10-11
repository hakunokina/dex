//! `inproc-core` only (#150): the core inside the app, as before B. Tauri side of `dex_core`: starts the [`Core`] with the app's paths, bridges core events to
//! `AppHandle::emit`, and cleans up its per-process files on exit. Commands take `State<CoreState>`.

use dex_core::{Core, EventSink, Events, Paths};
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tauri::{plugin::TauriPlugin, AppHandle, Emitter, Manager, Runtime};

use crate::local::{LocalState, LocalWs};
use dexd::tailnet::TailnetState;

pub type CoreState = Arc<Core>;

/// #146: with the loopback endpoint up (`DEX_LOCAL_WS=1`) the webview gets core events from its
/// bus only — never twice; without it, Tauri events as before.
struct TauriEvents<R: Runtime>(AppHandle<R>, Arc<TailnetState>, Arc<OnceLock<Arc<dexd::Server>>>);

impl<R: Runtime> EventSink for TauriEvents<R> {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        self.1.publish(event, payload.clone());
        let Some(local) = self.2.get() else {
            return self.0.emit(event, payload).map_err(|e| e.to_string());
        };
        local.publish_event(event, payload);
        Ok(())
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("dex-core")
        .setup(|app, _| {
            let storage = crate::app_paths::get(app);
            let paths = Paths { data_dir: storage.data_dir, config_dir: storage.config_dir };
            let tailnet = TailnetState::open(paths.data_dir.clone());
            dex_core::diagnostics::init(&paths.data_dir.join("logs"))?;
            let local = Arc::new(OnceLock::new());
            let core = Core::start(
                paths,
                Events::new(TauriEvents(app.clone(), tailnet.clone(), local.clone())),
            )?;
            app.manage::<CoreState>(core.clone());
            // Failing to open it leaves the app on `invoke`, as with the flag off.
            let ws = crate::local::enabled()
                .then(|| LocalWs::start(core.clone()).map_err(|e| eprintln!("dex: {e}")).ok())
                .flatten();
            if let Some(ws) = &ws {
                let _ = local.set(ws.server.clone());
                ws.server.set_tailnet(tailnet.clone());
            }
            app.manage(LocalState(ws));
            app.manage(tailnet.clone());
            tauri::async_runtime::spawn(tailnet.start_on_launch(core));
            Ok(())
        })
        .on_event(|app, event| {
            if let tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::Destroyed, .. } = event {
                if let Some(core) = app.try_state::<CoreState>() {
                    core.team_view_release(&format!("window:{label}"));
                }
            }
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app.try_state::<CoreState>() {
                    core.shutdown();
                }
            }
        })
        .build()
}
