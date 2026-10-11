// #243: Core's procedural drawing (mock.html `drawCore`, design #236). A chip body with pin legs, LED
// eyes and a corner badge; state = ring/LED colour + eye shape + badge (+ motion unless reduced).
// Colours come from the theme tokens (readPalette), never literals.
import { LOOKS, type CoreLook } from "./coreState.ts";
import { bodyContour, fragmentContour, type Fragment, type FragmentBody } from "../symbiote.ts";

export interface CorePalette {
  /** Ring/LED colour per state token. */
  tokens: Record<string, string>;
  chip: string;
  chipTop: string;
  pin: string;
  via: string;
  badgeBg: string;
  mouth: string;
  z: string;
}

export function readPalette(el: Element = document.documentElement): CorePalette {
  const css = getComputedStyle(el);
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  const tokens: Record<string, string> = {};
  for (const l of Object.values(LOOKS)) tokens[l.token] = v(l.token, "#888");
  tokens["--red-hi"] = v("--red-hi", "#ff5c5f");
  return {
    tokens,
    chip: v("--pcb-chip", "#1d2124"),
    chipTop: v("--surface-3", "#1e2024"),
    pin: v("--pcb-pin", "#71777b"),
    via: v("--pcb-via", "rgba(173,182,183,.22)"),
    badgeBg: v("--surface-1", "#121316"),
    mouth: v("--text", "#d5d8dd"),
    z: v("--muted", "#6c717a"),
  };
}

/** Reference space (units): the canvas is scaled to it. 64 (mock: 96) so the body fills a 32px slot. */
const REF = 64;
const BODY = 34;

/** Whether this look moves at all (no loop otherwise: one frame per change). */
export const animates = (look: CoreLook, reduced: boolean) => !reduced && (look.motion !== "still" || look.badge === "z");

export function drawCore(ctx: CanvasRenderingContext2D, look: CoreLook, t: number, reduced: boolean, pal: CorePalette) {
  const { width: w, height: h } = ctx.canvas;
  const color = pal.tokens[look.token] ?? pal.z;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const k = w / REF;
  ctx.save();
  ctx.scale(k, k);
  ctx.translate(REF / 2, REF / 2 + 2);

  let dx = 0, dy = 0, sc = 1;
  if (!reduced) {
    const m = look.motion;
    if (m === "breathe") sc = 1 + 0.02 * Math.sin(t / 700);
    else if (m === "bounce") dy = -2.5 * Math.abs(Math.sin(t / 450));
    else if (m === "hop") dy = -4 * Math.abs(Math.sin(t / 300));
    else if (m === "jump") dy = -6 * Math.abs(Math.sin(t / 260));
    else if (m === "shake") dx = 2 * Math.sin(t / 45);
    else if (m === "tremble") { dx = 0.8 * Math.sin(t / 30); dy = 0.8 * Math.cos(t / 37); }
    else if (m === "scan") dx = 1.5 * Math.sin(t / 500);
  }
  if (look.motion === "still") dy = 1;
  ctx.translate(dx, dy);
  ctx.scale(sc, sc);

  const b = BODY, half = b / 2;
  // pins: 3 per side
  ctx.fillStyle = pal.pin;
  const l = 5, gap = half + 1.2, ext = 3.4;
  for (const s of [-1, 0, 1]) {
    const p = s * (b / 4);
    ctx.fillRect(p - l / 2, -gap - ext, l, ext);
    ctx.fillRect(p - l / 2, gap, l, ext);
    ctx.fillRect(-gap - ext, p - l / 2, ext, l);
    ctx.fillRect(gap, p - l / 2, ext, l);
  }
  // body + ring (dashed while failing / running a tool — #229's broken ring)
  ctx.beginPath();
  ctx.roundRect(-half, -half, b, b, 7);
  const g = ctx.createLinearGradient(0, -half, 0, half);
  g.addColorStop(0, pal.chipTop);
  g.addColorStop(1, pal.chip);
  ctx.fillStyle = g;
  ctx.fill();
  ctx.lineWidth = 1.6;
  ctx.strokeStyle = color;
  if (look.motion === "shake" || look.motion === "tremble") ctx.setLineDash([5, 3]);
  ctx.stroke();
  ctx.setLineDash([]);
  // corner vias
  ctx.fillStyle = pal.via;
  for (const [vx, vy] of [[-1, -1], [1, -1], [-1, 1], [1, 1]]) {
    ctx.beginPath();
    ctx.arc(vx * (half - 4), vy * (half - 4), 1.4, 0, 7);
    ctx.fill();
  }
  drawEyes(ctx, look, color, t, reduced);
  // mouth: letterbox line; error = small open square, sleep = short line
  ctx.strokeStyle = pal.mouth;
  ctx.globalAlpha = 0.5;
  ctx.lineWidth = 1.2;
  ctx.lineCap = "round";
  ctx.beginPath();
  if (look.motion === "still") { ctx.moveTo(-3, 9); ctx.lineTo(3, 9); }
  else if (look.eye === "x") ctx.rect(-2.4, 7, 4.8, 4.8);
  else { ctx.moveTo(-4, 8.5); ctx.lineTo(4, 8.5); }
  ctx.stroke();
  ctx.globalAlpha = 1;
  drawBadge(ctx, look, color, t, reduced, pal);
  if (look.badge === "z" && !reduced) {
    ctx.fillStyle = pal.z;
    ctx.font = "7px monospace";
    const ph = (t / 900) % 1;
    for (let i = 0; i < 2; i++) {
      const p = (ph + i / 2) % 1;
      ctx.globalAlpha = 1 - p;
      ctx.fillText("z", half + 4 + p * 6, -half - 2 - p * 8);
    }
    ctx.globalAlpha = 1;
  }
  ctx.restore();
}

