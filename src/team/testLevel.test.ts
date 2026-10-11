import { test } from "node:test";
import assert from "node:assert/strict";
import { nextTestLevel } from "./testLevel.ts";

test("full and light switch in both directions", () => {
  assert.equal(nextTestLevel("full"), "light");
  assert.equal(nextTestLevel("light"), "full");
});

test("old hosts and unknown levels cannot offer a switch", () => {
  for (const level of [undefined, null, "", "none", "FULL"]) {
    assert.equal(nextTestLevel(level), null);
  }
});
