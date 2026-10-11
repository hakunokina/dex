import type { CoreStateId } from "./companion/coreState.ts";

export interface LiquidPoint { x: number; y: number }
export const TOUCH_MS = 1400;

export function reachAt(age: number): number {
  if (age < 0 || age >= TOUCH_MS) return 0;
  if (age < 600) return Math.sin(age / 600 * Math.PI / 2);
  if (age < 800) return 1;
  return Math.cos((age - 800) / 600 * Math.PI / 2);
}

/** Tapered ribbon following the existing circuit route, with a wave pinned at both ends. */
export function tentacleContour(route: readonly LiquidPoint[], reach: number, time: number): LiquidPoint[] {
  if (route.length < 2 || reach <= 0) return [];
  const lengths = route.slice(1).map((p, i) => Math.hypot(p.x - route[i].x, p.y - route[i].y));
  const total = lengths.reduce((a, b) => a + b, 0);
  if (!total) return [];
  const left: LiquidPoint[] = [], right: LiquidPoint[] = [];
  for (let i = 0; i <= 24; i++) {
    const u = i / 24;
    let distance = u * Math.min(1, reach) * total;
    let segment = 0;
    while (segment < lengths.length - 1 && distance > lengths[segment]) distance -= lengths[segment++];
    const a = route[segment], b = route[segment + 1];
    const len = lengths[segment] || 1;
    const k = distance / len, nx = -(b.y - a.y) / len, ny = (b.x - a.x) / len;
    const wave = Math.sin(u * Math.PI) * Math.sin(u * 13 - time / 160) * Math.min(7, total / 40);
    const width = 5 * Math.pow(1 - u, 0.7);
    const x = a.x + (b.x - a.x) * k, y = a.y + (b.y - a.y) * k;
    left.push({ x: x + nx * (wave + width), y: y + ny * (wave + width) });
    right.push({ x: x + nx * (wave - width), y: y + ny * (wave - width) });
  }
  return [...left, ...right.reverse()];
}

export class DelegationTouches {
  private seen: Set<string> | null = null;
  observe(jobs: readonly { key: string; fromAgent: string; toAgent: string; status: string }[]) {
    const contacts = this.seen ? jobs.filter(j => !this.seen!.has(j.key) && (j.status === "queued" || j.status === "running")) : [];
    this.seen = new Set(jobs.map(j => j.key));
    return contacts.map(j => ({ leader: j.fromAgent, worker: j.toAgent }));
  }
}

/** Four motion families, driven by the existing Core state and its entry clock. */
export function symbioteParams(state: CoreStateId, age: number) {
  if (state === "error") return { speed: 3.8, amplitude: 4, spikes: 10 };
  if (state === "thinking" || state === "tool" || state === "delegated" || state === "ask")
    return { speed: state === "tool" ? 3 : 2, amplitude: 6, spikes: 0 };
  if (state === "done") return { speed: 0.6, amplitude: 1.5 + 4 * Math.exp(-Math.max(0, age) / 900), spikes: 0 };
  return { speed: state === "sleep" ? 0.15 : 0.45, amplitude: state === "sleep" ? 1 : 2.5, spikes: 0 };
}

/** Periodic low-frequency noise with travelling lobes; bounded to the existing 120-unit square. */
export function bodyContour(state: CoreStateId, time: number, age: number, reduced: boolean): LiquidPoint[] {
  const p = symbioteParams(state, reduced ? 5000 : age);
  const t = reduced ? 0 : time / 1000 * p.speed;
  return Array.from({ length: 64 }, (_, i) => {
    const a = i / 64 * Math.PI * 2;
    const noise = Math.sin(a * 3 + t) * 0.5 + Math.sin(a * 5 - t * 1.3) * 0.3 + Math.cos(a * 7 + t * 0.7) * 0.2;
    const lobe = Math.pow((1 + Math.sin(a * 4 - t)) / 2, 5) * 5;
    const spikes = p.spikes * Math.pow((1 + Math.cos(a * 16)) / 2, 8);
    // Leave room for the 2.5% hover growth and outline inside the old body bounds.
    const r = Math.min(58, 46 + noise * p.amplitude + lobe + spikes);
    return { x: Math.cos(a) * r, y: Math.sin(a) * r };
  });
}

export interface FragmentBody extends LiquidPoint { s: number }
/** Top-left rectangles, including provider/name/role plates. */
export interface FragmentBox extends LiquidPoint { w: number; h: number }
export interface FragmentPath { points: LiquidPoint[]; lengths: number[]; length: number }
export interface Fragment extends LiquidPoint { radius: number; angle: number }

/** Segment's entry/exit parameters through a rectangle (slab intersection). */
function crossing(a: LiquidPoint, b: LiquidPoint, box: FragmentBox): [number, number] | null {
  let enter = 0, exit = 1;
  for (const [start, delta, min, max] of [
    [a.x, b.x - a.x, box.x, box.x + box.w],
    [a.y, b.y - a.y, box.y, box.y + box.h],
  ]) {
    if (Math.abs(delta) < 1e-9) { if (start < min || start > max) return null; }
    else {
      const u = (min - start) / delta, v = (max - start) / delta;
      enter = Math.max(enter, Math.min(u, v));
      exit = Math.min(exit, Math.max(u, v));
      if (enter > exit) return null;
    }
  }
  return [enter, exit];
}

