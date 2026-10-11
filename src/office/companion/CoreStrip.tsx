// #243: Core — the leader pane's 40px strip (design #236): the mascot, one status sentence, `? n`,
// shrink/hide, and a balloon carrying Dex's own decision cards (Proposals/Questions `Card`, same
// proposal_decide / question_answer paths — no new decision route).
import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { Minus, Maximize2, X } from "lucide-react";
import { onEvent, qid } from "../../api/hosts";
import { useTeams } from "../../team/store";
import { activeCount, useDelegations } from "../../delegations";
import { leaderPending, useQuestions } from "../../questions";
import { useProposals } from "../../team/Proposals";
import { animates, drawCore, readPalette } from "./coreEngine";
import {
  applyStep, coreSentence, DONE_MS, ERROR_MS, LOOKS, moved, nextWake, pickState, readStripMode, statusMap,
  STRIP_KEY, writeStripMode, type AgentStatus, type CoreInputs, type CoreLook, type Step, type StepEvent, type StripMode,
} from "./coreState";
import { setCoreShown, setDeferred, stripBalloonOpen, stripCovers, useOfficeLeaders } from "./coreShown";
import { CoreBalloon, deferDecisions, useDecisions } from "./CoreBalloon";
import "./core.css";

// ---- per-pane strip size (localStorage, shared by the strip and the pane's "show Core" button) ----

const modeListeners = new Set<() => void>();
const readRaw = () => {
  try {
    return localStorage.getItem(STRIP_KEY);
  } catch {
    return null;
  }
};
export function useStripMode(agentId: string): [StripMode, (m: StripMode) => void] {
  const raw = useSyncExternalStore((l) => (modeListeners.add(l), () => void modeListeners.delete(l)), readRaw);
  const set = (m: StripMode) => {
    try {
      localStorage.setItem(STRIP_KEY, writeStripMode(agentId, m, readRaw()));
    } catch {
      /* private mode: the choice lasts this view only */
    }
    for (const l of modeListeners) l();
  };
  return [readStripMode(agentId, raw), set];
}

// ---- reduced motion ----

const REDUCE = "(prefers-reduced-motion: reduce)";
export function useReducedMotion() {
  return useSyncExternalStore(
    (l) => {
      const m = matchMedia(REDUCE);
      m.addEventListener("change", l);
      return () => m.removeEventListener("change", l);
    },
    () => matchMedia(REDUCE).matches,
  );
}

/** The mascot. Loops only while visible and moving; a hidden tab / pane stops it (CPU 0). */
function CoreFace({ look, size }: { look: CoreLook; size: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const reduced = useReducedMotion();
  useEffect(() => {
    const canvas = ref.current!;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(size * dpr);
    canvas.height = Math.round(size * dpr);
    const pal = readPalette(canvas);
    const moving = animates(look, reduced);
    // Idle/sleep move slowly: a frame every 500 ms (Office's `schedule(… ? 0 : 500)`).
    const slow = look.id === "idle" || look.id === "sleep";
    let raf = 0, timer = 0, stopped = false, inView = true;
    const live = () => !stopped && inView && !document.hidden;
    const frame = (t: number) => {
      raf = 0;
      if (!live()) return; // resumed by visibility / intersection below
      drawCore(ctx, look, t, reduced, pal);
      if (!moving) return;
      if (slow) timer = window.setTimeout(() => { timer = 0; kick(); }, 500);
      else raf = requestAnimationFrame(frame);
    };
    const kick = () => {
      if (!raf && !timer && live()) raf = requestAnimationFrame(frame);
    };
    const io = new IntersectionObserver(([e]) => {
      inView = e.isIntersecting;
      kick();
    });
    io.observe(canvas);
    document.addEventListener("visibilitychange", kick);
    // First frame now (a hidden tab draws when it comes back).
    if (!document.hidden) drawCore(ctx, look, performance.now(), reduced, pal);
    kick();
    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
      clearTimeout(timer);
      io.disconnect();
      document.removeEventListener("visibilitychange", kick);
    };
  }, [look, size, reduced]);
  return <canvas ref={ref} className="core-face" style={{ width: size, height: size }} role="img" aria-label={`Core: ${look.label}`} />;
}

interface Props {
  agentId: string;
  status: AgentStatus;
  /** The pane's alert (agent/provider failure), shown as Core's `!`. */
  agentError: string | null;
  mode: StripMode;
  setMode: (m: StripMode) => void;
  /** Esc / defer: focus back to the terminal. */
  onDefer: () => void;
  /** The pane's view-toggle row (context meter): between the strip and the balloon (spec §4). */
  children: ReactNode;
}

