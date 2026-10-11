// Görev #33: UI density (Normal / Kompakt). A per-viewer convenience in localStorage; the tokens
// live in App.css under :root[data-density="compact"]. Default is compact.
import { refreshTermSize } from "./termZoom";

export type Density = "normal" | "compact";
const KEY = "dex.density";

export function density(): Density {
  try {
    if (localStorage.getItem(KEY) === "normal") return "normal";
  } catch { /* storage blocked: the default */ }
  return "compact";
}

/** Puts the stored density on <html>; call once before the first render. */
export function applyDensity(d: Density = density()) {
  document.documentElement.dataset.density = d;
}

/** Applies now and remembers it; open terminals follow the new --term-size (unless zoomed). */
export function setDensity(d: Density) {
  try {
    localStorage.setItem(KEY, d);
  } catch { /* still applies for this run */ }
  applyDensity(d);
  refreshTermSize();
}
