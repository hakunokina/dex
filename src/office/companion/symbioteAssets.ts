import normal128 from "./assets/symbiote/normal-128.png?url";
import ao128 from "./assets/symbiote/ao-128.png?url";
import albedo128 from "./assets/symbiote/albedo-128.png?url";
import mask128 from "./assets/symbiote/mask-128.png?url";
import normal512 from "./assets/symbiote/normal-512.png?url";
import ao512 from "./assets/symbiote/ao-512.png?url";
import albedo512 from "./assets/symbiote/albedo-512.png?url";
import mask512 from "./assets/symbiote/mask-512.png?url";
import { BODY_LIGHT } from "../symbiote.ts";
import { bakeBody, TextureMaps, TextureCache, type MapPixels, type BodyColors } from "./symbioteTexture.ts";

const urls = { 128: [normal128, ao128, albedo128, mask128], 512: [normal512, ao512, albedo512, mask512] };
const maps = new TextureMaps<MapPixels>();
const started = new Set<number>();
const failed = new Set<string>();
const cache = new TextureCache<{ bitmap: ImageBitmap | null; cancelled: boolean }>(8, entry => {
  entry.cancelled = true;
  entry.bitmap?.close();
});

async function readMap(url: string): Promise<MapPixels> {
  const image = new Image();
  image.src = url;
  await image.decode();
  const surface = new OffscreenCanvas(image.naturalWidth, image.naturalHeight);
  const ctx = surface.getContext("2d", { willReadFrequently: true })!;
  ctx.drawImage(image, 0, 0);
  return ctx.getImageData(0, 0, surface.width, surface.height);
}

/** Non-blocking lookup. Loading, mismatched data or raster failure all keep the analytic body. */
export function bodyTexture(size: 128 | 512, pal: BodyColors): ImageBitmap | null {
  if (!started.has(size)) {
    started.add(size);
    void maps.load(size, urls[size].map(readMap));
  }
  const ready = maps.get(size);
  if (!ready) return null;
  const key = [size, pal.chipTop, pal.pin, pal.mouth, pal.badgeBg, BODY_LIGHT.x, BODY_LIGHT.y, BODY_LIGHT.z].join("|");
  if (failed.has(key)) return null;
  const entry = cache.get(key, () => {
    const pending: { bitmap: ImageBitmap | null; cancelled: boolean } = { bitmap: null, cancelled: false };
    // Bake outside the requesting frame, once per resolution/palette/light combination.
    void Promise.resolve().then(() => {
      if (pending.cancelled) return;
      const data = bakeBody(ready, pal);
      const surface = new OffscreenCanvas(size, size);
      const ctx = surface.getContext("2d", { willReadFrequently: true })!;
      ctx.putImageData(new ImageData(data, size, size), 0, 0);
      pending.bitmap = surface.transferToImageBitmap();
    }).catch(() => { failed.add(key); });
    return pending;
  });
  return entry.bitmap;
}
