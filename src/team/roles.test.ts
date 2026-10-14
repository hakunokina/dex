import { test } from "node:test";
import assert from "node:assert/strict";
import { loadRoles } from "./roles.ts";

const up = () => false;

test("first load fetches, later reloads keep the cached list", async () => {
  let calls = 0;
  const fetch = async () => (calls++, ["dev"]);
  const first = await loadRoles<string>([], false, fetch, up);
  assert.deepEqual(first, ["dev"]);
  assert.deepEqual(await loadRoles(first, false, fetch, up), ["dev"]);
  assert.equal(calls, 1);
});

test("after a local reconnect the list is refreshed", async () => {
  assert.deepEqual(await loadRoles(["dev"], true, async () => ["dev", "assistant"], up), ["dev", "assistant"]);
});

test("a failed refetch keeps the old list", async () => {
  assert.deepEqual(await loadRoles(["dev"], true, () => Promise.reject(new Error("closing")), up), ["dev"]);
});

test("first load: failure throws while the core is up, empty while it is down", async () => {
  const fail = () => Promise.reject(new Error("x"));
  await assert.rejects(loadRoles<string>([], false, fail, up));
  assert.deepEqual(await loadRoles<string>([], false, fail, () => true), []);
});
