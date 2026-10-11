// #243: Core durum önceliği, uyku/ret zamanlayıcıları, agent.step ve balon tuşları.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  applyStep, balloonKey, coreSentence, ERROR_MS, moved, nextWake, pickState, readStripMode, SLEEP_MS, statusMap,
  writeStripMode, type CoreInputs, type KeyIn,
} from "./coreState.ts";

const T0 = 1_000_000;
const base = (o: Partial<CoreInputs> = {}): CoreInputs => ({
  status: "idle", delegated: 0, decisions: 0, workerAsks: 0, steps: [], agentError: null,
  errorUntil: 0, errorText: null, doneUntil: 0, doneText: null, lastEventAt: T0, now: T0, ...o,
});
const step = { tool: "Edit", label: "dosya yazıyor · world.ts", ts: T0 };

describe("pickState önceliği", () => {
  it("hata > soruyor > araç > devretti > düşünüyor > teslim > uyku > boşta", () => {
    const all = base({ status: "working", agentError: "kota", decisions: 1, steps: [step], delegated: 2, doneUntil: T0 + 1 });
    assert.equal(pickState(all), "error");
    assert.equal(pickState({ ...all, agentError: null }), "ask");
    assert.equal(pickState({ ...all, agentError: null, decisions: 0 }), "tool");
    assert.equal(pickState({ ...all, agentError: null, decisions: 0, steps: [] }), "delegated");
    assert.equal(pickState({ ...all, agentError: null, decisions: 0, steps: [], delegated: 0 }), "thinking");
    assert.equal(pickState(base({ doneUntil: T0 + 1 })), "done");
    assert.equal(pickState(base({ now: T0 + SLEEP_MS })), "sleep");
    assert.equal(pickState(base()), "idle");
  });
  it("hata ve soru birlikte: hata gösterilir (balon yine soruyu açar)", () => {
    assert.equal(pickState(base({ errorUntil: T0 + 5, decisions: 2 })), "error");
  });
  it("worker sorusu ve terminal beklemesi de soruyor", () => {
    assert.equal(pickState(base({ workerAsks: 1 })), "ask");
    assert.equal(pickState(base({ status: "waiting" })), "ask");
  });
  it("agent.step hiç gelmezse çalışan lider yalnız düşünüyor; boşta adım araç sayılmaz", () => {
    assert.equal(pickState(base({ status: "working" })), "thinking");
    assert.equal(pickState(base({ status: "idle", steps: [step] })), "idle");
  });
  it("kapalı oturum uyur", () => {
    assert.equal(pickState(base({ status: "offline" })), "sleep");
    assert.equal(coreSentence("sleep", base({ status: "offline" })), "oturum kapalı");
  });
});

describe("zamanlayıcılar", () => {
  it("uyku: 10 dk olaysız boşta; öncesinde değil, nextWake tam o an", () => {
    assert.equal(pickState(base({ now: T0 + SLEEP_MS - 1 })), "idle");
    assert.equal(pickState(base({ now: T0 + SLEEP_MS })), "sleep");
    assert.equal(nextWake(base()), T0 + SLEEP_MS);
    // Yeni olay sayacı sıfırlar.
    assert.equal(pickState(base({ lastEventAt: T0 + SLEEP_MS, now: T0 + SLEEP_MS + 1 })), "idle");
  });
  it("ret: ! 8 sn durur, sonra soru ya da boşta", () => {
    const until = T0 + ERROR_MS;
    assert.equal(ERROR_MS, 8000);
    assert.equal(pickState(base({ errorUntil: until, errorText: "öneri reddedildi", now: until - 1 })), "error");
    assert.equal(coreSentence("error", base({ errorUntil: until, errorText: "öneri reddedildi" })), "öneri reddedildi");
    assert.equal(pickState(base({ errorUntil: until, now: until })), "idle");
    assert.equal(pickState(base({ errorUntil: until, now: until, decisions: 1 })), "ask");
    assert.equal(nextWake(base({ errorUntil: until })), until);
  });
  it("geçiş tespiti: yalnız pending→rejected ve running→done sayılır, ilk görüş sayılmaz", () => {
    const prev = statusMap([{ key: "a", status: "pending" }, { key: "b", status: "approved" }]);
    const now = [{ key: "a", status: "rejected" }, { key: "b", status: "rejected" }, { key: "c", status: "rejected" }];
    assert.deepEqual(moved(prev, now, ["pending"], ["rejected"]).map((r) => r.key), ["a"]);
    const jobs = statusMap([{ key: "1", status: "running" }, { key: "2", status: "queued" }]);
    const after = [{ key: "1", status: "done" }, { key: "2", status: "failed" }];
    assert.deepEqual(moved(jobs, after, ["queued", "running"], ["done", "done_no_report"]).map((r) => r.key), ["1"]);
  });
});

