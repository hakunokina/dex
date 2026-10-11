// Palette ordering (pure — node:test): fuzzy rank, then grouped into Takımlar / Ajanlar / Eylemler.
import { fuzzyScore } from "./fuzzy.ts";

export type Group = "Takımlar" | "Ajanlar" | "Eylemler";
export const RECENT_GROUP = "Son kullanılan";
const ORDER: Group[] = ["Takımlar", "Ajanlar", "Eylemler"];

export interface Rankable {
  id: string;
  title: string;
  hint: string;
  keywords?: string;
  group: Group;
}

export interface Section<C> {
  label: string;
  items: C[];
}

function rank<C extends Rankable>(cmds: C[], query: string, recent: string[]): C[] {
  const rec = (id: string) => {
    const i = recent.indexOf(id);
    return i < 0 ? Infinity : i;
  };
  return cmds
    .map((c, i) => {
      const s = fuzzyScore(query, c.title) ?? (fuzzyScore(query, `${c.title} ${c.hint} ${c.keywords ?? ""}`) ?? NaN) - 200;
      const r = rec(c.id);
      return { c, i, s: s + (r < Infinity ? 40 - r * 4 : 0) };
    })
    .filter((x) => !Number.isNaN(x.s))
    .sort((a, b) => b.s - a.s || a.i - b.i)
    .map((x) => x.c);
}

/**
 * Empty query: recents first, then the fixed group order. With a query: groups follow their best
 * match, so the first row (Enter's target) is still the best hit overall.
 */
export function sections<C extends Rankable>(cmds: C[], query: string, recent: string[]): Section<C>[] {
  const out: Section<C>[] = [];
  const push = (label: string, c: C) => {
    const s = out.find((x) => x.label === label);
    if (s) s.items.push(c);
    else out.push({ label, items: [c] });
  };
  if (!query.trim()) {
    const recents = recent.map((id) => cmds.find((c) => c.id === id)).filter((c): c is C => !!c);
    if (recents.length) out.push({ label: RECENT_GROUP, items: recents });
    for (const g of ORDER) for (const c of cmds) if (c.group === g && !recents.includes(c)) push(g, c);
    return out;
  }
  for (const c of rank(cmds, query, recent)) push(c.group, c);
  return out;
}
