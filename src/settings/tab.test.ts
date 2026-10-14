import { test } from "node:test";
import assert from "node:assert/strict";
import { migrateTab } from "./tab.ts";

test("yeni sayfalar aynen geçer", () => {
  for (const t of ["general", "notifications", "connections", "providers", "chain", "resources", "gpu"] as const) {
    assert.equal(migrateTab(t), t);
  }
});

test("eski sekmeler yeni sayfalara göçer", () => {
  assert.equal(migrateTab("agents"), "resources");
  assert.equal(migrateTab("hosts"), "connections");
  assert.equal(migrateTab("advanced"), "general");
  assert.equal(migrateTab("providers"), "providers");
});

test("bilinmeyen ve boş değerler genele düşer", () => {
  assert.equal(migrateTab(null), "general");
  assert.equal(migrateTab(""), "general");
  assert.equal(migrateTab("yok"), "general");
});
