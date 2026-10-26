// #2443: "Tüm worker'lar" popover'ının kapsamı — uzmanlık aileleri ve Kart'ta fareyle seçim.
// Saf mantık (node:test); popover WorkersBulk.tsx'te, kart çizimi/olayları Office.tsx'te.

export interface Rect { x: number; y: number; w: number; h: number }

/** Two corners (any order) → a rect with non-negative size. */
export function rectFrom(a: { x: number; y: number }, b: { x: number; y: number }): Rect {
  return { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.abs(a.x - b.x), h: Math.abs(a.y - b.y) };
}

/** Below this many CSS px the pointer is a click, not a drag. */
export const DRAG_MIN = 4;
export const isDrag = (a: { x: number; y: number }, b: { x: number; y: number }) =>
  Math.abs(a.x - b.x) >= DRAG_MIN || Math.abs(a.y - b.y) >= DRAG_MIN;

/** Chips (centre x/y, size w/h — World's `Chip`) the rect touches; touching is enough. */
export function chipsInRect<T extends { x: number; y: number; w: number; h: number }>(chips: T[], r: Rect): T[] {
  return chips.filter((c) =>
    c.x + c.w / 2 >= r.x && c.x - c.w / 2 <= r.x + r.w && c.y + c.h / 2 >= r.y && c.y - c.h / 2 <= r.y + r.h);
}

interface Member { id: string; name: string; roleSlug: string | null; roleName: string | null }

/** A worker family: one role (`key` = its slug, "" = none) and its display name. */
export interface Family { key: string; label: string; hint: string | null; ids: string[] }

/** Agent names are the role's template plus a number (`Kernel-2`, dex-core `auto_name`). */
const stem = (name: string) => name.replace(/-\d+$/, "").trim();

/**
 * Workers grouped by role, in their given order. The label is the members' shared name stem
 * (Kernel, Lens…); hand-named members or two roles with one template (Bot) fall back to the role name.
 */
export function familiesOf(workers: Member[]): Family[] {
  const byKey = new Map<string, Member[]>();
  for (const w of workers) {
    const key = w.roleSlug ?? "";
    byKey.set(key, [...(byKey.get(key) ?? []), w]);
  }
  const out = [...byKey].map(([key, ms]) => {
    const stems = new Set(ms.map((m) => stem(m.name)));
    const role = ms[0].roleName ?? (key || null);
    const label = stems.size === 1 ? [...stems][0] : role ?? "Rolsüz";
    return { key, label, hint: role, ids: ms.map((m) => m.id) };
  });
  // The same label twice (two roles share a template): the role name tells them apart.
  const seen = new Map<string, number>();
  for (const f of out) seen.set(f.label, (seen.get(f.label) ?? 0) + 1);
  return out.map((f) => (seen.get(f.label)! > 1 && f.hint ? { ...f, label: `${f.label} (${f.hint})` } : f))
    .map((f) => (f.hint && f.label.includes(f.hint) ? { ...f, hint: null } : f));
}

export type Scope = { kind: "all" } | { kind: "roles"; keys: string[] } | { kind: "picked"; ids: string[] };

/** The workers a scope reaches; `null` = the whole team (the call carries no id filter). */
export function scopeIds(scope: Scope, workers: Member[]): string[] | null {
  if (scope.kind === "all") return null;
  if (scope.kind === "roles") return workers.filter((w) => scope.keys.includes(w.roleSlug ?? "")).map((w) => w.id);
  return workers.filter((w) => scope.ids.includes(w.id)).map((w) => w.id);
}

/** Popover title: "Tüm worker'lar", "Kernel, Lens · 3 worker", "Seçili · 2 worker". */
export function scopeTitle(scope: Scope, workers: Member[]): string {
  if (scope.kind === "all") return "Tüm worker'lar";
  const n = scopeIds(scope, workers)?.length ?? 0;
  if (scope.kind === "picked") return `Seçili · ${n} worker`;
  const { keys } = scope;
  const labels = familiesOf(workers).filter((f) => keys.includes(f.key)).map((f) => f.label);
  return `${labels.join(", ") || "Uzmanlık"} · ${n} worker`;
}