function drawEyes(ctx: CanvasRenderingContext2D, look: CoreLook, color: string, t: number, reduced: boolean) {
  const spread = look.eye === "wide" ? 7 : 6;
  const ey = look.eye === "down" ? 2.5 : look.motion === "still" ? 1.5 : 0;
  const ex = look.eye === "scan" && !reduced ? 3 * Math.sin(t / 500) : 0;
  ctx.lineCap = "round";
  for (const s of [-1, 1]) {
    const cx = s * spread + ex, cy = ey;
    if (look.eye === "dot" || look.eye === "scan" || look.eye === "down" || look.eye === "wide") {
      const r = look.eye === "wide" ? 3.4 : 2.8;
      ctx.save();
      ctx.shadowColor = color;
      ctx.shadowBlur = look.glow;
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(cx, cy, r, 0, 7);
      ctx.fill();
      ctx.restore();
      ctx.fillStyle = "rgba(255,255,255,.75)";
      ctx.beginPath();
      ctx.arc(cx - r * 0.3, cy - r * 0.3, r * 0.32, 0, 7);
      ctx.fill();
      continue;
    }
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.beginPath();
    if (look.eye === "happy") {
      ctx.save();
      ctx.shadowColor = color;
      ctx.shadowBlur = look.glow;
      ctx.arc(cx, cy + 1.2, 2.6, Math.PI * 1.15, Math.PI * 1.85);
      ctx.stroke();
      ctx.restore();
      continue;
    }
    if (look.eye === "line") {
      ctx.moveTo(cx - 2.6, cy);
      ctx.lineTo(cx + 2.6, cy);
    } else {
      ctx.moveTo(cx - 2.2, cy - 2.2);
      ctx.lineTo(cx + 2.2, cy + 2.2);
      ctx.moveTo(cx + 2.2, cy - 2.2);
      ctx.lineTo(cx - 2.2, cy + 2.2);
    }
    ctx.stroke();
  }
}

interface BadgeGeo { x: number; y: number; r: number; orbit: number; dot: number; gap: number; dotR: number; font: number; line: number; orbitLine: number }
const SMALL_BADGE: BadgeGeo = { x: BODY / 2 - 1, y: -BODY / 2 + 1, r: 7, orbit: 10, dot: 1.8, gap: 4.4, dotR: 1.9, font: 9, line: 1.4, orbitLine: 1.2 };

