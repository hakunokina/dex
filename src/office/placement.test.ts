// #270: ofis kartı panele oturur — ölçek (min. punto), oran eşikleri, geniş/kare/dik yerleşim, izler.
// #277: ince kenar (CSS px), ölçek içeriği doldurur (iki kenar birlikte, üst sınır yok).
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import type { Agent } from "../agents";
import { coreBody } from "./companion/coreXL.ts";
import {
  areaInset, BOARD_PAD_PX, COLUMNS_FROM, crew, ECHO_GAP, ECHO_H, EDGE_PX, fitUnit, isEcho, labelBand, layout, MIN_FONT_PX, MIN_U,
  ownershipKey, pickMode, PIN_CLEAR, routes, SMALLEST_FONT, STACK_BELOW, type Mode, type Point, type Target,
} from "./placement.ts";

const agent = (id: string, kind: "leader" | "worker" = "worker", roleName = id) => ({ id, name: id, kind, roleName }) as unknown as Agent;
const team = (n: number) => [agent("core", "leader"), ...Array.from({ length: n }, (_, i) => agent(`w${i}`))];

/** The panel (CSS px) → world size → layout, as Office does it. */
function board(cssW: number, cssH: number, n: number, prev: Mode | null = null) {
  const mode = pickMode(cssW / cssH, prev);
  const u = fitUnit(cssW, cssH, team(n), [], mode);
  const w = cssW / u, h = cssH / u;
  return { u, w, h, mode, cssW, cssH, targets: layout(team(n), [], w, h, mode, u) };
}

/** Chips + pins/badge (PIN_CLEAR) bounding box, CSS px. */
function contentPx(b: ReturnType<typeof board>) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const t of b.targets.values()) {
    x0 = Math.min(x0, t.x - t.w / 2 - PIN_CLEAR); x1 = Math.max(x1, t.x + t.w / 2 + PIN_CLEAR);
    y0 = Math.min(y0, t.y - t.h / 2 - PIN_CLEAR); y1 = Math.max(y1, t.y + t.h / 2 + PIN_CLEAR);
  }
  return { x0: x0 * b.u, y0: y0 * b.u, x1: x1 * b.u, y1: y1 * b.u };
}

const inside = (t: Target, w: number, h: number, u: number, m = Math.min(areaInset(u), w / 4, h / 4) - 1e-6) =>
  t.x - t.w / 2 >= m && t.x + t.w / 2 <= w - m && t.y - t.h / 2 >= m && t.y + t.h / 2 <= h - m;
const overlap = (a: Target, b: Target) =>
  Math.abs(a.x - b.x) < (a.w + b.w) / 2 && Math.abs(a.y - b.y) < (a.h + b.h) / 2;

function checkBoard(b: ReturnType<typeof board>) {
  const all = [...b.targets.values()];
  for (const t of all) assert.ok(inside(t, b.w, b.h, b.u), `taşma: ${JSON.stringify(t)} @ ${b.w}×${b.h}`);
  for (const t of all) assert.ok(t.h >= 12, `okunmaz çip: ${JSON.stringify(t)} @ ${b.w}×${b.h}`);
  for (let i = 0; i < all.length; i++) for (let j = i + 1; j < all.length; j++) assert.ok(!overlap(all[i], all[j]), "çip çakışması");
}

/** Panel örnekleri: dar/dik, kare, geniş (Kart ~950×570), çok geniş, büyük. */
const PANELS = [[420, 760], [480, 820], [640, 620], [950, 570], [1200, 600], [2400, 500], [1900, 1100], [3200, 1800]] as const;

describe("kenar payı (#277)", () => {
  it("panel → çip alanı ince şerit (sabit CSS px)", () => {
    assert.ok(BOARD_PAD_PX >= 4 && BOARD_PAD_PX <= 6);
    for (const u of [MIN_U, 1.5, 2, 4]) {
      // Çip alanına kadar sabit şerit; rozetin payı (PIN_CLEAR) hücrede, çiple ölçeklenir.
      assert.ok(Math.abs(areaInset(u) * u - (BOARD_PAD_PX + EDGE_PX)) < 1e-9);
      assert.ok(BOARD_PAD_PX + EDGE_PX <= 18);
    }
  });
  it("içerik panelden en fazla %1 taşar (kırpılır), kenara yakın", () => {
    for (const [w, h] of PANELS) for (const n of [2, 6, 10]) {
      const b = board(w, h, n);
      const c = contentPx(b);
      const tol = 0.01;
      assert.ok(c.x0 >= -w * tol && c.y0 >= -h * tol && c.x1 <= w * (1 + tol) && c.y1 <= h * (1 + tol), `${w}×${h} n=${n}: ${JSON.stringify(c)}`);
    }
  });
});

