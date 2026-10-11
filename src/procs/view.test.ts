import { test } from "node:test";
import assert from "node:assert/strict";
import { agentRows, cpuText, keepOrder, killQuestion, memText, totals, type AgentProcess } from "./view.ts";

// #925: each agent's entry is { processes, error? }.
const listed = (m: Record<string, AgentProcess[]>) =>
  agentRows(Object.fromEntries(Object.entries(m).map(([a, processes]) => [a, { processes }])));

const p = (pid: number, x: Partial<AgentProcess> = {}): AgentProcess => ({
  pid, ppid: 1, name: `p${pid}.exe`, cmd: `p${pid}`, cpuPct: 0, memMb: 0, kind: "other", root: false, ...x,
});

test("agentRows: en ağır ajan üstte (CPU, eşitse RAM), boş liste düşer", () => {
  const rows = listed({
    a: [p(1, { root: true, cpuPct: 1, memMb: 100 })],
    b: [p(2, { root: true, cpuPct: 0.5 }), p(3, { cpuPct: 4, memMb: 50 })],
    c: [p(4, { root: true, cpuPct: 1, memMb: 300 })],
    shell: [],
  });
  assert.deepEqual(rows.map((r) => r.agent), ["b", "c", "a"]);
  assert.equal(rows[0].cpuPct, 4.5);
  assert.equal(rows[0].memMb, 50);
});

test("agentRows: kök önce, çocuklar yüke göre; araç ve tarayıcı ayrı sayılır", () => {
  const [row] = listed({
    a: [
      p(10, { root: true, kind: "node" }),
      p(11, { cpuPct: 0.1, memMb: 80, kind: "browser-tool" }),
      p(12, { cpuPct: 2, kind: "cargo" }),
      p(13, { cpuPct: 0.1, memMb: 400, kind: "browser" }),
    ],
  });
  assert.deepEqual(row.procs.map((x) => x.pid), [10, 12, 13, 11]);
  assert.equal(row.browsers, 1);
  assert.equal(row.browserTools, 1);
});

test("agentRows: toplam yuvarlaması kayan nokta artığı bırakmaz", () => {
  const [row] = listed({ a: [p(1, { root: true, cpuPct: 0.1 }), p(2, { cpuPct: 0.2 })] });
  assert.equal(row.cpuPct, 0.3);
});

test("agentRows: okunamayan ajan hatasıyla kalır, diğerleri etkilenmez", () => {
  const r = agentRows({
    a: { processes: [p(1, { root: true, cpuPct: 1 })] },
    b: { processes: [], error: "ajanın süreç listesi (job) okunamadı" },
    shell: { processes: [] },
  });
  assert.deepEqual(r.map((x) => [x.agent, x.error]), [["a", undefined], ["b", "ajanın süreç listesi (job) okunamadı"]]);
  assert.equal("error" in r[0], false);
  assert.deepEqual(r[1].procs, []);
});

test("totals: tüm ajanlar ve araç/tarayıcı payı", () => {
  const rows = listed({
    a: [p(1, { root: true, cpuPct: 1, memMb: 100 }), p(2, { kind: "browser", memMb: 600, cpuPct: 0.4 })],
    b: [p(3, { root: true, memMb: 200 }), p(4, { kind: "browser-tool", memMb: 90 })],
  });
  assert.deepEqual(totals(rows), { cpuPct: 1.4, memMb: 990, browsers: 1, browserMemMb: 600, browserTools: 1, browserToolMemMb: 90 });
  assert.deepEqual(totals([]), { cpuPct: 0, memMb: 0, browsers: 0, browserMemMb: 0, browserTools: 0, browserToolMemMb: 0 });
});

test("totals: renderer/GPU RAM ve CPU dahil, tarayıcı sayısı köklerden; PID kapsamı ajan", () => {
  const rows = listed({
    a: [p(1, { root: true, kind: "node" }), p(2, { kind: "browser-tool", memMb: 90 }),
      p(3, { ppid: 2, kind: "browser", memMb: 600, cpuPct: 0.1 }),
      p(4, { ppid: 3, kind: "browser", memMb: 100, cpuPct: 0.2 }),
      p(5, { ppid: 3, kind: "browser", memMb: 50 }),
      p(6, { ppid: 2, kind: "browser", memMb: 200 })],
    b: [p(3, { ppid: 4, kind: "browser", memMb: 50 })],
  });
  assert.equal(rows.find(r => r.agent === "a")?.browsers, 2);
  assert.equal(rows.find(r => r.agent === "b")?.browsers, 1);
  assert.deepEqual(totals(rows), { cpuPct: 0.3, memMb: 1090, browsers: 3, browserMemMb: 1000, browserTools: 1, browserToolMemMb: 90 });
});

test("biçim: CPU yüzdesi, MB / GB", () => {
  assert.equal(cpuText(3), "%3.0");
  assert.equal(memText(512.4), "512 MB");
  assert.equal(memText(2048), "2.0 GB");
});

test("killQuestion: ne kapanacağını ve ajanın süreceğini söyler", () => {
  const q = killQuestion("Pixel", p(42, { kind: "browser", name: "chrome.exe", memMb: 300, cmd: "chrome.exe --headless" }));
  assert.match(q, /^Pixel ajanının alt süreci kapatılsın mı\?/);
  assert.match(q, /Tarayıcı · chrome\.exe \(pid 42\) · 300 MB/);
  assert.match(q, /chrome\.exe --headless/);
  assert.match(q, /Ajan çalışmaya devam eder/);
});

test("keepOrder: yenilemede ajan ve süreç sırası korunur, yeni gelen sona, giden düşer", () => {
  const first = listed({
    a: [p(1, { root: true, cpuPct: 5 }), p(2, { cpuPct: 3 }), p(3, { cpuPct: 1 })],
    b: [p(4, { root: true, cpuPct: 2 })],
  });
  assert.equal(keepOrder(null, first), first);
  const next = keepOrder(first, listed({
    a: [p(1, { root: true, cpuPct: 0 }), p(3, { cpuPct: 9 }), p(5, { cpuPct: 8 }), p(2, { cpuPct: 0 })],
    b: [p(4, { root: true, cpuPct: 20 })],
    c: [p(6, { root: true, cpuPct: 50 })],
    d: [p(7, { root: true, cpuPct: 1 })],
  }));
  assert.deepEqual(next.map((r) => r.agent), ["a", "b", "c", "d"]);
  assert.deepEqual(next[0].procs.map((x) => x.pid), [1, 2, 3, 5]);
  assert.equal(next[1].cpuPct, 20); // sayılar tazelenir
  const gone = keepOrder(next, listed({ b: [p(4, { root: true })], a: [p(1, { root: true }), p(5, {})] }));
  assert.deepEqual(gone.map((r) => [r.agent, r.procs.map((x) => x.pid)]), [["a", [1, 5]], ["b", [4]]]);
});

test("agentRows: bilinmeyen kind diğer olur, onay metni de diğer der", () => {
  for (const kind of ["future-kind", "playwright", "toString", "__proto__"]) {
    const [row] = listed({ a: [p(1, { kind: kind as AgentProcess["kind"] })] });
    assert.equal(row.procs[0].kind, "other");
    assert.match(killQuestion("Pixel", row.procs[0]), /Diğer ·/);
  }
});
