// #243: Core — the leader's mascot (design #236, .dex/design/core/spec.md). Pure state logic: which of
// the 8 looks wins, the strip's sentence, `agent.step` bookkeeping and the balloon's keys. No DOM, no
// host calls — CoreStrip.tsx feeds it and node:test covers it.

export type CoreStateId = "idle" | "thinking" | "tool" | "delegated" | "ask" | "done" | "error" | "sleep";
export type Eye = "dot" | "wide" | "down" | "scan" | "happy" | "line" | "x";
export type Badge = "?" | "!" | "✓" | "z" | "dots" | "orbit" | null;
export type Motion = "breathe" | "scan" | "tremble" | "bounce" | "hop" | "jump" | "shake" | "still";

export interface CoreLook {
  id: CoreStateId;
  label: string;
  /** CSS custom property carrying the ring/LED colour (spec §1: no new colours; Modern §7: work = --team, error = red). */
  token: string;
  glow: number;
  eye: Eye;
  badge: Badge;
  motion: Motion;
}

export const LOOKS: Record<CoreStateId, CoreLook> = {
  idle: { id: "idle", label: "boşta", token: "--muted", glow: 0, eye: "dot", badge: null, motion: "breathe" },
  thinking: { id: "thinking", label: "düşünüyor", token: "--pcb-provider", glow: 8, eye: "scan", badge: null, motion: "scan" },
  tool: { id: "tool", label: "araç çalıştırıyor", token: "--team", glow: 12, eye: "down", badge: "dots", motion: "tremble" },
  delegated: { id: "delegated", label: "worker'lara devretti", token: "--amber", glow: 8, eye: "dot", badge: "orbit", motion: "bounce" },
  ask: { id: "ask", label: "soruyor", token: "--amber", glow: 12, eye: "wide", badge: "?", motion: "hop" },
  done: { id: "done", label: "teslim aldı", token: "--green", glow: 10, eye: "happy", badge: "✓", motion: "jump" },
  error: { id: "error", label: "hata", token: "--red-hi", glow: 12, eye: "x", badge: "!", motion: "shake" },
  sleep: { id: "sleep", label: "uyuyor", token: "--faint", glow: 0, eye: "line", badge: "z", motion: "still" },
};

/** Patron: sleep after 10 min without an event — counted on while the tab is hidden. */
export const SLEEP_MS = 10 * 60_000;
/** Patron: a rejected proposal (or failed answer) shows `!` this long, then ask/idle again. */
export const ERROR_MS = 8_000;
/** Spec §2: a delivered job's `✓` stays 5 s. */
export const DONE_MS = 5_000;

export type AgentStatus = "offline" | "idle" | "working" | "waiting";

/** One running tool of the leader (`agent.step` start without its end yet). */
export interface Step {
  tool: string;
  label: string;
  ts: number;
}

/** Core contract (#243): `agent.step` payload. */
export interface StepEvent {
  agentId: string;
  phase: "start" | "end";
  tool: string;
  label: string;
  ok?: boolean;
  ts: number;
}

/** Claude runs tools in parallel: a start adds, an end removes the oldest of that tool (else the oldest). */
export function applyStep(steps: Step[], e: StepEvent): Step[] {
  if (e.phase === "start") return [...steps, { tool: e.tool, label: e.label, ts: e.ts }];
  if (!steps.length) return steps;
  const i = steps.findIndex((s) => s.tool === e.tool);
  return steps.filter((_, j) => j !== (i >= 0 ? i : 0));
}

export interface CoreInputs {
  status: AgentStatus;
  /** Active delegations sent by this leader. */
  delegated: number;
  /** Patron decisions in the balloon (pending proposals + escalated questions of this leader). */
  decisions: number;
  /** Worker questions waiting on this leader (the tab's `? n`). */
  workerAsks: number;
  steps: Step[];
  /** Agent/provider failure text (pane alert), if any. */
  agentError: string | null;
  /** Transient `!` (rejected proposal / failed answer) until this instant. */
  errorUntil: number;
  errorText: string | null;
  /** Transient `✓` until this instant. */
  doneUntil: number;
  doneText: string | null;
  /** Last time any input changed (sleep counter). */
  lastEventAt: number;
  now: number;
}

