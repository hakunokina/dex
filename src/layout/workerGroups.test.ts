import { test } from "node:test";
import assert from "node:assert/strict";
import { workerMoves, workerGroups } from "./workerGroups.ts";

test("FIFO: yeni queued iş önceki işin running geçişini maskelemez", () => {
  const queued = [2, 3].map(id => ({ id, toAgent: "w", status: "queued" }));
  assert.deepEqual(workerMoves(queued, [{ ...queued[0], status: "running" }, queued[1]]), [{ agentId: "w", finished: false }]);
});

test("FIFO: kuyruk varken bitiş sola, sıradaki running sağa taşır", () => {
  const running = { id: 2, toAgent: "w", status: "running" };
  const queued = { id: 3, toAgent: "w", status: "queued" };
  assert.deepEqual(workerMoves([running], [running, queued]), []);
  const done = { ...running, status: "done" };
  assert.deepEqual(workerMoves([running, queued], [done, queued]), [{ agentId: "w", finished: true }]);
  assert.deepEqual(workerMoves([done, queued], [done, { ...queued, status: "running" }]), [{ agentId: "w", finished: false }]);
});

test("açık işin üç bitiş durumu sola taşıma ister", () => {
  for (const status of ["done", "done_no_report", "failed"]) {
    assert.deepEqual(workerMoves([{ id: 1, toAgent: "w", status: "running" }], [{ id: 1, toAgent: "w", status }]), [{ agentId: "w", finished: true }]);
  }
});

test("yeni iş sağa döner; kuyruk, eski sonuç ve ilk liste taşımaz", () => {
  const done = { id: 1, toAgent: "w", status: "done" };
  assert.deepEqual(workerMoves([done], [done, { ...done, id: 2, status: "running" }]), [{ agentId: "w", finished: false }]);
  assert.deepEqual(workerMoves([done], [done, { ...done, id: 2, status: "queued" }]), []);
  assert.deepEqual(workerMoves([], [done]), []);
  assert.deepEqual(workerMoves([done], [done]), []);
  assert.deepEqual(workerMoves([{ ...done, status: "running" }, { ...done, id: 2, status: "running" }], [done, { ...done, id: 2, status: "running" }]), []);
});

test("Kart altındaki sağ grup ve hemen sol komşusu seçilir, diğer gruplar korunur", () => {
  const office = { left: 500, top: 0, width: 500, height: 300 };
  const groups = [
    { id: "elsewhere", left: 1100, top: 0, width: 400, height: 800 },
    { id: "done", left: 0, top: 304, width: 496, height: 400 },
    { id: "working", left: 500, top: 304, width: 500, height: 400 },
    { id: "below", left: 500, top: 708, width: 500, height: 200 },
  ];
  assert.deepEqual(workerGroups(office, groups), { working: new Set(["working"]), done: "done" });
  assert.deepEqual(workerGroups(office, []), { working: new Set() });
  assert.deepEqual(workerGroups({ ...office, width: 0 }, groups), { working: new Set() });
});
