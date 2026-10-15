// #1958: Kart A · Doku — durum → ışık (chipLook önceliği, ≤ 3 Hz, azaltılmış hareket), gövde/etiket
// geometrisi, bağ damarının yumuşatılması, GL çözünürlüğü.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  beatAt, chaikin, clockAt, coreVeins, dropAt, ECHO_BESIDE, echoBeside, echoDrift, echoLight, facingOf, footprint, glScale, GL_MAX_PIXELS, GL_MAX_SCALE,
  keyAzimuth, labelRect, LASER, laserAngle, lasing, lifeOf, linear, LOOK, markAlpha, markAt, pathLength, pointAt, resample, routeToBody, sacLook,
  smoothRoute, spotOf, strandTone, textBlock, workerLight,
} from "./symbioteCard.ts";

const near = (a: number, b: number, eps = 1e-6) => assert.ok(Math.abs(a - b) < eps, `${a} ≉ ${b}`);

/** Local maxima per second of a rhythm, sampled at 1 kHz over 12 s. */
function peaksPerSecond(f: (t: number) => number) {
  let peaks = 0;
  let prev = f(0), cur = f(0.001);
  for (let i = 2; i <= 12000; i++) {
    const next = f(i / 1000);
    if (cur > prev && cur >= next) peaks++;
    prev = cur; cur = next;
  }
  return peaks / 12;
}

describe("workerLight (chipLook önceliği)", () => {
  const done = { status: "done" }, failed = { status: "failed" }, other = { status: "done_no_report" };
  const look = (status: string, result: { status: string } | null, question: unknown) => {
    const l = workerLight(status, result, question, 0, false);
    return [l.tone, l.mark];
  };
  it("sonuç > soru/bekliyor > raporsuz > çalışıyor > boşta", () => {
    assert.deepEqual(look("working", done, { id: 1 }), ["green", "✓"]);
    assert.deepEqual(look("working", failed, { id: 1 }), ["redHi", "!"]);
    assert.deepEqual(look("working", other, { id: 1 }), ["amber", "?"]);
    assert.deepEqual(look("waiting", other, null), ["amber", "?"]);
    assert.deepEqual(look("working", other, null), ["amber", "!"]);
    assert.deepEqual(look("working", null, null), ["red", null]);
    assert.deepEqual(look("idle", null, null), ["muted", null]);
    assert.equal(workerLight("idle", failed, null, 0, false).error, true);
  });
  it("kapalı: gövde ışığı söner, okunmamış sonucun işareti rengiyle kalır", () => {
    const off = workerLight("offline", null, null, 0, false);
    assert.deepEqual([off.k, off.pulse, off.offline, off.mark], [0, 0, true, null]);
    const unread = workerLight("offline", done, null, 3, false);
    assert.deepEqual([unread.k, unread.offline, unread.mark, unread.tone], [0, true, "✓", "green"]);
  });
  it("boşta ışığı nokta ışığı eşiğinin (0.3) altında; çalışan ve sonuçlar üstünde", () => {
    assert.ok(workerLight("idle", null, null, 0, false).k <= 0.3);
    for (const l of [workerLight("working", null, null, 0, false), workerLight("idle", done, null, 0, false), workerLight("idle", failed, null, 0, false)]) {
      assert.ok(l.k > 0.3);
    }
  });
  it("hiçbir ritim 3 Hz'i geçmez (WCAG 2.3.1)", () => {
    const states: Array<[string, { status: string } | null, unknown]> = [
      ["idle", done, null], ["idle", failed, null], ["waiting", null, null], ["working", null, null], ["idle", other, null], ["idle", null, null],
    ];
    for (const [status, result, question] of states) {
      const hz = peaksPerSecond((t) => workerLight(status, result, question, t, false).pulse);
      assert.ok(hz <= 3, `${status}/${result?.status}: ${hz} Hz`);
    }
    for (const status of ["working", "waiting", "idle"]) {
      const hz = peaksPerSecond((t) => echoLight(status, t, false).pulse);
      assert.ok(hz <= 3, `Echo ${status}: ${hz} Hz`);
    }
    assert.ok(peaksPerSecond((t) => markAlpha("?", t, false)) <= 3);
    assert.ok(peaksPerSecond((t) => beatAt(t, false)) <= 3);
  });
  it("azaltılmış harekette ritim durur (nabız 1, kapalıda 0) ve zamandan bağımsız", () => {
    for (const t of [0, 1.3, 7.7]) {
      assert.equal(workerLight("working", null, null, t, true).pulse, 1);
      assert.equal(workerLight("idle", done, null, t, true).pulse, 1);
      assert.equal(workerLight("waiting", null, null, t, true).pulse, 1);
      assert.equal(workerLight("offline", null, null, t, true).pulse, 0);
      assert.equal(echoLight("working", t, true).pulse, 1);
      assert.deepEqual(echoDrift(t, true), echoDrift(0, true));
      assert.equal(beatAt(t, true), 0.3);
      assert.equal(keyAzimuth(t, true), keyAzimuth(0, true));
      assert.equal(markAlpha("?", t, true), 1);
      assert.equal(dropAt({ id: 1 }, null, t, true), null);
    }
  });
});

