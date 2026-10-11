// Patron düzeni (favori): bir takımın dock yerleşimini başka takımlara taşıyan saf eşleme.
// Kaydederken kaynak takımdaki lider/worker/shell id'leri sırayla saklanır; uygularken hedef
// takımın kendi ajanlarıyla (lider yuvası→lider, worker yuvası→worker) doldurulur. Ofis,
// pano, ayarlar, not gibi takıma özgü olmayan paneller aynen kalır.
import type { SerializedDockview } from "dockview-react";

export interface FavoriteRec {
  dock: SerializedDockview;
  leaders: string[];
  workers: string[];
  shells: string[];
  /** Kaynak takımın id'si (bilgi amaçlı). */
  from: string;
}

export interface FavoriteTarget {
  leaders: { id: string; name: string }[];
  workers: { id: string; name: string }[];
  /** Hedefteki shell id'leri (kaynak shell sırasına göre eşlenir; eksikse çağıran üretir). */
  shells: string[];
}

/** dockview grid düğümü — yalnızca leaf'lerdeki `views` dizilerindeki panel id'leri yeniden yazılır. */
type GridNode = { type: string; data: { id?: string; views?: unknown; activeView?: string } | GridNode[] };

function rewriteViews(node: GridNode | undefined, map: Map<string, string | null>): GridNode | null {
  if (!node) return null;
  if (node.type === "branch" && Array.isArray(node.data)) {
    const data = node.data.map(c => rewriteViews(c, map)).filter((c): c is GridNode => c !== null);
    return data.length ? { ...node, data } : null;
  }
  const d = node.data as { id?: string; views?: unknown; activeView?: string };
  if (node.type !== "leaf" || !Array.isArray(d.views)) return node; // float/özel düğüm: dokunma
  const views = (d.views as string[]).map(v => map.get(v) ?? v).filter(v => map.get(v) !== null);
  if (!views.length) return null; // yuvası düşen grup budanır
  const moved = d.activeView ? (map.get(d.activeView) ?? d.activeView) : undefined;
  const activeView = moved && views.includes(moved) ? moved : views[0];
  return { ...node, data: { ...d, views, activeView } };
}

export interface Remapped {
  dock: SerializedDockview;
  /** Eşlenecek hedef bulunamadığı için düşen kaynak panel id'leri. */
  dropped: string[];
}

/**
 * Favori dock'u hedef takıma uyarlar. Kaynak lider yuvası i → hedef lider i, worker i → worker i;
 * fazla kaynak yuva düşer, fazla hedef ajan panelsiz kalır (raf'tan açılır). Girdi değişmez.
 */
export function remapDock(dock: SerializedDockview, fav: Pick<FavoriteRec, "leaders" | "workers" | "shells">, t: FavoriteTarget): Remapped {
  const out = structuredClone(dock) as SerializedDockview & { panels: Record<string, { id: string; title?: string; params?: Record<string, string> }>; grid?: { root?: GridNode } };
  const map = new Map<string, string | null>();
  fav.leaders.forEach((id, i) => map.set(`agent:${id}`, i < t.leaders.length ? `agent:${t.leaders[i].id}` : null));
  fav.workers.forEach((id, i) => map.set(`agent:${id}`, i < t.workers.length ? `agent:${t.workers[i].id}` : null));
  fav.shells.forEach((id, i) => map.set(`shell:${id}`, i < t.shells.length ? `shell:${t.shells[i]}` : null));

  const dropped: string[] = [];
  const panels = out.panels ?? {};
  for (const [pid, spec] of Object.entries(panels)) {
    if (!map.has(pid)) continue; // ofis/pano/ayarlar/not: aynen
    const next = map.get(pid);
    if (next === null || next === undefined) {
      delete panels[pid];
      dropped.push(pid);
      continue;
    }
    if (next === pid) continue;
    delete panels[pid];
    const agent = t.leaders.concat(t.workers).find(a => `agent:${a.id}` === next);
    panels[next] = {
      ...spec,
      id: next,
      ...(agent ? { title: agent.name, params: { ...(spec.params ?? {}), agentId: agent.id } } : {}),
      ...(!agent && next.startsWith("shell:") ? { params: { ...(spec.params ?? {}), shellId: next.slice("shell:".length) } } : {}),
    };
  }
  if (out.grid?.root) out.grid.root = (rewriteViews(out.grid.root as GridNode, map) ?? out.grid.root) as typeof out.grid.root;
  return { dock: out as SerializedDockview, dropped };
}

