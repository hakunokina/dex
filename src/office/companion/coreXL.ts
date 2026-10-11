// #252: Core XL — the office's big Core (design #251, .dex/design/core-xl/spec.md). Pure logic: the
// leader's state (the strip's own pickState), done/error flashes, the gaze at a worker on a delegation /
// delivery, pupil tracking, blink, and the body's square inside the leader's layout target.
// world.ts feeds it once per step; node:test covers it. Drawing is coreEngine.ts `drawCoreXL`.
import { XL_BODY, XL_EYE } from "./coreEngine.ts";
import { DONE_MS, ERROR_MS, pickState, type AgentStatus, type CoreStateId, type Step } from "./coreState.ts";
import { FragmentMotion, bodyWeight } from "../symbiote.ts";

export type Vec = { x: number; y: number };
/** Centre-based layout target (world units). */
export type Box = { x: number; y: number; w: number; h: number };
export type Body = { x: number; y: number; s: number };

// ---- geometry (spec §2, §5) ----

/** Pin length outside the body (spec §2: 8×6). */
export const XL_PIN = 6;
/** Name + role lines under the body (spec §5.4). */
export const XL_LABEL = 28;
/** Below this side the leader stays the #229 chip (dense multi-leader boards). */
export const XL_MIN = 40;
/** The leader target's height for an XL body: pins + 120 + pins + labels (spec §5.1, patron: 120). */
export const XL_TARGET_H = XL_PIN + XL_BODY + XL_PIN + XL_LABEL;

/** The body's square inside the leader's target: ≤ 120, pins inside the width, labels under it. */
export function coreBody(b: Box): Body | null {
  const s = Math.min(XL_BODY, b.w - 2 * XL_PIN, b.h - 2 * XL_PIN - XL_LABEL);
  if (s < XL_MIN) return null;
  return { x: b.x, y: b.y - b.h / 2 + XL_PIN + s / 2, s };
}

/** Trace start on the body's side edge (spec §2: `(±s/2, spreadY)`), spread around the body's centre. */
export function corePort(body: Body, dir: -1 | 1, index: number, count: number): Vec {
  const spread = Math.min(Math.max(0, body.s - 18), Math.max(0, (count - 1) * 9));
  return { x: body.x + (dir * body.s) / 2, y: body.y - spread / 2 + index * (count > 1 ? spread / (count - 1) : 0) };
}

/** Single eye centre in world units. */
export function eyeAt(body: Body): Vec {
  const k = body.s / XL_BODY;
  return { x: body.x + XL_EYE.x * k, y: body.y + XL_EYE.y * k };
}

// ---- gaze (spec §4) ----

export const PUPIL_MAX = 4;
/** Distance (body units) where the pupil reaches its full 4 — closer, it eases to the centre. */
const REACH = 48;
export const FOLLOW_RATE = 10;
export const RETURN_RATE = 1.2;
/** Off the canvas, the last direction holds this long before the slow return. */
export const HOLD_MS = 1500;
/** Delegation / delivery: the eyes stay on the worker for the packet's run. */
export const GAZE_MS = 1200;

/**
 * Pupil offset (body units) from an eye towards a point: the direction, |v| ≤ 4. Deviation from the
 * spec's per-axis clamp: a clamp per axis looks diagonal for a cursor that is nearly level (4, 4).
 */
export function pupilToward(eye: Vec, target: Vec, scale = 1): Vec {
  const dx = (target.x - eye.x) / scale, dy = (target.y - eye.y) / scale;
  const d = Math.hypot(dx, dy);
  if (d < 1e-6) return { x: 0, y: 0 };
  const m = (PUPIL_MAX * Math.min(1, d / REACH)) / d;
  return { x: dx * m, y: dy * m };
}

/** `cur += (to − cur) × (1 − e^(−dt·rate))`; an infinite rate snaps (reduced motion). */
export const approach = (cur: number, to: number, dt: number, rate: number) =>
  rate === Infinity ? to : cur + (to - cur) * (1 - Math.exp(-dt * rate));

export interface Pointer {
  /** World units. */
  x: number;
  y: number;
  /** Over the office canvas. */
  inside: boolean;
  /** Still inside the window (`mouseout` to nowhere = left it). */
  inWindow: boolean;
  /** Animation clock (ms) of the last move / leave. */
  at: number;
}

