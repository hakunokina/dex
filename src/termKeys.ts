// FIX-1: a focused terminal keeps Tab (Claude Code's completion / mode switch needs it), so the
// keyboard needs another way out: Ctrl+Tab / Ctrl+Shift+Tab move focus to the next / previous control
// outside the terminal. Window capture, like the palette: it runs before xterm's own key handler.

const TABBABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]';

/** The xterm that had the keyboard last — the palette steals focus before its command runs. */
let lastTerm: Element | null = null;

const termOf = (el: EventTarget | null) => (el instanceof Element ? el.closest(".xterm") : null);

function tabbables(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(TABBABLE)].filter(el =>
    !(el as HTMLButtonElement).disabled && el.tabIndex >= 0 && !el.closest("[inert], .xterm") && el.checkVisibility({ visibilityProperty: true }),
  );
}

/** Focus the control after (or before) the terminal in document order, wrapping around. */
export function leaveTerminal(back = false, from: Element | null = termOf(document.activeElement) ?? lastTerm): boolean {
  if (!from?.isConnected) return false;
  const all = tabbables();
  if (!all.length) return false;
  const after = all.findIndex(el => from.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING);
  const next = back
    ? all[(after < 0 ? all.length : after) - 1] ?? all[all.length - 1]
    : all[after] ?? all[0];
  next.focus();
  return true;
}

function onKey(e: KeyboardEvent) {
  if (e.key !== "Tab" || !e.ctrlKey || e.altKey || e.metaKey) return;
  if (!termOf(e.target)) return;
  e.preventDefault();
  e.stopImmediatePropagation();
  leaveTerminal(e.shiftKey, termOf(e.target));
}

function onFocusIn(e: FocusEvent) {
  const t = termOf(e.target);
  if (t) lastTerm = t;
}

window.addEventListener("keydown", onKey, true);
window.addEventListener("focusin", onFocusIn, true);
import.meta.hot?.dispose(() => {
  window.removeEventListener("keydown", onKey, true);
  window.removeEventListener("focusin", onFocusIn, true);
});
