import { test } from "node:test";
import assert from "node:assert/strict";
import { fitConvertedModels, normalizeCloneTemplate } from "./cloneTemplate.ts";

const anthropic = { id: "anthropic", label: "Claude", baseUrl: null, hasToken: false, tokenMask: null, models: [{ id: "first", label: "First" }, { id: "second", label: "Second" }] };
const member = { name: "A", kind: "worker" as const, model: "claude-sonnet-5-5", provider: "glm", engine: "claude" as string | null, roleSlug: "qa" };

test("Codex and unknown-engine members become Anthropic Claude members", () => {
  const t = { kind: "main" as const, profile: "dev" as const, members: [
    { ...member, engine: "codex", model: "gpt-5.5", provider: null },
    { ...member, engine: "weird" },
    member,
    { ...member, engine: null },
  ] };
  const r = normalizeCloneTemplate(t as never, [anthropic]);
  assert.deepEqual(r.converted, [0, 1]);
  assert.deepEqual(r.template.members[0], { ...member, engine: null, provider: "anthropic", model: "first" });
  assert.deepEqual(r.template.members[1], { ...member, engine: null, provider: "anthropic", model: "first" });
  assert.equal(r.template.members[2], t.members[2]);
  assert.equal(r.template.members[3], t.members[3]);
});

test("templates without foreign engines stay untouched", () => {
  const t = { kind: "main" as const, profile: "dev" as const, members: [member] };
  const r = normalizeCloneTemplate(t as never, []);
  assert.deepEqual(r.converted, []);
  assert.equal(r.template, t);
});

test("pending catalog: create payload has no Codex member, model falls back to the default list", () => {
  const t = { kind: "main" as const, profile: "dev" as const, members: [{ ...member, engine: "codex", model: "gpt-5.5", provider: null }] };
  const sent = normalizeCloneTemplate(t as never, []).template.members;
  assert.ok(sent.every(m => m.engine == null || m.engine === "claude"));
  assert.equal(sent[0].provider, "anthropic");
  assert.equal(sent[0].model, "claude-fable-5-1");
});

test("catalog arrival moves converted members onto its Anthropic models only", () => {
  const t = { kind: "main" as const, profile: "dev" as const, members: [
    { ...member, engine: null, provider: "anthropic", model: "claude-fable-5-1" },
    { ...member, engine: null, provider: "anthropic", model: "second" },
    { ...member, engine: null, provider: "glm", model: "claude-fable-5-1" },
    { ...member, engine: null, provider: "anthropic", model: "claude-fable-5-1" },
  ] };
  const r = fitConvertedModels(t as never, [0, 1, 2], [anthropic]);
  assert.equal(r.members[0].model, "first");
  assert.equal(r.members[1], t.members[1]);
  assert.equal(r.members[2], t.members[2]);
  assert.equal(r.members[3], t.members[3]);
  assert.equal(fitConvertedModels(r, [0, 1, 2], [anthropic]), r);
});
