// #2086: "Ortak not" and "Takım notu" became one "Notlar" panel. A saved dock (the team's or the
// favourite) may still carry the two old pages: the first one in the layout (grid first, then floats)
// becomes the new panel in its place, the other leaves its group, and a group left empty goes.
// Pure (type-only import), unit-tested; panels.ts runs it before every `fromJSON`.
import type { SerializedDockview } from "dockview-react";

export const NOTES_ID = "notes";
export const NOTES_TITLE = "Notlar";
const OLD_IDS = new Set(["notes:shared", "notes:team"]);

type Group = { id?: string; views?: unknown; activeView?: string };
type Node = { type: string; data: Group | Node[]; size?: number };
type Dock = {
  grid?: { root?: Node };
  panels?: Record<string, unknown>;
  activeGroup?: string;
  floatingGroups?: { data: Group }[];
  popoutGroups?: { data: Group }[];
};

const viewsOf = (g: Group) => (Array.isArray(g.views) ? (g.views as string[]) : []);

function leaves(n: Node | undefined, out: Group[] = []): Group[] {
  if (!n) return out;
  if (Array.isArray(n.data)) n.data.forEach(c => leaves(c, out));
  else if (n.type === "leaf") out.push(n.data);
  return out;
}

/** The group with the old pages renamed (`keep` → the new id) or dropped; null when nothing is left. */
function rewrite(g: Group, keep: string | null): Group | null {
  const views = viewsOf(g);
  if (!views.some(v => OLD_IDS.has(v))) return g;
  const next = views.map(v => (v === keep ? NOTES_ID : v)).filter(v => !OLD_IDS.has(v));
  if (!next.length) return null;
  const active = g.activeView && OLD_IDS.has(g.activeView) ? NOTES_ID : g.activeView;
  return { ...g, views: next, activeView: active && next.includes(active) ? active : next[0] };
}

function prune(n: Node, keep: string | null, gone: Set<string>): Node | null {
  if (Array.isArray(n.data)) {
    const data = n.data.map(c => prune(c, keep, gone)).filter((c): c is Node => c !== null);
    return data.length ? { ...n, data } : null;
  }
  if (n.type !== "leaf") return n;
  const g = rewrite(n.data, keep);
  if (!g && n.data.id) gone.add(n.data.id);
  return g && { ...n, data: g };
}

/** The dock with the old notes pages folded into the one panel; the same object when there are none. */
export function migrateNotesDock<T extends SerializedDockview>(dock: T): T {
  const d = dock as unknown as Dock;
  const grid = leaves(d.grid?.root);
  const floats = [...(d.floatingGroups ?? []), ...(d.popoutGroups ?? [])].map(f => f.data);
  const all = [...grid, ...floats].flatMap(viewsOf);
  const hasOld = all.some(v => OLD_IDS.has(v)) || Object.keys(d.panels ?? {}).some(id => OLD_IDS.has(id));
  if (!hasOld) return dock;

  const out = structuredClone(d);
  const keep = all.includes(NOTES_ID) ? null : all.find(v => OLD_IDS.has(v)) ?? null;
  const gone = new Set<string>();
  const root = out.grid?.root && prune(out.grid.root, keep, gone);
  if (out.grid && root) out.grid.root = root;
  for (const key of ["floatingGroups", "popoutGroups"] as const) {
    if (!out[key]) continue;
    out[key] = out[key]!.flatMap(f => {
      const g = rewrite(f.data, keep);
      if (!g && f.data.id) gone.add(f.data.id);
      return g ? [{ ...f, data: g }] : [];
    });
  }
  const panels = out.panels ?? {};
  for (const id of OLD_IDS) delete panels[id];
  if (keep) panels[NOTES_ID] = { id: NOTES_ID, contentComponent: "notes", tabComponent: "notes", title: NOTES_TITLE };
  out.panels = panels;
  if (out.activeGroup && gone.has(out.activeGroup)) {
    out.activeGroup = [...leaves(out.grid?.root), ...(out.floatingGroups ?? []).map(f => f.data)]
      .find(g => viewsOf(g).includes(NOTES_ID))?.id;
  }
  return out as unknown as T;
}
