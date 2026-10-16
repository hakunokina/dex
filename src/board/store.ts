// FIX-5: the board's open/close store without the views, so the shortcut and the palette
// (`openBoard`) stay in the first-open chunk while `Board`/`BoardView` load lazily.
import { useSyncExternalStore } from "react";
import { inConfirm } from "../confirmModel";

let open = false;
const subs = new Set<() => void>();
const setOpen = (v: boolean) => {
  open = v;
  subs.forEach((f) => f());
};
export const toggleBoard = () => setOpen(!open);
export const closeBoard = () => setOpen(false);
export const isBoardOpen = () => open;
/** Command palette (DEX-6). */
export const openBoard = () => setOpen(true);
export const useBoardOpen = () =>
  useSyncExternalStore(
    (f) => (subs.add(f), () => subs.delete(f)),
    () => open,
  );

const isTyping = (t: EventTarget | null) =>
  t instanceof HTMLElement && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));

function onKey(e: KeyboardEvent) {
  // The key under Esc (physical, so any layout) toggles. It types a character, so while
  // typing (terminal, inputs) only Ctrl+it toggles and the plain key passes through.
  // Capture phase so the focused terminal never sees the toggle. Ctrl+B belongs to Claude.
  // DEX-41: in the notes editor every key is text, Ctrl+` included.
  const notes = !!(e.target as Element | null)?.closest?.(".notes-editor");
  if (e.code === "Backquote" && !e.shiftKey && !e.altKey && !e.metaKey && !notes && (e.ctrlKey || !isTyping(e.target))) {
    e.preventDefault();
    e.stopPropagation();
    toggleBoard();
  } else if (e.key === "Escape" && isBoardOpen() && !inConfirm(e.target)) {
    toggleBoard();
  }
}
window.addEventListener("keydown", onKey, true);
import.meta.hot?.dispose(() => window.removeEventListener("keydown", onKey, true));
