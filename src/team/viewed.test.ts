import { test } from "node:test";
import assert from "node:assert/strict";
import { createTeamViewing } from "./viewed.ts";

test("active team changes release the previous team and deduplicate unchanged viewing", () => {
  let state = { teamId: "remote:first", viewed: true, connected: true };
  const calls: unknown[] = [];
  const viewing = createTeamViewing(() => state, async (id, viewed) => { calls.push([id, viewed]); }, () => {});
  viewing.sync();
  viewing.sync();
  state = { ...state, teamId: "remote:second" };
  viewing.sync();
  assert.deepEqual(calls, [["remote:first", true], ["remote:first", false], ["remote:second", true]]);
});

test("hidden, blur, reconnect, empty team and disposal release viewing without polling", () => {
  let state = { teamId: "first", viewed: true, connected: true };
  const calls: unknown[] = [];
  const viewing = createTeamViewing(() => state, async (id, viewed) => { calls.push([id, viewed]); }, () => {});
  viewing.sync();
  state = { ...state, viewed: false };
  viewing.sync();
  viewing.sync();
  state = { ...state, viewed: true };
  viewing.sync();
  state = { ...state, connected: false };
  viewing.sync();
  state = { ...state, connected: true, viewed: false };
  viewing.sync();
  state = { ...state, viewed: true };
  viewing.sync();
  state = { ...state, teamId: "" };
  viewing.sync();
  state = { ...state, teamId: "second" };
  viewing.sync();
  viewing.dispose();
  viewing.sync(); // A queued initial onHostUp callback can arrive after StrictMode cleanup.
  assert.deepEqual(calls, [["first", true], ["first", false], ["first", true],
    ["first", false], ["first", true], ["first", false], ["second", true], ["second", false]]);
});

test("failed sends are reported and retried at the next event", async () => {
  let attempts = 0;
  let errors = 0;
  const viewing = createTeamViewing(() => ({ teamId: "first", viewed: true, connected: true }), async () => {
    if (++attempts === 1) throw new Error("disconnected");
  }, () => { errors++; });
  viewing.sync();
  await Promise.resolve();
  viewing.sync();
  assert.equal(errors, 1);
  assert.equal(attempts, 2);
});

test("a failed previous-team release retries without replaying stale viewing", async () => {
  let state = { teamId: "first", viewed: true, connected: true };
  const calls: unknown[] = [];
  let fail = true;
  const viewing = createTeamViewing(() => state, async (id, viewed) => {
    calls.push([id, viewed]);
    if (id === "first" && !viewed && fail) { fail = false; throw new Error("temporary"); }
  }, () => {});
  viewing.sync();
  state = { ...state, teamId: "second" };
  viewing.sync();
  await Promise.resolve();
  viewing.sync();
  assert.deepEqual(calls, [["first", true], ["first", false], ["second", true], ["first", false]]);
});

test("late failures and disconnects cannot replay an obsolete true", async () => {
  let state = { teamId: "first", viewed: true, connected: true };
  const calls: unknown[] = [];
  let reject!: (error: Error) => void;
  const viewing = createTeamViewing(() => state, async (id, viewed) => {
    calls.push([id, viewed]);
    if (calls.length === 1) await new Promise<void>((_, fail) => { reject = fail; });
  }, () => {});
  viewing.sync();
  state = { ...state, connected: false };
  viewing.sync();
  reject(new Error("old socket closed"));
  await new Promise<void>(resolve => setImmediate(resolve));
  state = { teamId: "second", viewed: true, connected: true };
  viewing.sync();
  assert.deepEqual(calls, [["first", true], ["second", true]]);
  viewing.dispose();
});
