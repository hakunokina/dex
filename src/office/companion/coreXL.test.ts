// #252: Core XL — gövde geometrisi/iz noktası/hit kutusu, göz bebeği matematiği, kırpma, durum eşlemesi.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { mixColor, XL_BODY } from "./coreEngine.ts";
import {
  approach, BLINK_MS, blinks, coreBody, corePort, CoreTracker, CoreXL, eyeAt, gazeGoal, GAZE_MS, HOLD_MS, lidAt, nextBlink,
  PUPIL_MAX, pupilToward, RETURN_RATE, XL_LABEL, XL_MIN, XL_PIN, XL_TARGET_H, type CoreSnapshot, type Pointer,
} from "./coreXL.ts";
import { DONE_MS, ERROR_MS, SLEEP_MS } from "./coreState.ts";

const near = (a: number, b: number, eps = 1e-6) => assert.ok(Math.abs(a - b) < eps, `${a} ≉ ${b}`);
const len = (v: { x: number; y: number }) => Math.hypot(v.x, v.y);

describe("coreBody / corePort (spec §2, §5)", () => {
  it("tek lider hedefi (132×160) → 120 birim gövde, pinler ve etiket kutuda", () => {
    const b = coreBody({ x: 480, y: 270, w: 132, h: XL_TARGET_H })!;
    assert.equal(b.s, XL_BODY);
    near(b.y - b.s / 2 - XL_PIN, 270 - XL_TARGET_H / 2); // üst pinler kutunun üstünde
    near(b.y + b.s / 2 + XL_PIN + XL_LABEL, 270 + XL_TARGET_H / 2); // etiketler altta
  });
  it("dar hücrede küçülür; çok küçükse null (eski çip kalır)", () => {
    assert.equal(coreBody({ x: 0, y: 0, w: 80, h: 160 })!.s, 80 - 2 * XL_PIN);
    assert.equal(coreBody({ x: 0, y: 0, w: 132, h: 2 * XL_PIN + XL_LABEL + XL_MIN - 1 }), null);
  });
  it("iz noktası gövdenin yan kenarında, gövde merkezi etrafına yayılır", () => {
    const b = { x: 100, y: 50, s: 120 };
    assert.deepEqual(corePort(b, -1, 0, 1), { x: 40, y: 50 });
    assert.deepEqual(corePort(b, 1, 0, 1), { x: 160, y: 50 });
    const top = corePort(b, 1, 0, 3), bottom = corePort(b, 1, 2, 3);
    near(top.y, 50 - 9);
    near(bottom.y, 50 + 9);
    // çok worker: yayılım gövdeyi aşmaz
    const many = corePort(b, 1, 29, 30);
    assert.ok(many.y <= b.y + b.s / 2);
  });
  it("hit kutusu = hedef kutu: gövde + pin payı + etiketler (world.hit kare testi aynen)", () => {
    const box = { x: 480, y: 270, w: 132, h: XL_TARGET_H };
    const b = coreBody(box)!;
    const inBox = (x: number, y: number) => Math.abs(x - box.x) <= box.w / 2 && Math.abs(y - box.y) <= box.h / 2;
    assert.ok(inBox(b.x + b.s / 2 + XL_PIN, b.y)); // sağ pin ucu
    assert.ok(inBox(b.x, b.y - b.s / 2 - XL_PIN)); // üst pin ucu
    assert.ok(inBox(b.x, b.y + b.s / 2 + XL_PIN + 20)); // isim satırı
    assert.ok(!inBox(b.x + b.s / 2 + XL_PIN + 1, b.y));
  });
});

