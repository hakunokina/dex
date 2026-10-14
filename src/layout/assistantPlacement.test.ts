import { test } from "node:test";
import assert from "node:assert/strict";
import { assistantDirection, opensAssistant } from "./assistantPlacement.ts";

test("asistan geniş çalışma alanında Core yanında, dar alanda altında açılır", () => {
  assert.equal(assistantDirection(1400), "right");
  assert.equal(assistantDirection(1260), "right");
  assert.equal(assistantDirection(1259), "below");
  assert.equal(assistantDirection(720), "below");
  assert.equal(assistantDirection(0), "below");
});

test("kapatılmış asistan paneli yeniden açılmaz; yeni asistan eklenince açılır", () => {
  assert.equal(opensAssistant("echo", undefined), true);
  assert.equal(opensAssistant("echo", "echo"), false);
  assert.equal(opensAssistant("echo-2", "echo"), true);
});