describe("canlılık (#2020: yalnız çalışan yol kırmızı)", () => {
  const done = { status: "done" }, failed = { status: "failed" };
  const life = (status: string, result: { status: string } | null, question: unknown, job = false) =>
    lifeOf(workerLight(status, result, question, 0, true), job);
  it("çalışan, soru, bekleyen, okunmamış sonuç ve süren iş canlı; boşta ve kapalı kuru", () => {
    for (const l of [life("working", null, null), life("waiting", null, null), life("idle", null, { id: 1 }),
      life("idle", done, null), life("idle", failed, null), life("idle", { status: "done_no_report" }, null), life("idle", null, null, true)]) {
      assert.equal(l, 1);
    }
    assert.equal(life("idle", null, null), 0);
    assert.equal(life("offline", null, null), 0);
    // Kapalı gövde karanlık ama okunmamış sonucu damarda okunur.
    assert.equal(life("offline", done, null), 1);
  });
  it("canlı kese şişer, parlar, zemine hale verir; kuru kese küçük, sönük, gri, halesiz", () => {
    const live = sacLook(workerLight("working", null, null, 0, true), 1);
    const idleLight = workerLight("idle", null, null, 0, true);
    const dry = sacLook(idleLight, lifeOf(idleLight, false));
    assert.ok(live.size > 1 && dry.size < 1);
    assert.ok(live.intensity / dry.intensity >= 6, `${live.intensity} / ${dry.intensity}`);
    assert.ok(live.halo > 0 && dry.halo === 0);
    assert.ok(dry.desat > 0.5 && live.desat === 0 && dry.dim > 0 && live.dim === 0);
    // Soru / teslim / hata çalışanla aynı ölçekte belirgin.
    for (const [r, q] of [[done, null], [failed, null], [null, { id: 1 }]] as const) {
      const l = workerLight("idle", r, q, 0, true);
      assert.ok(sacLook(l, lifeOf(l, false)).intensity >= live.intensity * 0.9);
    }
  });
  it("kapalı kese ışıksız ve batık; azaltılmış harekette çalışan kese şişkin ve sabit", () => {
    const off = sacLook(workerLight("offline", null, null, 0, false), 0);
    assert.deepEqual([off.intensity, off.halo, off.size, off.alpha], [0, 0, LOOK.worker.sink, 0.72]);
    const still = [0, 0.4, 2.2].map((t) => sacLook(workerLight("working", null, null, t, true), 1));
    for (const s of still) assert.deepEqual(s, still[0]);
    assert.ok(still[0].size >= 1 + LOOK.worker.swell - 1e-9);
  });
  it("çalışan kesenin atışı ≤ 3 Hz", () => {
    assert.ok(peaksPerSecond((t) => sacLook(workerLight("working", null, null, t, false), 1).size) <= 3);
  });
});

