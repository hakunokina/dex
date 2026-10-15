// #1958: the Kart's A · Doku look (design #1937; src/office/assets/symbiote-cards/HANDOFF.md). Pure: the
// look's numbers, state → light, the body / label geometry and the strand route smoothing. world.ts
// draws with it, litLayer.ts lights it; node:test covers it. Lengths are world units unless named *Css.
import type { Mode, Point, Target } from "./placement.ts";

export type Rgb = [number, number, number];

/** HANDOFF §4 (mock `LOOKS.A`): colours are sRGB hex, the shader takes them linear. */
export const LOOK = {
  /** World units per ground tile. */
  tile: 170,
  exposure: 1.25,
  vignette: 0.55,
  ambient: "#3a2a33", ambientK: 0.16,
  key: { color: "#ffd8d2", k: 0.32, elevation: 0.95, azimuth: -2.3, sweep: 0.05, fill: "#4a6a8a", fillK: 0.06 },
  spec: 0.55,
  /** Ground emissive: the vein pulse. */
  emit: "#ff4a3a",
  /** #2020: `dry` = a dormant strand's albedo (dark, low saturation); `dryRough` = no wet sheen on it. */
  strand: { width: 11, height: 1.8, blur: 1, wobble: 4, albedo: "#5a1626", rough: 0.2, glow: "#ff3b30", glowK: 0.16, dry: "#24181a", dryRough: 0.6 },
  core: { light: 2.6, radius: 120 },
  /**
   * #2020: `live` = a live sac's extra light (× `light`), `swell` = its heartbeat growth, `dry` = a dormant
   * sac's size and albedo darkening, `halo` = a live sac's light on the ground (radius × D).
   */
  worker: { aspect: 0.82, light: 1.6, translucency: 1, heightK: 0.5, sink: 0.9, shadow: 0.85, live: 1.8, swell: 0.1, dry: 0.92, dim: 0.55, halo: 1.3 },
  echo: { color: "#8fe6ea", light: 1.3, glow: 0.8, radius: 70, float: 3, bob: 2.2, alpha: 0.92, sway: 0.07 },
  /** The 2D fallback (no WebGL2, context lost, maps loading): a flat membrane and radial bodies. */
  fallback: { ground: "#0d0709", glow: "#3a0d12", strand: "#3c111b", dry: "#1c1012" },
} as const;

/** GL resolution: ≤ 1.5× and ≤ this many pixels (a weak GPU still holds a big panel). */
export const GL_MAX_SCALE = 1.5;
export const GL_MAX_PIXELS = 2.6e6;
export const glScale = (dpr: number, cssW: number, cssH: number) =>
  Math.max(0.5, Math.min(dpr || 1, GL_MAX_SCALE, Math.sqrt(GL_MAX_PIXELS / Math.max(1, cssW * cssH))));

/** sRGB colour (`#rgb`, `#rrggbb`, `rgb()` / `rgba()`) → linear RGB; unreadable → black. */
export function linear(color: string): Rgb {
  const c = color.trim();
  let rgb: number[] = [];
  const hex = /^#([0-9a-f]{6}|[0-9a-f]{3})(?![0-9a-f])/i.exec(c)?.[1];
  if (hex) rgb = (hex.length === 3 ? [...hex].map((h) => h + h) : [hex.slice(0, 2), hex.slice(2, 4), hex.slice(4, 6)]).map((h) => parseInt(h, 16));
  else rgb = (/rgba?\(([^)]+)\)/i.exec(c)?.[1] ?? "").split(/[\s,/]+/).slice(0, 3).map(Number);
  if (rgb.length !== 3 || rgb.some((v) => !Number.isFinite(v))) return [0, 0, 0];
  return rgb.map((v) => {
    const s = Math.max(0, Math.min(255, v)) / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  }) as Rgb;
}

export const scale = (c: Rgb, k: number): Rgb => [c[0] * k, c[1] * k, c[2] * k];

export function lightDir(azimuth: number, elevation: number): Rgb {
  return [Math.cos(elevation) * Math.cos(azimuth), Math.cos(elevation) * Math.sin(azimuth), Math.sin(elevation)];
}

