import { test } from "node:test";
import assert from "node:assert/strict";
import { chainView } from "./chainView.ts";

test("pre-#94 dexd view (no `disabled`) gets an empty list", () => {
  assert.deepEqual(chainView({ order: ["a", "b"], exhausted: [] }), { order: ["a", "b"], disabled: [], exhausted: [] });
});

test("garbage fields and null become empty lists", () => {
  assert.deepEqual(chainView({ order: "x", disabled: 1 } as never), { order: [], disabled: [], exhausted: [] });
  assert.deepEqual(chainView(null), { order: [], disabled: [], exhausted: [] });
});

test("a full view passes through", () => {
  const v = { order: ["a"], disabled: ["a"], exhausted: [{ provider: "a", resetAt: 1, reason: "quota" }] };
  assert.deepEqual(chainView(v), v);
});
