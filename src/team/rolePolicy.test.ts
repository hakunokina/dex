import { test } from "node:test";
import assert from "node:assert/strict";
import { patronWritable, isAssignable, memberRoles, assistantTaken } from "./rolePolicy.ts";

test("lider ve asistan doğrudan yazılabilir; worker izleme modunda kalır", () => {
  assert.equal(patronWritable({ kind: "leader", roleSlug: "lead" }), true);
  assert.equal(patronWritable({ kind: "worker", roleSlug: "assistant" }), true);
  assert.equal(patronWritable({ kind: "worker", roleSlug: "frontend" }), false);
  assert.equal(patronWritable(null), false);
});

test("asistan dağıtım hedefi değildir; diğer roller atanabilir", () => {
  assert.equal(isAssignable({ roleSlug: "assistant" }), false);
  assert.equal(isAssignable({ roleSlug: "frontend" }), true);
  assert.equal(isAssignable({ roleSlug: "lead" }), true);
  assert.equal(isAssignable({ roleSlug: null }), true);
});

test("asistan yalnız geliştirici takımının üye yolunda seçilir", () => {
  const roles = [
    { slug: "frontend", category: "eng" }, { slug: "assistant", category: "personal" },
    { slug: "teacher", category: "education" },
  ];
  assert.deepEqual(memberRoles(roles, "dev", false).map(r => r.slug), ["frontend"]);
  assert.deepEqual(memberRoles(roles, "dev", true).map(r => r.slug), ["frontend", "assistant"]);
  assert.deepEqual(memberRoles(roles, "edu", true).map(r => r.slug), ["teacher"]);
});

test("takım başına tek asistan; mevcut asistan kendi rolünü koruyabilir", () => {
  const members = [{ id: "echo", roleSlug: "assistant" }, { id: "pixel", roleSlug: "frontend" }];
  assert.equal(assistantTaken(members), true);
  assert.equal(assistantTaken(members, "pixel"), true);
  assert.equal(assistantTaken(members, "echo"), false);
  assert.equal(assistantTaken([]), false);
});