describe("damar rengi ve lazer (#2036: damar hep kırmızı, takım rengi yalnız Core gözünde)", () => {
  const done = { status: "done" }, failed = { status: "failed" };
  const tone = (status: string, result: { status: string } | null, question: unknown, dropping = false) =>
    strandTone(workerLight(status, result, question, 0, true).tone, question, result, dropping);
  it("çalışan, boşta ve kapalı damar tema kırmızısı; takım tonu yok", () => {
    for (const t of [tone("working", null, null), tone("idle", null, null), tone("offline", null, null), strandTone(null, null, null, false)]) {
      assert.equal(t, "red");
    }
  });
  it("soru / teslim / hata durum renkleri korunur; yürüyen damla kendi rengini taşır", () => {
    assert.equal(tone("idle", null, { id: 1 }), "amber");
    assert.equal(tone("idle", done, null), "green");
    assert.equal(tone("idle", failed, null), "redHi");
    assert.equal(tone("idle", null, { id: 1 }, true), "amber");
    assert.equal(tone("idle", done, null, true), "green");
    assert.equal(tone("idle", failed, null, true), "redHi");
    assert.equal(tone("idle", { status: "done_no_report" }, null, true), "amber");
  });
  it("lazer yalnız çalışan kesede; soru, sonuç, boşta, kapalı ve uyanan kesede yok", () => {
    const has = (status: string, result: { status: string } | null, question: unknown, job = false) => {
      const l = workerLight(status, result, question, 0, false);
      return lasing(l, lifeOf(l, job));
    };
    assert.equal(has("working", null, null), true);
    for (const off of [has("idle", null, null), has("offline", null, null), has("waiting", null, null), has("working", null, { id: 1 }),
      has("working", done, null), has("working", failed, null)]) assert.equal(off, false);
    const waking = { ...workerLight("offline", null, null, 0, false), tone: "red" as const, k: 0.4, offline: false };
    assert.equal(lasing(waking, lifeOf(waking, false)), false);
  });
  it(`lazer sakin döner (1 tur / ${LASER.period} sn), azaltılmış harekette durur`, () => {
    assert.ok(LASER.period >= 1.5 && LASER.period <= 2);
    const a = laserAngle(0, false, 3), b = laserAngle(LASER.period / 4, false, 3);
    near((b - a + Math.PI * 2) % (Math.PI * 2), Math.PI / 2, 1e-9);
    near(laserAngle(LASER.period * 7, false, 3), a, 1e-9);
    for (const t of [0, 0.4, 9.3]) assert.equal(laserAngle(t, true, 3), laserAngle(0, true, 3));
    // Komşu keseler aynı açıda başlamaz.
    assert.notEqual(laserAngle(0, false, 1), laserAngle(0, false, 2));
  });
});

describe("echo", () => {
  it("konuşuyor > bekliyor > boşta > kapalı (şiddet)", () => {
    const k = (s: string) => echoLight(s, 0, true).intensity;
    assert.ok(k("working") > k("waiting") && k("waiting") > k("idle") && k("idle") > 0);
    assert.equal(k("offline"), 0);
  });
  it("süzülür: zeminden 3 birim yukarıda ±2.2, hafif sallanır", () => {
    for (let t = 0; t < 10; t += 0.37) {
      const d = echoDrift(t, false);
      assert.ok(d.dy <= -0.8 + 1e-9 && d.dy >= -5.2 - 1e-9);
      assert.ok(Math.abs(d.rot) <= 0.07 + 1e-9);
    }
  });
});

