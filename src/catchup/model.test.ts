import { test } from "node:test";
import assert from "node:assert/strict";
import { ackWait, bucketCount, isEmpty, kindLabel, resolveRef, sections, SECTION_LIMIT, targetFor, visibleItems, waitingCount, whenAgo, type RefSources } from "./model.ts";
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

test("ref çözümü: her tür kendi tablosundan, çakışan devretme id'si yanlış ajanı açmaz (#2240)", () => {
  // GPU isteği #42, plan #42 ve öneri #42 ile aynı sayılı devretme #42 aynı anda var.
  const s: RefSources = {
    questions: [{ id: 7, worker: "qw", leader: "ql" }],
    proposals: [{ id: 9, leader: "pl" }],
    delegations: [{ id: 42, worker: "dw", leader: "dl" }, { id: 5, worker: "w5", leader: "l5" }],
  };
  // Başka tabloya ait ref'ler devretme listesinde aranmaz; GPU doğrudan Ayarlar › GPU'ya düşer.
  assert.deepEqual(resolveRef(item("gpu_approval", 42), s), {});
  assert.deepEqual(targetFor(item("gpu_approval", 42), resolveRef(item("gpu_approval", 42), s)), { type: "gpu" });
  assert.deepEqual(targetFor(item("gpu_approval", 42), { leader: "dl" }), { type: "gpu" });
  assert.deepEqual(resolveRef(item("plan_done", 42), s), {});
  assert.deepEqual(resolveRef(item("member_proposal", 42), s), {});
  assert.deepEqual(resolveRef(item("job_change_approval", 42), s), {});
  assert.deepEqual(resolveRef(item("memory", "shared/x"), s), {});
  // Kendi kaynağında bulunanlar çözülür.
  assert.deepEqual(resolveRef(item("question_open", 7), s), { worker: "qw", leader: "ql" });
  assert.deepEqual(resolveRef(item("member_proposal", 9), s), { leader: "pl" });
  assert.deepEqual(resolveRef(item("delegation_done", 42), s), { worker: "dw", leader: "dl" });
  assert.deepEqual(resolveRef(item("merge_waiting", 5), s), { worker: "w5", leader: "l5" });
  assert.deepEqual(resolveRef(item("review_fix", 5), s), { worker: "w5", leader: "l5" });
  // model_approval: önce öneri; önerisiz dalda çekirdek devretme id'si verir.
  assert.deepEqual(resolveRef(item("model_approval", 9), s), { leader: "pl" });
  assert.deepEqual(resolveRef(item("model_approval", 5), s), { worker: "w5", leader: "l5" });
  // Kaynağında yoksa boş (başka listeye kayma yok).
  assert.deepEqual(resolveRef(item("question_escalated", 42), s), {});
  assert.deepEqual(resolveRef(item("delegation_failed", 9), s), {});
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

test("ack bekleyişi yalnız görünür + odaklı + verili + ack'lenmemişken, değeri snapshot'ın generatedAt'i", () => {
  const base = { visible: true, focused: true, acked: false, generatedAt: 1000 };
  assert.equal(ackWait(base), 1000);
  assert.equal(ackWait({ ...base, visible: false }), null);
  assert.equal(ackWait({ ...base, focused: false }), null);
  assert.equal(ackWait({ ...base, acked: true }), null);
  assert.equal(ackWait({ ...base, generatedAt: undefined }), null);
});

test("ack yarış: bekleyiş ortasında yenilenen snapshot eski bekleyişle ack'lenmez (#2240)", () => {
  const base = { visible: true, focused: true, acked: false };
  // Panel T1 ile açıldı; sayaç bu değer için kuruldu.
  const armed = ackWait({ ...base, generatedAt: 1000 });
  assert.equal(armed, 1000);
  // 9. saniyede host olayı listeyi T2 ile değiştirdi: bekleyiş kimliği değişir
  // (sayaç T2 için baştan kurulur), kurulu değer T1 olarak kalır — ateş eden
  // sayaç ~1 sn görülen T2'yi değil, görülen T1'i ack'ler.
  const rearmed = ackWait({ ...base, generatedAt: 2000 });
  assert.notEqual(rearmed, armed);
  assert.equal(armed, 1000);
  assert.equal(rearmed, 2000);
});
