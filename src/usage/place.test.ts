// #2034: şerit konumu ve hover kartının yeri.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { cardSpot, parsePlace } from "./place.ts";

const view = { w: 1280, h: 800 };
const card = { w: 228, h: 200 };

describe("parsePlace", () => {
  it("bilinen değerler", () => {
    assert.equal(parsePlace("left"), "left");
    assert.equal(parsePlace("top"), "top");
    assert.equal(parsePlace("right"), "right");
  });
  it("boş/bilinmeyen → sağ", () => {
    assert.equal(parsePlace(null), "right");
    assert.equal(parsePlace("bottom"), "right");
  });
});

describe("cardSpot", () => {
  it("sağ: kart şeridin solunda, halkaya ortalı, ok sağda", () => {
    const strip = { left: 1224, right: 1280, top: 300, bottom: 500 };
    const s = cardSpot("right", { left: 1235, right: 1269, top: 380, bottom: 414 }, strip, card, view);
    assert.equal(s.left + card.w, strip.left - 8);
    assert.equal(s.top, 397 - 100);
    assert.equal(s.arrow, "right");
    assert.equal(s.arrowAt, 100);
  });
  it("sol: kart şeridin sağında, ok solda", () => {
    const strip = { left: 0, right: 56, top: 300, bottom: 500 };
    const s = cardSpot("left", { left: 11, right: 45, top: 380, bottom: 414 }, strip, card, view);
    assert.equal(s.left, 64);
    assert.equal(s.arrow, "left");
  });
  it("dikey: pencere kenarında kırpılır, ok halkayı gösterir", () => {
    const strip = { left: 1224, right: 1280, top: 0, bottom: 800 };
    const s = cardSpot("right", { left: 1235, right: 1269, top: 770, bottom: 790 }, strip, card, view);
    assert.equal(s.top, 800 - 200 - 8);
    assert.equal(s.arrowAt, card.h - 14);
  });
  it("üst: kart şeridin altında, halkaya ortalı, ok üstte", () => {
    const strip = { left: 700, right: 1100, top: 0, bottom: 60 };
    const s = cardSpot("top", { left: 900, right: 928, top: 16, bottom: 44 }, strip, card, view);
    assert.equal(s.top, 68);
    assert.equal(s.left, 914 - 114);
    assert.equal(s.arrow, "top");
    assert.equal(s.arrowAt, 114);
    assert.equal(s.maxHeight, 800 - 68 - 8);
  });
  it("üst: sağ kenardaki halkada kart pencere içinde kalır, ok yine halkada", () => {
    const strip = { left: 900, right: 1140, top: 0, bottom: 60 };
    const s = cardSpot("top", { left: 1110, right: 1138, top: 16, bottom: 44 }, strip, card, { w: 1150, h: 800 });
    assert.equal(s.left + card.w, 1150 - 8);
    assert.equal(s.arrowAt, 1124 - s.left);
  });
});