function drawBadge(ctx: CanvasRenderingContext2D, look: CoreLook, color: string, t: number, reduced: boolean, pal: CorePalette, geo = SMALL_BADGE) {
  if (!look.badge) return;
  const br = geo.r;
  ctx.save();
  ctx.translate(geo.x, geo.y);
  if (look.badge === "orbit") {
    ctx.strokeStyle = color;
    ctx.globalAlpha = 0.35;
    ctx.lineWidth = geo.orbitLine;
    ctx.beginPath();
    ctx.arc(0, 0, geo.orbit, 0, 7);
    ctx.stroke();
    ctx.globalAlpha = 1;
    const a = reduced ? -Math.PI / 4 : t / 600;
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(Math.cos(a) * geo.orbit, Math.sin(a) * geo.orbit, geo.dot, 0, 7);
    ctx.fill();
  } else if (look.badge === "dots") {
    ctx.fillStyle = color;
    for (let i = 0; i < 3; i++) {
      ctx.globalAlpha = reduced || Math.floor(t / 300) % 3 >= i ? 1 : 0.3;
      ctx.beginPath();
      ctx.arc((i - 1) * geo.gap, 0, geo.dotR, 0, 7);
      ctx.fill();
    }
  } else {
    ctx.beginPath();
    ctx.arc(0, 0, br, 0, 7);
    ctx.fillStyle = pal.badgeBg;
    ctx.fill();
    ctx.lineWidth = geo.line;
    ctx.strokeStyle = color;
    ctx.stroke();
    ctx.fillStyle = color;
    ctx.font = `bold ${geo.font}px monospace`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(look.badge, 0, geo.font / 18);
  }
  ctx.restore();
}

// ---- #252: Core XL — the office's big Core (design #251 mock `drawCore`). Same CoreLook and tokens;
// drawn into the office's world space in body units (120), with pupils and blink lids.
// #770: the XL body is an organic liquid silhouette; the strip keeps its small chip.

/** XL body side in units (patron #252: 120). */
export const XL_BODY = 120;
const XL_HALF = XL_BODY / 2;
/** Cyclops: one large red eye at the liquid body's centre. */
export const XL_EYE = { x: 0, y: 0, r: 16, wide: 19, pupil: 5 };
/** Spec §3 glow per state (the small Core's are for a 34-unit body). */
const XL_GLOW: Record<CoreLook["id"], number> = { idle: 0, thinking: 14, tool: 20, delegated: 14, ask: 20, done: 18, error: 20, sleep: 0 };
const XL_BADGE: BadgeGeo = { x: XL_HALF - 4, y: -XL_HALF + 4, r: 16, orbit: 22, dot: 3.4, gap: 10, dotR: 4.4, font: 17, line: 1.8, orbitLine: 1.6 };
/** State mark colour fade; the eyes retain their red identity. */
export const XL_FADE_MS = 180;

export interface CoreXLFrame {
  /** Animation clock (ms). */
  t: number;
  /** Time in the current state (ms) — entry moves (pop, shake, hop, bounce) run off it. */
  age: number;
  /** Colour of the previous state while fading in (null: none). */
  from: string | null;
  /** Single pupil offset (body units, |v| ≤ 4). */
  pupil: { x: number; y: number };
  /** Blink lid 0 (open) → 1 (shut). */
  lid: number;
  /** Hover 0 → 1 (×1.04). */
  hover: number;
  reduced: boolean;
  /** Provider label on the top edge (world.ts's #229 label). */
  provider: { label: string; color: string } | null;
  /** Offline click wake-up progress (0 → 1). */
  boot: number;
  boardBg: string;
  edge: string;
  mono: string;
}

/** `#rgb`/`#rrggbb` lerp; anything else snaps to `b` (theme tokens are hex today). */
export function mixColor(a: string, b: string, k: number): string {
  const pa = hex(a), pb = hex(b);
  if (!pa || !pb || k >= 1) return b;
  const c = pa.map((v, i) => Math.round(v + (pb[i] - v) * Math.max(0, k)));
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}
function hex(v: string): number[] | null {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(v.trim());
  if (!m) return null;
  const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
}

