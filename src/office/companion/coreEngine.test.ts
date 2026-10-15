import assert from "node:assert/strict";
import { test } from "node:test";
import { drawCoreXL, drawCoreFeet, XL_EYE, type CorePalette, type CoreXLFrame } from "./coreEngine.ts";
import { LOOKS } from "./coreState.ts";
import { createFootState, fragmentPaths, plantedFeet } from "../symbiote.ts";
import { CoreXL, FOOT_RESET_GAP_MS } from "./coreXL.ts";

const generatedStamps: { width: number; height: number; stops: [number, string][] }[] = [];
const originalOffscreenCanvas = globalThis.OffscreenCanvas;
globalThis.OffscreenCanvas = class {
  width: number; height: number;
  constructor(w: number, h: number) { this.width = w; this.height = h; }
  getContext() {
    const stops: [number, string][] = [];
    generatedStamps.push({ width: this.width, height: this.height, stops });
    return { translate() {}, scale() {}, beginPath() {}, arc() {}, fill() {},
      createRadialGradient: () => ({ addColorStop: (at: number, color: string) => stops.push([at, color]) }) };
  }
  transferToImageBitmap() { return { width: this.width, height: this.height }; }
} as unknown as typeof OffscreenCanvas;
test.after(() => { globalThis.OffscreenCanvas = originalOffscreenCanvas; });

const palette: CorePalette = { tokens: { "--red-hi": "#ff0000" }, chip: "#111111", chipTop: "#222222", pin: "#777777", via: "#555555", badgeBg: "#080808", mouth: "#eeeeee", z: "#666666" };
const frame: CoreXLFrame = { t: 0, age: 5000, from: null, pupil: { x: 4, y: 0 }, lid: 0, hover: 0, reduced: false, provider: null, boot: 0, boardBg: "#111111", edge: "#333333", mono: "monospace" };

function record(id: keyof typeof LOOKS, overrides: Partial<CoreXLFrame> = {}, palettes = [palette], texture: ImageBitmap | null = null, size = 120) {
  type Op = { name: string; args: number[] };
  const fills: { color: unknown; path: Op[] }[] = [], strokes: { color: unknown; path: Op[] }[] = [];
  const rects: number[][] = [];
  const images: { surface: unknown; args: number[]; beforeFills: number }[] = [];
  const scales: number[][] = [];
  const gradients: { kind: string; args: number[]; stops: [number, string][] }[] = [];
  const gradient = (kind: string, args: number[]) => {
    const stops: [number, string][] = [];
    const value = { kind, args, stops, addColorStop: (at: number, color: string) => stops.push([at, color]) };
    gradients.push(value);
    return value;
  };
  let path: Op[] = [];
  const ctx = {
    globalAlpha: 1, fillStyle: "", strokeStyle: "",
    save() {}, restore() {}, translate() {}, clip() {}, closePath() {}, fillText() {},
    scale: (...args: number[]) => scales.push(args),
    drawImage: (surface: unknown, ...args: number[]) => images.push({ surface, args, beforeFills: fills.length }),
    createRadialGradient: (...args: number[]) => gradient("radial", args),
    createLinearGradient: (...args: number[]) => gradient("linear", args),
    beginPath: () => { path = []; },
    moveTo: (...args: number[]) => path.push({ name: "move", args }),
    lineTo: (...args: number[]) => path.push({ name: "line", args }),
    arc: (...args: number[]) => path.push({ name: "arc", args }),
    ellipse: (...args: number[]) => path.push({ name: "ellipse", args }),
    rect: (...args: number[]) => path.push({ name: "rect", args }),
    bezierCurveTo: (...args: number[]) => path.push({ name: "curve", args }),
    fillRect: (...args: number[]) => rects.push(args),
    fill() { fills.push({ color: this.fillStyle, path: [...path] }); },
    stroke() { strokes.push({ color: this.strokeStyle, path: [...path] }); },
  };
  for (const pal of palettes) drawCoreXL(ctx as unknown as CanvasRenderingContext2D, 100, 100, size, LOOKS[id], { ...frame, ...overrides }, pal, texture);
  return { ctx: ctx as unknown as CanvasRenderingContext2D, fills, strokes, rects, scales, gradients, images };
}