/** The key light's azimuth: a slow sweep, still under reduced motion. */
export const keyAzimuth = (time: number, reduced: boolean) => LOOK.key.azimuth + (reduced ? 0 : time * LOOK.key.sweep);

/** §8: two rings leave Core every 2.38 s; reduced motion holds them. */
export const beatAt = (time: number, reduced: boolean) => (reduced ? 0.3 : (time * 0.42) % 1);

// ---- state → light (HANDOFF §5: today's chipLook priority and colours) ----

export type Tone = "green" | "redHi" | "amber" | "red" | "muted";
export type Mark = "✓" | "!" | "?";

export interface Light {
  /** The light's and the mark's colour. */
  tone: Tone;
  /** Light strength (0 = dark); the body's emissive intensity is `k × LOOK.worker.light`. */
  k: number;
  /** 0..1 rhythm; 1 under reduced motion, 0 when off. */
  pulse: number;
  mark: Mark | null;
  error: boolean;
  offline: boolean;
}

const soft = (hz: number, t: number) => 0.5 + 0.5 * Math.sin(t * Math.PI * 2 * hz);

/**
 * The mark and light by today's chipLook priority: an unread result wins, then a question / waiting (one
 * amber look), then working. Offline only darkens the body (an unread ✓ stays readable on it).
 * No rhythm passes 3 Hz (WCAG 2.3.1): done 0.35, error 0.7, ask 1.1, working 1.15.
 */
export function workerLight(
  status: string, result: { status: string } | null, question: unknown, time: number, reduced: boolean,
): Light {
  const look = lookOf(status, result, question, time, reduced);
  // The tone stays the mark's colour; the dark body ignores it.
  return status === "offline" ? { ...look, k: 0, pulse: 0, offline: true } : look;
}

function lookOf(status: string, result: { status: string } | null, question: unknown, time: number, reduced: boolean): Light {
  const base = { mark: null, error: false, offline: false };
  if (result?.status === "done") return { ...base, tone: "green", k: 1, pulse: reduced ? 1 : 0.6 + 0.4 * soft(0.35, time), mark: "✓" };
  if (result?.status === "failed") {
    return { ...base, tone: "redHi", k: 1.1, pulse: reduced ? 1 : 0.55 + 0.45 * Math.abs(Math.sin(time * Math.PI * 0.7)), mark: "!", error: true };
  }
  if (question || status === "waiting") return { ...base, tone: "amber", k: 1, pulse: reduced ? 1 : soft(1.1, time), mark: "?" };
  if (result) return { ...base, tone: "amber", k: 0.9, pulse: 1, mark: "!" }; // done_no_report
  if (status === "working") return { ...base, tone: "red", k: 1, pulse: reduced ? 1 : soft(1.15, time) ** 2.5 };
  return { ...base, tone: "muted", k: 0.22, pulse: 1 };
}

/**
 * #2020: how alive a worker's strand reads, 0 (dormant: idle, offline) .. 1 (live: working, an active job,
 * a question, an unread result). Only live strands keep the red; the rest dry out.
 */
export function lifeOf(light: Light, job: boolean): number {
  if (light.tone === "muted") return job ? 1 : 0;
  // A mark lights the strand fully (an offline body too); an offline click's wake-up ramps in.
  return light.offline || light.mark ? 1 : Math.max(0, Math.min(1, light.k));
}

/** #2020: a worker sac by its light and life: dormant ones shrink, dim and grey; live ones swell and glow. */
export function sacLook(light: Light, life: number) {
  const w = LOOK.worker;
  const live = light.offline ? 0 : life;
  return {
    intensity: light.offline ? 0 : light.k * w.light * (1 + (w.live - 1) * live),
    /** Size factor: a dormant sac w.dry, a live one beats up to 1 + swell (held at the top under reduced motion). */
    size: (light.offline ? w.sink : w.dry + (1 - w.dry) * live) + w.swell * live * light.pulse,
    desat: light.offline ? 0.75 : 0.7 * (1 - live),
    dim: light.offline ? 0 : w.dim * (1 - live),
    alpha: light.offline ? 0.72 : 1 - 0.12 * (1 - live),
    /** The ground halo's strength (0: none). */
    halo: live * (0.4 + 0.6 * light.pulse),
  };
}

