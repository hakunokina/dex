import { test } from "node:test";
import assert from "node:assert/strict";
import { PtySnapshotQueue } from "./ptySnapshotQueue.ts";

test("an exit arriving before the attach reply follows snapshot and queued output", () => {
  const queue = new PtySnapshotQueue();
  const seen: string[] = [];
  queue.deliver(() => seen.push("tail"));
  queue.deliver(() => seen.push("exit"));
  assert.deepEqual(seen, []);
  queue.flush(() => seen.push("snapshot"));
  assert.deepEqual(seen, ["snapshot", "tail", "exit"]);
});

test("after replay finishes, live messages are delivered immediately", () => {
  const queue = new PtySnapshotQueue();
  const seen: string[] = [];
  queue.deliver(() => seen.push("size"));
  queue.flush(() => seen.push("snapshot"));
  queue.deliver(() => seen.push("live"));
  queue.deliver(() => seen.push("exit"));
  assert.deepEqual(seen, ["snapshot", "size", "live", "exit"]);
});
