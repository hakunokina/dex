import { test } from "node:test";
import assert from "node:assert/strict";
import { mountedTeams } from "./mountedTeams.ts";

test("only the active team mounts on startup", () => {
  assert.deepEqual(mountedTeams([], ["a", "b", "c"], "b"), ["b"]);
});

test("switching retains visited teams without adding unopened ones", () => {
  const first = mountedTeams([], ["a", "b", "c"], "a");
  const second = mountedTeams(first, ["a", "b", "c"], "b");
  assert.deepEqual(second, ["a", "b"]);
  assert.equal(mountedTeams(second, ["a", "b", "c", "d"], "a"), second);
});

test("deleted teams are released and unknown active ids are ignored", () => {
  assert.deepEqual(mountedTeams(["a", "b"], ["b", "c"], "missing"), ["b"]);
  assert.deepEqual(mountedTeams(["a"], [], ""), []);
});