/** §9: Echo keeps its own colour; talking (≈1.2 + 1.8 Hz beat) / waiting / idle read in its strength and rhythm. */
export function echoLight(status: string, time: number, reduced: boolean) {
  const talk = status === "working", wait = status === "waiting";
  const intensity = (status === "offline" ? 0 : talk ? 1.25 : wait ? 0.9 : 0.55) * LOOK.echo.glow;
  const pulse = reduced ? 1
    : talk ? 0.5 + 0.5 * Math.sin(time * 9.5) * Math.sin(time * 2.1)
      : wait ? 0.5 + 0.5 * Math.sin(time * 6.9) : 0.75 + 0.25 * Math.sin(time * 1.4);
  return { intensity, pulse };
}

/** Echo's drift (world units) and sway (rad); still under reduced motion. */
export function echoDrift(time: number, reduced: boolean) {
  return reduced ? { dy: -LOOK.echo.float, rot: 0 } : { dy: Math.sin(time * 1.3) * LOOK.echo.bob - LOOK.echo.float, rot: Math.sin(time * 0.9) * LOOK.echo.sway };
}

/** The `?` mark's blink alpha (~1.1 Hz). */
export const markAlpha = (mark: Mark | null, time: number, reduced: boolean) => (mark === "?" && !reduced ? 0.55 + 0.45 * Math.sin(time * 6.9) : 1);

/** Core eye's light (§4 stateK): sleep 0.15, idle 0.6, else by the look's glow; the lid closes it. */
export function coreEyeK(state: string, glow: number, lid: number) {
  const k = state === "sleep" ? 0.15 : state === "idle" ? 0.6 : 0.8 + glow / 24;
  return LOOK.core.light * k * (1 - 0.85 * lid);
}

// ---- bodies and labels (§7) ----

export interface Spot {
  /** Body centre and size: D wide, `fh` tall. */
  bx: number; by: number; D: number; fh: number;
  /** +1: the body looks right (left column), −1: left, 0: no side (rows, Echo). */
  facing: 1 | -1 | 0;
  /** The label's column; `align` right in the left column. */
  textL: number; textR: number; align: "left" | "right";
}

/** Columns: a worker left of Core faces right. Rows and Echo have no side. */
export function facingOf(slot: { x: number }, core: { x: number } | null, mode: Mode | undefined, echo: boolean): 1 | -1 | 0 {
  if (echo || !core || mode !== "columns") return 0;
  return slot.x < core.x ? 1 : -1;
}

/** The body at the slot's Core-facing end, the label outside it. */
export function spotOf(slot: Target, facing: 1 | -1 | 0, echo: boolean): Spot {
  const aspect = echo ? 1 : LOOK.worker.aspect;
  const D = Math.max(1, Math.min(slot.w * (echo ? 0.4 : 0.42), (slot.h * 0.94) / aspect));
  const fh = D * aspect;
  const bx = facing === 1 ? slot.x + slot.w / 2 - D / 2 - 1 : slot.x - slot.w / 2 + D / 2 + 1;
  const gap = 6;
  const textL = facing === 1 ? slot.x - slot.w / 2 : bx + D / 2 + gap;
  const textR = facing === 1 ? bx - D / 2 - gap : slot.x + slot.w / 2;
  return { bx, by: slot.y, D, fh, facing, textL, textR, align: facing === 1 ? "right" : "left" };
}

/** Label rows: name (+ role from 28 units, + cost pill from 40); y of each row. */
export function textBlock(spot: Spot, slotH: number, hasCost: boolean) {
  const detailed = slotH >= 28;
  const rows = detailed && slotH >= 40 && hasCost ? 3 : detailed ? 2 : 1;
  const total = [14, 12, 15].slice(0, rows).reduce((a, b) => a + b, 0);
  const top = spot.by - total / 2 + (detailed ? 2 : 0);
  return { rows, top, nameY: top + 7, roleY: top + 20, costY: top + 33.5 };
}

export type Rect = { x: number; y: number; w: number; h: number };

