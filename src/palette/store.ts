// FIX-5: the palette's open/close store + global shortcut, without the panel UI. Imported by
// `App` (first-open chunk) so Ctrl+K works before the palette view itself loads lazily.
import { useSyncExternalStore } from "react";

let isOpen = false;
let returnFocus: HTMLElement | null = null;
const subs = new Set<() => void>();
export function setPaletteOpen(v: boolean, restore = false) {
  if (v === isOpen) return;
  if (v) returnFocus = document.activeElement as HTMLElement | null;
  else if (restore) returnFocus?.focus();
  isOpen = v;
  subs.forEach((f) => f());
}
export const usePaletteOpen = () => useSyncExternalStore((f) => (subs.add(f), () => subs.delete(f)), () => isOpen);

function onKey(e: KeyboardEvent) {
  // The terminal and the notes editor (DEX-41) keep Ctrl+K for themselves.
  const inTerm = !!(e.target as Element | null)?.closest?.(".xterm, .notes-editor");
  const k = e.code === "KeyK" && !e.altKey && !e.shiftKey && (e.ctrlKey ? !e.metaKey && !inTerm : e.metaKey);
  const p = e.code === "KeyP" && e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey;
  if (k || p) {
    e.preventDefault();
    e.stopImmediatePropagation();
    setPaletteOpen(!isOpen, true);
  } else if (isOpen && e.key === "Escape") {
    e.preventDefault();
    e.stopImmediatePropagation();
    setPaletteOpen(false, true);
  }
}
// Registered before any component's (Board closes on Esc in a window capture listener too —
// the palette's Esc must win).
window.addEventListener("keydown", onKey, true);
import.meta.hot?.dispose(() => window.removeEventListener("keydown", onKey, true));
