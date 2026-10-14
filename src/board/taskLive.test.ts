import assert from "node:assert/strict";
import { test } from "node:test";
import { doingRank, LIVE_LABEL, plainPreview, taskLive } from "./taskLive.ts";
import type { AgentStatus } from "../agents.ts";

const job = (patch = {}) => ({
  id: 1740, host: "local", taskId: 12, status: "running", toAgent: "local:pixel",
  modelApproval: null, ...patch,
});
const statusOf = (s: AgentStatus) => (_id: string) => s;
const task = (patch = {}) => ({ id: 12, status: "doing", ...patch });

test("çalışan ajan + running devretme → Çalışıyor ve iş numarası", () => {
  assert.deepEqual(taskLive(task(), "local", [job()], statusOf("working")),
    { kind: "working", delegationId: 1740 });
});

test("aktif devretme ama ajan çalışmıyorsa → Sırada", () => {
  for (const [d, s] of [
    [job({ status: "queued" }), "working"],
    [job(), "idle"],
    [job(), "offline"],
    [job({ toAgent: "" }), "idle"],
  ] as const)
    assert.equal(taskLive(task(), "local", [d], statusOf(s)).kind, "queued");
});

test("ajan soru soruyorsa ya da model onayı bekliyorsa → Patrondan bekliyor", () => {
  assert.equal(taskLive(task(), "local", [job()], statusOf("waiting")).kind, "patron");
  assert.equal(taskLive(task(), "local", [job({ modelApproval: "pending" })], statusOf("working")).kind, "patron");
});

test("devretme bitti ama görev açık → Birleştirme bekliyor; görev bittiyse rozet yok", () => {
  assert.deepEqual(taskLive(task(), "local", [job({ status: "done" })], statusOf("idle")),
    { kind: "merge", delegationId: 1740 });
  assert.deepEqual(taskLive(task({ status: "done" }), "local", [job({ status: "done" })], statusOf("idle")),
    { kind: null, delegationId: 1740 });
});

test("en yeni devretme kazanır; başka host/görev karışmaz; devretmesiz görevde numara yok", () => {
  const list = [job({ id: 1739, status: "done" }), job({ id: 1740 }), job({ id: 9999, taskId: 13 }), { ...job({ id: 1750, host: "remote" }) }];
  assert.deepEqual(taskLive(task(), "local", list, statusOf("working")), { kind: "working", delegationId: 1740 });
  assert.deepEqual(taskLive(task(), "local", [], statusOf("working")), { kind: null, delegationId: null });
  assert.deepEqual(taskLive(task(), "local", [job({ status: "failed" })], statusOf("idle")),
    { kind: null, delegationId: 1740 });
});

test("Sürüyor sıralaması: çalışan üstte, rozetsiz en altta", () => {
  const kinds = ["merge", null, "patron", "working", "queued"] as const;
  assert.deepEqual([...kinds].sort((a, b) => doingRank(a) - doingRank(b)),
    ["working", "queued", "patron", "merge", null]);
  assert.deepEqual(Object.keys(LIVE_LABEL).sort(), ["merge", "patron", "queued", "working"]);
});

test("önizleme: başlıklar atılır, biçim imleri temizlenir, ilk iki cümle", () => {
  assert.equal(plainPreview("## Patronun sözü\nGörevler ekranına el atalım. Sürüyorda 11 tane var.\nÜçüncü cümle."), "Görevler ekranına el atalım. Sürüyorda 11 tane var.…");
  assert.equal(plainPreview("## Amaç\n**Kalın** ve `kod` ile - liste\n1. ilk adım"), "Kalın ve kod ile - liste ilk adım");
  assert.equal(plainPreview("[metin](https://x) burada"), "metin burada");
  assert.equal(plainPreview("## Yalnız başlık\n"), "");
  assert.equal(plainPreview(""), "");
});

test("önizleme 220 karakteri aşmaz", () => {
  const long = `## Başlık\n${"cümle bir. ".repeat(60)}`;
  const out = plainPreview(long);
  assert.ok(out.length <= 221 && out.endsWith("…"));
});
