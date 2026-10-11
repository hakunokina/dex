// Hafıza haritası paneli (Kademe 1, salt okuma): force graph + sağ detay + sol-alt ajan listesi.
// Düzenleme düğmesi yok. Veri src/memory/api.ts'ten (Kernel-1).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { forceCenter, forceCollide, forceLink, forceManyBody, forceSimulation, type Simulation, type SimulationLinkDatum, type SimulationNodeDatum } from "d3-force";
import { Maximize, Minus, Plus, RefreshCw } from "lucide-react";
import { renderMarkdown } from "../chat/md";
import { membersOf, useTeams } from "../team/store";
import { activeProjects, memoryProjectId } from "../project/model";
import { reloadProjects, useTeamProjects } from "../project/store";
import "../project/project.css";
import { memoryGraph, memoryRead, MemoryUnsupportedError, type MemDoc, type MemGraph, type MemNode } from "./api";
import { agentColor, buildAdjacency, degree, filterNodes, nodeRadius, resolveLink } from "./graph";
import "./memory.css";

interface SimNode extends SimulationNodeDatum {
  id: string;
  mem: MemNode;
}
interface SimLink extends SimulationLinkDatum<SimNode> {
  key: string;
}

interface View {
  x: number;
  y: number;
  k: number;
}

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** [[slug]] → tıklanabilir düğme; geri kalan renderMarkdown'dan (md.ts'e dokunmadan). */
function renderBody(body: string): string {
  const parts = body.split(/\[\[([^\]\n]+)\]\]/g);
  return parts
    .map((p, i) => (i % 2 === 1 ? `<button type="button" class="mem-link" data-mem="${esc(p.trim())}">${esc(p.trim())}</button>` : renderMarkdown(p)))
    .join("");
}

const SCOPE_LABEL = { shared: "Ortak", agent: "Ajan", missing: "Eksik", team: "Takım", project: "Proje", private: "Özel" } as const;

