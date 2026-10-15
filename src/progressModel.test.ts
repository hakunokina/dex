import { test } from "node:test";
import assert from "node:assert/strict";
import { ago, jobProgress, percent, progressKey, progressLine, workerProgress, type Progress } from "./progressModel.ts";

const p = (patch: Partial<Progress> = {}): Progress => ({
  agentId: "local:kaan", delegationId: 7, total: 7, done: 3, active: "Testleri koşuyor", updatedAt: 0, ...patch,
});
const job = (id: number, status: "running" | "done" | "queued" = "running", toAgent = "local:kaan") => ({ id, host: "local", status, toAgent });

test("yalnız çalışan işin dolu listesi ilerleme sayılır", () => {
  const all = new Map([[progressKey("local", 7), p()], [progressKey("local", 8), p({ delegationId: 8, total: 0, done: 0 })]]);
  assert.equal(jobProgress(job(7), all)?.done, 3);
  assert.equal(jobProgress(job(7, "done"), all), null, "biten iş");
  assert.equal(jobProgress(job(8), all), null, "boş liste");
  assert.equal(jobProgress({ ...job(7), host: "srv" }, all), null, "başka sunucunun #7'si");
});

test("worker'ın en yeni çalışan işi", () => {
  const all = new Map([[progressKey("local", 7), p()], [progressKey("local", 9), p({ delegationId: 9, done: 1 })]]);
  assert.equal(workerProgress("local:kaan", [job(7), job(9), job(10, "queued")], all)?.delegationId, 9);
  assert.equal(workerProgress("local:ayla", [job(7)], all), null);
});

test("yüzde ve satır", () => {
  assert.equal(percent({ done: 3, total: 7 }), 43);
  assert.equal(percent({ done: 0, total: 0 }), 0);
  assert.equal(ago(59_000), "az önce");
  assert.equal(ago(5 * 60_000), "5 dk önce");
  assert.equal(ago(125 * 60_000), "2 sa önce");
  assert.equal(progressLine(p(), 2 * 60_000), "%43 · 3/7 · Testleri koşuyor · 2 dk önce");
  assert.equal(progressLine(p({ active: null, done: 7 }), 0), "%100 · 7/7 · az önce");
});
