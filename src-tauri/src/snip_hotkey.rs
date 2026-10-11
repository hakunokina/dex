//! The screenshot trigger — a system-wide hotkey. Default Ctrl+Shift+S
//! takes other apps' "Save as" while Dex runs. Changeable in Settings;
//! stored in `<app data>/snip.json` (desktop-local, like tailnet.json — not a host setting).
//! A failed registration (another app owns the combo) keeps Dex running, shows an OS notification
//! and is reported in Settings.
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(any(windows, test))]
use std::str::FromStr;
use std::sync::Mutex;

use serde::Serialize;
#[cfg(any(windows, test))]
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
#[cfg(windows)]
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
#[cfg(any(windows, test))]
use tauri_plugin_global_shortcut::{Modifiers, Shortcut};
#[cfg(windows)]
use tauri_plugin_notification::NotificationExt;

pub const DEFAULT: &str = "Ctrl+Shift+S";

#[cfg(any(windows, test))]
#[derive(Serialize, Deserialize)]
struct Saved {
    hotkey: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub hotkey: String,
    /// Registered with the OS right now.
    pub active: bool,
    pub error: Option<String>,
}

pub struct HotkeyState {
    #[cfg(windows)]
    file: PathBuf,
    view: Mutex<View>,
}

/// A hotkey Dex may take system-wide: parseable and holding Ctrl, Alt or Win (a bare key or
/// Shift+key would swallow ordinary typing in every app).
#[cfg(any(windows, test))]
fn parse(s: &str) -> Result<Shortcut, String> {
    let sc = Shortcut::from_str(s.trim()).map_err(|e| format!("geçersiz kısayol: {e}"))?;
    if !sc.mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER) {
        return Err("kısayolda Ctrl, Alt ya da Win olmalı".into());
    }
    Ok(sc)
}

/// Stored hotkey; missing, unreadable or no longer valid → the default.
#[cfg(any(windows, test))]
fn load(raw: Option<&[u8]>) -> String {
    raw.and_then(|v| serde_json::from_slice::<Saved>(v).ok())
        .map(|s| s.hotkey)
        .filter(|h| parse(h).is_ok())
        .unwrap_or_else(|| DEFAULT.into())
}

/// Plugin with the trigger handler: only one shortcut is ever registered, so any press is ours.
#[cfg(windows)]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _, e| {
            if e.state != ShortcutState::Pressed {
                return;
            }
            let app = app.clone();
            // Off the event-loop thread: building webview windows there deadlocks on Windows.
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::snip::start(&app) {
                    notify(&app, &format!("Ekran alıntısı başlatılamadı: {e}"));
                }
            });
        })
        .build()
}

#[cfg(not(windows))]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new().build()
}

/// App setup: load the saved hotkey and register it (off the main thread — the plugin's register
/// waits on the event loop, which is not running yet during setup).
#[cfg(windows)]
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let file = crate::app_paths::get(app).data_dir.join("snip.json");
    let hotkey = load(std::fs::read(&file).ok().as_deref());
    app.manage(HotkeyState { file, view: Mutex::new(View { hotkey: hotkey.clone(), active: false, error: None }) });
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let res = parse(&hotkey).and_then(|sc| app.global_shortcut().register(sc).map_err(|e| e.to_string()));
        let state = app.state::<HotkeyState>();
        let mut v = state.view.lock().unwrap();
        match res {
            Ok(()) => v.active = true,
            Err(e) => {
                let msg = format!("Ekran alıntısı kısayolu {hotkey} kaydedilemedi ({e}). Ayarlar'dan başka bir kısayol seçin.");
                v.error = Some(msg.clone());
                drop(v);
                notify(&app, &msg);
            }
        }
    });
    Ok(())
}

#[cfg(not(windows))]
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    app.manage(HotkeyState {
        view: Mutex::new(View { hotkey: DEFAULT.into(), active: false, error: Some(crate::snip::UNSUPPORTED.into()) }),
    });
    Ok(())
}

#[cfg(windows)]
fn notify(app: &AppHandle, body: &str) {
    let _ = app.notification().builder().title("Dex").body(body).show();
}

#[tauri::command]
pub fn snip_hotkey_get(state: State<HotkeyState>) -> View {
    state.view.lock().unwrap().clone()
}

/// Register the new hotkey first; only on success drop the old one and save. On failure the old
/// one stays as it was.
#[cfg(windows)]
#[tauri::command]
pub async fn snip_hotkey_set(app: AppHandle, state: State<'_, HotkeyState>, hotkey: String) -> Result<View, String> {
    let hotkey = hotkey.trim().to_string();
    let sc = parse(&hotkey)?;
    let gs = app.global_shortcut();
    let old = state.view.lock().unwrap().clone();
    let old_sc = old.active.then(|| parse(&old.hotkey).ok()).flatten();
    if old_sc != Some(sc) {
        gs.register(sc).map_err(|e| format!("{hotkey} kaydedilemedi (başka bir uygulama kullanıyor olabilir): {e}"))?;
        if let Some(o) = old_sc {
            let _ = gs.unregister(o);
        }
    }
    let bytes = serde_json::to_vec(&Saved { hotkey: hotkey.clone() }).map_err(|e| e.to_string())?;
    std::fs::write(&state.file, bytes).map_err(|e| e.to_string())?;
    let mut v = state.view.lock().unwrap();
    *v = View { hotkey, active: true, error: None };
    Ok(v.clone())
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn snip_hotkey_set(app: AppHandle, state: State<'_, HotkeyState>, hotkey: String) -> Result<View, String> {
    let _ = (app, state, hotkey);
    Err(crate::snip::UNSUPPORTED.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_ctrl_shift_s_and_parses() {
        let sc = parse(DEFAULT).unwrap();
        assert_eq!(sc, parse("Ctrl+Shift+S").unwrap());
        assert_eq!(sc.mods, Modifiers::CONTROL | Modifiers::SHIFT);
    }

    #[test]
    fn rejects_bare_and_shift_only_keys() {
        assert!(parse("S").is_err());
        assert!(parse("Shift+S").is_err());
        assert!(parse("Ctrl+").is_err());
        assert!(parse("Ctrl+Shift+S+A").is_err());
        assert!(parse("Alt+F9").is_ok());
        assert!(parse(" super+KeyS ").is_ok());
    }

    #[test]
    fn load_falls_back_to_default() {
        assert_eq!(load(None), DEFAULT);
        assert_eq!(load(Some(b"bozuk")), DEFAULT);
        assert_eq!(load(Some(br#"{"hotkey":"S"}"#)), DEFAULT, "geçersiz kayıt → varsayılan");
        assert_eq!(load(Some(br#"{"hotkey":"Ctrl+Alt+P"}"#)), "Ctrl+Alt+P");
    }
}
