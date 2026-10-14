// Görev #32: one xterm font size for every terminal pane — Ctrl+wheel steps it, Ctrl+0 resets it.
// Kept in localStorage (a per-viewer convenience); panes follow it through ZOOM_EVENT.
import type { Terminal } from "@xterm/xterm";
import { termFontSize, termLineHeight } from "./theme";

const KEY = "dex.termFontSize";
const MIN = 9;
const MAX = 24;
export const ZOOM_EVENT = "dex:term-font";

const clamp = (n: number) => Math.min(MAX, Math.max(MIN, Math.round(n)));

/** The stored size, else the design token's default. */
export function termSize(): number {
  try {
    const saved = Number(localStorage.getItem(KEY));
    if (saved) return clamp(saved);
  } catch { /* storage blocked: the default */ }
  return termFontSize();
}

export function setTermSize(n: number | null) {
  const next = n === null ? termFontSize() : clamp(n);
  try {
    if (n === null) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, String(next));
  } catch { /* still applies for this run */ }
  window.dispatchEvent(new CustomEvent(ZOOM_EVENT, { detail: next }));
}

/** The density tokens changed: open panes take termSize() (a stored zoom still wins) and --term-line again. */
export function refreshTermSize() {
  window.dispatchEvent(new CustomEvent(ZOOM_EVENT, { detail: termSize() }));
}

/** For the pane's custom key handler: true when Ctrl+0 was taken (xterm must not see it). */
export function zoomKey(e: KeyboardEvent): boolean {
  if (!e.ctrlKey || e.altKey || e.shiftKey || e.metaKey || e.code !== "Digit0") return false;
  if (e.type === "keydown") {
    e.preventDefault(); // the webview's own Ctrl+0 page zoom
    setTermSize(null);
  }
  return true;
}

/**
 * Ctrl+wheel over `host` steps the size (never the page zoom); every size change, from any pane,
 * is applied to `term` and `refit` runs — its fit fires term.onResize, which resizes the PTY.
 */
export function bindZoom(term: Terminal, host: HTMLElement, refit: () => void): () => void {
  const onWheel = (e: WheelEvent) => {
    if (!e.ctrlKey || e.deltaY === 0) return;
    e.preventDefault();
    e.stopPropagation(); // xterm would scroll the buffer
    setTermSize(termSize() + (e.deltaY < 0 ? 1 : -1));
  };
  const onSize = (e: Event) => {
    const size = (e as CustomEvent<number>).detail;
    const line = termLineHeight(); // #33: the density token can change it too
    if (term.options.fontSize === size && term.options.lineHeight === line) return;
    term.options.fontSize = size;
    term.options.lineHeight = line;
    refit();
  };
  host.addEventListener("wheel", onWheel, { capture: true, passive: false });
  window.addEventListener(ZOOM_EVENT, onSize);
  return () => {
    host.removeEventListener("wheel", onWheel, { capture: true });
    window.removeEventListener(ZOOM_EVENT, onSize);
  };
}
