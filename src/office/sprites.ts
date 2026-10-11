// #401: the office's generated artwork (.dex/design/office-assets) — pure geometry and state mapping.
// world.ts draws with it once assets/index.ts has decoded the images; node:test covers it.
import type { Eye } from "./companion/coreState.ts";

export type CoreSprite = "idle" | "working" | "error" | "sleeping";

/** By the look's eye (coreState LOOKS): the four drawn expressions; ask / done / delegated keep their badge on top. */
const BY_EYE: Record<Eye, CoreSprite> = { dot: "idle", wide: "idle", happy: "idle", scan: "working", down: "working", x: "error", line: "sleeping" };
export const coreSpriteOf = (eye: Eye): CoreSprite => BY_EYE[eye];

/** The expression swap's cross-fade (handoff: no motion beyond it); reduced motion swaps at once. */
export const SPRITE_FADE_MS = 200;
export function fadeIn(age: number, reduced: boolean): number {
  if (reduced || !(age >= 0)) return 1;
  return Math.min(1, age / SPRITE_FADE_MS);
}

/**
 * Core: the four images share one square crop (union of their silhouettes), so expressions do not jump.
 * The square is the procedural body + pins (`s + 2·pin`): the artwork never leaves today's silhouette.
 */
export function coreSpriteRect(body: { x: number; y: number; s: number }, pin: number) {
  const side = body.s + 2 * pin;
  return { x: body.x - side / 2, y: body.y - side / 2, w: side, h: side };
}

export type Rect = { x: number; y: number; w: number; h: number };
export type Slice = { src: Rect; dst: Rect };

/** worker.webp (512×400, cropped to its silhouette): the body inside the side / top pins, in image px. */
export const WORKER_IMG = { w: 512, h: 400, body: { l: 36, t: 20, r: 474, b: 362 }, cap: 106 } as const;

/**
 * Lider #401 (A): 3-slice — the body fills the chip box, pins stay outside it; the caps (side pins, corners)
 * keep their aspect, the middle band stretches across. Null when the caps do not fit (procedural chip).
 */
export function workerSlices(box: Rect, img = WORKER_IMG): Slice[] | null {
  const { body, cap } = img;
  const k = box.h / (body.b - body.t);
  if (!(k > 0)) return null;
  const top = box.y - body.t * k;
  const height = img.h * k;
  const left = box.x - body.l * k;
  const right = box.x + box.w + (img.w - body.r) * k;
  const capW = cap * k;
  const mid = right - left - 2 * capW;
  if (mid <= 0) return null;
  return [
    { src: { x: 0, y: 0, w: cap, h: img.h }, dst: { x: left, y: top, w: capW, h: height } },
    { src: { x: cap, y: 0, w: img.w - 2 * cap, h: img.h }, dst: { x: left + capW, y: top, w: mid, h: height } },
    { src: { x: img.w - cap, y: 0, w: cap, h: img.h }, dst: { x: right - capW, y: top, w: capW, h: height } },
  ];
}

/** PCB tile on screen (CSS px): 512 px image at DPR 2. */
export const PCB_TILE_PX = 256;
