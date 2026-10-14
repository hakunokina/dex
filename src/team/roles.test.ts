import { test } from "node:test";
import assert from "node:assert/strict";
import { roleSync } from "./roles.ts";

/** A fetch whose answers the test resolves/rejects by hand, in any order. */
function manual() {
  const pending: { resolve: (r: string[]) => void; reject: (e: Error) => void }[] = [];
  const fetch = () => new Promise<string[]>((resolve, reject) => void pending.push({ resolve, reject }));
  return { fetch, pending };
}

test("a fetch writes its list and clears stale", async () => {
  const written: string[][] = [];
  const sync = roleSync(async () => ["dev", "assistant"], r => void written.push(r));
  await sync.refresh();
  assert.deepEqual(written, [["dev", "assistant"]]);
  assert.equal(sync.stale, false);
});

test("a failed fetch keeps stale set and writes nothing; the next try brings the new roles", async () => {
  const written: string[][] = [];
  let fail = true;
  const sync = roleSync(async () => {
    if (fail) throw new Error("closing");
    return ["dev", "assistant"];
  }, r => void written.push(r));
  await assert.rejects(sync.refresh());
  assert.equal(sync.stale, true);
  assert.deepEqual(written, []);
  fail = false;
  await sync.refresh();
  assert.equal(sync.stale, false);
  assert.deepEqual(written, [["dev", "assistant"]]);
});

test("an older fetch answering late does not overwrite the newer list", async () => {
  const { fetch, pending } = manual();
  const written: string[][] = [];
  const sync = roleSync(fetch, r => void written.push(r));
  const old = sync.refresh();
  const fresh = sync.refresh();
  pending[1].resolve(["dev", "assistant"]);
  await fresh;
  pending[0].resolve(["dev"]);
  await old;
  assert.deepEqual(written, [["dev", "assistant"]]);
});

test("an older fetch failing late does not mark the newer list stale", async () => {
  const { fetch, pending } = manual();
  const sync = roleSync(fetch, () => {});
  const old = sync.refresh();
  const fresh = sync.refresh();
  pending[1].resolve(["dev"]);
  await fresh;
  pending[0].reject(new Error("closing"));
  await assert.rejects(old);
  assert.equal(sync.stale, false);
});

test("the newest fetch failing leaves stale set even if an older one succeeds late", async () => {
  const { fetch, pending } = manual();
  const written: string[][] = [];
  const sync = roleSync(fetch, r => void written.push(r));
  const old = sync.refresh();
  const fresh = sync.refresh();
  pending[1].reject(new Error("x"));
  await assert.rejects(fresh);
  pending[0].resolve(["dev"]);
  await old;
  assert.equal(sync.stale, true);
  assert.deepEqual(written, []);
});
