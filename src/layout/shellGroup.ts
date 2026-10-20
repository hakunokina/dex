// #2266: the terminal group is fixed — the one grid group that stays (empty) when its last tab closes,
// so "Yeni terminal" and Süreçler always have a home. Every other group still goes when emptied.
// Pure, unit-tested; panels.ts resolves it from the live grid and remembers the id in the layout store.

export type GridGroup = { id: string; panels: string[] };

/** The tabs that make a group the terminal group when no id is remembered yet. */
export const isShellPanel = (id: string) => id.startsWith("shell:") || id === "procs";

/**
 * The fixed group's id: the remembered one while that group lives; else the first group holding a
 * terminal (or Süreçler); else an empty grid group (only the fixed one is ever left empty); else null —
 * the caller creates it once, at the default spot.
 */
export function resolveShellGroup(groups: GridGroup[], saved: string | undefined): string | null {
  if (saved && groups.some(g => g.id === saved)) return saved;
  return groups.find(g => g.panels.some(isShellPanel))?.id ?? groups.find(g => !g.panels.length)?.id ?? null;
}

/** Whether a group emptied by a close is removed: every group but the fixed one. */
export const removesEmptyGroup = (shellGroup: string | null, groupId: string) => shellGroup !== groupId;