describe("gövde ve etiket", () => {
  const slot = { x: 100, y: 50, w: 98, h: 48 };
  it("sütunda gövde Core'a bakan uçta, etiket dışta (sol sütunda sağa hizalı)", () => {
    const left = spotOf(slot, facingOf(slot, { x: 300 }, "columns", false), false);
    assert.equal(left.facing, 1);
    assert.ok(left.bx + left.D / 2 <= slot.x + slot.w / 2 && left.bx > slot.x);
    assert.ok(left.textR <= left.bx - left.D / 2 && left.textL === slot.x - slot.w / 2);
    assert.equal(left.align, "right");
    const right = spotOf(slot, facingOf(slot, { x: 0 }, "columns", false), false);
    assert.equal(right.facing, -1);
    assert.ok(right.bx < slot.x && right.textL >= right.bx + right.D / 2 && right.textR === slot.x + slot.w / 2);
    assert.equal(right.align, "left");
  });
  it("sıra modu ve Echo yönsüz; Echo yuvanın %40'ı, kare", () => {
    assert.equal(facingOf(slot, { x: 300 }, "stack", false), 0);
    assert.equal(facingOf(slot, { x: 300 }, "columns", true), 0);
    const echo = spotOf({ x: 0, y: 0, w: 132, h: 44 }, 0, true);
    near(echo.D, Math.min(132 * 0.4, 44 * 0.94));
    near(echo.fh, echo.D);
  });
  it("gövde yuvaya sığar (yükseklikte %94)", () => {
    for (const h of [12, 30, 48, 64]) {
      const s = spotOf({ ...slot, h }, 1, false);
      assert.ok(s.fh <= h * 0.94 + 1e-9 && s.D <= slot.w * 0.42 + 1e-9);
    }
  });
  it("etiket satırları: ad, rol (≥28), maliyet (≥40 ve maliyet varsa)", () => {
    const s = spotOf(slot, 1, false);
    assert.equal(textBlock(s, 20, true).rows, 1);
    assert.equal(textBlock(s, 30, true).rows, 2);
    assert.equal(textBlock(s, 48, false).rows, 2);
    assert.equal(textBlock(s, 48, true).rows, 3);
    const block = textBlock(s, 48, true);
    const r = labelRect(s, block);
    assert.ok(r.y <= block.top - 10 + 1e-9 && r.w === s.textR - s.textL && r.y + r.h >= block.costY + 6.5);
  });
  it("#2020 Echo: gövde Core ad bloğunun hemen solunda, bloğun ortasının biraz üstünde; etiket gövdenin altında ortalı", () => {
    const core = { x: 300, nameY: 200, roleY: 215 };
    const e = echoBeside(core, 60, 41);
    const s = e.spot;
    near(s.bx + s.D / 2, core.x - 30 - ECHO_BESIDE.gap);
    assert.ok(s.D <= ECHO_BESIDE.maxD && s.fh === s.D && s.facing === 0);
    assert.ok(s.by < (core.nameY + core.roleY) / 2 && s.by > core.nameY - 10);
    assert.ok(e.nameY > s.by + s.D / 2 && e.roleY > e.nameY);
    near((s.textL + s.textR) / 2, s.bx);
    // Küçük gövde küçük kalır.
    assert.equal(echoBeside(core, 60, 20).spot.D, 20);
  });
  it("işaret dış üst köşede, saat iç üst köşede", () => {
    const left = spotOf(slot, 1, false), right = spotOf(slot, -1, false);
    assert.ok(markAt(left).x < left.bx && clockAt(left).x > left.bx);
    assert.ok(markAt(right).x > right.bx && clockAt(right).x < right.bx);
    assert.ok(markAt(left).y < left.by);
  });
  it("footprint: kapsama kutusu → oran ve merkez sapması", () => {
    const f = footprint({ res: 512, bounds: [42, 75, 482, 437] });
    near(f.w, 441 / 512); near(f.h, 363 / 512);
    near(f.cx, 262.5 / 512 - 0.5); near(f.cy, 256.5 / 512 - 0.5);
  });
});