describe("göz bebeği (spec §4)", () => {
  it("yön imlece; sapma ≤ 4; yakında merkeze yumuşar", () => {
    const far = pupilToward({ x: 0, y: 0 }, { x: 500, y: 10 });
    near(len(far), PUPIL_MAX);
    assert.ok(far.x > 3.99 && Math.abs(far.y) < 0.1); // neredeyse yatay imleç → yatay bakış (eksen kırpması değil)
    assert.ok(len(pupilToward({ x: 0, y: 0 }, { x: 6, y: 0 })) < 1);
    assert.deepEqual(pupilToward({ x: 5, y: 5 }, { x: 5, y: 5 }), { x: 0, y: 0 });
    // dünya → gövde birimi: küçük gövdede aynı uzaklık daha uzak sayılır
    assert.ok(len(pupilToward({ x: 0, y: 0 }, { x: 24, y: 0 }, 0.5)) > len(pupilToward({ x: 0, y: 0 }, { x: 24, y: 0 }, 1)));
  });
  it("yumuşatma 1−e^(−dt·k); sonsuz hız = anında (reduced motion)", () => {
    near(approach(0, 4, 0.1, 10), 4 * (1 - Math.exp(-1)));
    assert.equal(approach(0, 4, 0.016, Infinity), 4);
  });
  const eye = { x: 0, y: 0 };
  const ptr = (o: Partial<Pointer>): Pointer => ({ x: 100, y: 0, inside: true, inWindow: true, at: 0, ...o });
  const goal = (o: Partial<Parameters<typeof gazeGoal>[0]>) =>
    gazeGoal({ id: "idle", eye, scale: 1, pointer: null, look: null, now: 0, reduced: false, ...o });
  it("canvas üstünde imleci izler", () => {
    const g = goal({ pointer: ptr({}) });
    near(g.to.x, 4);
    assert.equal(g.rate, 10);
  });
  it("canvas dışında 1.5 sn son yön tutulur, sonra yavaş merkeze", () => {
    const out = ptr({ inside: false, at: 1000 });
    near(goal({ pointer: out, now: 1000 + HOLD_MS - 1 }).to.x, 4);
    const back = goal({ pointer: out, now: 1000 + HOLD_MS });
    assert.deepEqual(back.to, { x: 0, y: 0 });
    assert.equal(back.rate, RETURN_RATE);
  });
  it("pencere dışında: tutma yok, hemen yavaş dönüş", () => {
    const g = goal({ pointer: ptr({ inside: false, inWindow: false, at: 1000 }), now: 1001 });
    assert.deepEqual(g.to, { x: 0, y: 0 });
    assert.equal(g.rate, RETURN_RATE);
  });
  it("worker bakışı imleci ezer; araç = aşağı; uyku/teslim/hata gözbebeksiz", () => {
    near(goal({ pointer: ptr({ x: 100 }), look: { x: -100, y: 0 } }).to.x, -4);
    assert.deepEqual(goal({ id: "tool", pointer: ptr({}) }).to, { x: 0, y: 3 });
    for (const id of ["sleep", "done", "error"] as const) assert.deepEqual(goal({ id, pointer: ptr({}) }).to, { x: 0, y: 0 });
  });
  it("düşünürken tarama ±3; reduced motion'da tarama yok, imleç anında", () => {
    assert.ok(Math.abs(goal({ id: "thinking", now: 300 }).to.x - 3) < 1e-9);
    const r = goal({ id: "thinking", now: 300, reduced: true, pointer: ptr({}) });
    near(r.to.x, 4);
    assert.equal(r.rate, Infinity);
  });
  it("tek göz gövdenin merkezindedir ve gövdeyle ölçeklenir", () => {
    assert.deepEqual(eyeAt({ x: 100, y: 100, s: 120 }), { x: 100, y: 100 });
    assert.deepEqual(eyeAt({ x: 100, y: 100, s: 60 }), { x: 100, y: 100 });
  });
});

describe("kırpma (spec §4)", () => {
  it("120 ms kapak; aralık 2.8–5.2 sn; soru/uyku/reduced'da yok", () => {
    assert.equal(lidAt(1000, -1), 0);
    near(lidAt(1000 + BLINK_MS / 2, 1000), 1);
    assert.equal(lidAt(1000 + BLINK_MS, 1000), 0);
    assert.equal(nextBlink(0, 0), 2800);
    assert.equal(nextBlink(0, 1), 5200);
    assert.equal(blinks("idle", false), true);
    assert.equal(blinks("ask", false), false);
    assert.equal(blinks("sleep", false), false);
    assert.equal(blinks("idle", true), false);
  });
});

const snap = (o: Partial<CoreSnapshot> = {}): CoreSnapshot => ({
  status: "idle", jobs: [], proposals: [], escalated: 0, workerAsks: 0, steps: [], agentError: null, ...o,
});