test("XL texture is clipped below unchanged eyes/marks and stays with reduced motion; tiny bodies fall back", () => {
  const texture = { width: 128, height: 128 } as ImageBitmap;
  for (const reduced of [false, true]) {
    const drawn = record("idle", { reduced }, [palette], texture);
    assert.equal(drawn.images.filter(image => image.surface === texture).length, 1);
    assert.equal(drawn.images[1].beforeFills, 0);
    assert.ok(drawn.fills.some(fill => fill.color === "#ff0000"));
    assert.ok(drawn.strokes.some(stroke => stroke.color === palette.mouth));
  }
  assert.equal(record("idle", {}, [palette], texture, 39).images.length, 1);
  assert.ok(record("idle").fills.some(fill => typeof fill.color === "object"));
});

test("all eight XL expressions draw one centred red eye and preserve the mouth", () => {
  for (const id of Object.keys(LOOKS) as (keyof typeof LOOKS)[]) {
    const { fills, strokes } = record(id);
    const eyeFills = fills.filter(p => p.color === "#ff0000");
    // The error badge also uses red; its circular rim is separate from the eye expression.
    const eyeStrokes = strokes.filter(p => p.color === "#ff0000" && (p.path[0].name !== "arc" || p.path[0].args[2] === 12));
    if (["done", "sleep", "error"].includes(id)) {
      assert.equal(eyeFills.length, 0, id);
      assert.equal(eyeStrokes.length, 1, id);
      const path = eyeStrokes[0].path;
      if (id === "done") assert.deepEqual(path[0].args.slice(0, 3), [0, 6, 12]);
      if (id === "sleep") assert.deepEqual(path.map(p => p.args), [[-12, 0], [12, 0]]);
      if (id === "error") assert.deepEqual(path.map(p => p.args), [[-10, -10], [10, 10], [10, -10], [-10, 10]]);
    } else {
      assert.equal(eyeFills.length, 1, id);
      assert.deepEqual(eyeFills[0].path[0].args.slice(0, 3), [0, 0, id === "ask" ? XL_EYE.wide : XL_EYE.r]);
      const pupil = fills.find(p => p.color === "#0c0e10")!;
      assert.deepEqual(pupil.path[0].args.slice(0, 3), [4, 0, XL_EYE.pupil]);
    }
    assert.ok(strokes.some(p => p.color === palette.mouth && (id === "error" ? p.path[0].name === "rect" : p.path.some(o => o.name === "move" && o.args[1] === 32))), id);
  }
});

test("#2036: the frame's eye colour paints only the eye; mouth, rim and boot rim keep their colours", () => {
  for (const id of ["idle", "ask", "done"] as const) {
    const { fills, strokes } = record(id, { eye: "#d2b6ff" });
    assert.equal(fills.filter(p => p.color === "#ff0000").length, 0, id);
    const eye = id === "done" ? strokes.filter(p => p.color === "#d2b6ff") : fills.filter(p => p.color === "#d2b6ff");
    assert.equal(eye.length, 1, id);
    assert.ok(strokes.some(p => p.color === palette.mouth), id);
  }
  const boot = record("idle", { eye: "#d2b6ff", boot: 0.5 });
  assert.ok(boot.strokes.some(p => p.color === "#ff0000"));
  // Omitted: the eye stays --red-hi (Ana Takım unchanged).
  assert.equal(record("idle").fills.filter(p => p.color === "#ff0000").length, 1);
});

test("contact shadow precedes the body and fixed highlight/rim remain with reduced motion", () => {
  for (const reduced of [false, true]) {
    const { fills, strokes, gradients, images } = record("idle", { reduced });
    assert.deepEqual(images[0].args, [-53, 23, 112, 32]);
    assert.equal(images[0].beforeFills, 0);
    assert.deepEqual(generatedStamps[0].stops, [[0, "rgba(0,0,0,.65)"], [0.45, "rgba(0,0,0,.32)"], [1, "rgba(0,0,0,0)"]]);
    const liquid = gradients.find(g => g.kind === "radial")!;
    assert.ok(liquid.args[0] < 0 && liquid.args[1] < 0);
    assert.deepEqual(liquid.stops[0], [0, palette.pin]);
    assert.ok(fills.some(p => p.color === liquid));
    const rim = gradients.find(g => g.kind === "linear")!;
    assert.deepEqual(rim.args, [-58, -58, 58, 58]);
    assert.ok(strokes.some(p => p.color === rim));
  }
});

test("static contact/light gradients are reused and palette changes refresh the light", () => {
  const same = record("idle", {}, [palette, palette]);
  assert.equal(generatedStamps.filter(s => s.height === 48).length, 1);
  assert.equal(same.images[0].surface, same.images[1].surface);
  assert.equal(same.gradients.filter(g => g.kind === "radial").length, 1);
  assert.equal(same.gradients.filter(g => g.kind === "linear").length, 1);
  const changed = record("idle", {}, [palette, { ...palette, mouth: "#ffffff" }]).gradients;
  assert.equal(changed.filter(g => g.kind === "linear").length, 2);
  assert.ok(changed.some(g => g.kind === "linear" && g.stops[0][1] === "#ffffff"));
});