/** States whose eyes have a pupil (LED dot / wide / scan / down). */
export const hasPupil = (id: CoreStateId) => id === "idle" || id === "thinking" || id === "tool" || id === "delegated" || id === "ask";

/** Where one pupil heads and how fast: worker gaze > thinking scan > cursor (held 1.5 s) > centre. */
export function gazeGoal(o: {
  id: CoreStateId; eye: Vec; scale: number; pointer: Pointer | null; look: Vec | null; now: number; reduced: boolean;
}): { to: Vec; rate: number } {
  const rate = o.reduced ? Infinity : FOLLOW_RATE;
  if (o.id === "tool") return { to: { x: 0, y: 3 }, rate }; // "down": tracking off
  if (!hasPupil(o.id)) return { to: { x: 0, y: 0 }, rate };
  if (o.look) return { to: pupilToward(o.eye, o.look, o.scale), rate };
  if (o.id === "thinking" && !o.reduced) return { to: { x: 3 * Math.sin((o.now / 600) * Math.PI), y: 0 }, rate };
  const p = o.pointer;
  if (p && (p.inside || (p.inWindow && o.now - p.at < HOLD_MS))) return { to: pupilToward(o.eye, p, o.scale), rate };
  return { to: { x: 0, y: 0 }, rate: o.reduced ? Infinity : RETURN_RATE };
}

// ---- blink (spec §4: 120 ms, every 2.8–5.2 s; none while asking, asleep or reduced) ----

export const BLINK_MS = 120;
export const nextBlink = (now: number, rand: number) => now + 2800 + rand * 2400;
export const blinks = (id: CoreStateId, reduced: boolean) => !reduced && hasPupil(id) && id !== "ask";
export function lidAt(now: number, start: number) {
  if (start < 0) return 0;
  const k = (now - start) / BLINK_MS;
  return k < 0 || k >= 1 ? 0 : Math.sin(k * Math.PI);
}

// ---- state (spec §3; same inputs and priority as the strip, #243) ----

const ACTIVE = new Set(["queued", "running"]);
const DELIVERED = new Set(["done", "done_no_report"]);

export interface CoreSnapshot {
  status: AgentStatus;
  /** Delegations this leader sent. */
  jobs: readonly { id: number; toAgent: string; status: string }[];
  /** This leader's member proposals (pending → rejected flashes `!`). */
  proposals: readonly { key: string; status: string }[];
  /** Escalated questions of this leader (patron decisions besides pending proposals). */
  escalated: number;
  workerAsks: number;
  steps: readonly Step[];
  agentError: string | null;
}

/** Per-leader bookkeeping the strip keeps in React state: flashes, sleep counter, the worker gaze. */
export class CoreTracker {
  state: CoreStateId = "idle";
  /** The state before the last change (its colour fades out). */
  prev: CoreStateId | null = null;
  /** Animation clock (ms) of the last state change. */
  since = -Infinity;
  /** The worker the eyes are on, until (animation ms). */
  look: { worker: string; until: number } | null = null;
  private jobs: Map<number, string> | null = null;
  private proposals: Map<string, string> | null = null;
  private errorUntil = 0;
  private doneUntil = 0;
  private lastEventAt = 0;
  private signature: string | null = null;