/** The big Core centred at (cx, cy), body `size` world units (≤ 120). */
export function drawCoreXL(ctx: CanvasRenderingContext2D, cx: number, cy: number, size: number, look: CoreLook, f: CoreXLFrame, pal: CorePalette) {
  const target = pal.tokens[look.token] ?? pal.z;
  const fading = f.from !== null && f.age < XL_FADE_MS;
  const color = fading ? mixColor(f.from!, target, f.age / XL_FADE_MS) : target;
  const glow = XL_GLOW[look.id] * (fading ? f.age / XL_FADE_MS : 1);
  const colorEye = pal.tokens["--red-hi"] ?? color;
  const grow = f.reduced ? 1 : 1 + 0.025 * f.hover;
  ctx.save();
  ctx.translate(cx, cy);
  ctx.scale(size / XL_BODY * grow, size / XL_BODY * grow);
  if (look.id === "sleep") ctx.globalAlpha *= 0.6;
  const contour = bodyContour(look.id, f.t, f.age, f.reduced);
  ctx.beginPath();
  ctx.moveTo(contour[0].x, contour[0].y);
  for (let i = 1; i < contour.length; i++) ctx.lineTo(contour[i].x, contour[i].y);
  ctx.closePath();
  const liquid = ctx.createRadialGradient(-18, -22, 2, 0, 0, 64);
  liquid.addColorStop(0, pal.chip);
  liquid.addColorStop(0.45, pal.badgeBg);
  liquid.addColorStop(1, "#030405");
  ctx.fillStyle = liquid;
  ctx.fill();
  ctx.strokeStyle = f.boot > 0 ? colorEye : f.edge;
  ctx.lineWidth = 1.2;
  ctx.stroke();
  // Reflections stay clipped to the small body; no full-board filters or pixel reads.
  ctx.save();
  ctx.clip();
  const phase = f.reduced ? 0 : f.t / 1700;
  ctx.strokeStyle = pal.mouth;
  ctx.lineCap = "round";
  for (let i = 0; i < 3; i++) {
    const flow = Math.sin(phase + i * 2) * 7;
    ctx.globalAlpha *= i ? 0.65 : 0.2;
    ctx.lineWidth = 3 - i * 0.8;
    ctx.beginPath();
    ctx.moveTo(-39 + i * 10, 25);
    ctx.bezierCurveTo(-52 + i * 12 + flow, -20, -19 + flow, -51 + i * 7, 21 + i * 9, -30 + flow);
    ctx.stroke();
  }
  ctx.restore();
  drawEyeXL(ctx, look, colorEye, Math.max(10, glow), XL_EYE.x, f.pupil, f.lid, pal);
  // mouth: letterbox line; error = open square
  ctx.save();
  ctx.strokeStyle = pal.mouth;
  ctx.globalAlpha *= 0.55;
  ctx.lineWidth = 1.6;
  ctx.lineCap = "round";
  ctx.beginPath();
  if (look.eye === "x") ctx.rect(-4, 28, 8, 8);
  else { ctx.moveTo(-7, 32); ctx.lineTo(7, 32); }
  ctx.stroke();
  ctx.restore();
  drawMarksXL(ctx, look, color, f, pal);
  ctx.restore();
}

/** Provider label, badge and sleep z's over the liquid body. */
function drawMarksXL(ctx: CanvasRenderingContext2D, look: CoreLook, color: string, f: CoreXLFrame, pal: CorePalette) {
  const half = XL_HALF;
  const s = XL_BODY;
  if (f.provider) {
    ctx.font = `700 9px ${f.mono}`;
    const lw = Math.min(s - 40, ctx.measureText(f.provider.label).width + 12);
    ctx.beginPath();
    ctx.roundRect(-lw / 2, -half - 7, lw, 13, 4);
    ctx.fillStyle = f.boardBg; ctx.fill();
    ctx.strokeStyle = f.provider.color; ctx.lineWidth = 1; ctx.stroke();
    ctx.fillStyle = f.provider.color; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.fillText(f.provider.label, 0, -half - 0.5, lw - 8);
  }
  drawBadge(ctx, look, color, f.t, f.reduced, pal, XL_BADGE);
  if (look.badge === "z" && !f.reduced) {
    ctx.fillStyle = pal.z;
    ctx.font = "13px monospace";
    const ph = (f.t / 1600) % 1;
    const base = ctx.globalAlpha;
    for (let i = 0; i < 2; i++) {
      const q = (ph + i / 2) % 1;
      ctx.globalAlpha = base * (1 - q);
      ctx.fillText("z", half + 8 + q * 12, -half - 4 - q * 16);
    }
    ctx.globalAlpha = base;
  }
}