describe("ölçek (fitUnit)", () => {
  it("min. punto: en küçük yazı ekranda ≥ 9 px, küçük panel tabanda durur", () => {
    assert.ok(SMALLEST_FONT * MIN_U >= MIN_FONT_PX);
    assert.equal(board(300, 200, 4).u, MIN_U);
    for (const [w, h] of [[240, 200], [520, 700], [960, 540], ...PANELS]) assert.ok(SMALLEST_FONT * board(w, h, 6).u >= MIN_FONT_PX);
  });
  it("üst sınır yok: panel büyüdükçe çip, Core ve yazı büyümeye devam eder", () => {
    let prev = 0;
    for (const k of [1, 1.5, 2, 3, 4]) {
      const b = board(950 * k, 570 * k, 6);
      assert.ok(b.u > prev * 1.3, `${k}: ${b.u} ≤ ${prev}`);
      prev = b.u;
    }
    // Aynı oran, iki kat panel: ~iki kat ölçek (kenar şeridi sabit px olduğu için biraz fazlası).
    const small = board(950, 570, 6), big = board(1900, 1140, 6);
    assert.ok(big.u / small.u >= 2 && big.u / small.u < 2.1, `${big.u / small.u}`);
    assert.equal(coreBody(big.targets.get("core")!)!.s, 120);
  });
  it("ölçek iki kenara birden bakar: dar kenar değil içerik bağlar", () => {
    // Aynı yükseklik, daha geniş panel → aynı ya da daha büyük ölçek; aynı genişlik, daha uzun → da.
    assert.ok(board(1200, 570, 6).u >= board(950, 570, 6).u);
    assert.ok(board(950, 700, 6).u >= board(950, 570, 6).u);
  });
  for (const [w, h] of PANELS) {
    it(`${w}×${h}: çipler tam boy ve %3 büyük ölçekte artık sığmaz (kart dolu)`, () => {
      const b = board(w, h, 6);
      if (b.u === MIN_U) return; // okunur taban: küçük panelde çipler sıkışır (ayrı testler)
      const core = b.targets.get("core")!;
      assert.equal(coreBody(core)!.s, 120);
      const workers = [...b.targets].filter(([id]) => id !== "core").map(([, t]) => t);
      for (const t of workers) assert.ok(t.w >= 98 - 0.01, `dar çip ${t.w}`);
      // Sıkışmadan büyütülebilecek pay kalmamış: biraz daha büyük ölçekte bir şey küçülür.
      const u2 = b.u * 1.03;
      const t2 = layout(team(6), [], w / u2, h / u2, b.mode, u2);
      const shrunk = [...t2].some(([id, t]) => t.w < b.targets.get(id)!.w - 0.01 || t.h < b.targets.get(id)!.h - 0.01);
      assert.ok(shrunk, `${w}×${h}: u=${b.u} daha büyüyebilirdi`);
      // Kullanılabilir alanın büyük kısmı dolu: iki kenarda ≥ %75, birinde ≥ %80 (çok geniş: COLUMNS_MAX_W ortalar).
      const c = contentPx(b);
      const fx = (c.x1 - c.x0) / w, fy = (c.y1 - c.y0) / h;
      assert.ok(Math.min(fx, fy) >= 0.75 && Math.max(fx, fy) >= 0.8, `${w}×${h}: ${JSON.stringify(c)}`);
    });
  }
});

describe("oran eşikleri (pickMode)", () => {
  it(`≥ ${COLUMNS_FROM} sütun, < ${STACK_BELOW} sıra, arada önceki mod kalır`, () => {
    assert.equal(pickMode(16 / 9, null), "columns");
    assert.equal(pickMode(1, null), "stack");
    assert.equal(pickMode(0.6, "columns"), "stack");
    assert.equal(pickMode(1.2, "columns"), "columns");
    assert.equal(pickMode(1.2, "stack"), "stack");
  });
});

