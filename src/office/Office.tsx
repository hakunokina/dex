import { useEffect, useRef } from "react";
import { HoverCard } from "./hovercard";
import { World } from "./world";
import { advance, worldFor } from "./worlds";
import { readOfficeTheme } from "./theme";
import { readPalette } from "./companion/coreEngine";
import { LitLayer } from "./litLayer";
import { cardMaps } from "./assets";
import { glScale } from "./symbioteCard";
import { CoreBalloon, useDecisions } from "./companion/CoreBalloon";
import { setDeferred, setOfficeShown, useOfficeLeaders } from "./companion/coreShown";
import { useReducedMotion } from "./companion/CoreStrip";
import { getTeam, membersOf, useTeams } from "../team/store";
import { agentPanelId, boundApi, openPanel } from "../layout/panels";
import { getStatus, type Agent } from "../agents";
import { playCircuit } from "./circuitFx";
import { EmptyState } from "../EmptyState";
import "./office.css";

interface Props {
  teamId: string;
  focusedId: string | null;
  onSelect: (id: string) => void;
  /** A mobile agent screen can cover the still-mounted board. */
  active?: boolean;
}

export function Office({ teamId, focusedId, onSelect, active = true }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const glRef = useRef<HTMLCanvasElement>(null);
  const focusedRef = useRef(focusedId);
  focusedRef.current = focusedId;
  const view = useRef({ scale: 1, ox: 0, oy: 0, dpr: 1 });
  const worldRef = useRef<World | null>(null);
  const hoverIdRef = useRef<string | null>(null);
  const keyboardId = useRef<string | null>(null);
  const requestFrameRef = useRef(() => {});
  const activeRef = useRef(active);
  activeRef.current = active;
  useEffect(() => { requestFrameRef.current(); }, [active]);
  // #252: decision balloons above the big Cores, placed by the frame loop (leader id → element).
  const balloons = useRef(new Map<string, HTMLDivElement>());
  const reduced = useReducedMotion();
  useEffect(() => {
    worldFor(teamId).reduced = reduced;
    requestFrameRef.current();
  }, [teamId, reduced]);

  useEffect(() => {
    const canvas = canvasRef.current!;
    const glCanvas = glRef.current!;
    const ctx = canvas.getContext("2d")!;
    const world = worldFor(teamId);
    worldRef.current = world;
    const card = new HoverCard(canvas.parentElement!);
    const theme = readOfficeTheme(canvas);
    const corePalette = readPalette(canvas);
    // #1958: the lit membrane under the ink, made once the maps are decoded and the board is first shown.
    // No WebGL2 (or a lost context): the world draws its flat fallback into the ink canvas instead.
    let lit: LitLayer | null = null;
    let litFailed = false;
    const litLayer = () => {
      if (!lit && !litFailed) {
        const maps = cardMaps();
        if (maps) {
          try { lit = new LitLayer(glCanvas, maps); } catch { litFailed = true; }
        }
      }
      return lit?.ready ? lit : null;
    };
    // #252: this office's big Cores on screen carry their leaders' balloons (the strip's stays shut).
    const shownToken = {};
    let cssW = 0;
    let cssH = 0;
    let inView = false;
    let stopped = false;
    let raf = 0;
    let timer = 0;

    // #247/#252: another team's stage is visibility:hidden, which the observer counts as visible.
    const drawable = () => activeRef.current && inView && !document.hidden && cssW > 0 && cssH > 0 && canvas.checkVisibility({ visibilityProperty: true });

    // #270/#277: the board fills the panel — the world picks how many CSS px one unit is (`world.unit`,
    // with the team: chips, Core and fonts grow with the panel). Drawn right away: no cleared canvas reaches paint.
    const resize = () => {
      const dpr = window.devicePixelRatio || 1;
      const { width, height } = canvas.getBoundingClientRect();
      cssW = width;
      cssH = height;
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      const k = glScale(dpr, width, height);
      glCanvas.width = Math.max(1, Math.round(width * k));
      glCanvas.height = Math.max(1, Math.round(height * k));
      if (width <= 0 || height <= 0) return schedule();
      world.resize(width, height);
      view.current = { scale: world.unit * dpr, dpr, ox: 0, oy: 0 };
      if (raf) cancelAnimationFrame(raf);
      if (timer) clearTimeout(timer);
      raf = timer = 0;
      frame(performance.now());
    };

    const frame = (now: number) => {
      raf = 0;
      if (stopped || !drawable()) {
        card.hide();
        setOfficeShown(shownToken, null);
        return schedule(800);
      }
      advance(teamId, now);
      world.teamName = getTeam(teamId)?.name ?? "";
      // #277: a team / ownership change refits the unit.
      view.current.scale = world.unit * view.current.dpr;
      const { scale, ox, oy, dpr } = view.current;
      const layer = litLayer();
      glCanvas.style.visibility = layer ? "" : "hidden";
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.setTransform(scale, 0, 0, scale, ox, oy);
      const anchors = world.draw(ctx, keyboardId.current ?? focusedRef.current, theme, corePalette, !layer);
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      layer?.draw(world.scene, cssW, cssH, world.unit);
      setOfficeShown(shownToken, anchors.filter((anchor) => anchor.body).map((anchor) => anchor.agent.id));
      for (const anchor of anchors) {
        const el = anchor.body && balloons.current.get(anchor.agent.id);
        if (!el || !anchor.body) continue;
        // Spec §5.5: right above the body (and its provider label), tail on Core's centre.
        const x = (ox + anchor.body.x * scale) / dpr;
        const top = (oy + (anchor.body.y - anchor.body.s / 2 - 10) * scale) / dpr;
        const w = el.offsetWidth;
        const left = Math.max(8, Math.min(cssW - w - 8, x - w / 2));
        el.style.left = `${left}px`;
        el.style.bottom = `${cssH - top + 8}px`;
        el.style.setProperty("--tail-x", `${Math.max(12, Math.min(w - 12, x - left))}px`);
        el.style.setProperty("--max-h", `${Math.max(80, Math.min(360, top - 16))}px`);
        el.dataset.placed = "";
      }

      const hovered = hoverIdRef.current
        ? anchors.find((anchor) => anchor.agent.id === hoverIdRef.current)
        : undefined;
      if (hovered) {
        card.update(
          hovered.agent,
          (ox + hovered.x * scale) / dpr,
          (oy + (hovered.y - hovered.top) * scale) / dpr,
          cssW,
          cssH,
        );
      } else card.hide();
      schedule(world.needsAnimation() ? 0 : 500);
    };

    function schedule(delay = 0) {
      if (stopped) return;
      if (raf) cancelAnimationFrame(raf);
      if (timer) clearTimeout(timer);
      raf = timer = 0;
      if (!activeRef.current || document.hidden || !inView) {
        card.hide();
        setOfficeShown(shownToken, null);
        return;
      }
      if (delay) timer = window.setTimeout(() => {
        timer = 0;
        raf = requestAnimationFrame(frame);
      }, delay);
      else raf = requestAnimationFrame(frame);
    }
    requestFrameRef.current = schedule;

    const ro = new ResizeObserver(resize);
    ro.observe(canvas);
    const io = new IntersectionObserver(([entry]) => {
      inView = entry.isIntersecting;
      if (!inView) card.hide();
      schedule();
    });
    io.observe(canvas);
    const onVisibility = () => schedule();
    document.addEventListener("visibilitychange", onVisibility);
    // #252 spec §4: the cursor left the window → the eyes start home at once (no 1.5 s hold).
    const onWindowOut = (e: MouseEvent) => {
      if (!e.relatedTarget) world.pointerOut(true);
    };
    document.addEventListener("mouseout", onWindowOut);
    resize();

    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
      clearTimeout(timer);
      ro.disconnect();
      io.disconnect();
      document.removeEventListener("visibilitychange", onVisibility);
      document.removeEventListener("mouseout", onWindowOut);
      setOfficeShown(shownToken, null);
      card.destroy();
      lit?.destroy();
      requestFrameRef.current = () => {};
    };
  }, [teamId]);

  function onClick(e: React.MouseEvent<HTMLCanvasElement>) {
    const agent = hitTest(e);
    if (!agent) return;
    selectAgent(agent);
  }

  function selectAgent(agent: Agent) {
    if (agent.kind === "leader") setDeferred(agent.id, null);
    worldRef.current?.activate(agent.id);
    // #411: an awake worker's pane opens (App) — a red trace runs from its chip to where it landed.
    const fx = agent.kind === "worker" && getStatus(agent.id) !== "offline" && !reduced;
    const before = fx ? paneOf(agent.id) : undefined;
    const wasShown = before?.api.isVisible ?? false;
    const from = fx ? chipBox(agent.id) : null;
    onSelect(agent.id);
    requestFrameRef.current();
    if (!from) return;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const pane = paneOf(agent.id);
      if (!pane) return;
      // It was in another tab (or is still hidden): to its tab header; on screen already: a short flash on it.
      const tab = pane.view.tab.element;
      const target = (before && !wasShown) || !pane.api.isVisible ? (tab.isConnected ? tab : null) : pane.group.element;
      const box = target?.getBoundingClientRect();
      if (box?.width && box.height) playCircuit(from, box, wasShown);
    }));
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLCanvasElement>) {
    const chips = worldRef.current?.chips ?? [];
    if (!chips.length) return;
    const index = Math.max(0, chips.findIndex(c => c.agent.id === keyboardId.current));
    if (["ArrowRight", "ArrowDown", "ArrowLeft", "ArrowUp", "Home", "End"].includes(e.key)) {
      e.preventDefault();
      const next = e.key === "Home" ? 0 : e.key === "End" ? chips.length - 1 :
        (index + (e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 1) + chips.length) % chips.length;
      keyboardId.current = chips[next].agent.id;
      e.currentTarget.setAttribute("aria-label", `${chips[next].agent.name}. Ok tuşlarıyla ajan seç, Enter ile aç`);
      requestFrameRef.current();
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      selectAgent(chips[index].agent);
    }
  }

  const paneOf = (agentId: string) => boundApi(teamId)?.getPanel(agentPanelId(agentId));

  /** The chip's box in viewport px (the frame's view transform). */
  function chipBox(agentId: string) {
    const chip = worldRef.current?.chips.find((item) => item.agent.id === agentId);
    const canvas = canvasRef.current;
    if (!chip || !canvas) return null;
    const rect = canvas.getBoundingClientRect();
    const { scale, ox, oy, dpr } = view.current;
    return {
      left: rect.left + (ox + (chip.x - chip.w / 2) * scale) / dpr,
      top: rect.top + (oy + (chip.y - chip.h / 2) * scale) / dpr,
      width: (chip.w * scale) / dpr,
      height: (chip.h * scale) / dpr,
    };
  }

  function toWorld(e: React.MouseEvent<HTMLCanvasElement>) {
    const rect = e.currentTarget.getBoundingClientRect();
    const { scale, ox, oy, dpr } = view.current;
    return { x: ((e.clientX - rect.left) * dpr - ox) / scale, y: ((e.clientY - rect.top) * dpr - oy) / scale };
  }

  function hitTest(e: React.MouseEvent<HTMLCanvasElement>) {
    const { x, y } = toWorld(e);
    return worldRef.current?.hit(x, y) ?? null;
  }

  function onPointerMove(e: React.PointerEvent<HTMLCanvasElement>) {
    if (e.pointerType === "touch") return;
    const agent = hitTest(e);
    const { x, y } = toWorld(e);
    worldRef.current?.pointAt(x, y);
    hoverIdRef.current = agent?.id ?? null;
    if (worldRef.current) worldRef.current.hoverId = hoverIdRef.current;
    e.currentTarget.style.cursor = agent ? "pointer" : "default";
    requestFrameRef.current();
  }

  function onMouseLeave() {
    hoverIdRef.current = null;
    if (worldRef.current) {
      worldRef.current.hoverId = null;
      worldRef.current.pointerOut(false);
    }
    requestFrameRef.current();
  }

  const place = (leaderId: string) => (el: HTMLDivElement | null) => {
    if (el) {
      balloons.current.set(leaderId, el);
      requestFrameRef.current();
    } else balloons.current.delete(leaderId);
  };

  return (
    <div className="office-box">
      <canvas ref={glRef} className="office-lit" aria-hidden="true" />
      <canvas
        ref={canvasRef}
        className="office"
        onClick={onClick}
        onPointerMove={onPointerMove}
        onPointerLeave={onMouseLeave}
        onPointerDown={e => { if (e.pointerType === "touch") onMouseLeave(); }}
        tabIndex={0}
        role="button"
        onKeyDown={onKeyDown}
        onFocus={e => {
          const chip = worldRef.current?.chips.find(c => c.agent.id === focusedRef.current) ?? worldRef.current?.chips[0];
          keyboardId.current = chip?.agent.id ?? null;
          e.currentTarget.setAttribute("aria-label", `${chip?.agent.name ?? "Ajan kartı"}. Ok tuşlarıyla ajan seç, Enter ile aç`);
          requestFrameRef.current();
        }}
        onBlur={() => { keyboardId.current = null; requestFrameRef.current(); }}
        aria-label="Ajan kartı. Ok tuşlarıyla ajan seç, Enter ile aç"
      />
      <Balloons teamId={teamId} place={place} onFail={(id) => worldRef.current?.coreFail(id)} />
      <EmptyTeam teamId={teamId} />
    </div>
  );
}

