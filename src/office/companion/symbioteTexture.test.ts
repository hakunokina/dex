import { test } from "node:test";
import assert from "node:assert/strict";
import { TextureMaps, textureResolution, radialUV, shadeBody, TextureCache, bakeBody } from "./symbioteTexture.ts";

test("texture maps are atomic while loading and after an asset failure", async () => {
  const maps = new TextureMaps();
  let finish!: (value: number) => void;
  const pending = new Promise<number>(resolve => { finish = resolve; });
  const loading = maps.load(128, [Promise.resolve(1), Promise.resolve(2), Promise.resolve(3), pending]);
  assert.equal(maps.get(128), null);
  finish(4);
  await loading;
  assert.deepEqual(maps.get(128), [1, 2, 3, 4]);
  const failed = new TextureMaps();
  await failed.load(512, [Promise.resolve(1), Promise.reject(new Error("missing AO")), Promise.resolve(3), Promise.resolve(4)]);
  assert.equal(failed.get(512), null);
  assert.equal(textureResolution(39, 200), null);
  assert.equal(textureResolution(40, 128), 128);
  assert.equal(textureResolution(120, 129), 512);
});

test("radial UV removes camera padding and keeps the centre and camera Y", () => {
  assert.deepEqual(radialUV(0, 0, 50, 1), { u: 0.5, v: 0.5 });
  assert.deepEqual(radialUV(25, 0, 50, 1), { u: 0.7, v: 0.5 });
  assert.deepEqual(radialUV(0, -25, 50, 1), { u: 0.5, v: 0.3 });
});

test("shading uses mapped normals, AO and mask coverage without replacing the theme", () => {
  const pixels = (...rgba: number[]) => ({ width: 1, height: 1, data: new Uint8ClampedArray(rgba) });
  const mask = pixels(128, 128, 128, 128), albedo = pixels(255, 255, 255, 255);
  const pal = { chipTop: "#333333", pin: "#777777", mouth: "#eeeeee", badgeBg: "#080808" };
  const light = pixels(76, 198, 221, 255), dark = pixels(179, 57, 221, 255);
  const fullAO = pixels(255, 255, 255, 255), occluded = pixels(0, 0, 0, 255);
  const lit = shadeBody([light, fullAO, albedo, mask], pal);
  assert.ok(lit[0] > shadeBody([dark, fullAO, albedo, mask], pal)[0]);
  assert.ok(lit[0] > shadeBody([light, occluded, albedo, mask], pal)[0]);
  assert.equal(lit[3], 128); // Coverage is applied once, not grayscale times identical alpha.
  assert.ok(shadeBody([light, fullAO, albedo, mask], { ...pal, chipTop: "#993333" })[0] > lit[0]);
  assert.throws(() => shadeBody([light, fullAO, albedo, { ...mask, width: 2 }], pal));
});

test("texture cache bakes once per resolution/palette and releases evicted surfaces", () => {
  const closed: number[] = [], cache = new TextureCache<number>(2, value => closed.push(value));
  let bakes = 0;
  const get = (key: string) => cache.get(key, () => ++bakes);
  assert.equal(get("128:dark"), 1);
  assert.equal(get("128:dark"), 1);
  assert.equal(get("512:dark"), 2);
  assert.equal(get("128:light"), 3);
  assert.deepEqual(closed, [1]);
  assert.equal(bakes, 3);
});

test("radial baking maps the padded mask to the procedural body and extends its coverage for clipping", () => {
  const size = 32, data = new Uint8ClampedArray(size * size * 4);
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = (y * size + x) * 4;
    data.set([128, 128, 255, 255], i);
  }
  const mask = { width: size, height: size, data: data.slice() };
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = (y * size + x) * 4;
    const coverage = Math.hypot(x + 0.5 - 16, y + 0.5 - 16) < 8 ? 255 : 0;
    mask.data.set([coverage, coverage, coverage, coverage], i);
  }
  const map = { width: size, height: size, data };
  const output = bakeBody([map, map, map, mask], { chipTop: "#333333", pin: "#777777", mouth: "#eeeeee", badgeBg: "#080808" });
  assert.equal(output[(16 * size + 16) * 4 + 3], 255);
  assert.equal(output[(16 * size + 28) * 4 + 3], 255); // Source padding must not shrink the visible body.
});
