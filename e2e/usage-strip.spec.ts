// #54: sağ kenar kullanım şeridi. e2e köprüsünde usage_get yok (dexd'de CLIENT_ONLY) → şerit
// "TM bağlı değil"de açılır; Rust köprüsünün olayı (usage_snapshot) taklit edilince veriyi çizer;
// Ayarlar → Genel'den kapanır/açılır. #139: şerit yer ayırmaz, sahnenin üstünde yüzer.
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const LIVE = {
  source: "token-monitor",
  status: "live",
  tokPerSec: 113.2,
  rateAt: 0, // test içinde Date.now()
  todayTokens: 221_700_000,
  todayCostUsd: 94.04,
  providers: [
    { id: "claude", remainingPercent: 66, window: "session", resetsAt: "2099-01-01T12:00:00.000Z" },
    { id: "codex", remainingPercent: 97, window: "weekly", resetsAt: null },
    { id: "muse", remainingPercent: null, window: null, resetsAt: null },
    { id: "zai", remainingPercent: 100, window: "session", resetsAt: null },
    { id: "acme-code", remainingPercent: 40, window: "daily", resetsAt: null },
  ],
  hasSecret: true,
  updatedAt: 0,
};

test("Kullanım şeridi: TM yokken 'TM bağlı değil', olayla veriyi çizer, Ayarlar → Genel'den kapanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.evaluate(() => localStorage.removeItem("dex.usageStrip"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  await expect(strip).toHaveAttribute("data-status", "offline");
  await expect(strip).toContainText("TM bağlı değil");

  // #139: şerit yer ayırmaz — sahne (dockview) tam genişlikte, şerit sağ kenarda onun üstünde;
  // yarı saydam arka plan, fare yalnız şeridin kutusunda (yanındaki terminal tıklanabilir).
  const edges = await app.evaluate(() => {
    const strip = document.querySelector(".usage-strip")!;
    const r = strip.getBoundingClientRect();
    const dock = document.querySelector(".team-stage:not(.hidden) .dv-dockview, .team-stage:not(.hidden) .dockview-theme-dex");
    const at = document.elementFromPoint(r.left - 4, r.top + r.height / 2);
    return {
      stage: document.querySelector(".stage")!.getBoundingClientRect(),
      body: document.querySelector(".app-body")!.getBoundingClientRect(),
      dockRight: dock ? dock.getBoundingClientRect().right : null,
      strip: r,
      vw: document.documentElement.clientWidth,
      leftOfStrip: at ? strip.contains(at) : null,
      bg: getComputedStyle(strip, "::before").backgroundColor,
      z: Number(getComputedStyle(strip).zIndex),
    };
  });
  expect(Math.round(edges.stage.width)).toBe(Math.round(edges.body.width));
  expect(Math.round(edges.stage.right)).toBe(edges.vw);
  expect(edges.dockRight).not.toBeNull();
  expect(edges.dockRight!).toBeGreaterThan(edges.strip.left); // panel şeridin altına uzanır
  expect(Math.round(edges.strip.right)).toBe(edges.vw);
  expect(Math.round(edges.strip.width)).toBe(56);
  expect(edges.leftOfStrip).toBe(false);
  expect(edges.bg).toMatch(/(rgba|color)\(.*[ /,]\s*0?\.\d+\)$/); // yarı saydam
  expect(edges.z).toBeLessThan(90); // yüzen dockview gruplarının (--z-float) altında

  await app.evaluate((s) => {
    const now = Date.now();
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } };
    return w.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "usage_snapshot", payload: { ...s, rateAt: now, updatedAt: now } });
  }, LIVE);
  await expect(strip).toHaveAttribute("data-status", "live");
  await expect(strip).toContainText("113");
  await expect(strip).toContainText("221.7M");
  await expect(strip).toContainText("$94.0");
  const claude = strip.locator('[data-provider="claude"]');
  await expect(claude).toContainText("66%");
  await expect(claude).toHaveAttribute("title", /Claude — 5 saatlik pencere: %66 kalan\nsıfırlanma/);
  await expect(strip.locator('[data-provider="muse"]')).toContainText("--");

  // #118: şerit sağa yaslı ama dikeyde ortalı (içerik kısayken üstte değil ortada yüzer).
  const centered = await app.evaluate(() => {
    const r = document.querySelector(".usage-strip")!.getBoundingClientRect();
    const body = document.querySelector(".app-body")!.getBoundingClientRect();
    return { top: r.top - body.top, bottom: body.bottom - r.bottom };
  });
  expect(centered.top).toBeGreaterThan(0);
  expect(Math.abs(centered.top - centered.bottom)).toBeLessThan(2);

  // #118: hover kartı — her halkada TM Muse tarzı sade kart (ad + güncellenme + pencere).
  await claude.hover();
  const card = app.locator('[data-card="claude"]');
  await expect(card).toBeVisible();
  await expect(card).toContainText("Claude");
  await expect(card).toContainText("Oturum");
  await expect(card).toContainText("%66 kaldı");
  await expect(card).toContainText(/Sıfırlanma/);
  await expect(card).toContainText(/önce güncellendi|az önce güncellendi/);
  // Kart pencere içinde: sağ kenar şeridin solunda, tamamı görünür alanda.
  const cardBox = await card.boundingBox();
  const stripBox = await strip.boundingBox();
  expect(cardBox!.x + cardBox!.width).toBeLessThanOrEqual(stripBox!.x);
  expect(cardBox!.y).toBeGreaterThanOrEqual(0);
  const vh = await app.evaluate(() => innerHeight);
  expect(cardBox!.y + cardBox!.height).toBeLessThanOrEqual(vh);
  // Penceresiz sağlayıcıda kart durum metnini gösterir.
  await strip.locator('[data-provider="muse"]').hover();
  const museCard = app.locator('[data-card="muse"]');
  await expect(museCard).toBeVisible();
  await expect(museCard).toContainText("Muse Code");
  await expect(museCard).toContainText("şu an veri yok");

  // TM dock'u gibi logolar: bilinen sağlayıcıda maske-boyalı logo, bilinmeyende baş harf; halka
  // marka renginde, siyah markalı (z.ai) halka koyu yüzeyde metin rengine düşer.
  const marks = await app.evaluate(() => [...document.querySelectorAll<HTMLElement>(".usage-prov")].map((p) => {
    const mark = p.querySelector<HTMLElement>(".usage-mark");
    const box = (mark ?? p.querySelector<HTMLElement>(".usage-glyph"))!.getBoundingClientRect();
    const fill = p.querySelector(".usage-ring-fill");
    return {
      id: p.dataset.provider,
      mask: mark ? getComputedStyle(mark).maskImage : null,
      glyph: p.querySelector(".usage-glyph")?.textContent ?? null,
      ring: fill ? getComputedStyle(fill).stroke : null,
      size: Math.round(box.width),
    };
  }));
  expect(marks.map((m) => m.id)).toEqual(["claude", "codex", "muse", "zai", "acme-code"]);
  for (const m of marks.slice(0, 4)) {
    expect(m.mask, m.id).toMatch(/^url\(/);
    expect(m.size, m.id).toBe(15);
  }
  expect(marks[4]).toMatchObject({ mask: null, glyph: "A" });
  expect(marks[0].ring).toBe("rgb(204, 124, 94)");
  expect(marks[1].ring).toBe("rgb(73, 163, 176)");
  expect(marks[2].ring, "veri yok: dolgu yok").toBeNull();
  expect(marks[3].ring, "--text").toBe("rgb(213, 216, 221)");

  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Genel" }).click();
  const toggle = app.getByRole("checkbox", { name: "Sağ kenarda göster" });
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await expect(strip).toHaveCount(0);
  expect(await app.evaluate(() => localStorage.getItem("dex.usageStrip"))).toBe("off");
  await toggle.check();
  await expect(strip).toBeVisible();
  await app.getByRole("button", { name: "Ayarlar panelini kapat" }).click();
});