describe("agent.step", () => {
  it("start araç durumuna ve etiketi cümleye; end önceki duruma döner", () => {
    let s = applyStep([], { agentId: "l", phase: "start", tool: "Edit", label: "dosya yazıyor · world.ts", ts: 1 });
    assert.equal(pickState(base({ status: "working", steps: s })), "tool");
    assert.equal(coreSentence("tool", base({ steps: s })), "dosya yazıyor · world.ts");
    s = applyStep(s, { agentId: "l", phase: "end", tool: "Edit", label: "", ok: true, ts: 2 });
    assert.deepEqual(s, []);
    assert.equal(pickState(base({ status: "working", steps: s, delegated: 1 })), "delegated");
  });
  it("paralel araçlar: biri bitince diğeri sürer, son etiket görünür", () => {
    let s = applyStep([], { agentId: "l", phase: "start", tool: "Bash", label: "komut · pnpm test", ts: 1 });
    s = applyStep(s, { agentId: "l", phase: "start", tool: "Read", label: "okuyor · a.ts", ts: 2 });
    s = applyStep(s, { agentId: "l", phase: "end", tool: "Bash", label: "", ts: 3 });
    assert.deepEqual(s.map((x) => x.tool), ["Read"]);
    assert.deepEqual(applyStep([], { agentId: "l", phase: "end", tool: "X", label: "", ts: 1 }), []);
  });
});

describe("balon tuşları", () => {
  const k = (key: string, o: Partial<KeyIn> = {}): KeyIn => ({ key, inText: false, onButton: false, ...o });
  const prop = { kind: "proposal" as const, options: 0 };
  const ask = { kind: "question" as const, options: 3 };
  it("öneri: E ekle, R reddet (büyük/küçük), başka harf yok", () => {
    assert.deepEqual(balloonKey(k("e"), prop), { click: "e" });
    assert.deepEqual(balloonKey(k("E"), prop), { click: "e" });
    assert.deepEqual(balloonKey(k("R"), prop), { click: "r" });
    assert.equal(balloonKey(k("y"), prop), null);
    assert.equal(balloonKey(k("1"), prop), null);
  });
  it("soru: 1–seçenek sayısı, fazlası yok; ↵ gönder (butonda değilken)", () => {
    assert.deepEqual(balloonKey(k("2"), ask), { click: "2" });
    assert.equal(balloonKey(k("4"), ask), null);
    assert.equal(balloonKey(k("0"), ask), null);
    assert.deepEqual(balloonKey(k("Enter"), ask), { click: "Enter" });
    assert.equal(balloonKey(k("Enter", { onButton: true }), ask), null);
    assert.equal(balloonKey(k("e"), ask), null);
  });
  it("metin alanında harf/rakam/↵ yazıya kalır; Esc yine erteler", () => {
    assert.equal(balloonKey(k("e", { inText: true }), prop), null);
    assert.equal(balloonKey(k("1", { inText: true }), ask), null);
    assert.equal(balloonKey(k("Enter", { inText: true }), ask), null);
    assert.equal(balloonKey(k("Escape", { inText: true }), ask), "defer");
    assert.equal(balloonKey(k("Escape"), prop), "defer");
  });
  it("değiştirici tuşlar (Ctrl+R vb.) bizim değil", () => {
    assert.equal(balloonKey(k("r", { ctrlKey: true }), prop), null);
    assert.equal(balloonKey(k("e", { altKey: true }), prop), null);
  });
});

describe("şerit tercihi (pane başına)", () => {
  it("varsayılan açık; pane'ler ayrı; bozuk değer açık", () => {
    let raw = writeStripMode("a", "compact", null);
    raw = writeStripMode("b", "hidden", raw);
    assert.equal(readStripMode("a", raw), "compact");
    assert.equal(readStripMode("b", raw), "hidden");
    assert.equal(readStripMode("c", raw), "open");
    assert.equal(readStripMode("a", writeStripMode("a", "open", raw)), "open");
    assert.equal(readStripMode("a", "{bozuk"), "open");
  });
});
