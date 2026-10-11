import { test } from "node:test";
import assert from "node:assert/strict";
import { countAt } from "./motion.ts";

test("countAt: starts at from, ends at to, eases out, clamps k", () => {
  assert.equal(countAt(1248, 1385, 0), 1248);
  assert.equal(countAt(1248, 1385, 1), 1385);
  assert.ok(countAt(0, 100, 0.5) > 50, "ease-out: past halfway at half time");
  assert.equal(countAt(0, 100, -1), 0);
  assert.equal(countAt(0, 100, 2), 100);
  assert.equal(countAt(5, 2, 1), 2, "counts down too");
});
