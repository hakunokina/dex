// #2258: a leader's pane in its own native window (PiP-like). Pure model — no Tauri, no React:
// the records layout.json keeps, the window label/url derivation and the "detach or focus" rule.
// The main window owns every record (src/layout/store.ts `popouts`); the pop-out window only
// shows the pane (src/lead/LeadWindow.tsx) and reports its bounds / on-top switch back.

/** Logical pixels (what `WebviewWindow` options take). */
export interface PopoutBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface PopoutRec {
  teamId: string;
  /** The window exists (or did when the app last ran: reopened at startup). */
  open: boolean;
  onTop: boolean;
  /** Last known place; null until the window first moved/resized. */
  bounds: PopoutBounds | null;
}

export const POPOUT_LABEL_PREFIX = "lead-";
/** Default window size and the smallest the terminal stays readable at. */
export const POPOUT_SIZE = { width: 720, height: 480, minWidth: 360, minHeight: 240 } as const;

/** Tauri window labels allow `a-zA-Z0-9-/:_`; an agent id (`<host>:<id>`) fits, anything else is replaced. */
export const popoutLabel = (agentId: string) => `${POPOUT_LABEL_PREFIX}${agentId.replace(/[^A-Za-z0-9\-/:_]/g, "_")}`;
export const isPopoutLabel = (label: string) => label.startsWith(POPOUT_LABEL_PREFIX);

/** The pop-out page: the same bundle, routed by `pane=lead` (src/main.tsx). */
export function popoutUrl(teamId: string, agentId: string, onTop: boolean): string {
  const q = new URLSearchParams({ pane: "lead", team: teamId, agent: agentId });
  if (onTop) q.set("ontop", "1");
  return `index.html?${q}`;
}

export interface LeadQuery {
  teamId: string;
  agentId: string;
  onTop: boolean;
}
/** `?pane=lead&team=…&agent=…[&ontop=1]`; null when this page is not a pop-out. */
export function parseLeadQuery(search: string): LeadQuery | null {
  const q = new URLSearchParams(search);
  const teamId = q.get("team");
  const agentId = q.get("agent");
  if (q.get("pane") !== "lead" || !teamId || !agentId) return null;
  return { teamId, agentId, onTop: q.get("ontop") === "1" };
}

/** Same pane again → the open window comes forward instead of a second one. */
export const detachAction = (popouts: Record<string, PopoutRec>, agentId: string): "focus" | "open" =>
  popouts[agentId]?.open ? "focus" : "open";

export function parseBounds(raw: unknown): PopoutBounds | null {
  const { x, y, width, height } = (raw ?? {}) as Record<string, unknown>;
  const n = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
  return n(x) && n(y) && n(width) && n(height) && width > 0 && height > 0 ? { x, y, width, height } : null;
}

/** layout.json `popouts`: `{ [agentId]: { teamId, open, onTop, bounds } }`; a bad entry is dropped. */
export function parsePopouts(raw: unknown): Record<string, PopoutRec> {
  const out: Record<string, PopoutRec> = {};
  if (typeof raw !== "object" || raw === null) return out;
  for (const [agentId, rec] of Object.entries(raw as Record<string, unknown>)) {
    const { teamId, open, onTop, bounds } = (rec ?? {}) as Record<string, unknown>;
    if (!agentId || typeof teamId !== "string" || !teamId) continue;
    out[agentId] = { teamId, open: open === true, onTop: onTop === true, bounds: parseBounds(bounds) };
  }
  return out;
}

/** `WebviewWindow` options for a (re)opened pop-out: its last place, else the default size. */
export function popoutWindowOptions(rec: PopoutRec, title: string, url: string) {
  return {
    url,
    title,
    ...(rec.bounds ?? { width: POPOUT_SIZE.width, height: POPOUT_SIZE.height }),
    minWidth: POPOUT_SIZE.minWidth,
    minHeight: POPOUT_SIZE.minHeight,
    alwaysOnTop: rec.onTop,
    decorations: false,
    theme: "dark" as const,
    backgroundColor: "#0e0f11",
  };
}
