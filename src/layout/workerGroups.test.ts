import { test } from "node:test";
import assert from "node:assert/strict";
import { movePlaysFx, reconcileWorkerRows, workerGroups, workerMoves, workerStep } from "./workerGroups.ts";

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

test("taşıma efekti yalnız ekrandaki takımda ve hareket azaltılmamışken oynar", () => {
  assert.equal(movePlaysFx("main", "main", false), true);
  assert.equal(movePlaysFx("main", "jezdev", false), false);
  assert.equal(movePlaysFx("main", "", false), false);
  assert.equal(movePlaysFx("main", "main", true), false);
});

// #2267: the patron's layout — Kernel-1 left (no role), Kernel-4 under the Kart with a "done" record from
// days ago, Pixel-1 closed. The record contradicts the rule (under the Kart = working): geometry wins.
test("Kart altındaki grubun eski 'bitti' kaydı atılır: o grup çalışan, solu bitti", () => {
  const stale = { working: new Set<string>(), done: "7" };
  assert.deepEqual(reconcileWorkerRows(stale, { working: new Set(["7"]), done: "3" }), { working: new Set(["7"]), done: "3" });
  // No left neighbour: the stale "done" is not kept as the finished row either.
  assert.deepEqual(reconcileWorkerRows(stale, { working: new Set(["7"]) }), { working: new Set(["7"]) });
});

test("geometriyle uyuşan kayıt olduğu gibi kalır (geniş düzende iki çalışan grup dahil)", () => {
  const wide = { working: new Set(["a", "b"]), done: "d" };
  assert.equal(reconcileWorkerRows(wide, { working: new Set(["b"]), done: "a" }), wide);
  const noDone = { working: new Set(["a", "b"]) };
  assert.equal(reconcileWorkerRows(noDone, { working: new Set(["b"]), done: "a" }), noDone); // "a" is a working group, not the finished row
  assert.deepEqual(reconcileWorkerRows({ working: new Set(["b"]) }, { working: new Set(["b"]), done: "d" }), { working: new Set(["b"]), done: "d" });
});

test("geometri yokken (gizli takım, Kart altı boş) kayıt korunur", () => {
  const rec = { working: new Set(["w"]), done: "d" };
  assert.equal(reconcileWorkerRows(rec, undefined), rec);
  assert.equal(reconcileWorkerRows(rec, { working: new Set() }), rec);
});

test("kayıtlı çalışan grup Kart altında değilse Kart altındaki kazanır; sol komşu yoksa eski bitti grubu kalır", () => {
  assert.deepEqual(reconcileWorkerRows({ working: new Set(["x"]), done: "d" }, { working: new Set(["u"]) }), { working: new Set(["u"]), done: "d" });
  assert.deepEqual(reconcileWorkerRows({ working: new Set(["x"]), done: "d" }, { working: new Set(["u"]), done: "l" }), { working: new Set(["u"]), done: "l" });
});

test("kapalı panel iş başlayınca açılır, bitince açılmaz; açık panel yalnız kendi sırasından taşınır", () => {
  const rows = { working: new Set(["w"]), done: "d" };
  assert.equal(workerStep(null, rows, false), "open"); // the regression: this was never a move
  assert.equal(workerStep(null, rows, true), null);
  assert.equal(workerStep("d", rows, false), "working");
  assert.equal(workerStep("w", rows, false), null);
  assert.equal(workerStep("w", rows, true), "done");
  assert.equal(workerStep("d", rows, true), null);
  assert.equal(workerStep("elsewhere", rows, false), null);
  assert.equal(workerStep("elsewhere", rows, true), null);
});
