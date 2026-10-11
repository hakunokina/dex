import { test } from "node:test";
import assert from "node:assert/strict";
import { startOnce } from "./startOnce.ts";

test("a mounting pane and a hidden start share one pending backend start", async () => {
  let calls = 0;
  let finish!: () => void;
  const first = startOnce("remote:agent", () => { calls++; return new Promise<void>(resolve => { finish = resolve; }); });
  const second = startOnce("remote:agent", async () => { calls++; });
  assert.equal(first, second);
  await Promise.resolve();
  assert.equal(calls, 1);
  finish();
  await first;
});

test("failed starts propagate and can be retried", async () => {
  await assert.rejects(startOnce("retry", async () => { throw new Error("start failed"); }), /start failed/);
  let retried = false;
  await startOnce("retry", async () => { retried = true; });
  assert.equal(retried, true);
});

test("different hosts start independently", async () => {
  const calls: string[] = [];
  await Promise.all(["one:agent", "two:agent"].map(id => startOnce(id, async () => { calls.push(id); })));
  assert.deepEqual(calls, ["one:agent", "two:agent"]);
});
