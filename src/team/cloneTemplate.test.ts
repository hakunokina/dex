import { test } from "node:test";
import assert from "node:assert/strict";
import { normalizeCloneTemplate } from "./cloneTemplate.ts";

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
  assert.equal(r.converted, 2);
  assert.deepEqual(r.template.members[0], { ...member, engine: null, provider: "anthropic", model: "first" });
  assert.deepEqual(r.template.members[1], { ...member, engine: null, provider: "anthropic", model: "first" });
  assert.equal(r.template.members[2], t.members[2]);
  assert.equal(r.template.members[3], t.members[3]);
});

test("templates without foreign engines stay untouched", () => {
  const t = { kind: "main" as const, profile: "dev" as const, members: [member] };
  const r = normalizeCloneTemplate(t as never, []);
  assert.equal(r.converted, 0);
  assert.equal(r.template, t);
});
