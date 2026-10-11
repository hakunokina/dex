// #430: Kart'ta Core'un adı ve rolü plakanın üstünde — çizim sırası, çağrıları kaydeden sahte context ile.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { drawCoreName, type CoreNameSpec } from "./coreName.ts";

type Call = { op: "fill" | "fillText"; text?: string; style: unknown; alpha: number };

/** Records fill / fillText with the fill style and alpha at that moment; 7 px per character. */
function recorder() {
  const calls: Call[] = [];
  const stack: { fillStyle: unknown; globalAlpha: number }[] = [];
  const ctx = {
    fillStyle: "" as unknown, globalAlpha: 1, font: "", textAlign: "", textBaseline: "",
    save() { stack.push({ fillStyle: this.fillStyle, globalAlpha: this.globalAlpha }); },
    restore() { Object.assign(this, stack.pop()); },
    measureText: (text: string) => ({ width: [...text].length * 7 }),
    beginPath() {}, moveTo() {}, lineTo() {}, quadraticCurveTo() {}, closePath() {},
    fill() { calls.push({ op: "fill", style: this.fillStyle, alpha: this.globalAlpha }); },
    fillText(text: string) { calls.push({ op: "fillText", text, style: this.fillStyle, alpha: this.globalAlpha }); },
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls };
}

const spec = (over: Partial<CoreNameSpec> = {}): CoreNameSpec => ({
  x: 100, nameY: 50, roleY: 65, maxW: 200, name: "Ana Core", role: "LİDER", cost: "1.2k",
  plate: "#0b0f0d", nameColor: "#ff5a6e", roleColor: "#8a8f8c", font: "sans", mono: "mono", ...over,
});

describe("drawCoreName", () => {
  it("PCB hazırken plaka önce, ad (takım renginde) ve rol üstünde, hap en sonda", () => {
    const { ctx, calls } = recorder();
    const pills: string[] = [];
    drawCoreName(ctx, spec(), (text) => { pills.push(text); ctx.fillText(`pill:${text}`, 0, 0); });
    assert.deepEqual(calls.map((c) => c.text ?? c.op), ["fill", "Ana Core", "LİDER", "pill:1.2k"]);
    assert.equal(calls[0].style, "#0b0f0d");
    assert.equal(calls[0].alpha, 0.85);
    assert.equal(calls[1].style, "#ff5a6e");
    assert.equal(calls[1].alpha, 1);
    assert.deepEqual(pills, ["1.2k"]);
  });

  it("ad çizildikten sonra hiçbir dolgu yapılmaz (ad örtülmez)", () => {
    const { ctx, calls } = recorder();
    drawCoreName(ctx, spec({ cost: null }), () => {});
    const at = calls.findIndex((c) => c.text === "Ana Core");
    assert.ok(at > 0);
    assert.ok(calls.slice(at).every((c) => c.op === "fillText"));
  });

  it("PCB yokken plaka yok; ad ve rol yine çizilir", () => {
    const { ctx, calls } = recorder();
    drawCoreName(ctx, spec({ plate: null, cost: null }), () => {});
    assert.deepEqual(calls.map((c) => c.text ?? c.op), ["Ana Core", "LİDER"]);
  });
});
