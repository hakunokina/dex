import { test } from "node:test";
import assert from "node:assert/strict";
import { heartbeatView } from "./heartbeat.ts";

test("nabız süresi saniye ve dakika sınırlarında biçimlenir", () => {
  for (const [ms, label] of [[0, "0s"], [3999, "3s"], [59999, "59s"], [60000, "1dk"], [720000, "12dk"]] as const) {
    assert.equal(heartbeatView(1000, "idle", 1000 + ms)?.label, label);
  }
});

test("offline sayaç gizlenir, bilinmeyen nabız süre uydurmaz, gelecek zaman sıfırlanır", () => {
  assert.equal(heartbeatView(1000, "offline", 5000), null);
  assert.deepEqual(heartbeatView(null, "idle", 5000), { label: "—", quiet: false });
  assert.deepEqual(heartbeatView(6000, "working", 5000), { label: "0s", quiet: false });
});

test("iki dakikadan uzun sessizlik yalnız çalışan ajanı uyarır", () => {
  assert.equal(heartbeatView(0, "working", 120000)?.quiet, false);
  assert.equal(heartbeatView(0, "working", 121000)?.quiet, true);
  for (const status of ["idle", "waiting"]) assert.equal(heartbeatView(0, status, 720000)?.quiet, false);
});
