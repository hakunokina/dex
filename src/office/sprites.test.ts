// #401: ofis görselleri — Core ifade eşlemesi, solma, ortak kırpma karesi, worker 3-slice geometrisi.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { LOOKS } from "./companion/coreState.ts";
import { coreSpriteOf, coreSpriteRect, fadeIn, SPRITE_FADE_MS, WORKER_IMG, workerSlices } from "./sprites.ts";

const near = (a: number, b: number, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} ≉ ${b}`);

describe("coreSpriteOf", () => {
  it("dört ifade: boşta, çalışıyor (araç dahil), hata, uyuyor", () => {
    const by = Object.fromEntries(Object.values(LOOKS).map((look) => [look.id, coreSpriteOf(look.eye)]));
    assert.deepEqual(by, {
      idle: "idle", thinking: "working", tool: "working", delegated: "idle",
      ask: "idle", done: "idle", error: "error", sleep: "sleeping",
    });
  });
  it("görsel dışı hâllerde rozet kalır (ask ?, done ✓, delegated orbit)", () => {
    assert.equal(LOOKS.ask.badge, "?");
    assert.equal(LOOKS.done.badge, "✓");
    assert.equal(LOOKS.delegated.badge, "orbit");
  });
});

describe("fadeIn", () => {
  it("geçişte 0 → 1, sonra 1", () => {
    assert.equal(fadeIn(0, false), 0);
    near(fadeIn(SPRITE_FADE_MS / 2, false), 0.5);
    assert.equal(fadeIn(SPRITE_FADE_MS * 3, false), 1);
  });
  it("reduced motion ve ilk kare (since = -∞) anında", () => {
    assert.equal(fadeIn(0, true), 1);
    assert.equal(fadeIn(Infinity, false), 1);
    assert.equal(fadeIn(NaN, false), 1);
  });
});

describe("coreSpriteRect", () => {
  it("gövde + pin karesi, gövde merkezinde", () => {
    assert.deepEqual(coreSpriteRect({ x: 100, y: 50, s: 120 }, 6), { x: 34, y: -16, w: 132, h: 132 });
  });
});

describe("workerSlices", () => {
  const box = { x: 10, y: 20, w: 98, h: 48 };
  const { body, cap } = WORKER_IMG;
  const k = box.h / (body.b - body.t);

  it("gövde çip kutusuna oturur, pinler dışarıda", () => {
    const [l, m, r] = workerSlices(box)!;
    near(l.dst.x, box.x - body.l * k);
    near(r.dst.x + r.dst.w, box.x + box.w + (WORKER_IMG.w - body.r) * k);
    near(l.dst.y, box.y - body.t * k);
    near(l.dst.y + l.dst.h, box.y + box.h + (WORKER_IMG.h - body.b) * k);
    assert.ok(l.dst.x < box.x && r.dst.x + r.dst.w > box.x + box.w);
    // dikişsiz: kapak | orta | kapak
    near(l.dst.x + l.dst.w, m.dst.x);
    near(m.dst.x + m.dst.w, r.dst.x);
  });

  it("kapaklar oranını korur, yalnız orta bant gerilir", () => {
    const [l, m, r] = workerSlices(box)!;
    near(l.dst.w / l.src.w, k);
    near(r.dst.w / r.src.w, k);
    near(l.dst.h / l.src.h, k);
    assert.ok(m.dst.w / m.src.w > k);
    assert.deepEqual([l.src.x, l.src.w, m.src.x, m.src.w, r.src.x, r.src.w], [0, cap, cap, WORKER_IMG.w - 2 * cap, WORKER_IMG.w - cap, cap]);
  });

  it("kapaklar sığmıyorsa null (prosedürel çip)", () => {
    assert.equal(workerSlices({ x: 0, y: 0, w: 10, h: 48 }), null);
    assert.equal(workerSlices({ x: 0, y: 0, w: 98, h: 0 }), null);
  });

  it("en küçük ayrıntılı çipte (52×28) de çalışır", () => {
    assert.ok(workerSlices({ x: 0, y: 0, w: 52, h: 28 }));
  });
});