export function CoreStrip({ agentId, status, agentError, mode, setMode, onDefer, children }: Props) {
  const delegations = useDelegations();
  const questions = useQuestions();
  const proposals = useProposals();

  // Patron decisions of this leader, oldest first (the balloon's queue); Esc deferral is per leader (#252).
  const { items, deferred } = useDecisions(agentId);

  // ---- agent.step (core #243 contract): the leader's running tools ----
  const [steps, setSteps] = useState<Step[]>([]);
  useEffect(() => {
    setSteps([]);
    return onEvent<StepEvent>("agent.step", (e, host) => {
      if (qid(host, e.payload.agentId) === agentId) setSteps((s) => applyStep(s, e.payload));
    });
  }, [agentId]);
  // A turn that ended took its tools with it (an `end` lost on a reconnect must not pin "araç").
  useEffect(() => {
    if (status !== "working" && status !== "waiting") setSteps([]);
  }, [status]);

  // ---- transitions: rejected proposal → `!` 8 s; delivered job → `✓` 5 s ----
  const [flash, setFlash] = useState({ errorUntil: 0, errorText: null as string | null, doneUntil: 0, doneText: null as string | null });
  const fail = (text: string) => setFlash((f) => ({ ...f, errorUntil: Date.now() + ERROR_MS, errorText: text }));
  const seenProposals = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    const mine = proposals.filter((p) => p.leader === agentId);
    if (seenProposals.current && moved(seenProposals.current, mine, ["pending"], ["rejected"]).length) fail("öneri reddedildi");
    seenProposals.current = statusMap(mine);
  }, [proposals, agentId]);
  const seenJobs = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    const mine = delegations.filter((d) => d.fromAgent === agentId);
    const done = seenJobs.current ? moved(seenJobs.current, mine, ["queued", "running"], ["done", "done_no_report"]) : [];
    if (done.length) setFlash((f) => ({ ...f, doneUntil: Date.now() + DONE_MS, doneText: `teslim aldı · ${done.map((d) => `#${d.id}`).join(", ")}` }));
    seenJobs.current = statusMap(mine);
  }, [delegations, agentId]);

  const delegated = activeCount(delegations, agentId, "from");
  const workerAsks = leaderPending(questions, agentId);
  const lastStep = steps.length ? steps[steps.length - 1].ts : 0;

  // ---- sleep counter: any input change is an event (timestamps, so a hidden tab keeps counting) ----
  const signature = [status, delegated, items.length, workerAsks, steps.length, lastStep, agentError, flash.errorUntil, flash.doneUntil].join("|");
  const [lastEventAt, setLastEventAt] = useState(() => Date.now());
  useEffect(() => setLastEventAt(Date.now()), [signature]);

  const [now, setNow] = useState(() => Date.now());
  const inputs: CoreInputs = {
    status, delegated, decisions: items.length, workerAsks, steps, agentError,
    errorUntil: flash.errorUntil, errorText: flash.errorText, doneUntil: flash.doneUntil, doneText: flash.doneText,
    lastEventAt, now: Math.max(now, lastEventAt),
  };
  const state = pickState(inputs);
  const wake = nextWake(inputs);
  useEffect(() => {
    if (wake === null) return;
    const t = window.setTimeout(() => setNow(Date.now()), Math.max(0, wake - Date.now()) + 20);
    return () => clearTimeout(t);
  }, [wake]);
  const look = LOOKS[state];
  const sentence = coreSentence(state, inputs);

  // ---- balloon: Esc defers until a new card arrives; #252: the office's big Core carries it while on screen ----
  const office = useOfficeLeaders();
  const open = stripBalloonOpen(mode, items.length, deferred, office.has(agentId));
  const defer = () => {
    deferDecisions(agentId, items);
    onDefer();
  };

  // #247: register while open and on screen. A background dock tab is display:none (the observer sees
  // it); another team's stage is visibility:hidden, which it does not — re-checked on every tab switch.
  const stripRef = useRef<HTMLDivElement>(null);
  const { activeId } = useTeams();
  useEffect(() => {
    const el = stripRef.current;
    const token = {};
    if (!el || mode !== "open") return;
    const io = new IntersectionObserver(([e]) => {
      const inView = e.isIntersecting && el.checkVisibility({ visibilityProperty: true });
      setCoreShown(token, stripCovers(mode, inView) ? agentId : null);
    });
    io.observe(el);
    return () => {
      io.disconnect();
      setCoreShown(token, null);
    };
  }, [mode, agentId, activeId]);

  const compact = mode === "compact";
  const label = `Core · ${sentence}`;
  const strip = (
    <div ref={stripRef} className={`core-strip${compact ? " compact" : ""}`} data-state={state} title={compact ? label : undefined}>
      <CoreFace look={look} size={compact ? 20 : 32} />
      <span className="core-say" role="status" aria-live="polite">
        {compact ? <span className="sr-only">{label}</span> : <><b>Core</b> · {sentence}</>}
      </span>
      {items.length > 0 && (
        <button className="core-ask" aria-expanded={!deferred} onClick={() => (deferred ? setDeferred(agentId, null) : defer())}
          title={deferred ? "Bekleyen kararları aç" : "Kararları ertele (Esc)"}>
          ? {items.length}
        </button>
      )}
      <button className="core-btn" onClick={() => setMode(compact ? "open" : "compact")} title={compact ? "Core'u büyüt" : "Core'u küçült"}
        aria-label={compact ? "Core'u büyüt" : "Core'u küçült"}>
        {compact ? <Maximize2 /> : <Minus />}
      </button>
      <button className="core-btn" onClick={() => setMode("hidden")} title="Core'u gizle" aria-label="Core'u gizle"><X /></button>
    </div>
  );
  return (
    <>
      {mode !== "hidden" && strip}
      {children}
      {open && <CoreBalloon agentId={agentId} items={items} onFail={fail} onDefer={onDefer} />}
    </>
  );
}