describe("bağ damarı", () => {
  const route = [{ x: 0, y: 0 }, { x: 40, y: 0 }, { x: 80, y: 40 }, { x: 120, y: 40 }];
  it("yumuşatma uçları korur (port ve gövde bağlı kalır), aynı tohum aynı eğri", () => {
    const a = smoothRoute(route, 3), b = smoothRoute(route, 3), c = smoothRoute(route, 4);
    assert.deepEqual(a[0], route[0]);
    near(a[a.length - 1].x, 120); near(a[a.length - 1].y, 40);
    assert.deepEqual(a, b);
    assert.notDeepEqual(a, c);
    // ~2 birim adım, kıvrım ≤ 4 birim: yol octilinear rotadan çok sapmaz.
    for (let i = 1; i < a.length; i++) assert.ok(Math.hypot(a[i].x - a[i - 1].x, a[i].y - a[i - 1].y) < 3);
    assert.ok(Math.abs(pathLength(a) - pathLength(route)) < pathLength(route) * 0.15);
  });
  it("chaikin ve yeniden örnekleme ucu düşürmez", () => {
    const c = chaikin(route, 3);
    assert.deepEqual([c[0], c[c.length - 1]], [route[0], route[3]]);
    const r = resample(c, 2);
    assert.deepEqual([r[0], r[r.length - 1]], [route[0], route[3]]);
  });
  it("rota ucu gövdenin Core'a bakan kenarına; sıra modunda üst/alt kenara", () => {
    const side = spotOf({ x: 200, y: 40, w: 98, h: 48 }, -1, false);
    const end = routeToBody(route, side).at(-1)!;
    near(end.x, side.bx + side.D * 0.42); near(end.y, 40);
    const row = spotOf({ x: 200, y: 140, w: 98, h: 48 }, 0, false);
    const down = routeToBody([{ x: 0, y: 0 }, { x: 200, y: 0 }, { x: 200, y: 116 }], row);
    assert.deepEqual(down.at(-1), { x: row.bx, y: row.by - row.fh * 0.42 });
    assert.equal(down.at(-2)!.x, row.bx);
  });
  it("pointAt uçlarda ve ortada", () => {
    const line = [{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 10, y: 10 }];
    assert.deepEqual(pointAt(line, 0), { x: 0, y: 0 });
    assert.deepEqual(pointAt(line, 0.5), { x: 10, y: 0 });
    assert.deepEqual(pointAt(line, 1), { x: 10, y: 10 });
  });
  it("damla işçiden Core'a akar (1 → 0); soru sonuçtan önce gelir", () => {
    const back = [0, 0.5, 1, 1.5].map((t) => dropAt(null, { id: 2 }, t, false)!);
    for (const v of back) assert.ok(v > 0 && v <= 1);
    assert.ok(back[1] < back[0]);
    near(dropAt({ id: 7 }, { id: 2 }, 1, false)!, 1 - ((0.24 + 7 * 0.11) % 1));
    assert.equal(dropAt(null, null, 1, false), null);
  });
  it("Core çapası: 9 kök damar, gövdenin içinden dışa", () => {
    const body = { x: 100, y: 100, s: 120 };
    const veins = coreVeins(body);
    assert.equal(veins.length, 9);
    assert.deepEqual(veins, coreVeins(body));
    for (const v of veins) {
      near(Math.hypot(v[0].x - body.x, v[0].y - body.y), body.s * 0.3, 1e-6);
      assert.ok(Math.hypot(v.at(-1)!.x - body.x, v.at(-1)!.y - body.y) > body.s);
    }
  });
});

describe("renk ve çözünürlük", () => {
  it("sRGB → doğrusal: hex, kısa hex, rgb(); okunamazsa siyah", () => {
    assert.deepEqual(linear("#ffffff"), [1, 1, 1]);
    assert.deepEqual(linear("#000"), [0, 0, 0]);
    near(linear("#808080")[0], 0.2158605, 1e-6);
    assert.deepEqual(linear("rgb(255, 0, 0)"), [1, 0, 0]);
    assert.deepEqual(linear("rgba(255 255 255 / 0.5)"), [1, 1, 1]);
    assert.deepEqual(linear("var(--x)"), [0, 0, 0]);
  });
  it(`GL çözünürlüğü ≤ ${GL_MAX_SCALE}× ve ≤ ${GL_MAX_PIXELS / 1e6} Mpx`, () => {
    assert.equal(glScale(1, 800, 600), 1);
    assert.equal(glScale(2, 800, 600), GL_MAX_SCALE);
    const k = glScale(2, 2560, 1440);
    assert.ok(2560 * 1440 * k * k <= GL_MAX_PIXELS + 1);
    assert.ok(glScale(3, 8000, 8000) >= 0.5);
  });
});
