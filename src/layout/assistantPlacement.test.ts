import { test } from "node:test";
import assert from "node:assert/strict";
import { opensAssistant } from "./assistantPlacement.ts";

test("kapatılmış asistan paneli yeniden açılmaz; yeni asistan eklenince açılır", () => {
  assert.equal(opensAssistant("echo", undefined), true);
  assert.equal(opensAssistant("echo", "echo"), false);
  assert.equal(opensAssistant("echo-2", "echo"), true);
});
