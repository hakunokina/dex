import { test } from "node:test";
import assert from "node:assert/strict";
import { assistantDirection } from "./assistantPlacement.ts";

test("asistan geniş çalışma alanında Core yanında, dar alanda altında açılır", () => {
  assert.equal(assistantDirection(1400), "right");
  assert.equal(assistantDirection(1260), "right");
  assert.equal(assistantDirection(1259), "below");
  assert.equal(assistantDirection(720), "below");
  assert.equal(assistantDirection(0), "below");
});
