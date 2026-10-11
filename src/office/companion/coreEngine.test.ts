import assert from "node:assert/strict";
import { test } from "node:test";
import { drawCoreXL, XL_EYE, type CorePalette, type CoreXLFrame } from "./coreEngine.ts";
import { LOOKS } from "./coreState.ts";

const palette: CorePalette = { tokens: { "--red-hi": "#ff0000" }, chip: "#111111", chipTop: "#222222", pin: "#777777", via: "#555555", badgeBg: "#080808", mouth: "#eeeeee", z: "#666666" };
const frame: CoreXLFrame = { t: 0, age: 5000, from: null, pupil: { x: 4, y: 0 }, lid: 0, hover: 0, reduced: false, provider: null, boot: 0, boardBg: "#111111", edge: "#333333", mono: "monospace" };

function record(id: keyof typeof LOOKS, overrides: Partial<CoreXLFrame> = {}) {
  type Op = { name: string; args: number[] };
  const fills: { color: unknown; path: Op[] }[] = [], strokes: { color: unknown; path: Op[] }[] = [];
  const rects: number[][] = [];
  const scales: number[][] = [];
  let path: Op[] = [];
  const ctx = {
    globalAlpha: 1, fillStyle: "", strokeStyle: "",
    save() {}, restore() {}, translate() {}, clip() {}, closePath() {}, fillText() {},
    scale: (...args: number[]) => scales.push(args),
    createRadialGradient: () => ({ addColorStop() {} }),
    beginPath: () => { path = []; },
    moveTo: (...args: number[]) => path.push({ name: "move", args }),
    lineTo: (...args: number[]) => path.push({ name: "line", args }),
    arc: (...args: number[]) => path.push({ name: "arc", args }),
    rect: (...args: number[]) => path.push({ name: "rect", args }),
    bezierCurveTo: (...args: number[]) => path.push({ name: "curve", args }),
    fillRect: (...args: number[]) => rects.push(args),
    fill() { fills.push({ color: this.fillStyle, path: [...path] }); },
    stroke() { strokes.push({ color: this.strokeStyle, path: [...path] }); },
  };
  drawCoreXL(ctx as unknown as CanvasRenderingContext2D, 100, 100, 120, LOOKS[id], { ...frame, ...overrides }, palette);
  return { fills, strokes, rects, scales };
}

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

test("one blink lid covers the enlarged eye; hover scales the shared body/eye space", () => {
  const { rects, scales } = record("idle", { lid: 1, hover: 1 });
  assert.deepEqual(rects, [[-17, -17, 34, 33]]);
  assert.deepEqual(scales, [[1.025, 1.025]]);
  assert.deepEqual(record("idle", { hover: 1, reduced: true }).scales, [[1, 1]]);
});