function drawEyeXL(ctx: CanvasRenderingContext2D, look: CoreLook, color: string, glow: number, cx: number,
  pupil: { x: number; y: number }, lid: number, pal: CorePalette) {
  const ey = XL_EYE.y;
  ctx.save();
  ctx.lineCap = "round";
  if (look.eye === "line" || look.eye === "happy" || look.eye === "x") {
    ctx.strokeStyle = color;
    ctx.lineWidth = 3;
    ctx.beginPath();
    if (look.eye === "line") { ctx.moveTo(cx - 12, ey); ctx.lineTo(cx + 12, ey); }
    else if (look.eye === "happy") { ctx.shadowColor = color; ctx.shadowBlur = glow; ctx.arc(cx, ey + 6, 12, Math.PI * 1.15, Math.PI * 1.85); }
    else { ctx.moveTo(cx - 10, ey - 10); ctx.lineTo(cx + 10, ey + 10); ctx.moveTo(cx + 10, ey - 10); ctx.lineTo(cx - 10, ey + 10); }
    ctx.stroke();
    ctx.restore();
    return;
  }
  const r = look.eye === "wide" ? XL_EYE.wide : XL_EYE.r;
  if (glow) { ctx.shadowColor = color; ctx.shadowBlur = glow; }
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.arc(cx, ey, r, 0, 7);
  ctx.fill();
  ctx.shadowBlur = 0;
  ctx.fillStyle = "#0c0e10"; // spec §2: the pupil's own dark, on every LED colour in both themes
  ctx.beginPath();
  ctx.arc(cx + pupil.x, ey + pupil.y, XL_EYE.pupil, 0, 7);
  ctx.fill();
  ctx.fillStyle = "rgba(255,255,255,.85)";
  ctx.beginPath();
  ctx.arc(cx + pupil.x - 1.2, ey + pupil.y - 1.2, 1.3, 0, 7);
  ctx.fill();
  if (lid > 0) {
    // the lid comes down from the top (spec §4: h = 2r × blink)
    ctx.fillStyle = pal.badgeBg;
    ctx.fillRect(cx - r - 1, ey - r - 1, 2 * r + 2, 2 * r * lid + 1);
  }
  ctx.restore();
}

/** PCB layer: small liquid bodies and a narrowing neck while they split/merge with Core. */
export function drawCoreFragments(ctx: CanvasRenderingContext2D, body: FragmentBody, pieces: readonly Fragment[], phase: number, pal: CorePalette, edge: string) {
  for (const piece of pieces) {
    const dx = piece.x - body.x, dy = piece.y - body.y, distance = Math.hypot(dx, dy);
    // Fully merged droplets are covered by the body; skip their draw calls.
    if (distance < body.s * 0.3) continue;
    ctx.save();
    const ux = dx / distance, uy = dy / distance, nx = -uy, ny = ux;
    const gap = (distance - body.s * 0.39) / (piece.radius * 3);
    if (gap < 1) {
      const root = { x: body.x + ux * body.s * 0.33, y: body.y + uy * body.s * 0.33 };
      const middle = { x: (root.x + piece.x) / 2, y: (root.y + piece.y) / 2 };
      const neck = piece.radius * Math.max(0.08, 1 - Math.max(0, gap));
      ctx.beginPath();
      ctx.moveTo(root.x + nx * piece.radius, root.y + ny * piece.radius);
      ctx.bezierCurveTo(middle.x + nx * neck, middle.y + ny * neck, middle.x + nx * neck, middle.y + ny * neck, piece.x + nx * piece.radius * 0.65, piece.y + ny * piece.radius * 0.65);
      ctx.lineTo(piece.x - nx * piece.radius * 0.65, piece.y - ny * piece.radius * 0.65);
      ctx.bezierCurveTo(middle.x - nx * neck, middle.y - ny * neck, middle.x - nx * neck, middle.y - ny * neck, root.x - nx * piece.radius, root.y - ny * piece.radius);
      ctx.closePath();
      ctx.fillStyle = pal.badgeBg;
      ctx.fill();
    }
    const contour = fragmentContour(piece, phase);
    ctx.beginPath();
    ctx.moveTo(contour[0].x, contour[0].y);
    for (let i = 1; i < contour.length; i++) ctx.lineTo(contour[i].x, contour[i].y);
    ctx.closePath();
    const liquid = ctx.createRadialGradient(piece.x - piece.radius * 0.3, piece.y - piece.radius * 0.4, 0, piece.x, piece.y, piece.radius * 1.5);
    liquid.addColorStop(0, pal.chip);
    liquid.addColorStop(0.4, pal.badgeBg);
    liquid.addColorStop(1, "#030405");
    ctx.fillStyle = liquid; ctx.fill();
    ctx.strokeStyle = edge; ctx.lineWidth = 0.6 * body.s / XL_BODY; ctx.stroke();
    ctx.clip();
    ctx.globalAlpha *= 0.28;
    ctx.strokeStyle = pal.mouth;
    ctx.lineWidth = 1.2 * body.s / XL_BODY;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(piece.x - piece.radius * 0.65, piece.y + piece.radius * 0.25);
    ctx.bezierCurveTo(piece.x - piece.radius, piece.y - piece.radius, piece.x, piece.y - piece.radius, piece.x + piece.radius * 0.65, piece.y - piece.radius * 0.4);
    ctx.stroke();
    ctx.restore();
  }
}
