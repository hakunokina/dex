/** Keep data/shading independent of Vite asset imports and the browser. */
import { bodyLighting, bodyContour } from "../symbiote.ts";
import type { CorePalette } from "./coreEngine.ts";

export interface MapPixels { width: number; height: number; data: Uint8ClampedArray }
export type BodyColors = Pick<CorePalette, "chipTop" | "pin" | "mouth" | "badgeBg">;

export function textureResolution(cssBody: number, physicalBody: number): 128 | 512 | null {
  return cssBody < 40 ? null : physicalBody > 128 ? 512 : 128;
}

/** Canvas point → camera model → padded orthographic UV; source camera +Y points up. */
export function radialUV(x: number, y: number, contourRadius: number, modelRadius: number) {
  const radius = Math.hypot(x, y);
  const scale = radius ? modelRadius / contourRadius : 0;
  return { u: 0.5 + x * scale / 2.5, v: 0.5 + y * scale / 2.5 };
}

function linear(value: number) {
  const s = value / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}
function srgb(value: number) {
  const v = Math.max(0, Math.min(1, value));
  return 255 * (v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055);
}
function color(hex: string) {
  const short = /^#[0-9a-f]{3}$/i.test(hex);
  const h = short ? [...hex.slice(1)].map(v => v + v).join("") : hex.slice(1);
  if (!/^[0-9a-f]{6}$/i.test(h)) throw new Error("Unsupported body color");
  return [0, 2, 4].map(i => linear(parseInt(h.slice(i, i + 2), 16)));
}

/** All pixel work happens at bake time. Normal/AO/mask bytes never receive gamma conversion. */
export function shadeBody(maps: readonly MapPixels[], pal: BodyColors) {
  if (maps.length !== 4 || maps.some(m => m.width !== maps[0].width || m.height !== maps[0].height || m.data.length !== m.width * m.height * 4))
    throw new Error("Mismatched body maps");
  const [normal, ao, albedo, mask] = maps;
  const base = color(pal.chipTop), pin = color(pal.pin), rim = color(pal.mouth), floor = color(pal.badgeBg);
  const output = new Uint8ClampedArray(normal.data.length);
  for (let i = 0; i < output.length; i += 4) {
    const light = bodyLighting(0, 0, { x: normal.data[i] / 127.5 - 1, y: normal.data[i + 1] / 127.5 - 1, z: normal.data[i + 2] / 127.5 - 1 });
    const ambient = 0.34 + 0.86 * light.diffuse, occlusion = 0.35 + 0.65 * ao.data[i] / 255;
    for (let c = 0; c < 3; c++) {
      const tint = base[c] * (0.88 + 0.12 * linear(albedo.data[i + c]));
      output[i + c] = srgb((tint * ambient + floor[c] * 0.1 + pin[c] * light.specular * 0.65 + rim[c] * light.rim * 0.12) * occlusion);
    }
    output[i + 3] = mask.data[i];
  }
  return output;
}

export const TEXTURE_HALF = 60;
const reference = bodyContour("idle", 0, 5000, true);
export const REFERENCE_EXTENT = {
  x: Math.max(...reference.map(p => Math.abs(p.x))),
  y: Math.max(...reference.map(p => Math.abs(p.y))),
};

/** Bake radial UV into the reference body once. Coverage outside it is edge-extended;
 * the live procedural clip owns the silhouette even when its lobes change. */
export function bakeBody(maps: readonly MapPixels[], pal: BodyColors) {
  const shaded = shadeBody(maps, pal), mask = maps[3], size = mask.width;
  if (mask.height !== size) throw new Error("Body maps must be square");
  const radii = reference.map((_, n) => {
    const a = n / reference.length * Math.PI * 2, dx = Math.cos(a), dy = Math.sin(a);
    let radius = 0;
    for (let r = 0; r < size / 2; r += 0.5) {
      const x = Math.min(size - 1, Math.max(0, Math.floor(size / 2 + dx * r)));
      const y = Math.min(size - 1, Math.max(0, Math.floor(size / 2 + dy * r)));
      if (mask.data[(y * size + x) * 4] >= 250) radius = r;
    }
    return Math.max(0, radius - 0.5) / size * 2.5;
  });
  if (radii.some(r => r === 0)) throw new Error("Empty body mask");
  const output = new Uint8ClampedArray(shaded.length);
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const px = ((x + 0.5) / size * 2 - 1) * TEXTURE_HALF;
    const py = ((y + 0.5) / size * 2 - 1) * TEXTURE_HALF;
    const angle = (Math.atan2(py, px) / (Math.PI * 2) + 1) % 1 * reference.length;
    const a = Math.floor(angle), b = (a + 1) % reference.length, k = angle - a;
    const refRadius = Math.hypot(reference[a].x, reference[a].y) * (1 - k) + Math.hypot(reference[b].x, reference[b].y) * k;
    const modelRadius = radii[a] * (1 - k) + radii[b] * k;
    const radius = Math.hypot(px, py), edge = Math.min(1, refRadius * 0.99 / radius);
    const uv = radialUV(px * edge, py * edge, refRadius, modelRadius);
    const sx = Math.max(0, Math.min(size - 1, Math.floor(uv.u * size))), sy = Math.max(0, Math.min(size - 1, Math.floor(uv.v * size)));
    const from = (sy * size + sx) * 4, to = (y * size + x) * 4;
    output.set(shaded.subarray(from, from + 4), to);
  }
  return output;
}

/** Bounded shared cache; card resize/animation does not create unbounded bitmap variants. */
export class TextureCache<T> {
  private readonly values = new Map<string, T>();
  private readonly limit: number;
  private readonly release: (value: T) => void;
  constructor(limit: number, release: (value: T) => void) { this.limit = limit; this.release = release; }
  get(key: string, bake: () => T): T {
    const cached = this.values.get(key);
    if (cached !== undefined) return cached;
    const value = bake();
    if (this.values.size >= this.limit) {
      const oldest = this.values.keys().next().value!;
      this.release(this.values.get(oldest)!); this.values.delete(oldest);
    }
    this.values.set(key, value);
    return value;
  }
}

/** Publish a resolution only when all four maps succeeded. Failed loads stay procedural. */
export class TextureMaps<T> {
  private readonly ready = new Map<number, readonly T[]>();
  private readonly pending = new Map<number, Promise<void>>();
  get(size: number): readonly T[] | null { return this.ready.get(size) ?? null; }
  load(size: number, maps: readonly Promise<T>[]): Promise<void> {
    let pending = this.pending.get(size);
    if (!pending) {
      pending = Promise.all(maps).then(values => { if (values.length === 4) this.ready.set(size, values); }, () => {});
      this.pending.set(size, pending);
    }
    return pending;
  }
}
