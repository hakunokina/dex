// #2134: popover yerleşimi — sağa yaslı, kenardan taşmaz, gerekirse yukarı açılır.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { bulkSpot } from "./bulkPlace.ts";

const view = { w: 1280, h: 800 };
const pop = { w: 300, h: 260 };

describe("bulkSpot", () => {
  it("aşağıda yer varsa düğmenin altında, sağa yaslı", () => {
    const s = bulkSpot({ left: 900, right: 928, top: 40, bottom: 68 }, pop, view);
    assert.equal(s.above, false);
    assert.equal(s.top, 74);
    assert.equal(s.left + pop.w, 928);
    assert.equal(s.maxHeight, 800 - 8 - 74);
  });
  it("düğme sol kenardaysa popover pencere içinde kalır", () => {
    const s = bulkSpot({ left: 232, right: 260, top: 40, bottom: 68 }, pop, view);
    assert.equal(s.left, 8);
    assert.equal(s.left + pop.w <= view.w - 8, true);
  });
  it("aşağıda yer yoksa yukarı açılır", () => {
    const s = bulkSpot({ left: 900, right: 928, top: 700, bottom: 728 }, pop, view);
    assert.equal(s.above, true);
    assert.equal(s.top + pop.h, 700 - 6);
    assert.equal(s.maxHeight, 700 - 6 - 8);
  });
  it("iki yönde de sığmazsa büyük tarafa sınırlanır", () => {
    const s = bulkSpot({ left: 900, right: 928, top: 180, bottom: 208 }, pop, { w: 1280, h: 300 });
    assert.equal(s.above, true); // üstte 166, altta 84 var
    assert.equal(s.maxHeight, 166);
    assert.equal(s.top, 8);
    const t = bulkSpot({ left: 900, right: 928, top: 60, bottom: 88 }, pop, { w: 1280, h: 300 });
    assert.equal(t.above, false);
    assert.equal(t.maxHeight, 300 - 8 - 94);
    assert.equal(t.top, 94);
  });
});
