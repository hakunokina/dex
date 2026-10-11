// #411: Kart çipinden açılan paneye giden kırmızı devre izi — dik açılı yol, hedefin kenarında biter.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { circuitPath, pathLength, type Box, type Pt } from "./circuit.ts";

const chip: Box = { left: 100, top: 100, width: 60, height: 30 };
const orthogonal = (points: Pt[]) => points.slice(1).every((p, i) => p.x === points[i].x || p.y === points[i].y);
const onEdge = (p: Pt, b: Box) =>
  p.x >= b.left && p.x <= b.left + b.width && p.y >= b.top && p.y <= b.top + b.height &&
  (p.x === b.left || p.x === b.left + b.width || p.y === b.top || p.y === b.top + b.height);

describe("circuitPath", () => {
  it("sağdaki panel: çipin sağ kenarından çıkar, dik açılarla panelin sol kenarına varır", () => {
    const pane: Box = { left: 400, top: 300, width: 300, height: 200 };
    const path = circuitPath(chip, pane);
    assert.deepEqual(path[0], { x: 160, y: 115 });
    assert.ok(orthogonal(path));
    assert.ok(onEdge(path[path.length - 1], pane));
    assert.equal(path[path.length - 1].x, 400);
    assert.equal(path.length, 4);
  });
  it("aşağıdaki panel: dikey eksenden çıkar, üst kenarda biter", () => {
    const pane: Box = { left: 0, top: 600, width: 800, height: 200 };
    const path = circuitPath(chip, pane);
    assert.deepEqual(path[0], { x: 130, y: 130 });
    assert.ok(orthogonal(path));
    assert.deepEqual(path[path.length - 1], { x: 130, y: 600 });
  });
  it("hizalıysa ara kırılım düşer (sıfır uzunluklu parça yok)", () => {
    const path = circuitPath(chip, { left: 500, top: 50, width: 100, height: 200 });
    assert.deepEqual(path, [{ x: 160, y: 115 }, { x: 500, y: 115 }]);
    assert.ok(path.slice(1).every((p, i) => p.x !== path[i].x || p.y !== path[i].y));
  });
  it("çip panelin içindeyse yol yok (yalnız vurgu)", () => {
    assert.deepEqual(circuitPath(chip, { left: 0, top: 0, width: 1000, height: 1000 }), []);
  });
  it("sekme başlığı çipin hemen üstünde: kenara düz atlar", () => {
    const path = circuitPath(chip, { left: 0, top: 80, width: 1000, height: 24 });
    assert.equal(path[path.length - 1].y, 104);
    assert.ok(orthogonal(path));
  });
});

describe("pathLength", () => {
  it("parça uzunluklarının toplamı", () => {
    assert.equal(pathLength([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 10, y: 5 }]), 15);
    assert.equal(pathLength([]), 0);
  });
});