describe("yerleşim", () => {
  it("geniş panel: worker'lar Core'un solunda ve sağında", () => {
    const b = board(1200, 600, 6);
    assert.equal(b.mode, "columns");
    const core = b.targets.get("core")!;
    const xs = [...b.targets].filter(([id]) => id !== "core").map(([, t]) => t.x);
    assert.ok(xs.some((x) => x < core.x - core.w) && xs.some((x) => x > core.x + core.w));
    checkBoard(b);
  });
  it("çok geniş panel: içerik ortalanır, izler uzamaz", () => {
    const b = board(2400, 500, 4);
    const core = b.targets.get("core")!;
    assert.ok(Math.abs(core.x - b.w / 2) < 1);
    for (const [id, t] of b.targets) if (id !== "core") assert.ok(Math.abs(t.x - core.x) < 460);
  });
  for (const [name, w, h] of [["kare", 640, 620], ["dik", 480, 820]] as const) {
    it(`${name} panel: worker'lar Core'un üstünde ve altında sıra halinde, kart dolar`, () => {
      const b = board(w, h, 6);
      assert.equal(b.mode, "stack");
      const core = b.targets.get("core")!;
      assert.ok(Math.abs(core.x - b.w / 2) < 1);
      const workers = [...b.targets].filter(([id]) => id !== "core").map(([, t]) => t);
      const above = workers.filter((t) => t.y + t.h / 2 < core.y - core.h / 2);
      const below = workers.filter((t) => t.y - t.h / 2 > core.y + core.h / 2);
      assert.equal(above.length + below.length, 6);
      assert.ok(above.length && below.length);
      checkBoard(b);
    });
  }
  it("küçük panelde Core XL kalır (gövde ≥ 40)", () => {
    for (const [w, h, n] of [[280, 300, 3], [300, 200, 4], [240, 420, 2]] as const) {
      const b = board(w, h, n);
      assert.ok(coreBody(b.targets.get("core")!), `${w}×${h}: Core çipe düştü`);
    }
  });
  it("küçük ve kalabalık panelde de taşma / çakışma yok", () => {
    for (const [w, h, n] of [[320, 300, 8], [260, 240, 4], [360, 700, 12], [900, 380, 10], [500, 500, 0], [500, 500, 1]] as const) checkBoard(board(w, h, n));
  });
});

const octilinear = (pts: Point[]) => pts.slice(1).every((p, i) => {
  const dx = Math.abs(p.x - pts[i].x), dy = Math.abs(p.y - pts[i].y);
  return dx < 1e-6 || dy < 1e-6 || Math.abs(dx - dy) < 1e-6;
});

describe("izler", () => {
  it("sıra düzeni: üsttekiler gövdenin üst kenarından, alttakiler yanlardan, etiket bandına girmeden", () => {
    const b = board(640, 640, 7);
    const core = b.targets.get("core")!;
    const body = coreBody(core)!;
    const workers = [...b.targets].filter(([id]) => id !== "core").map(([id, t]) => ({ id, ...t }));
    const paths = routes({ id: "core", ...core }, workers);
    assert.equal(paths.size, 7);
    const band = labelBand(core);
    const labelsTop = body.y + body.s / 2;
    for (const w of workers) {
      const pts = paths.get(w.id)!;
      assert.ok(octilinear(pts), `${w.id}: ${JSON.stringify(pts)}`);
      const end = pts[pts.length - 1];
      if (w.y < core.y) {
        assert.ok(pts[0].y < body.y - body.s / 2 && Math.abs(pts[0].x - core.x) <= body.s / 2);
        assert.deepEqual(end, { x: w.x, y: w.y + w.h / 2 });
      } else {
        assert.equal(Math.abs(pts[0].x - core.x), body.s / 2);
        assert.deepEqual(end, { x: w.x, y: w.y - w.h / 2 });
        // Etiketlerin altına (gövde altı → hücre) dikey inen parça bandın dışında.
        for (let i = 1; i < pts.length; i++) {
          const [a, c] = [pts[i - 1], pts[i]];
          if (Math.max(a.y, c.y) > labelsTop && Math.min(a.y, c.y) < core.y + core.h / 2) {
            assert.ok(Math.min(Math.abs(a.x - core.x), Math.abs(c.x - core.x)) >= band - 1e-6, `${w.id} bandda: ${JSON.stringify(pts)}`);
          }
        }
      }
    }
  });
  it("sütun düzeni: izler gövdenin yan kenarından çıkar (eski davranış)", () => {
    const b = board(1200, 600, 4);
    const core = b.targets.get("core")!;
    const body = coreBody(core)!;
    const workers = [...b.targets].filter(([id]) => id !== "core").map(([id, t]) => ({ id, ...t }));
    for (const [, pts] of routes({ id: "core", ...core }, workers)) {
      assert.ok(Math.abs(Math.abs(pts[0].x - core.x) - body.s / 2) < 1e-9);
      assert.ok(octilinear(pts));
    }
  });
});

