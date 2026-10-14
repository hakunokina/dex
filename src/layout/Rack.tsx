// DEX-36 worker rack: every leader and the red workers (rackVisible) with status glow and marks. A click opens
// that one's panel (or brings it forward) and focuses it. #42: one row in the top bar (no dock panel,
// no height of its own); chips that don't fit scroll sideways (wheel, keyboard focus). #46: agents
// only — shells show as panes, and "Yeni terminal" is on their group's header (Workspace.tsx).
import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { getStatus, type AgentStatus } from "../agents";
import { STATUS_LABEL } from "../TerminalPane";
import { activeCount, lastResultLine, unreadResult, useDelegations } from "../delegations";
import { useQuestions, workerQuestion } from "../questions";
import { stallText, stalledFor, useStalled } from "../stall";
import { slowReply, useSlowReply } from "../firstReply";
import { useTeams } from "../team/store";
import { isAssignable } from "../team/rolePolicy";
import type { AgentRec } from "../team/api";
import { revealAgent } from "./panels";
import { rackOrder, rackVisible } from "./rackOrder";
import { reducedMotion } from "../motion";
import "./rack.css";

interface Props {
  teamId: string;
  focusedId: string | null;
  onFocus: (id: string) => void;
}

export function Rack({ teamId, focusedId, onFocus }: Props) {
  const members = useTeams().agents.filter(a => a.teamId === teamId);
  const leaders = members.filter(a => a.kind === "leader");
  const assistants = members.filter(a => a.roleSlug === "assistant");
  const workers = members.filter(a => a.kind === "worker" && isAssignable(a));
  const statuses = useStatuses(members.map(a => a.id));
  const delegations = useDelegations();
  const ordered = rackOrder(rackVisible(workers, statuses, id => activeCount(delegations, id, "to")), statuses);
  const questions = useQuestions();

  // DEX-19: a finished result the leader hasn't taken in yet ("done" green / "warn" amber).
  const unread = (id: string) => {
    const d = unreadResult(delegations, id);
    return !d ? "" : d.status === "done" ? "done" : "warn";
  };
  const title = (a: AgentRec) => {
    const st = STATUS_LABEL[statuses[a.id] ?? "offline"];
    const active = activeTarget(a);
    const route = active ? `\nAktif: ${active} · asıl: ${a.provider}` : "";
    if (a.kind === "leader") return `${a.name} — lider · ${st}${route}${stallLine(a.id)}`;
    const line = lastResultLine(delegations, a.id);
    const q = workerQuestion(questions, a.id);
    const ask = q ? `\n${q.status === "escalated" ? "Patrona çıktı" : "Lidere sordu"}: ${q.question}` : "";
    return `${a.name}${a.title ? ` — ${a.title}` : ""} · ${st}${route}${stallLine(a.id)}${line ? `\n${line}` : ""}${ask}`;
  };
  const pick = (id: string) => {
    onFocus(id);
    void revealAgent(teamId, id);
  };
  const chip = (a: AgentRec) => {
    const st = statuses[a.id] ?? "offline";
    return (
      <button key={a.id} data-agent={a.id} className={`rack-chip ${a.kind} ${st} ${a.id === focusedId ? "focused" : ""} ${unread(a.id)}`}
        onClick={() => pick(a.id)} title={title(a)}>
        {a.kind === "leader" && <span className="rack-badge">L</span>}
        <span className={`dot ${st}`} />
        <span className="rack-name">{a.name}</span>
        {activeTarget(a) && <span className="rack-provider">{activeTarget(a)}</span>}
        <AgentMarks id={a.id} />
      </button>
    );
  };

  const scroll = useRef<HTMLDivElement>(null);
  const [more, setMore] = useState({ left: false, right: false });
  useEffect(() => {
    const el = scroll.current;
    if (!el) return;
    const sync = () => setMore({ left: el.scrollLeft > 0, right: el.scrollLeft + el.clientWidth < el.scrollWidth - 1 });
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    for (const c of el.children) ro.observe(c);
    el.addEventListener("scroll", sync, { passive: true });
    return () => {
      ro.disconnect();
      el.removeEventListener("scroll", sync);
    };
  }, [members.length]);
  useSlide(scroll, ordered.map(a => a.id).join());

  return (
    <div className="rack" data-tauri-drag-region>
      <div ref={scroll} role="group" aria-label="Takım ajanları"
        className={`rack-scroll ${more.left ? "more-left" : ""} ${more.right ? "more-right" : ""}`}
        onWheel={e => {
          if (e.deltaY && !e.deltaX) e.currentTarget.scrollLeft += e.deltaY;
        }}>
        {leaders.map(chip)}
        {assistants.map(chip)}
        {leaders.length > 0 && ordered.length > 0 && <span className="rack-sep" />}
        {ordered.map(chip)}
      </div>
    </div>
  );
}

