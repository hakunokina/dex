import { test } from "node:test";
import assert from "node:assert/strict";
import { coreDrift, describe, isOlder, readServerInfo, supports } from "./serverInfo.ts";

const NEW = { protocol: 1, server: "dexd", version: "0.1.0", commit: "abc1234567", commitTime: 2000, methods: ["memory_graph", "chat_history"] };
// Pre-#142 dexd hello.
const OLD = { protocol: 1, server: "dexd", version: "0.1.0", device: { id: "d1" } };

test("readServerInfo: new and old hello", () => {
  assert.deepEqual(readServerInfo(NEW), { version: "0.1.0", commit: "abc1234567", commitTime: 2000, methods: ["memory_graph", "chat_history"] });
  assert.deepEqual(readServerInfo(OLD), { version: "0.1.0", commit: null, commitTime: null, methods: null });
  assert.equal(readServerInfo(null), null);
  assert.equal(readServerInfo("x"), null);
  assert.deepEqual(readServerInfo({ methods: ["a", 3, null], commitTime: "5" })?.methods, ["a"]);
  assert.equal(readServerInfo({ commitTime: "5" })?.commitTime, null);
});

test("supports: only a published list can say no", () => {
  const info = readServerInfo(NEW);
  assert.equal(supports(info, "memory_graph"), true);
  assert.equal(supports(info, "model_news"), false);
  assert.equal(supports(readServerInfo(OLD), "model_news"), true);
  assert.equal(supports(null, "model_news"), true);
  assert.equal(supports(readServerInfo({ ...NEW, methods: [] }), "memory_graph"), false);
});

test("isOlder: by commit time; pre-#142 host is older; no git build can't tell", () => {
  const ref = readServerInfo({ commitTime: 3000 });
  assert.equal(isOlder(readServerInfo(NEW), ref), true);
  assert.equal(isOlder(readServerInfo({ ...NEW, commitTime: 3000 }), ref), false);
  assert.equal(isOlder(readServerInfo({ ...NEW, commitTime: 4000 }), ref), false);
  assert.equal(isOlder(readServerInfo(OLD), ref), true);
  assert.equal(isOlder(readServerInfo({ ...NEW, commitTime: 0 }), ref), false);
  assert.equal(isOlder(readServerInfo(NEW), readServerInfo({ commitTime: 0 })), false);
  assert.equal(isOlder(readServerInfo(NEW), null), false);
  assert.equal(isOlder(null, ref), false);
});

test("describe", () => {
  assert.equal(describe(readServerInfo(NEW)), "dexd 0.1.0 · abc1234567");
  assert.equal(describe(readServerInfo(OLD)), "dexd 0.1.0");
  assert.equal(describe(readServerInfo({})), null);
});

test("coreDrift: the local core against this UI's build", () => {
  const ui = { commit: "abc1234567", commitTime: 2000 };
  assert.equal(coreDrift(readServerInfo(NEW), ui), null, "same commit");
  assert.equal(coreDrift(readServerInfo({ ...NEW, commit: "0ld0000000", commitTime: 1000 }), ui), "older");
  assert.equal(coreDrift(readServerInfo({ ...NEW, commit: "4ew0000000", commitTime: 3000 }), ui), "newer");
  assert.equal(coreDrift(readServerInfo({ ...NEW, commit: "s1b0000000" }), ui), "other", "same time, another commit");
  assert.equal(coreDrift(readServerInfo(OLD), ui), "older", "pre-#142 core");
  assert.equal(coreDrift(readServerInfo({ ...NEW, commit: "unknown", commitTime: 0 }), ui), null, "core built without git");
  assert.equal(coreDrift(readServerInfo({ ...NEW, commitTime: 1 }), { commit: "unknown", commitTime: 0 }), null, "UI built without git");
  assert.equal(coreDrift(null, ui), null, "not connected");
});