describe("Echo yuvası (#1958)", () => {
  const echo = () => ({ ...agent("echo"), roleSlug: "assistant" }) as unknown as Agent;
  const withEcho = (n: number, first = false) => {
    const [core, ...rest] = team(n);
    return first ? [core, echo(), ...rest] : [core, ...rest, echo()];
  };
  function echoBoard(cssW: number, cssH: number, n: number, first = false) {
    const agents = withEcho(n, first);
    const mode = pickMode(cssW / cssH, null);
    const u = fitUnit(cssW, cssH, agents, [], mode);
    const w = cssW / u, h = cssH / u;
    return { u, w, h, mode, cssW, cssH, targets: layout(agents, [], w, h, mode, u) };
  }

  it("asistan isEcho; lider ve sıradan worker değil", () => {
    assert.equal(isEcho(echo()), true);
    assert.equal(isEcho(agent("w0")), false);
    assert.equal(isEcho({ ...agent("core", "leader"), roleSlug: "assistant" } as unknown as Agent), false);
  });

  for (const [w, h] of [[1200, 600], [950, 570], [640, 620], [480, 820], [1900, 1100]] as const) for (const first of [false, true]) {
    it(`${w}×${h}${first ? " (rol sırasında ilk)" : ""}: Echo Core etiketinin hemen altında, aynı eksende; sütunlara karışmaz`, () => {
      const b = echoBoard(w, h, 6, first);
      const core = b.targets.get("core")!;
      const slot = b.targets.get("echo")!;
      assert.ok(Math.abs(slot.x - core.x) < 1e-6, "aynı dikey eksen");
      assert.ok(Math.abs(slot.w - core.w) < 1e-6, "Core sütunu genişliğinde");
      const gap = (slot.y - slot.h / 2) - (core.y + core.h / 2);
      assert.ok(Math.abs(gap - ECHO_GAP) < 1e-6, `Core'a yakın: ${gap}`);
      // Worker'lar Echo'suz takımdaki yerleşimle aynı sayıda ve aynı tarafta (Echo bir sütun/sıra yuvası almaz).
      const workers = [...b.targets].filter(([id]) => id !== "core" && id !== "echo").map(([, t]) => t);
      assert.equal(workers.length, 6);
      for (const t of workers) assert.ok(Math.abs(t.x - core.x) > core.w / 2 || Math.abs(t.y - core.y) > core.h / 2 + slot.h, "worker Core sütununda değil");
      checkBoard(b);
    });
  }

  it("tam boyda Echo yuvası; Core gövdesi 120 kalır", () => {
    for (const [w, h] of [[1200, 600], [640, 620], [1900, 1100]] as const) {
      const b = echoBoard(w, h, 6);
      if (b.u === MIN_U) continue;
      assert.ok(coreBody(b.targets.get("core")!)!.s >= 120 - 0.02);
      assert.ok(b.targets.get("echo")!.h >= ECHO_H - 0.01);
    }
  });

  it("Echo'nun izi yok; ownership anahtarı Echo'yu worker sırasına katmaz", () => {
    const agents = withEcho(4, true);
    const b = echoBoard(1200, 600, 4, true);
    const core = b.targets.get("core")!;
    const workers = crew(agents).workers.map((a) => ({ id: a.id, ...b.targets.get(a.id)! }));
    assert.ok(!crew(agents).workers.some((a) => a.id === "echo"));
    assert.deepEqual(crew(agents).echoes.map((a) => a.id), ["echo"]);
    assert.ok(!routes({ id: "core", ...core }, workers).has("echo"));
    assert.ok(!ownershipKey(agents, []).includes("echo"));
  });

  it("Echo'lu yalnız Core (boş takım) ve küçük paneller taşmaz", () => {
    for (const [w, h, n] of [[500, 500, 0], [320, 300, 4], [260, 240, 2], [900, 380, 10], [360, 700, 12]] as const) {
      const b = echoBoard(w, h, n);
      assert.ok(b.targets.get("echo"), `${w}×${h}`);
      checkBoard(b);
    }
  });

  it("kısa Kart: önce Echo küçülür (iki satır), Core XL kalır; en küçük panelde de", () => {
    // Varsayılan düzende Kart ~834×235 (1400×900 pencere) ve ~1146×298 (1920×1080).
    const short = echoBoard(834, 235, 8);
    assert.ok(short.targets.get("echo")!.h >= 28 - 1e-6);
    assert.ok(coreBody(short.targets.get("core")!)!.s >= 74);
    const wide = echoBoard(1146, 298, 8);
    assert.equal(coreBody(wide.targets.get("core")!)!.s, 120);
    for (const [w, h, n] of [[594, 165, 8], [280, 300, 3], [300, 200, 4], [240, 420, 2], [500, 180, 0]] as const) {
      const b = echoBoard(w, h, n);
      assert.ok(coreBody(b.targets.get("core")!), `${w}×${h}: Core çipe düştü`);
      const core = b.targets.get("core")!, slot = b.targets.get("echo")!;
      assert.ok(slot.y - slot.h / 2 >= core.y + core.h / 2, `${w}×${h}: Echo Core'un üstüne biniyor`);
    }
  });

  it("çok liderli takımda Echo ilk Core'un altında", () => {
    const agents = [agent("core", "leader"), agent("b", "leader"), agent("w0"), agent("w1"), echo()];
    const t = layout(agents, [], 900, 500, "columns", MIN_U);
    const slot = t.get("echo")!, core = t.get("core")!;
    assert.ok(Math.abs(slot.x - core.x) < 1e-6 && slot.y > core.y);
  });
});