/** The label's box (provider badge to the last row): the strand layer's calm zone under it. */
export function labelRect(spot: Spot, block: ReturnType<typeof textBlock>): Rect {
  return { x: spot.textL, y: block.top - 10, w: Math.max(0, spot.textR - spot.textL), h: (block.rows === 3 ? 41 : block.rows === 2 ? 26 : 14) + 10 };
}

/** #2020 (patron): Echo beside its Core's name — `gap` from the name block, body ≤ `maxD`, `lift` above its middle. */
export const ECHO_BESIDE = { gap: 8, maxD: 34, lift: 3, labelW: 120 };

/**
 * #2020: Echo's body just left of its Core's name block (`blockW` wide, centred on `core.x`), a little above
 * the block's middle; its Core-style label (name, then role + cost) centred under the body.
 */
export function echoBeside(core: { x: number; nameY: number; roleY: number }, blockW: number, D: number) {
  const d = Math.max(1, Math.min(D, ECHO_BESIDE.maxD));
  const bx = core.x - blockW / 2 - ECHO_BESIDE.gap - d / 2;
  const by = (core.nameY + core.roleY) / 2 - ECHO_BESIDE.lift;
  const half = ECHO_BESIDE.labelW / 2;
  const spot: Spot = { bx, by, D: d, fh: d, facing: 0, textL: bx - half, textR: bx + half, align: "left" };
  const nameY = by + d / 2 + 10;
  return { spot, nameY, roleY: nameY + 15, maxW: ECHO_BESIDE.labelW };
}

/** The status mark's centre: the body's outer top corner. */
export const markAt = (spot: Spot) => ({ x: spot.bx + (spot.facing === 1 ? -1 : 1) * spot.D * 0.38, y: spot.by - spot.fh * 0.46 });

/** The stall clock / slow glass: the body's inner top corner (the mark keeps the outer one). */
export const clockAt = (spot: Spot) => ({ x: spot.bx - (spot.facing === 1 ? -1 : 1) * spot.D * 0.38, y: spot.by - spot.fh * 0.46 });

export interface Footprint { w: number; h: number; cx: number; cy: number }

/** The map's coverage box (meta.json `bounds`, map px) → share of the map and offset of its centre. */
export function footprint(meta: { res: number; bounds: readonly number[] }): Footprint {
  const [x0, y0, x1, y1] = meta.bounds;
  const w = (x1 - x0 + 1) / meta.res, h = (y1 - y0 + 1) / meta.res;
  return { w, h, cx: (x0 + x1 + 1) / 2 / meta.res - 0.5, cy: (y0 + y1 + 1) / 2 / meta.res - 0.5 };
}

// ---- strands (§6) ----

export function chaikin(points: Point[], n = 3): Point[] {
  let p = points;
  for (let k = 0; k < n && p.length > 2; k++) {
    const out: Point[] = [p[0]];
    for (let i = 0; i < p.length - 1; i++) {
      const a = p[i], b = p[i + 1];
      out.push({ x: a.x * 0.75 + b.x * 0.25, y: a.y * 0.75 + b.y * 0.25 }, { x: a.x * 0.25 + b.x * 0.75, y: a.y * 0.25 + b.y * 0.75 });
    }
    out.push(p[p.length - 1]);
    p = out;
  }
  return p;
}

export function resample(points: Point[], step: number): Point[] {
  const out: Point[] = [points[0]];
  let carry = 0;
  for (let i = 0; i < points.length - 1; i++) {
    const a = points[i], b = points[i + 1];
    const len = Math.hypot(b.x - a.x, b.y - a.y);
    let d = step - carry;
    while (d <= len) { out.push({ x: a.x + (b.x - a.x) * d / len, y: a.y + (b.y - a.y) * d / len }); d += step; }
    carry = len - (d - step);
  }
  const last = points[points.length - 1];
  const tail = out[out.length - 1];
  if (Math.hypot(last.x - tail.x, last.y - tail.y) > 0.3) out.push(last);
  else out[out.length - 1] = last;
  return out;
}

/** Seeded LCG (the mock's): the same layout draws the same strands. */
export function rand(seed: number) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
}