/** Only the nearby prefix of each PCB trace is used; full contours stay clear of protected areas. */
export function fragmentPaths(body: FragmentBody, traces: readonly (readonly LiquidPoint[])[], blocks: readonly FragmentBox[], board: FragmentBox): FragmentPath[] {
  const clearance = 14 * body.s / 120;
  const bounds = { x: board.x + clearance, y: board.y + clearance, w: board.w - 2 * clearance, h: board.h - 2 * clearance };
  if (bounds.w <= 0 || bounds.h <= 0 || !crossing(body, body, bounds)) return [];
  const obstacles = blocks.map(b => ({ x: b.x - clearance, y: b.y - clearance, w: b.w + 2 * clearance, h: b.h + 2 * clearance }));
  // A solitary Core still sheds pieces. These short, bent paths avoid its lower label band.
  const fallback = [-1, 1].flatMap(dir => [-1, 1].map(up => [
    { x: body.x + dir * body.s * 0.5, y: body.y + up * body.s * 0.12 },
    { x: body.x + dir * body.s * 0.82, y: body.y + up * body.s * 0.12 },
    { x: body.x + dir * body.s * 0.95, y: body.y + up * body.s * 0.25 },
  ]));
  const paths: FragmentPath[] = [];
  for (const route of traces.length ? traces : fallback) {
    const points: LiquidPoint[] = [{ x: body.x, y: body.y }];
    const lengths: number[] = [];
    let length = 0;
    for (const b of route) {
      const a = points[points.length - 1];
      const distance = Math.hypot(b.x - a.x, b.y - a.y);
      if (!distance) continue;
      let stop = Math.min(1, (body.s * 1.05 - length) / distance);
      stop = Math.min(stop, crossing(a, b, bounds)?.[1] ?? 0);
      for (const box of obstacles) {
        const hit = crossing(a, b, box);
        if (hit) stop = Math.min(stop, hit[0]);
      }
      if (stop > 0) {
        points.push({ x: a.x + (b.x - a.x) * stop, y: a.y + (b.y - a.y) * stop });
        lengths.push(distance * stop);
        length += distance * stop;
      }
      if (stop < 1 || length >= body.s * 1.05) break;
    }
    if (length > body.s * 0.5) paths.push({ points, lengths, length });
  }
  return traces.length && !paths.length ? fragmentPaths(body, [], blocks, board) : paths;
}

/** Roaming droplets around the Core (patron: three times the first four). */
export const FRAGMENTS = 12;

/** A route whose far end lies more left/right of the body than above/below it. */
export function sideways(path: FragmentPath, body: FragmentBody): boolean {
  const end = path.points[path.points.length - 1];
  return Math.abs(end.x - body.x) > Math.abs(end.y - body.y);
}

/** Persistent phase avoids teleports on work/idle transitions; error pulls each piece down its path. */
export class FragmentMotion {
  phase = 0;
  private reach = 1;
  step(state: CoreStateId, dt: number, reduced: boolean) {
    if (reduced) return;
    const seconds = Math.max(0, dt);
    this.reach = state === "error" ? Math.max(0, this.reach - seconds / 0.6) : Math.min(1, this.reach + seconds / 0.9);
    if (state !== "error") {
      const active = state === "thinking" || state === "tool" || state === "delegated" || state === "ask";
      this.phase += seconds / (active ? 7 : state === "sleep" ? 24 : 16);
    }
  }
  sample(paths: readonly FragmentPath[], body: FragmentBody, reduced: boolean): Fragment[] {
    if (reduced || !paths.length) return [];
    // Side routes (leaving left/right) carry twice the pieces of upper/lower ones (patron).
    const slots = paths.flatMap(p => (sideways(p, body) ? [p, p] : [p]));
    const count = Math.round(FRAGMENTS * slots.length / paths.length);
    return Array.from({ length: count }, (_, i) => {
      const path = slots[i % slots.length];
      const travel = Math.sin((this.phase + i / count + 0.125) * Math.PI) ** 2;
      let distance = (body.s * 0.3 + (path.length - body.s * 0.3) * travel) * this.reach;
      let segment = 0;
      while (segment < path.lengths.length - 1 && distance > path.lengths[segment]) distance -= path.lengths[segment++];
      const a = path.points[segment], b = path.points[segment + 1];
      const k = distance / path.lengths[segment];
      return { x: a.x + (b.x - a.x) * k, y: a.y + (b.y - a.y) * k, radius: (6.5 + (i % 4) * 0.6) * body.s / 120, angle: Math.atan2(b.y - a.y, b.x - a.x) };
    });
  }
}

/** Twenty vertices per droplet, elongated along its trace; no pixel filters or offscreen surfaces. */
export function fragmentContour(piece: Fragment, phase: number): LiquidPoint[] {
  const c = Math.cos(piece.angle), s = Math.sin(piece.angle);
  return Array.from({ length: 20 }, (_, i) => {
    const a = i / 20 * Math.PI * 2;
    const r = piece.radius * (1 + 0.1 * Math.sin(a * 3 + phase * 25) + 0.06 * Math.cos(a * 5 - phase * 17));
    const x = Math.cos(a) * r * 1.25, y = Math.sin(a) * r * 0.85;
    return { x: piece.x + c * x - s * y, y: piece.y + s * x + c * y };
  });
}