/** Spec §2 priority: error > ask > tool > delegated > thinking > done > sleep > idle. */
export function pickState(i: CoreInputs): CoreStateId {
  if (i.agentError || i.now < i.errorUntil) return "error";
  if (i.decisions > 0 || i.workerAsks > 0 || i.status === "waiting") return "ask";
  const busy = i.status === "working";
  // No `agent.step` ever arriving → a working leader is only "thinking" (never a false claim).
  if (busy && i.steps.length) return "tool";
  if (i.delegated > 0) return "delegated";
  if (busy) return "thinking";
  if (i.now < i.doneUntil) return "done";
  if (i.status === "offline" || i.now - i.lastEventAt >= SLEEP_MS) return "sleep";
  return "idle";
}

/** The next instant `pickState` may change on its own (a timer re-renders the strip then). */
export function nextWake(i: CoreInputs): number | null {
  const at = [i.errorUntil, i.doneUntil, i.lastEventAt + SLEEP_MS].filter((t) => t > i.now);
  return at.length ? Math.min(...at) : null;
}

/** The strip's status sentence (after "Core ·"). */
export function coreSentence(state: CoreStateId, i: CoreInputs): string {
  switch (state) {
    case "error":
      return i.agentError ?? i.errorText ?? "hata";
    case "ask": {
      const parts: string[] = [];
      if (i.decisions) parts.push(`${i.decisions} karar seni bekliyor`);
      if (i.workerAsks) parts.push(`${i.workerAsks} worker sorusu cevap bekliyor`);
      if (i.status === "waiting") parts.push("terminalde izin/girdi bekliyor");
      return parts.join(" — ");
    }
    case "tool":
      return i.steps[i.steps.length - 1].label || "araç çalıştırıyor";
    case "delegated":
      return `${i.delegated} worker çalışıyor`;
    case "thinking":
      return "düşünüyor…";
    case "done":
      return i.doneText ?? "teslim aldı";
    case "sleep":
      return i.status === "offline" ? "oturum kapalı" : "uyuyor";
    default:
      return "boşta";
  }
}

// ---- transitions read off the row lists ----

/** Keys of rows whose status moved `from` → one of `to` since `prev` (prev = key → last status seen). */
export function moved<R extends { key: string; status: string }>(prev: Map<string, string>, rows: R[], from: string[], to: string[]): R[] {
  return rows.filter((r) => {
    const was = prev.get(r.key);
    return was !== undefined && from.includes(was) && to.includes(r.status);
  });
}

export const statusMap = (rows: { key: string; status: string }[]) => new Map(rows.map((r) => [r.key, r.status]));

// ---- balloon keys (patron: E/R, 1–9 + ↵, Esc; only while focus is inside the balloon) ----

export interface KeyIn {
  key: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
  /** Focus is in a text field (the card's name/answer input). */
  inText: boolean;
  /** Focus is on a button (its own Enter/Space clicks it natively). */
  onButton: boolean;
}

export type CardKind = "proposal" | "question";

/** `{click}`: the card button with that `data-key`; `defer`: Esc. `null`: the key is not ours. */
export function balloonKey(e: KeyIn, card: { kind: CardKind; options: number }): { click: string } | "defer" | null {
  if (e.ctrlKey || e.altKey || e.metaKey) return null;
  if (e.key === "Escape") return "defer";
  if (e.inText) return null; // typing: the field's own Enter answers, letters are letters
  const k = e.key.toLowerCase();
  if (card.kind === "proposal") {
    if (k === "e" || k === "r") return { click: k };
    return null;
  }
  if (/^[1-9]$/.test(k) && Number(k) <= card.options) return { click: k };
  if (e.key === "Enter" && !e.onButton) return { click: "Enter" };
  return null;
}

// ---- strip size, per pane (localStorage `dex.core.strip`) ----

export type StripMode = "open" | "compact" | "hidden";
export const STRIP_KEY = "dex.core.strip";

export function readStripMode(agentId: string, raw: string | null): StripMode {
  try {
    const m = raw ? (JSON.parse(raw) as Record<string, unknown>)[agentId] : null;
    return m === "compact" || m === "hidden" ? m : "open";
  } catch {
    return "open";
  }
}

export function writeStripMode(agentId: string, mode: StripMode, raw: string | null): string {
  let all: Record<string, unknown> = {};
  try {
    const v = raw ? JSON.parse(raw) : null;
    if (v && typeof v === "object") all = v;
  } catch {
    /* a broken value is replaced */
  }
  if (mode === "open") delete all[agentId];
  else all[agentId] = mode;
  return JSON.stringify(all);
}