/** Modern §6: a team without workers says so (Core sleeps, nothing is broken). Clicks pass through to the board but the button. */
function EmptyTeam({ teamId }: { teamId: string }) {
  useTeams();
  if (membersOf(teamId).some((agent) => agent.kind === "worker")) return null;
  return (
    <EmptyState
      mood="team"
      title="Bu takımda ajan yok"
      className="office-empty"
      actions={<button onClick={() => void openPanel(teamId, "team")}>Worker ekle</button>}
    >
      Uyuyor, bozuk değil. Bir worker ekle, Core çevresinde canlansınlar.
    </EmptyState>
  );
}

/** #252: one balloon per leader whose big Core is on screen (patron: right above Core). */
function Balloons({ teamId, place, onFail }: {
  teamId: string;
  place: (leaderId: string) => (el: HTMLDivElement | null) => void;
  onFail: (leaderId: string) => void;
}) {
  useTeams();
  const shown = useOfficeLeaders();
  const leaders = membersOf(teamId).filter((agent) => agent.kind === "leader" && shown.has(agent.id));
  return leaders.map((agent) => <OfficeBalloon key={agent.id} leaderId={agent.id} place={place(agent.id)} onFail={() => onFail(agent.id)} />);
}

function OfficeBalloon({ leaderId, place, onFail }: { leaderId: string; place: (el: HTMLDivElement | null) => void; onFail: () => void }) {
  const { items, deferred } = useDecisions(leaderId);
  if (!items.length) return null;
  if (deferred) return (
    <div className="office-deferred" ref={place}>
      <button className="core-ask" aria-label={`${items.length} bekleyen kararı aç`} aria-expanded={false}
        title="Bekleyen kararları aç" onClick={() => setDeferred(leaderId, null)}>
        ? {items.length}
      </button>
    </div>
  );
  return <CoreBalloon agentId={leaderId} items={items} onFail={onFail} onDefer={() => {}} className="core-balloon-office" ref={place} />;
}