export function MemoryMap({ teamId }: { teamId: string }) {
  const projects = useTeamProjects(teamId);
  const team = useTeams().teams.find(t => t.id === teamId);
  const [choice, setChoice] = useState<string | undefined>();
  const open = activeProjects(projects.list);
  const candidate = choice === undefined ? (team?.defaultProjectId ?? (open.length === 1 ? open[0].id : null)) : choice || null;
  const projectId = memoryProjectId(projects.list, candidate);
  // Reset in this render: the keyed graph and its detail unmount together; reopening does not
  // resurrect the old explicit selection. Only a successfully loaded list is authoritative.
  if (projects.status === "ready" && candidate && !projectId) setChoice("");
  if (projects.status === "loading") return <div className="mem-state" role="status">Projeler yükleniyor…</div>;
  if (projects.status === "error") return <div className="mem-state" role="alert">{projects.error} <button onClick={() => void reloadProjects(teamId)}>Yeniden dene</button></div>;
  if (projects.status === "unsupported") return <MemoryGraph key={teamId} teamId={teamId} projectId={null} legacy />;
  return <div className="project-memory">
    <label className="project-memory-scope">Hafıza bağlamı
      <select aria-label="Hafıza bağlamı" className="project-pick" value={projectId ?? ""} onChange={e => setChoice(e.target.value)}>
        <option value="">Takım (projesiz)</option>
        {open.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
      </select>
    </label>
    <MemoryGraph key={`${teamId}:${projectId ?? "team"}`} teamId={teamId} projectId={projectId} legacy={false} />
  </div>;
}

function MemoryGraph({ teamId, projectId, legacy }: { teamId: string; projectId: string | null; legacy: boolean }) {
  const [status, setStatus] = useState<"loading" | "unsupported" | "error" | "ready">("loading");
  const [graph, setGraph] = useState<MemGraph>({ nodes: [], edges: [] });
  const [selected, setSelected] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const [agentId, setAgentId] = useState<string | null>(null);
  const [scope, setScope] = useState<MemNode["scope"] | null>(null);
  const [view, setViewState] = useState<View>({ x: 0, y: 0, k: 1 });
  const viewRef = useRef(view);
  const setView = useCallback((v: View | ((prev: View) => View)) => {
    setViewState((prev) => {
      const next = typeof v === "function" ? v(prev) : v;
      viewRef.current = next;
      return next;
    });
  }, []);
  const [doc, setDoc] = useState<MemDoc | null>(null);
  const [docLoading, setDocLoading] = useState(false);
  const [docError, setDocError] = useState<string | null>(null);
  const requestRef = useRef(0);
  const [, setTick] = useState(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const simRef = useRef<Simulation<SimNode, SimLink> | null>(null);
  const nodesRef = useRef<SimNode[]>([]);
  const dragRef = useRef<{ id: string; moved: boolean } | null>(null);
  const panRef = useRef<{ sx: number; sy: number; x: number; y: number } | null>(null);

  const load = useCallback(async () => {
    const request = ++requestRef.current;
    setStatus("loading");
    try {
      const g = await memoryGraph(teamId, projectId, legacy);
      if (request !== requestRef.current) return;
      setGraph(g);
      setSelected((s) => (s && g.nodes.some((n) => n.id === s) ? s : null));
      setStatus("ready");
    } catch (err) {
      if (request !== requestRef.current) return;
      setStatus(err instanceof MemoryUnsupportedError ? "unsupported" : "error");
    }
  }, [teamId, projectId, legacy]);

  useEffect(() => {
    void load();
    return () => { requestRef.current++; };
  }, [load]);

  const adj = useMemo(() => buildAdjacency(graph.nodes, graph.edges), [graph]);
  const visible = useMemo(() => filterNodes(graph.nodes, { q, agentId, scope }), [graph, q, agentId, scope]);
  const selNeighbors = useMemo(() => (selected ? new Set([selected, ...Array.from(adj.get(selected) ?? [])]) : null), [adj, selected]);

  const agents = useMemo(() => {
    const counts = new Map<string, number>();
    for (const n of graph.nodes) if (n.agentId) counts.set(n.agentId, (counts.get(n.agentId) ?? 0) + 1);
    const names = new Map(membersOf(teamId).map((a) => [a.id, a.name]));
    return [...counts.entries()]
      .map(([id, count]) => ({ id, count, name: names.get(id) ?? id }))
      .sort((a, b) => a.name.localeCompare(b.name, "tr"));
  }, [graph, teamId]);

  // ---- d3-force simülasyonu ----
  useEffect(() => {
    if (status !== "ready") return;
    const nodes: SimNode[] = graph.nodes.map((m) => ({ id: m.id, mem: m }));
    nodesRef.current = nodes;
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const links: SimLink[] = [];
    for (const e of graph.edges) {
      const s = byId.get(e.from);
      const t = byId.get(e.to);
      if (s && t && s !== t) links.push({ source: s, target: t, key: `${e.from}→${e.to}` });
    }
    const sim = forceSimulation(nodes)
      .force("link", forceLink<SimNode, SimLink>(links).id((d) => d.id).distance(90))
      .force("charge", forceManyBody().strength(-160))
      .force("center", forceCenter(0, 0))
      .force("collide", forceCollide<SimNode>((d) => nodeRadius(degree(adj, d.id)) + 6));
    sim.on("tick", () => setTick((t) => t + 1));
    simRef.current = sim;
    return () => {
      sim.stop();
      simRef.current = null;
    };
  }, [status, graph, adj]);

  const nodeById = useCallback((id: string) => nodesRef.current.find((n) => n.id === id), []);

  const focusNode = useCallback((id: string) => {
    setSelected(id);
    const n = nodeById(id);
    if (n?.x != null && n?.y != null) setView((v) => ({ ...v, x: n.x!, y: n.y! }));
  }, [nodeById]);

  // Detay: seçim değişince gövdeyi oku.
  useEffect(() => {
    if (!selected) {
      setDoc(null);
      return;
    }
    const n = graph.nodes.find((m) => m.id === selected);
    if (!n || n.scope === "missing") {
      setDoc(null);
      return;
    }
    let live = true;
    setDoc(null);
    setDocError(null);
    setDocLoading(true);
    memoryRead(teamId, selected, projectId, legacy)
      .then((d) => {
        if (live) setDoc(d);
      })
      .catch((err) => {
        if (live) {
          setDocError(err instanceof Error ? err.message : String(err));
          if (err instanceof MemoryUnsupportedError) setStatus("unsupported");
        }
      })
      .finally(() => {
        if (live) setDocLoading(false);
      });
    return () => {
      live = false;
    };
  }, [selected, teamId, graph, projectId, legacy]);

  // ---- görünüm: tekerlek zoom, zemin sürükle-pan ----
  const toWorld = (sx: number, sy: number): { x: number; y: number } => {
    const r = wrapRef.current!.getBoundingClientRect();
    const v = viewRef.current;
    return { x: v.x + (sx - r.left - r.width / 2) / v.k, y: v.y + (sy - r.top - r.height / 2) / v.k };
  };

  const onWheel = (e: React.WheelEvent) => {
    const k = Math.min(3, Math.max(0.3, view.k * (e.deltaY > 0 ? 0.9 : 1.1)));
    const r = wrapRef.current!.getBoundingClientRect();
    const px = e.clientX - r.left - r.width / 2;
    const py = e.clientY - r.top - r.height / 2;
    const wx = view.x + px / view.k;
    const wy = view.y + py / view.k;
    setView({ k, x: wx - px / k, y: wy - py / k });
  };

  const fit = () => {
    const ns = nodesRef.current.filter((n) => n.x != null);
    if (!ns.length) {
      setView({ x: 0, y: 0, k: 1 });
      return;
    }
    const xs = ns.map((n) => n.x!);
    const ys = ns.map((n) => n.y!);
    const r = wrapRef.current!.getBoundingClientRect();
    const k = Math.min(2, Math.max(0.3, Math.min((r.width - 80) / (Math.max(...xs) - Math.min(...xs) + 120), (r.height - 80) / (Math.max(...ys) - Math.min(...ys) + 120))));
    setView({ k, x: (Math.min(...xs) + Math.max(...xs)) / 2, y: (Math.min(...ys) + Math.max(...ys)) / 2 });
  };

  const onNodePointerDown = (id: string) => (e: React.PointerEvent) => {
    e.stopPropagation();
    (e.target as Element).setPointerCapture(e.pointerId);
    dragRef.current = { id, moved: false };
    const n = nodeById(id);
    if (n && n.x != null) {
      n.fx = n.x;
      n.fy = n.y;
    }
    simRef.current?.alphaTarget(0.3).restart();
  };
  const onNodePointerMove = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    d.moved = true;
    const n = nodeById(d.id);
    if (n) {
      const w = toWorld(e.clientX, e.clientY);
      n.fx = w.x;
      n.fy = w.y;
    }
  };
  const onNodePointerUp = (id: string) => () => {
    const d = dragRef.current;
    dragRef.current = null;
    simRef.current?.alphaTarget(0);
    const n = nodeById(id);
    if (n) {
      n.fx = null;
      n.fy = null;
    }
    if (d && !d.moved) setSelected(id);
  };

  const onBodyClick = (e: React.MouseEvent) => {
    const b = (e.target as HTMLElement).closest("[data-mem]");
    if (!b) return;
    const hit = selected ? resolveLink(graph.edges, selected, b.getAttribute("data-mem")!) : null;
    if (hit && visible.has(hit)) focusNode(hit);
  };

  const out = selected ? graph.edges.filter((e) => e.from === selected).map((e) => e.to) : [];
  const incoming = selected ? graph.edges.filter((e) => e.to === selected).map((e) => e.from) : [];
  const selNode = selected ? graph.nodes.find((n) => n.id === selected) : null;
  const selAgentName = selNode?.agentId ? agents.find((a) => a.id === selNode.agentId)?.name ?? selNode.agentId : null;

  if (status === "loading") return <div className="mem-state">Hafıza haritası yükleniyor…</div>;
  if (status === "unsupported") return <div className="mem-state">Bu sunucu hafıza haritasını desteklemiyor.</div>;
  if (status === "error") {
    return (
      <div className="mem-state">
        Hafıza haritası okunamadı. <button type="button" className="mem-btn" onClick={() => void load()}>Yeniden dene</button>
      </div>
    );
  }
  if (!graph.nodes.length) {
    return (
      <div className="mem-state">
        Henüz hafıza yok. <button type="button" className="mem-btn" onClick={() => void load()}>Yenile</button>
      </div>
    );
  }

  return (
    <div className="mem-map">
      <div className="mem-toolbar" role="toolbar" aria-label="Hafıza haritası araçları">
        <span className="mem-count">{graph.nodes.length} hafıza · {graph.edges.length} bağ</span>
        <input className="mem-search" type="search" placeholder="Hafızalarda ara…" aria-label="Hafızalarda ara" value={q} onChange={(e) => setQ(e.target.value)} />
        <select className="mem-select" aria-label="Kapsam süzgeci" value={scope ?? ""} onChange={(e) => setScope((e.target.value || null) as typeof scope)}>
          <option value="">Tüm kapsamlar</option>
          {legacy ? <><option value="shared">Ortak</option><option value="agent">Ajan</option></> :
            <><option value="team">Takım</option><option value="project">Proje</option><option value="private">Özel</option></>}
          <option value="missing">Eksik</option>
        </select>
        <button type="button" className="mem-btn" title="Yaklaştır" aria-label="Yaklaştır" onClick={() => setView((v) => ({ ...v, k: Math.min(3, v.k * 1.2) }))}><Plus /></button>
        <button type="button" className="mem-btn" title="Uzaklaştır" aria-label="Uzaklaştır" onClick={() => setView((v) => ({ ...v, k: Math.max(0.3, v.k / 1.2) }))}><Minus /></button>
        <button type="button" className="mem-btn" title="Sığdır" aria-label="Grafikte sığdır" onClick={fit}><Maximize /></button>
        <button type="button" className="mem-btn" title="Yenile" aria-label="Hafıza haritasını yenile" onClick={() => void load()}><RefreshCw /></button>
      </div>
      <div className="mem-body">
        <div
          className="mem-canvas-wrap" ref={wrapRef}
          onWheel={onWheel}
          onPointerDown={(e) => {
            panRef.current = { sx: e.clientX, sy: e.clientY, x: view.x, y: view.y };
            (e.target as Element).setPointerCapture?.(e.pointerId);
          }}
          onPointerMove={(e) => {
            const p = panRef.current;
            if (p) setView((v) => ({ ...v, x: p.x - (e.clientX - p.sx) / v.k, y: p.y - (e.clientY - p.sy) / v.k }));
          }}
          onPointerUp={() => {
            panRef.current = null;
          }}
        >
          <svg className="mem-canvas" role="group" aria-label="Hafıza grafiği">
            <g transform={`translate(${view.x} ${view.y}) scale(${view.k})`}>
              {nodesRef.current.length > 0 && (
                <g>
                  {graph.edges.map((e) => {
                    const s = nodeById(e.from);
                    const t = nodeById(e.to);
                    if (!s || !t || s.x == null || t.x == null) return null;
                    const on = !selNeighbors || (selNeighbors.has(e.from) && selNeighbors.has(e.to));
                    return <line key={`${e.from}→${e.to}`} x1={s.x} y1={s.y} x2={t.x} y2={t.y} className={`mem-edge${on ? "" : " dim"}`} />;
                  })}
                  {nodesRef.current.map((sn) => {
                    if (sn.x == null || sn.y == null) return null;
                    const deg = degree(adj, sn.id);
                    const isSel = sn.id === selected;
                    const inFilter = visible.has(sn.id);
                    const on = inFilter && (!selNeighbors || selNeighbors.has(sn.id));
                    const missing = sn.mem.scope === "missing";
                    return (
                      <g key={sn.id} transform={`translate(${sn.x} ${sn.y})`} opacity={on ? 1 : 0.12}>
                        <title>{sn.mem.name}</title>
                        <circle
                          r={nodeRadius(deg)} fill={agentColor(sn.mem)}
                          className={`mem-node${isSel ? " sel" : ""}${missing ? " missing" : ""}`}
                          tabIndex={0} role="button" aria-label={`${sn.mem.name}${deg ? `, ${deg} bağ` : ""}`}
                          onPointerDown={onNodePointerDown(sn.id)}
                          onPointerMove={onNodePointerMove}
                          onPointerUp={onNodePointerUp(sn.id)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              focusNode(sn.id);
                            }
                          }}
                        />
                        {(isSel || view.k > 1.4) && (
                          <text y={-nodeRadius(deg) - 4} textAnchor="middle" className="mem-label">{sn.mem.name}</text>
                        )}
                      </g>
                    );
                  })}
                </g>
              )}
            </g>
          </svg>
          <div className="mem-agents" role="group" aria-label="Ajan süzgeci">
            <button type="button" className={`mem-agent${agentId === null ? " on" : ""}`} onClick={() => setAgentId(null)}>
              Tümü <b>{graph.nodes.length}</b>
            </button>
            {agents.map((a) => (
              <button key={a.id} type="button" className={`mem-agent${agentId === a.id ? " on" : ""}`}
                onClick={() => setAgentId((cur) => (cur === a.id ? null : a.id))} title={`${a.name} — yalnız onun hafızaları`}>
                <span className="mem-agent-dot" style={{ background: agentColor({ scope: "agent", agentId: a.id }) }} />
                {a.name} <b>{a.count}</b>
              </button>
            ))}
          </div>
        </div>
        <aside className="mem-detail" aria-label="Hafıza detayı">
          {!selNode ? (
            <p className="mem-hint">İnceleyeceğin hafızayı seç.</p>
          ) : (
            <>
              <h2>{selNode.name}</h2>
              <div className="mem-badges">
                <span className="mem-badge">{SCOPE_LABEL[selNode.scope]}</span>
                {(selNode.scope === "agent" || selNode.scope === "private") && selAgentName && <span className="mem-badge">{selAgentName}</span>}
                {selNode.type && <span className="mem-badge dim">{selNode.type}</span>}
              </div>
              <p className="mem-meta">Güncellendi: {new Date(selNode.updatedAt).toLocaleString("tr-TR")}</p>
              {selNode.description && <p className="mem-desc">{selNode.description}</p>}
              {docLoading ? (
                <p className="mem-hint">Okunuyor…</p>
              ) : docError ? (
                <p className="mem-hint" role="alert">Hafıza okunamadı: {docError}</p>
              ) : doc ? (
                <div className="mem-body-md chat-md" onClick={onBodyClick} dangerouslySetInnerHTML={{ __html: renderBody(doc.body) }} />
              ) : selNode.scope === "missing" ? (
                <p className="mem-hint">Bu hafıza dosyası bulunamadı (bağlantı kopuk).</p>
              ) : null}
              {(out.length > 0 || incoming.length > 0) && (
                <div className="mem-links">
                  <h3>Bağlı hafızalar</h3>
                  {[
                    { title: "Giden", ids: out },
                    { title: "Gelen", ids: incoming },
                  ].map((g) =>
                    g.ids.length ? (
                      <div key={g.title}>
                        <h4>{g.title}</h4>
                        <ul>
                          {g.ids.map((id) => {
                            const m = graph.nodes.find((n) => n.id === id);
                            if (!m) return null;
                            return (
                              <li key={id}>
                                <button type="button" className="mem-link" onClick={() => focusNode(id)}>{m.name}</button>
                              </li>
                            );
                          })}
                        </ul>
                      </div>
                    ) : null,
                  )}
                </div>
              )}
            </>
          )}
        </aside>
      </div>
    </div>
  );
}
