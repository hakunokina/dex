import { test } from "node:test";
import assert from "node:assert/strict";
import { providerModels } from "./providerModels.ts";

test("Anthropic fallback, legacy aliases and empty local catalogs retain their behavior", () => {
  assert.equal(providerModels([], "anthropic").length, 4);
  assert.deepEqual(providerModels([], "local"), []);
  const p = { id: "anthropic", label: "Claude", baseUrl: null, hasToken: false, tokenMask: null, models: [{ id: "opus", label: "Opus", legacy: true }, { id: "current", label: "Current" }] };
  assert.deepEqual(providerModels([p], "anthropic"), [p.models[1]]);
  assert.deepEqual(providerModels([p], "anthropic", "opus"), p.models);
});
