// #38: Windows Terminal copy/paste in the xterm panes. xterm itself turns Ctrl+C / Ctrl+V into ^C / ^V
// and cancels the keydown, so the webview never copies or pastes.
import type { Terminal } from "@xterm/xterm";

/**
 * For the pane's custom key handler: true when the key is copy/paste (xterm must not see it).
 * Ctrl(+Shift)+C copies the selection and clears it; plain Ctrl+C with nothing selected stays ^C.
 * Ctrl(+Shift)+V is left to the webview: its native paste event goes through xterm's paste path
 * (bracketed paste, onData) — no clipboard-read permission needed, and a watched pane's onData filter
 * still drops it (#22).
 */
export function clipboardKey(term: Terminal, e: KeyboardEvent): boolean {
  if (!e.ctrlKey || e.altKey || e.metaKey) return false;
  if (e.code === "KeyV") return true;
  if (e.code !== "KeyC" || (!e.shiftKey && !term.hasSelection())) return false;
  if (e.type === "keydown") {
    e.preventDefault(); // Ctrl+Shift+C: the webview's inspect-element
    if (term.hasSelection()) {
      void navigator.clipboard.writeText(term.getSelection()).catch(() => {});
      term.clearSelection();
    }
  }
  return true;
}