describe("CoreTracker — durum eşlemesi (strip'le aynı öncelik)", () => {
  const T = 1_000_000;
  it("8 durum", () => {
    const at = (o: Partial<CoreSnapshot>) => new CoreTracker().observe(snap(o), T, 0);
    assert.equal(at({}), "idle");
    assert.equal(at({ status: "working" }), "thinking");
    assert.equal(at({ status: "working", steps: [{ tool: "Bash", label: "ls", ts: 1 }] }), "tool");
    assert.equal(at({ jobs: [{ id: 1, toAgent: "w", status: "running" }] }), "delegated");
    assert.equal(at({ escalated: 1 }), "ask");
    assert.equal(at({ proposals: [{ key: "p1", status: "pending" }] }), "ask");
    assert.equal(at({ status: "waiting" }), "ask");
    assert.equal(at({ agentError: "kota" }), "error");
    assert.equal(at({ status: "offline" }), "sleep");
  });
  it("öncelik: hata > soru > araç > devretti > düşünüyor", () => {
    const t = new CoreTracker();
    const busy = { status: "working" as const, steps: [{ tool: "Bash", label: "ls", ts: 1 }], jobs: [{ id: 1, toAgent: "w", status: "running" }] };
    assert.equal(t.observe(snap({ ...busy, escalated: 1, agentError: "x" }), T, 0), "error");
    assert.equal(t.observe(snap({ ...busy, escalated: 1 }), T, 0), "ask");
    assert.equal(t.observe(snap(busy), T, 0), "tool");
  });
  it("araç adımları yalnız tur sürerken sayılır", () => {
    assert.equal(new CoreTracker().observe(snap({ status: "idle", steps: [{ tool: "Bash", label: "", ts: 1 }] }), T, 0), "idle");
  });
  it("teslim → ✓ 5 sn + worker'a 1.2 sn bakış; devretme → bakış", () => {
    const t = new CoreTracker();
    t.observe(snap({ jobs: [{ id: 1, toAgent: "w1", status: "running" }] }), T, 0); // ilk bakış: geçmiş, bakış yok
    assert.equal(t.look, null);
    t.observe(snap({ jobs: [{ id: 1, toAgent: "w1", status: "running" }, { id: 2, toAgent: "w2", status: "queued" }] }), T, 100);
    assert.deepEqual(t.look, { worker: "w2", until: 100 + GAZE_MS });
    const state = t.observe(snap({ jobs: [{ id: 1, toAgent: "w1", status: "done" }, { id: 2, toAgent: "w2", status: "failed" }] }), T + 10, 200);
    assert.equal(state, "done");
    assert.deepEqual(t.look, { worker: "w1", until: 200 + GAZE_MS });
    assert.equal(t.observe(snap({ jobs: [{ id: 1, toAgent: "w1", status: "done" }] }), T + 10 + DONE_MS, 200 + GAZE_MS), "idle");
    assert.equal(t.look, null);
  });
  it("öneri reddi / balon hatası → ! 8 sn", () => {
    const t = new CoreTracker();
    t.observe(snap({ proposals: [{ key: "p", status: "pending" }] }), T, 0);
    assert.equal(t.observe(snap({ proposals: [{ key: "p", status: "rejected" }] }), T, 0), "error");
    assert.equal(t.observe(snap({ proposals: [{ key: "p", status: "rejected" }] }), T + ERROR_MS, 0), "idle");
    t.fail(T + ERROR_MS);
    assert.equal(t.observe(snap(), T + ERROR_MS + 1, 0), "error");
  });
  it("10 dk olaysız → uyku; değişim since/prev'i günceller", () => {
    const t = new CoreTracker();
    t.observe(snap(), T, 0);
    assert.equal(t.observe(snap(), T + SLEEP_MS, 5000), "sleep");
    assert.equal(t.prev, "idle");
    assert.equal(t.since, 5000);
  });
});

describe("CoreXL.step", () => {
  const body = { x: 480, y: 256, s: 120 };
  const base = { now: 1, dt: 0.05, body, workerAt: () => null, hovered: false, reduced: false, rand: () => 0 };
  it("göz bebekleri imlece yaklaşır, reduced'da anında oturur", () => {
    const c = new CoreXL();
    const pointer: Pointer = { x: 900, y: 256, inside: true, inWindow: true, at: 0 };
    c.step({ ...base, snap: snap(), anim: 0, pointer });
    assert.ok(c.pupil.x > 0 && c.pupil.x < 4);
    const r = new CoreXL();
    r.step({ ...base, snap: snap(), anim: 0, pointer, reduced: true });
    near(len(r.pupil), 4, 1e-3);
  });
  it("devretmede bakış worker'a döner", () => {
    const c = new CoreXL();
    const worker = { x: 100, y: 256 };
    c.step({ ...base, snap: snap(), anim: 0, pointer: null });
    for (let i = 1; i <= 10; i++) {
      c.step({ ...base, snap: snap({ jobs: [{ id: 1, toAgent: "w", status: "running" }] }), anim: i * 50, pointer: null, workerAt: () => worker });
    }
    assert.ok(c.pupil.x < -3);
  });
  it("kırpma zamanında kapak iner, soruda inmez", () => {
    const c = new CoreXL();
    c.step({ ...base, snap: snap(), anim: 0, pointer: null }); // sıradaki kırpma: 2800
    c.step({ ...base, snap: snap(), anim: 2800, pointer: null });
    c.step({ ...base, snap: snap(), anim: 2860, pointer: null });
    near(c.lidNow, 1);
    const a = new CoreXL();
    for (const anim of [0, 2800, 2860]) a.step({ ...base, snap: snap({ escalated: 1 }), anim, pointer: null });
    assert.equal(a.lidNow, 0);
  });
});

describe("XL color transition", () => {
  it("renk lerp hex; hex değilse hedefe atlar", () => {
    assert.equal(mixColor("#000000", "#ffffff", 0.5), "rgb(128,128,128)");
    assert.equal(mixColor("#000", "#fff", 0), "rgb(0,0,0)");
    assert.equal(mixColor("oklch(1 0 0)", "#fff", 0.5), "#fff");
    assert.equal(mixColor("#000", "#fff", 1), "#fff");
  });
});
