//! #2258: leader pop-out windows (`lead-*`), opened by the webview itself (`WebviewWindow`,
//! src/layout/popoutWindows.ts) — no command here. The shell only ties their lifetime to the main
//! window's: they close with it, and a destroyed one is reported to the main window
//! (`dex:lead-closed`, its label), since a webview cannot report its own destruction.

use tauri::{Emitter, Manager, Runtime, Window, WindowEvent};

const LABEL_PREFIX: &str = "lead-";
const MAIN: &str = "main";
const CLOSED_EVENT: &str = "dex:lead-closed";

fn is_popout(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if !matches!(event, WindowEvent::Destroyed) {
        return;
    }
    let label = window.label();
    if label == MAIN {
        for (l, w) in window.app_handle().webview_windows() {
            if is_popout(&l) {
                let _ = w.close();
            }
        }
    } else if is_popout(label) {
        let _ = window.app_handle().emit_to(MAIN, CLOSED_EVENT, label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lead_windows_are_popouts() {
        assert!(is_popout("lead-kaan"));
        assert!(is_popout("lead-h1a2b3:a1b2"));
        assert!(!is_popout("main"));
        assert!(!is_popout("snip-0"));
    }
}
