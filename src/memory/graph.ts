// Hafıza haritasının saf mantığı: renk atama, komşuluk, filtre. DOM yok — birim testli.
// Sözleşme tipleri src/memory/api.ts'ten (Kernel-1); bu dosya yalnız saf mantık.
export type { MemEdge, MemGraph, MemNode, MemDoc } from "./api";
import type { MemEdge, MemNode } from "./api";

export type MemScope = MemNode["scope"];

/** Ajan başına sabit renk: id karmasından palet turu. shared tek nötr renk. */
const AGENT_COLORS = [
  "#7aa2f7", // mavi
  "#bb9af7", // mor
  "#7dcfff", // camgöbeği
  "#9ece6a", // yeşil
  "#e0af68", // turuncu
  "#f7768e", // pembe
  "#ff9e64", // kehribar
  "#89ddff", // açık camgöbeği
];
export const SHARED_COLOR = "#8b93a1";
export const MISSING_COLOR = "#4a4e55";

const hash = (s: string): number => {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
};

export function agentColor(node: Pick<MemNode, "scope" | "agentId">): string {
  if (node.scope === "shared" || node.scope === "team" || node.scope === "project") return SHARED_COLOR;
  if (node.scope === "missing" || !node.agentId) return MISSING_COLOR;
  return AGENT_COLORS[hash(node.agentId) % AGENT_COLORS.length];
}

/** Yönsüz komşuluk: kenar giden+gelen iki yönlü sayılır. */
export function buildAdjacency(nodes: MemNode[], edges: MemEdge[]): Map<string, Set<string>> {
  const ids = new Set(nodes.map((n) => n.id));
  const adj = new Map<string, Set<string>>(nodes.map((n) => [n.id, new Set<string>()]));
  for (const e of edges) {
    if (!ids.has(e.from) || !ids.has(e.to) || e.from === e.to) continue;
    adj.get(e.from)!.add(e.to);
    adj.get(e.to)!.add(e.from);
  }
  return adj;
}

export const degree = (adj: Map<string, Set<string>>, id: string): number => adj.get(id)?.size ?? 0;

export const neighbors = (adj: Map<string, Set<string>>, id: string): Set<string> =>
  new Set(adj.get(id) ?? []);

export interface MemFilter {
  q: string;
  agentId: string | null;
  scope: MemScope | null;
}

/** Arama ad+description'da (küçük harf, içerir); ajan/scope birebir. */
export function filterNodes(nodes: MemNode[], f: MemFilter): Set<string> {
  const q = f.q.trim().toLocaleLowerCase("tr");
  const out = new Set<string>();
  for (const n of nodes) {
    if (f.agentId && n.agentId !== f.agentId) continue;
    if (f.scope && n.scope !== f.scope) continue;
    if (q && !`${n.name}\n${n.description}`.toLocaleLowerCase("tr").includes(q)) continue;
    out.add(n.id);
  }
  return out;
}

/** Bağlantı sayısından düğüm yarıçapı: 5–14 px arası. */
export const nodeRadius = (deg: number): number => Math.min(14, 5 + Math.sqrt(Math.max(0, deg)) * 2.2);

/**
 * Gövdedeki `[[slug]]` → hedef düğüm id'si. Çözümü sunucu yaptı (aynı scope → shared → diğer
 * ajanlar); burada yalnız `from`'un giden kenarlarında slug'la biten hedef aranır.
 */
export function resolveLink(edges: MemEdge[], from: string, slug: string): string | null {
  const s = slug.split("|")[0].trim();
  // A qualified cross-scope link (`team/<tid>/shared/x`) names its target's full id.
  return edges.find((e) => e.from === from && (e.to === s || e.to.endsWith(`/${s}`)))?.to ?? null;
}
