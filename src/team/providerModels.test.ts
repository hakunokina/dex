import { test } from "node:test";
import assert from "node:assert/strict";
import { providerModels } from "./providerModels.ts";

test("gateway never offers Claude fallback models without an account catalog", () => {
  assert.deepEqual(providerModels([], "gateway"), []);
  assert.deepEqual(providerModels([{ id: "gateway", label: "Gateway", baseUrl: null, models: [], hasToken: false, tokenMask: null }], "gateway"), []);
});

test("gateway offers only the account catalog and preserves an existing unavailable model as a label", () => {
  const p = { id: "gateway", label: "Gateway", baseUrl: null, hasToken: true, tokenMask: null, models: [{ id: "account-model", label: "Hesabın modeli" }] };
  assert.deepEqual(providerModels([p], "gateway"), p.models);
  assert.deepEqual(providerModels([p], "gateway", "removed-model"), [...p.models, { id: "removed-model", label: "removed-model (listede yok)" }]);
});

test("Anthropic fallback, legacy aliases and empty local catalogs retain their behavior", () => {
  assert.equal(providerModels([], "anthropic").length, 4);
  assert.deepEqual(providerModels([], "local"), []);
  const p = { id: "anthropic", label: "Claude", baseUrl: null, hasToken: false, tokenMask: null, models: [{ id: "opus", label: "Opus", legacy: true }, { id: "current", label: "Current" }] };
  assert.deepEqual(providerModels([p], "anthropic"), [p.models[1]]);
  assert.deepEqual(providerModels([p], "anthropic", "opus"), p.models);
});