/** A sideways wobble along the normal, tapered to zero at both ends (ports and bodies stay attached). */
export function organic(points: Point[], wobble: number, seed: number): Point[] {
  if (!wobble || points.length < 3) return points;
  const r = rand(seed);
  const phase = r() * 6.28, freq = 0.045 + r() * 0.03;
  let total = 0;
  const acc = points.map((p, i) => (i ? (total += Math.hypot(p.x - points[i - 1].x, p.y - points[i - 1].y)) : 0));
  return points.map((p, i) => {
    const a = points[Math.max(0, i - 1)], b = points[Math.min(points.length - 1, i + 1)];
    const len = Math.hypot(b.x - a.x, b.y - a.y) || 1;
    const nx = -(b.y - a.y) / len, ny = (b.x - a.x) / len;
    const u = acc[i] / (total || 1);
    const taper = Math.max(0, Math.sin(Math.PI * Math.min(1, u * 1.15)) * Math.min(1, acc[i] / 20, (total - acc[i]) / 14));
    const off = wobble * Math.sin(acc[i] * freq + phase) * taper;
    return { x: p.x + nx * off, y: p.y + ny * off };
  });
}

/** The octilinear route → the soft strand the feet, droplets and tentacle follow (`index`: its seed). */
export const smoothRoute = (points: Point[], index: number) =>
  points.length < 2 ? points : organic(resample(chaikin(points, 3), 2), LOOK.strand.wobble, index * 97 + 13);

/** The route's end moved onto the body's Core-facing edge (rows: its top / bottom). */
export function routeToBody(points: Point[], spot: Spot): Point[] {
  const pts = points.map((p) => ({ ...p }));
  const end = pts[pts.length - 1];
  if (!end) return pts;
  if (spot.facing) end.x = spot.bx - spot.facing * spot.D * 0.42;
  else {
    const before = pts[pts.length - 2];
    if (before && Math.abs(before.x - end.x) < 0.01) before.x = spot.bx;
    end.x = spot.bx;
    end.y = spot.by + (end.y < spot.by ? -spot.fh * 0.42 : spot.fh * 0.42);
  }
  return pts;
}

export function pathLength(points: Point[]) {
  let total = 0;
  for (let i = 1; i < points.length; i++) total += Math.hypot(points[i].x - points[i - 1].x, points[i].y - points[i - 1].y);
  return total;
}

export function pointAt(points: Point[], u: number): Point {
  let d = Math.max(0, Math.min(1, u)) * pathLength(points);
  for (let i = 1; i < points.length; i++) {
    const len = Math.hypot(points[i].x - points[i - 1].x, points[i].y - points[i - 1].y);
    if (d <= len) {
      const k = len ? d / len : 0;
      return { x: points[i - 1].x + (points[i].x - points[i - 1].x) * k, y: points[i - 1].y + (points[i].y - points[i - 1].y) * k };
    }
    d -= len;
  }
  return points[points.length - 1];
}

/** A drop travelling back worker → Core (`along` 1 → 0 on the Core-first route); null: none / still. */
export function dropAt(question: { id: number } | null, result: { id: number } | null, time: number, reduced: boolean) {
  if (reduced || (!question && !result)) return null;
  const p = question ? (time * 0.24 + question.id * 0.11) % 1 : (time * 0.28 + result!.id * 0.07) % 1;
  return 1 - p;
}

/** §6 Core anchor: 9 root veins around the body, tapering outward (seed 1937). */
export function coreVeins(body: { x: number; y: number; s: number }): Point[][] {
  const r = rand(1937);
  const veins: Point[][] = [];
  for (let i = 0; i < 9; i++) {
    const a = (i / 9) * Math.PI * 2 + r() * 0.5;
    const len = body.s * (0.75 + r() * 0.55);
    const pts: Point[] = [];
    for (let s = 0; s <= len; s += 3) {
      const bend = Math.sin(s * 0.05 + i) * 0.25;
      pts.push({ x: body.x + Math.cos(a + bend) * (body.s * 0.3 + s), y: body.y + Math.sin(a + bend) * (body.s * 0.3 + s) });
    }
    veins.push(pts);
  }
  return veins;
}
