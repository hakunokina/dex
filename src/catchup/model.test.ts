import { test } from "node:test";
import assert from "node:assert/strict";
import { ackDue, bucketCount, isEmpty, kindLabel, sections, SECTION_LIMIT, targetFor, visibleItems, waitingCount, whenAgo } from "./model.ts";
import type { Catchup, CatchupItem, CatchupKind } from "../api/catchup.ts";

const item = (kind: CatchupKind, ref: number | string = 1): CatchupItem => ({ kind, id: `${kind}:${ref}`, title: "t", at: 0, ref });
const bucket = (items: CatchupItem[], more = 0) => ({ items, more });
const catchup = (o: Partial<Catchup> = {}): Catchup => ({
  teamId: "t", since: 10, cursor: null, generatedAt: 20,
  waiting: bucket([]), changed: bucket([]), memories: bucket([]), ...o,
});

test("bölüm sırası: Seni bekleyenler, Sen yokken değişenler, Yeni", () => {
  const c = catchup();
  assert.deepEqual(sections(c).map(s => s.key), ["waiting", "changed", "memories"]);
  assert.deepEqual(sections(c).map(s => s.title), ["Seni bekleyenler", "Sen yokken değişenler", "Yeni"]);
});

test("rozet yalnız bekleyenleri sayar, more dahil", () => {
  const c = catchup({
    waiting: bucket([item("question_escalated"), item("merge_waiting")], 3),
    changed: bucket([item("delegation_done")], 9),
    memories: bucket([item("memory", "shared/x")]),
  });
  assert.equal(waitingCount(c), 5);
  assert.equal(bucketCount(c.changed), 10);
  assert.equal(isEmpty(c), false);
  assert.equal(isEmpty(catchup()), true);
  assert.equal(isEmpty(catchup({ memories: bucket([], 1) })), false);
});

test("+N daha: katlı sınır, açılınca yalnız sunucunun more'u saklı kalır", () => {
  const b = bucket(Array.from({ length: SECTION_LIMIT + 4 }, (_, i) => item("delegation_done", i)), 7);
  const folded = visibleItems(b, false);
  assert.equal(folded.shown.length, SECTION_LIMIT);
  assert.equal(folded.hidden, 4 + 7);
  const open = visibleItems(b, true);
  assert.equal(open.shown.length, SECTION_LIMIT + 4);
  assert.equal(open.hidden, 7);
  assert.equal(visibleItems(bucket([item("memory", "a")]), false).hidden, 0);
});

test("tıklama hedefi: panel düzeyi eşleme", () => {
  const both = { worker: "w1", leader: "l1" };
  assert.deepEqual(targetFor(item("delegation_done"), both), { type: "agent", agentId: "w1" });
  assert.deepEqual(targetFor(item("merge_waiting"), {}), { type: "board" });
  assert.deepEqual(targetFor(item("model_approval"), { leader: "l1" }), { type: "agent", agentId: "l1" });
  assert.deepEqual(targetFor(item("member_proposal"), both), { type: "agent", agentId: "l1" });
  assert.deepEqual(targetFor(item("question_escalated"), both), { type: "agent", agentId: "w1" });
  assert.deepEqual(targetFor(item("question_open"), {}), { type: "board" });
  assert.deepEqual(targetFor(item("gpu_approval"), {}), { type: "gpu" });
  assert.deepEqual(targetFor(item("plan_done"), both), { type: "board" });
  assert.deepEqual(targetFor(item("memory", "shared/x"), both), { type: "memory" });
});

test("etiketler Türkçe; bilinmeyen tür imzalısı kalır", () => {
  assert.equal(kindLabel("question_escalated"), "Soru sana çıktı");
  assert.equal(kindLabel("delegation_merged"), "Birleşti");
  assert.equal(kindLabel("yeni_tur"), "yeni_tur");
});

test("göreli zaman: az önce / dk / sa / gün, gelecek az önceye kısılır", () => {
  const now = 10 * 86_400_000;
  assert.equal(whenAgo(now + 1000, now), "az önce");
  assert.equal(whenAgo(now - 5 * 60_000, now), "5 dk önce");
  assert.equal(whenAgo(now - 3 * 3_600_000, now), "3 sa önce");
  assert.equal(whenAgo(now - 2 * 86_400_000, now), "2 gün önce");
});

test("ack yalnız görünür + odaklı + verili + ack'lenmemişken", () => {
  const base = { visible: true, focused: true, acked: false, hasData: true };
  assert.equal(ackDue(base), true);
  assert.equal(ackDue({ ...base, visible: false }), false);
  assert.equal(ackDue({ ...base, focused: false }), false);
  assert.equal(ackDue({ ...base, acked: true }), false);
  assert.equal(ackDue({ ...base, hasData: false }), false);
});
