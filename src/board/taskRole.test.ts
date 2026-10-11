import assert from "node:assert/strict";
import { test } from "node:test";
import { taskRoleName } from "./taskRole.ts";

const task = { id: 12, agentId: null };
const delegation = { taskId: 12, host: "local", targetRole: "frontend" };
const roles = [{ slug: "frontend", name: "Frontend Mühendisi" }];

test("atanmamış görevin rolü aynı hosttaki devretmeden görünen adla çözülür", () => {
  assert.equal(taskRoleName(task, "local", [
    { ...delegation, host: "remote", targetRole: "backend" },
    { ...delegation, taskId: 13, targetRole: "backend" },
    delegation,
  ], roles), "Frontend Mühendisi");
});

test("atanmış görevde rol çipi gösterilmez", () => {
  assert.equal(taskRoleName({ ...task, agentId: "pixel" }, "local", [delegation], roles), null);
});

test("katalog yüklenmemişse veya rol bilinmiyorsa slug gösterilir", () => {
  assert.equal(taskRoleName(task, "local", [delegation], []), "frontend");
  assert.equal(taskRoleName(task, "local", [{ ...delegation, targetRole: "unknown" }], roles), "unknown");
});

test("eşleşen rol yoksa eski host ve boş liste çip üretmez", () => {
  for (const list of [[], [{ ...delegation, host: "remote" }], [{ ...delegation, taskId: 13 }],
    [{ ...delegation, targetRole: undefined }], [{ ...delegation, targetRole: null }],
    [{ ...delegation, targetRole: "" }]]) {
    assert.equal(taskRoleName(task, "local", list, roles), null);
  }
});
