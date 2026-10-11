//! #151 (B2): the tray icon of the desktop's core (`dexd --local`, core_proc.rs).
//!
//! - Tooltip: "Dex çekirdeği: çalışıyor / kapanıyor (n sn) / yok" and how many agents run — dexd's
//!   own `core_life` (its grace countdown, not a guess here), polled over one probe connection.
//! - No core: why (the last start's failure, #203) in the tooltip and as the menu's first line.
//! - Menu: Pencereyi aç · Çekirdeği yeniden başlat (onay; #161: kuyruk korunur, liderler `--resume`
//!   ile geri gelir) · Çekirdeği kapat (onay; ajanlar kapanır, kuyruk düşer, liderler açılmaz) ·
//!   Çık (this app goes; the core keeps its 120 s rule).
//! - Closing the window destroys it (its socket goes, so dexd's grace starts) but the app stays,
//!   tray only, showing the countdown; when the core is gone — or the grace passed while another
//!   window keeps it — the app exits and the icon with it. "Pencereyi aç" builds the window again.

use crate::core_proc::{CoreProc, Ws};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// dexd's no-window grace (crates/dexd/src/local.rs GRACE) until its `core_life` says otherwise.
const GRACE: Duration = Duration::from_secs(120);

/// #161: what a restart and a shutdown really do (dex-core restart.rs).
const RESTART_TEXT: &str = "Çekirdek şimdi yeniden başlatılsın mı? Koşan işler yarıda kalır; kuyruktaki işler korunur, açık liderler kaldıkları oturumla yeniden açılır.";
const SHUTDOWN_TEXT: &str = "Çekirdek kapatılsın mı? Tüm ajanlar kapanır; kuyruktaki işler düşer ve liderler bir sonraki açılışta kendiliğinden açılmaz.";

pub struct Tray<R: Runtime> {
    icon: TrayIcon<R>,
    /// Disabled: the core's state, and why it is missing.
    status: MenuItem<R>,
    shutdown: MenuItem<R>,
    /// The window was closed at: the app lives on in the tray until the core's grace is over.
    closed_at: Mutex<Option<Instant>>,
    /// A confirmed restart / stop is running: a core gone for a moment is not the end.
    acting: AtomicBool,
}

/// Windows cuts tooltips at 127 UTF-16 units; menu lines stay short.
fn cut(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// The tooltip for dexd's `core_life` (None: no core — `why`, the last start's failure; `{}`: a
/// core too old to tell). `update`: the shell found it outdated (dev, #494).
pub fn tooltip(life: Option<&Value>, why: Option<&str>, update: bool) -> String {
    let Some(l) = life else {
        return match why {
            Some(w) => cut(format!("Dex çekirdeği: yok — {w}"), 120),
            None => "Dex çekirdeği: yok".into(),
        };
    };
    let mut state = match l["shutdownInSecs"].as_u64() {
        Some(n) => format!("kapanıyor ({n} sn)"),
        None => "çalışıyor".into(),
    };
    // #494: a dev update waits for the patron's restart.
    if update {
        state.push_str(" · güncelleme bekliyor");
    }
    match l["agents"].as_u64() {
        Some(n) => format!("Dex çekirdeği: {state} · {n} ajan çalışıyor"),
        None => format!("Dex çekirdeği: {state}"),
    }
}

/// The menu's first line.
pub fn status_line(life: Option<&Value>, why: Option<&str>, update: bool) -> String {
    match (life, why) {
        (Some(_), _) if update => "Çekirdek çalışıyor · güncelleme bekliyor".into(),
        (Some(_), _) => "Çekirdek çalışıyor".into(),
        (None, Some(w)) => cut(format!("Çekirdek yok: {w}"), 90),
        (None, None) => "Çekirdek yok".into(),
    }
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", status_line(None, None, false), false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Pencereyi aç", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Çekirdeği yeniden başlat…", true, None::<&str>)?;
    let shutdown = MenuItem::with_id(app, "shutdown", "Çekirdeği kapat…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Çık", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &PredefinedMenuItem::separator(app)?, &open, &PredefinedMenuItem::separator(app)?, &restart, &shutdown, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut b = TrayIconBuilder::with_id("dex-core").menu(&menu).tooltip(tooltip(None, None, false)).show_menu_on_left_click(false);
    if let Some(i) = app.default_window_icon() {
        b = b.icon(i.clone());
    }
    let icon = b
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => open_window(app),
            "restart" => confirm(app, RESTART_TEXT, "Yeniden başlat", |p| async move { p.restart(false).await }),
            "shutdown" => confirm(app, SHUTDOWN_TEXT, "Kapat", |p| async move { p.stop().await }),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|t, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                open_window(t.app_handle());
            }
        })
        .build(app)?;
    app.manage(Tray { icon, status, shutdown, closed_at: Mutex::new(None), acting: AtomicBool::new(false) });
    let app = app.clone();
    tauri::async_runtime::spawn(async move { poll(app).await });
    Ok(())
}

/// The last window closed: stay in the tray (the caller prevents the exit) and count down.
pub fn window_closed<R: Runtime>(app: &AppHandle<R>) {
    if let Some(t) = app.try_state::<Tray<R>>() {
        *t.closed_at.lock().unwrap() = Some(Instant::now());
    }
}

