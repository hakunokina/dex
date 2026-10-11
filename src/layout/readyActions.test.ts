import { test } from "node:test";
import assert from "node:assert/strict";
import { ReadyActions } from "./readyActions.ts";

test("first-visit panel and focus requests wait for the restored workspace", () => {
  const pending = new ReadyActions();
  const applied: string[] = [];
  for (const command of ["open worker", "focus worker", "toast open"]) {
    pending.run("hidden", false, () => applied.push(command));
  }
  assert.deepEqual(applied, []);
  applied.push("restore closed worker layout");
  pending.flush("hidden");
  assert.deepEqual(applied, ["restore closed worker layout", "open worker", "focus worker", "toast open"]);
  pending.flush("hidden");
  assert.equal(applied.length, 4);
});

test("ready teams run immediately while other teams keep their requests", () => {
  const pending = new ReadyActions();
  const applied: string[] = [];
  pending.run("one", false, () => applied.push("one"));
  pending.run("two", false, () => applied.push("two"));
  pending.run("ready", true, () => applied.push("ready"));
  assert.deepEqual(applied, ["ready"]);
  pending.flush("two");
  assert.deepEqual(applied, ["ready", "two"]);
  pending.flush("one");
  assert.deepEqual(applied, ["ready", "two", "one"]);
});