  /** `now` = wall clock (flashes, sleep — they count on while the office is hidden); `anim` = animation ms. */
  observe(s: CoreSnapshot, now: number, anim: number): CoreStateId {
    const seenJobs = this.jobs;
    for (const job of s.jobs) {
      const was = seenJobs?.get(job.id);
      if (!seenJobs) continue; // first look: what is already there is history
      if (was === undefined && ACTIVE.has(job.status)) this.look = { worker: job.toAgent, until: anim + GAZE_MS };
      else if (was !== undefined && ACTIVE.has(was) && DELIVERED.has(job.status)) {
        this.doneUntil = now + DONE_MS;
        this.look = { worker: job.toAgent, until: anim + GAZE_MS };
      }
    }
    this.jobs = new Map(s.jobs.map((j) => [j.id, j.status]));
    if (this.proposals && s.proposals.some((p) => this.proposals!.get(p.key) === "pending" && p.status === "rejected")) this.fail(now);
    this.proposals = new Map(s.proposals.map((p) => [p.key, p.status]));
    if (this.look && anim >= this.look.until) this.look = null;

    const delegated = s.jobs.filter((j) => ACTIVE.has(j.status)).length;
    const decisions = s.proposals.filter((p) => p.status === "pending").length + s.escalated;
    const busy = s.status === "working" || s.status === "waiting";
    const steps = busy ? s.steps : [];
    const lastStep = steps.length ? steps[steps.length - 1].ts : 0;
    const signature = [s.status, delegated, decisions, s.workerAsks, steps.length, lastStep, s.agentError, this.errorUntil, this.doneUntil].join("|");
    if (signature !== this.signature) {
      this.signature = signature;
      this.lastEventAt = now;
    }
    const next = pickState({
      status: s.status, delegated, decisions, workerAsks: s.workerAsks, steps: [...steps], agentError: s.agentError,
      errorUntil: this.errorUntil, errorText: null, doneUntil: this.doneUntil, doneText: null,
      lastEventAt: this.lastEventAt, now: Math.max(now, this.lastEventAt),
    });
    if (next !== this.state) {
      this.prev = this.state;
      this.state = next;
      this.since = anim;
    }
    return next;
  }

  /** A decision sent from the office balloon failed (or a proposal was rejected): `!` for 8 s. */
  fail(now: number) {
    this.errorUntil = now + ERROR_MS;
  }
}

// ---- per-leader runtime: state + pupils + blink + hover ----

/** Spec §2: hover ×1.04 in ~150 ms. */
const HOVER_RATE = 20;

export interface CoreStepIn {
  snap: CoreSnapshot;
  /** Wall clock ms. */
  now: number;
  /** Animation clock ms. */
  anim: number;
  dt: number;
  body: Body;
  pointer: Pointer | null;
  /** Centre of a worker's chip (world units), or null when it is gone. */
  workerAt: (id: string) => Vec | null;
  hovered: boolean;
  reduced: boolean;
  rand?: () => number;
}

export class CoreXL {
  readonly tracker = new CoreTracker();
  readonly pupil: Vec = { x: 0, y: 0 };
  readonly fragments = new FragmentMotion();
  readonly weight: Vec = { x: 0, y: 0 };
  hover = 0;
  private blinkAt = -1;
  private blinkStart = -1;
  private lid = 0;

  step(i: CoreStepIn) {
    const rand = i.rand ?? Math.random;
    const id = this.tracker.observe(i.snap, i.now, i.anim);
    const look = this.tracker.look ? i.workerAt(this.tracker.look.worker) : null;
    const scale = i.body.s / XL_BODY;
    const g = gazeGoal({ id, eye: eyeAt(i.body), scale, pointer: i.pointer, look, now: i.anim, reduced: i.reduced });
    this.pupil.x = approach(this.pupil.x, g.to.x, i.dt, g.rate);
    this.pupil.y = approach(this.pupil.y, g.to.y, i.dt, g.rate);
    this.fragments.step(id, i.dt, i.reduced);
    const weight = bodyWeight(i.anim, i.reduced || id === "sleep");
    const rate = i.reduced ? Infinity : FOLLOW_RATE;
    this.weight.x = approach(this.weight.x, weight.x, i.dt, rate);
    this.weight.y = approach(this.weight.y, weight.y, i.dt, rate);
    this.hover = i.reduced ? Number(i.hovered) : approach(this.hover, Number(i.hovered), i.dt, HOVER_RATE);
    if (!blinks(id, i.reduced)) {
      this.blinkStart = -1;
      this.blinkAt = -1;
    } else {
      if (this.blinkAt < 0) this.blinkAt = nextBlink(i.anim, rand());
      if (this.blinkStart < 0 && i.anim >= this.blinkAt) this.blinkStart = i.anim;
      if (this.blinkStart >= 0 && i.anim - this.blinkStart >= BLINK_MS) {
        this.blinkStart = -1;
        this.blinkAt = nextBlink(i.anim, rand());
      }
    }
    this.lid = lidAt(i.anim, this.blinkStart);
    return id;
  }

  get state() { return this.tracker.state; }
  get lidNow() { return this.lid; }
}