/// The window forward — or built again from tauri.conf when it was closed.
pub fn open_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(t) = app.try_state::<Tray<R>>() {
        *t.closed_at.lock().unwrap() = None;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let Some(cfg) = app.config().app.windows.first() else { return };
    match tauri::WebviewWindowBuilder::from_config(app, cfg).and_then(|b| b.build()) {
        Ok(w) => {
            let _ = w.set_focus();
        }
        Err(e) => eprintln!("dex: pencere açılamadı: {e}"),
    }
}

fn confirm<R, F, Fut>(app: &AppHandle<R>, text: &str, ok: &str, act: F)
where
    R: Runtime,
    F: FnOnce(Arc<CoreProc>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let app2 = app.clone();
    app.dialog()
        .message(text)
        .title("Dex çekirdeği")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(ok.into(), "Vazgeç".into()))
        .show(move |yes| {
            if !yes {
                return;
            }
            let proc = app2.state::<Arc<CoreProc>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                app2.state::<Tray<R>>().acting.store(true, Ordering::SeqCst);
                let r = act(proc).await;
                app2.state::<Tray<R>>().acting.store(false, Ordering::SeqCst);
                if let Err(e) = r {
                    app2.dialog().message(e).title("Dex çekirdeği").kind(MessageDialogKind::Error).show(|_| {});
                }
            });
        });
}

async fn poll<R: Runtime>(app: AppHandle<R>) {
    let proc = app.state::<Arc<CoreProc>>().inner().clone();
    let mut conn: Option<Ws> = None;
    loop {
        let life = proc.life(&mut conn).await;
        let why = if life.is_none() { proc.last_error() } else { None };
        let update = life.is_some() && proc.update_pending();
        let t = app.state::<Tray<R>>();
        let _ = t.icon.set_tooltip(Some(tooltip(life.as_ref(), why.as_deref(), update)));
        let _ = t.status.set_text(status_line(life.as_ref(), why.as_deref(), update));
        let _ = t.shutdown.set_enabled(life.is_some());
        let closed = *t.closed_at.lock().unwrap();
        if let Some(at) = closed.filter(|_| !t.acting.load(Ordering::SeqCst)) {
            let grace = life.as_ref().and_then(|l| l["graceSecs"].as_u64()).map_or(GRACE, Duration::from_secs);
            // The core went (its grace ran out, or "Çekirdeği kapat"), or another window keeps it.
            if life.is_none() || at.elapsed() >= grace {
                app.exit(0);
                return;
            }
        }
        tokio::time::sleep(if closed.is_some() { Duration::from_secs(1) } else { Duration::from_secs(3) }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tooltip_tells_state_countdown_and_agents() {
        assert_eq!(tooltip(None, None, false), "Dex çekirdeği: yok");
        assert_eq!(tooltip(Some(&json!({"shutdownInSecs": null, "agents": 3})), None, false), "Dex çekirdeği: çalışıyor · 3 ajan çalışıyor");
        assert_eq!(tooltip(Some(&json!({"shutdownInSecs": 87, "agents": 0})), None, false), "Dex çekirdeği: kapanıyor (87 sn) · 0 ajan çalışıyor");
        assert_eq!(tooltip(Some(&json!({})), None, false), "Dex çekirdeği: çalışıyor", "an older dexd without core_life");
        assert_eq!(
            tooltip(Some(&json!({"shutdownInSecs": null, "agents": 4})), None, true),
            "Dex çekirdeği: çalışıyor · güncelleme bekliyor · 4 ajan çalışıyor"
        );
        // #494: dexd's own quiet wait (an earlier shell's) is taken back, not shown.
        assert_eq!(tooltip(Some(&json!({"agents": 1, "updatePending": true})), None, false), "Dex çekirdeği: çalışıyor · 1 ajan çalışıyor");
        // Windows cuts tooltips at 127 UTF-16 units.
        assert!(tooltip(Some(&json!({"shutdownInSecs": 120, "agents": 99})), None, true).encode_utf16().count() < 128);
    }

    /// #203: no core → why, in the tooltip (cut to fit) and as the menu's first line.
    #[test]
    fn no_core_tells_why() {
        let why = format!(r"yerel çekirdek açılırken çıktı (kod 1): bilinmeyen seçenek: --local — C:\fixtures\user\AppData\Roaming\{}\logs\dexd-stderr.log", crate::desktop_storage::LEGACY_IDENTIFIER);
        let t = tooltip(None, Some(&why), false);
        assert!(t.starts_with("Dex çekirdeği: yok — yerel çekirdek açılırken çıktı (kod 1)"), "{t}");
        assert!(t.encode_utf16().count() < 128 && t.ends_with('…'), "{t}");
        assert_eq!(status_line(None, Some("çekirdek programı yok"), false), "Çekirdek yok: çekirdek programı yok");
        assert_eq!(status_line(Some(&json!({})), Some("eski hata"), false), "Çekirdek çalışıyor");
        assert!(status_line(None, Some(&why), false).chars().count() <= 91);
        assert_eq!(status_line(Some(&json!({})), None, true), "Çekirdek çalışıyor · güncelleme bekliyor");
    }
}
