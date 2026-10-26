// #2443: kapsam seçici — uzmanlık aileleri, seçim dikdörtgeni, başlık özeti.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { chipsInRect, familiesOf, isDrag, rectFrom, scopeIds, scopeTitle } from "./workerPick.ts";

const w = (id: string, name: string, roleSlug: string | null, roleName: string | null) => ({ id, name, roleSlug, roleName });
const crew = [
  w("a", "Pixel-1", "frontend", "Frontend"),
  w("b", "Kernel-1", "backend", "Backend"),
  w("c", "Kernel-2", "backend", "Backend"),
  w("d", "Lens-1", "code-review", "Kod inceleme"),
  w("e", "Bot-1", "code-automation", "Kod otomasyonu"),
  w("f", "Bot-2", "n8n-automation", "n8n otomasyonu"),
  w("g", "Ayşe", null, null),
];

describe("rectFrom / isDrag", () => {
  it("köşe sırası fark etmez", () => {
    assert.deepEqual(rectFrom({ x: 10, y: 40 }, { x: 4, y: 20 }), { x: 4, y: 20, w: 6, h: 20 });
  });
  it("4 px altı tıklamadır", () => {
    assert.equal(isDrag({ x: 0, y: 0 }, { x: 3, y: 3 }), false);
    assert.equal(isDrag({ x: 0, y: 0 }, { x: 0, y: 4 }), true);
  });
});

describe("chipsInRect", () => {
  const chips = [{ id: 1, x: 10, y: 10, w: 4, h: 4 }, { id: 2, x: 30, y: 10, w: 4, h: 4 }, { id: 3, x: 10, y: 30, w: 4, h: 4 }];
  it("dikdörtgene değen çipler seçilir", () => {
    assert.deepEqual(chipsInRect(chips, { x: 11, y: 0, w: 20, h: 9 }).map((c) => c.id), [1, 2]);
  });
  it("değmeyen dışarıda kalır", () => {
    assert.deepEqual(chipsInRect(chips, { x: 13, y: 13, w: 14, h: 14 }).map((c) => c.id), []);
  });
  it("kenar teması sayılır", () => {
    assert.deepEqual(chipsInRect(chips, { x: 12, y: 28, w: 0, h: 0 }).map((c) => c.id), [3]);
  });
});

describe("familiesOf", () => {
  const fams = familiesOf(crew);
  it("rol başına bir aile, ad kökü etiket, rol adı ipucu", () => {
    assert.deepEqual(fams.slice(0, 3).map((f) => [f.key, f.label, f.hint, f.ids]), [
      ["frontend", "Pixel", "Frontend", ["a"]],
      ["backend", "Kernel", "Backend", ["b", "c"]],
      ["code-review", "Lens", "Kod inceleme", ["d"]],
    ]);
  });
  it("aynı kökü taşıyan iki rol rol adıyla ayrılır", () => {
    assert.deepEqual(fams.filter((f) => f.label.startsWith("Bot")).map((f) => [f.label, f.hint]), [
      ["Bot (Kod otomasyonu)", null],
      ["Bot (n8n otomasyonu)", null],
    ]);
  });
  it("rolsüz worker kendi adıyla, elle adlandırılmış karışık aile rol adıyla", () => {
    assert.deepEqual(fams.at(-1), { key: "", label: "Ayşe", hint: null, ids: ["g"] });
    const mixed = familiesOf([w("x", "Kernel-1", "backend", "Backend"), w("y", "Veri", "backend", "Backend")]);
    assert.deepEqual(mixed.map((f) => [f.label, f.hint]), [["Backend", null]]);
    assert.equal(familiesOf([w("x", "A", null, null), w("y", "B", null, null)])[0].label, "Rolsüz");
  });
});

describe("scopeIds / scopeTitle", () => {
  it("tümü filtresiz gider", () => {
    assert.equal(scopeIds({ kind: "all" }, crew), null);
    assert.equal(scopeTitle({ kind: "all" }, crew), "Tüm worker'lar");
  });
  it("uzmanlık seçili rollerin worker'larını verir", () => {
    const s = { kind: "roles" as const, keys: ["backend", "code-review"] };
    assert.deepEqual(scopeIds(s, crew), ["b", "c", "d"]);
    assert.equal(scopeTitle(s, crew), "Kernel, Lens · 3 worker");
  });
  it("seçili yalnız hâlâ worker olanları sayar", () => {
    const s = { kind: "picked" as const, ids: ["a", "lead", "g"] };
    assert.deepEqual(scopeIds(s, crew), ["a", "g"]);
    assert.equal(scopeTitle(s, crew), "Seçili · 2 worker");
  });
});