/** #285: FLIP — when the order changes, chips slide from their old spot (reduced motion: they jump).
 *  React moves a reordered chip's DOM node, which drops its focus; the focused chip gets it back. */
function useSlide(box: RefObject<HTMLDivElement | null>, order: string) {
  const at = useRef(new Map<string, number>());
  const last = useRef(order);
  const focused = useRef<string | null>(null);
  // Read before React reorders (render runs ahead of the commit).
  const active = typeof document === "undefined" ? null : document.activeElement;
  focused.current = active instanceof HTMLElement && box.current?.contains(active) ? active.dataset.agent ?? null : null;
  // Every commit: chips also shift when a neighbour's marks change, so the "from" spot stays current.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const chips = [...el.querySelectorAll<HTMLElement>(":scope > .rack-chip[data-agent]")];
    const id = focused.current;
    if (id && !el.contains(document.activeElement)) chips.find(c => c.dataset.agent === id)?.focus({ preventScroll: true });
    const prev = at.current;
    at.current = new Map(chips.map(c => [c.dataset.agent!, c.offsetLeft])); // scroll- and transform-free
    if (last.current === order) return;
    last.current = order;
    if (reducedMotion()) return;
    const css = getComputedStyle(el);
    const ms = parseFloat(css.getPropertyValue("--dur-pop")) || 180;
    const easing = css.getPropertyValue("--spring").trim() || "ease-out";
    for (const c of chips) {
      const from = prev.get(c.dataset.agent!);
      const dx = from === undefined ? 0 : from - c.offsetLeft;
      if (Math.abs(dx) > 0.5) c.animate([{ transform: `translateX(${dx}px)` }, { transform: "none" }], { duration: ms, easing });
    }
  });
}

function activeTarget(a: AgentRec) {
  if (a.activeProvider === "local") return "Yerel";
  if (a.activeProvider) return a.activeProvider.toUpperCase();
  return null;
}

/** `!` unreported finish · clock stalled · `?` question out · delegated-job count (rack chip + agent tab). */
export function AgentMarks({ id }: { id: string }) {
  const delegations = useDelegations();
  const questions = useQuestions();
  useStalled();
  useSlowReply();
  const d = unreadResult(delegations, id);
  const n = activeCount(delegations, id, "to");
  return (
    <span className="marks">
      {d?.status === "done" && <span className="mark-done" title="Okunmamış sonuç">✓</span>}
      {d && d.status !== "done" && <span className="mark-warn" title="Uyarılı sonuç">!</span>}
      {stalledFor(id) !== null && <StallClock />}
      {slowReply(id) !== null && <SlowGlass title={slowReply(id)!} />}
      {workerQuestion(questions, id) && <span className="mark-ask">?</span>}
      {n > 0 && <span className="mark-deleg" title="Devredilen iş (süren + kuyrukta)">{n}</span>}
    </span>
  );
}

function stallLine(id: string) {
  const min = stalledFor(id);
  const slow = slowReply(id);
  return (slow ? `\n⏳ ${slow}` : "") + (min === null ? "" : `\n⚠ ${stallText(min)}`);
}

/** DEX-28: amber clock (SVG: an emoji would ignore the color). */
function StallClock() {
  return (
    <svg className="mark-stall" viewBox="0 0 12 12" width="11" height="11" aria-label="takılmış olabilir">
      <circle cx="6" cy="6" r="5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M6 3v3l2 1.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

/** FIX-3: amber hourglass — the first reply is late (not DEX-28's clock: that is silence mid-turn). */
function SlowGlass({ title }: { title: string }) {
  return (
    <svg className="mark-stall" viewBox="0 0 12 12" width="11" height="11" aria-label="yanıt gecikiyor">
      <title>{title}</title>
      <path d="M3 1.5h6M3 10.5h6M3.5 1.5c0 3 5 3 5 4.5s-5 1.5-5 4.5M8.5 1.5c0 3-5 3-5 4.5s5 1.5 5 4.5" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

/** One agent's status, polled like the rest of the UI (hooks feed `getStatus`, not React). */
export function useAgentStatus(id: string): AgentStatus {
  return useStatuses([id])[id] ?? "offline";
}

function useStatuses(ids: string[]): Record<string, AgentStatus> {
  const key = ids.join();
  const read = () => Object.fromEntries(ids.map(id => [id, getStatus(id)]));
  const [s, setS] = useState(read);
  useEffect(() => {
    let last = "";
    const tick = () => {
      const next = read();
      const sig = JSON.stringify(next);
      if (sig !== last) {
        last = sig;
        setS(next);
      }
    };
    tick();
    const t = setInterval(tick, 400);
    return () => clearInterval(t);
  }, [key]);
  return s;
}
