import { test } from "node:test";
import assert from "node:assert/strict";
import type { Delegation } from "./delegations.ts";
import { activeCount, unreadResult, roleQueueSummary } from "./delegationsModel.ts";

const job = (patch: Partial<Delegation> = {}): Delegation => ({
  id: 1498, host: "local", key: "local:1498", fromAgent: "core", toAgent: "",
  teamId: "team", serviceTeamId: null, prompt: "review", status: "queued",
  summary: null, resultPath: null, taskId: null, createdAt: 1, updatedAt: 1,
  targetRole: "code-reviewer", queuePosition: 1, ...patch,
});

test("rol kuyruğu yalnız takımın atanmamış queued işlerini sayar", () => {
  assert.deepEqual(roleQueueSummary([
    job(), job({ id: 1502, queuePosition: 2 }), job({ toAgent: "lens" }),
    job({ teamId: "other" }), job({ status: "running" }), job({ status: "done" }),
  ], "team"), { count: 2, title: "code-reviewer: #1498 (1), #1502 (2)" });
});

test("rol adları katalogdan gelir; her rolün işleri kuyruk sırasına göre gruplanır", () => {
  const list = [job({ id: 1502, queuePosition: 2 }), job({ id: 1503, targetRole: "backend" }), job()];
  assert.deepEqual(roleQueueSummary(list, "team", [{ slug: "code-reviewer", name: "Code Reviewer" }]), {
    count: 3, title: "Code Reviewer: #1498 (1), #1502 (2)\nbackend: #1503 (1)",
  });
  assert.equal(list[0].id, 1502);
});

test("eski çekirdek alanları yoksa veya null ise kuyruk sıfırdır", () => {
  assert.deepEqual(roleQueueSummary([
    job({ targetRole: undefined, queuePosition: undefined }), job({ targetRole: null }),
    job({ queuePosition: null }), job({ targetRole: "" }),
  ], "team"), { count: 0, title: "" });
  assert.deepEqual(roleQueueSummary([], "team"), { count: 0, title: "" });
});

test("boş hedef hiçbir worker'a aktif iş veya okunmamış sonuç sayılmaz", () => {
  const list = [job(), job({ id: 1502, status: "done" })];
  for (const worker of ["lens", "remote:lens", ""]) {
    assert.equal(activeCount(list, worker, "to"), 0);
    assert.equal(unreadResult(list, worker, () => false), null);
  }
  assert.equal(activeCount(list, "core", "from"), 1);
});

test("atanmış worker sayımı ve en yeni okunmamış sonuç davranışı korunur", () => {
  const done = job({ toAgent: "lens", status: "done" });
  assert.equal(activeCount([job({ toAgent: "lens" })], "lens", "to"), 1);
  assert.equal(unreadResult([done], "lens", () => false), done);
  assert.equal(unreadResult([done], "lens", () => true), null);
  assert.equal(unreadResult([done, job({ id: 1502, toAgent: "lens" })], "lens", () => false), null);
});