test("four support feet share two ribbon strokes and retain all planted contacts", () => {
  const body = { x: 160, y: 160, s: 120 };
  const paths = fragmentPaths(body, [], [], { x: 0, y: 0, w: 320, h: 320 });
  const feet = plantedFeet(paths, body, 0, true, createFootState());
  assert.equal(feet.length, 4);
  const { ctx, strokes, fills, images } = record("idle", {}, []);
  drawCoreFeet(ctx, body, feet, palette, "#333333");
  assert.equal(strokes.length, 2);
  assert.equal(strokes[0].path.filter(p => p.name === "move").length, 4);
  assert.equal(images.length, 4);
  assert.equal(fills.length, 1);
  assert.equal(fills[0].path.filter(p => p.name === "ellipse").length, 4);
});

test("one blink lid covers the enlarged eye; hover scales the shared body/eye space", () => {
  const { rects, scales } = record("idle", { lid: 1, hover: 1 });
  assert.deepEqual(rects, [[-17, -17, 34, 33]]);
  assert.deepEqual(scales, [[1.025, 1.025]]);
  assert.deepEqual(record("idle", { hover: 1, reduced: true }).scales, [[1, 1]]);
});

test("Core foot state is reused but resets on a new canvas, visibility gap and motion preference", () => {
  const body = { x: 160, y: 160, s: 120 }, moved = { ...body, x: 170 };
  const board = { x: 0, y: 0, w: 400, h: 400 };
  const paths = fragmentPaths(body, [], [], board), movedPaths = fragmentPaths(moved, [], [], board);
  const core = new CoreXL(), surface = {}, replacement = {};
  const feet = core.sampleFeet(paths, body, 0, false, surface, 0);
  const slot = feet[0], x = slot.x, y = slot.y;
  assert.equal(core.sampleFeet(movedPaths, moved, 100, false, surface, 100), feet);
  assert.equal(feet[0], slot);
  assert.deepEqual({ x: slot.x, y: slot.y }, { x, y });
  core.sampleFeet(movedPaths, moved, 100, false, replacement, 101);
  assert.notEqual(slot.x, x);
  core.sampleFeet(paths, body, 100, false, replacement, 2000);
  assert.equal(slot.x, x);
  core.sampleFeet(movedPaths, moved, 200, true, replacement, 2010);
  assert.equal(slot.lift, 0);
  const reducedX = slot.x;
  core.sampleFeet(movedPaths, moved, 9000, true, replacement, 2020);
  assert.equal(slot.x, reducedX);
  core.sampleFeet(paths, body, 300, false, replacement, 2030);
  assert.equal(slot.x, x);
  const fresh = new CoreXL().sampleFeet(movedPaths, moved, 300, false, surface, 2030);
  assert.notEqual(fresh[0], slot);
});

test("500ms gaps preserve stance or early stepping; longer gaps replant on the first frame", () => {
  const body = { x: 160, y: 160, s: 120 }, moved = { ...body, x: 230 };
  const board = { x: 0, y: 0, w: 480, h: 320 }, surface = {};
  const paths = fragmentPaths(body, [], [], board), movedPaths = fragmentPaths(moved, [], [], board);
  const core = new CoreXL();
  const foot = core.sampleFeet(paths, body, 0, false, surface, 0)[0];
  const contact = { x: foot.x, y: foot.y };
  core.sampleFeet(paths, body, 100, false, surface, FOOT_RESET_GAP_MS);
  assert.deepEqual({ x: foot.x, y: foot.y }, contact);
  core.sampleFeet(movedPaths, moved, 110, false, surface, FOOT_RESET_GAP_MS + 10);
  assert.deepEqual({ x: foot.x, y: foot.y }, contact);
  core.sampleFeet(movedPaths, moved, 260, false, surface, FOOT_RESET_GAP_MS + 160);
  assert.ok(foot.lift > 0);
  core.sampleFeet(movedPaths, moved, 410, false, surface, FOOT_RESET_GAP_MS + 310);
  assert.equal(foot.lift, 0);
  assert.notDeepEqual({ x: foot.x, y: foot.y }, contact);
  core.sampleFeet(paths, body, 410, false, surface, 2 * FOOT_RESET_GAP_MS + 311);
  assert.deepEqual({ x: foot.x, y: foot.y }, contact);
  assert.equal(foot.lift, 0);
});