// #311: GPU hücresi (gpu_overview köprüde hazır) ve worker'ın işi LLM yüzünden beklerse tek toast.
test("Kullanım şeridi: GPU sahibi + günlük kullanım; gpu-waiting işe bir kez toast", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit GPU ${test.info().workerIndex}`);
  const [leader, worker] = await teamAgents(dexd, team.id).then((a) => [a.find((x) => x.kind === "leader")!, a.find((x) => x.kind !== "leader")!]);
  const overview = {
    tokenSet: true, connected: true, authorized: true, via: "primary", url: "http://192.0.2.10:8100", models: [],
    status: { owner: "llm", llmModel: "qwen3-coder-next", queued: 2, runningJob: null, dailyUsed: 9, dailyLimit: 120 },
    error: null, errorKind: null, checkedAt: 1,
  };
  await app.addInitScript((o) => { window.__dexFake = { ...window.__dexFake, gpu_overview: o, gpu_request_list: [] }; }, overview);
  await app.evaluate(() => localStorage.removeItem("dex.usageStrip"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.evaluate((s) => {
    const now = Date.now();
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } };
    return w.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "usage_snapshot", payload: { ...s, rateAt: now, updatedAt: now } });
  }, LIVE);
  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  const gpu = strip.locator(".usage-gpu");
  await expect(gpu).toHaveAttribute("data-gpu-owner", "LLM");
  await expect(gpu).toContainText("9/120");
  await expect(gpu).toContainText("2 sırada");
  await expect(gpu).toHaveAttribute("title", "GPU: LLM'in (qwen3-coder-next) · kuyrukta 2 iş · bugün 9/120 görsel");
  // Şerit genişliği değişmez, hücre taşmaz; klavyeyle odaklanır.
  const box = await app.evaluate(() => {
    const s = document.querySelector(".usage-strip")!;
    const c = document.querySelector(".usage-gpu")!;
    return { w: s.getBoundingClientRect().width, overflow: [...c.children].some((x) => x.scrollWidth > x.clientWidth + 1 || x.getBoundingClientRect().right > s.getBoundingClientRect().right) };
  });
  expect(Math.round(box.w)).toBe(56);
  expect(box.overflow).toBe(false);
  await gpu.focus();
  await expect(gpu).toBeFocused();

  const emit = (payload: unknown) =>
    app.evaluate((payload) => {
      const s = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
      if (!s) throw new Error("açık soket yok");
      s.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "gpu-waiting", payload }) }));
    }, payload);
  const wait = { agent: worker.id, agentName: worker.name, teamId: team.id, jobId: "j_7", waitReason: "owner_llm_active", etaS: 365 };
  await emit(wait);
  const toasts = app.getByRole("region", { name: "Bildirimler" }).locator(".toast");
  await expect(toasts.filter({ hasText: `${worker.name} GPU bekliyor` })).toHaveCount(1);
  await expect(toasts.filter({ hasText: `${worker.name} GPU bekliyor` })).toContainText("LLM'in boşa çıkınca başlayacak (en erken ~6 dk)");
  await toasts.filter({ hasText: `${worker.name} GPU bekliyor` }).getByRole("button", { name: "Bildirimi kapat" }).click();
  await expect(toasts.filter({ hasText: "GPU bekliyor" })).toHaveCount(0);
  // Aynı iş yeniden yoklanınca (olay tekrar) toast dönmez; yeni iş yeni toast.
  await emit(wait);
  await emit({ ...wait, jobId: "j_8" });
  await expect(toasts.filter({ hasText: "GPU bekliyor" })).toHaveCount(1);
});

// #314: GPU hücresi TM durumundan bağımsız — TM offline/yetkisizken de şeritte görünür.
test("Kullanım şeridi: TM offline ya da yetkisizken GPU hücresi görünür", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit GPU off ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  const overview = {
    tokenSet: true, connected: true, authorized: true, via: "primary", url: "http://192.0.2.10:8100", models: [],
    status: { owner: "idle", llmModel: null, queued: 1, runningJob: null, dailyUsed: 3, dailyLimit: 120 },
    error: null, errorKind: null, checkedAt: 1,
  };
  await app.addInitScript((o) => { window.__dexFake = { ...window.__dexFake, gpu_overview: o, gpu_request_list: [] }; }, overview);
  await app.evaluate(() => localStorage.removeItem("dex.usageStrip"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  const gpu = strip.locator(".usage-gpu");
  await expect(strip).toHaveAttribute("data-status", "offline");
  await expect(strip).toContainText("TM bağlı değil");
  await expect(gpu).toContainText("3/120");
  await expect(gpu).toContainText("1 sırada");
  const fits = () => app.evaluate(() => {
    const s = document.querySelector(".usage-strip")!.getBoundingClientRect();
    const c = document.querySelector(".usage-gpu")!;
    return { w: Math.round(s.width), overflow: [...c.children].some((x) => x.scrollWidth > x.clientWidth + 1 || x.getBoundingClientRect().right > s.right) };
  });
  expect(await fits()).toEqual({ w: 56, overflow: false });
  await gpu.focus();
  await expect(gpu).toBeFocused();

  await app.evaluate(() => {
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } };
    return w.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "usage_snapshot", payload: {
      source: "token-monitor", status: "unauthorized", tokPerSec: null, rateAt: null, todayTokens: null, todayCostUsd: null,
      providers: [], hasSecret: false, updatedAt: null,
    } });
  });
  await expect(strip).toHaveAttribute("data-status", "unauthorized");
  await expect(strip).toContainText("TM anahtarı gerekli");
  await expect(gpu).toContainText("3/120");
  expect(await fits()).toEqual({ w: 56, overflow: false });
});
