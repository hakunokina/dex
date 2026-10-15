// #2051: Kart kare döngüsü — tek hata döngüyü öldürmez; boşta kalan zincir ekrandaysa yeniden başlar.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { onceLog, revive, type LoopState } from "./frameLoop.ts";

const idle: LoopState = { stopped: false, pending: false, active: true, hidden: false, inView: true };

describe("revive", () => {
  it("ekrandaki Kart'ta kare istenmiyorsa zincir ölmüştür: yeniden planla", () => {
    assert.equal(revive(idle), "schedule");
  });
  it("gözlemciye göre görünmüyorsa yeniden sorar (kaçan IO değişimi)", () => {
    assert.equal(revive({ ...idle, inView: false }), "observe");
  });
  it("kare bekliyorsa, durdurulmuşsa, etkin değilse ya da sayfa gizliyse dokunmaz", () => {
    assert.equal(revive({ ...idle, pending: true }), "none");
    assert.equal(revive({ ...idle, stopped: true }), "none");
    assert.equal(revive({ ...idle, active: false }), "none");
    assert.equal(revive({ ...idle, hidden: true, inView: false }), "none");
  });
});

describe("onceLog", () => {
  it("aynı hatayı bir kez, farklısını ayrıca loglar", () => {
    const lines: unknown[][] = [];
    const log = onceLog((...args) => lines.push(args));
    const boom = new TypeError("x is undefined");
    for (let i = 0; i < 60; i++) log("kare çizilemedi", boom);
    log("kare çizilemedi", new TypeError("x is undefined"));
    log("kare çizilemedi", new RangeError("bad size"));
    log("döngü durmuştu");
    log("döngü durmuştu");
    assert.deepEqual(lines.map((l) => l[0]), ["[Kart] kare çizilemedi", "[Kart] kare çizilemedi", "[Kart] döngü durmuştu"]);
    assert.equal(lines[0][1], boom);
    assert.equal(lines[2].length, 1);
  });
});
