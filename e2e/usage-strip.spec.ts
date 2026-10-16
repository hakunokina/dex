// #54/#1861: sağ kenar kullanım şeridi. Veri dexd'den (usage_get + usage-snapshot olayı): sağlayıcı
// başına 5 saatlik (dış yay) ve haftalık (iç yay) kalan kota, bugünün token'ı. Ayarlar → Genel'den
// kapanır/açılır. #139: şerit yer ayırmaz, sahnenin üstünde yüzer.
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

// Gerçek ölçüm makinenin kimliklerine bağlı: görüntü __dexFake ile verilir (measuredAt sayfada damgalanır).
const LIVE = {
  providers: [
    { id: "anthropic", fiveHour: { remainingPercent: 66, resetsAt: "2099-01-01T12:00:00.000Z" }, weekly: { remainingPercent: 41, resetsAt: "2099-01-05T00:00:00.000Z" }, note: null as string | null, measuredAt: 0 as number | null, todayTokens: 200_000_000 },
    // #2019: ikinci Claude hesabı (bağlıyken) Claude'un hemen arkasında, kendi ölçümüyle.
    { id: "anthropic-2", fiveHour: { remainingPercent: 30, resetsAt: "2099-01-01T14:00:00.000Z" }, weekly: { remainingPercent: 70, resetsAt: "2099-01-06T00:00:00.000Z" }, note: null, measuredAt: 0, todayTokens: 0 },
    { id: "glm", fiveHour: { remainingPercent: 100, resetsAt: null }, weekly: { remainingPercent: 8, resetsAt: null }, note: null, measuredAt: 0, todayTokens: 21_700_000 },
    { id: "muse", fiveHour: null, weekly: null, note: "Muse anahtar ucu kullanım bildirmiyor", measuredAt: null, todayTokens: 0 },
  ],
  todayTokens: 221_700_000,
  weekTokens: 1_500_000_000 as number | undefined,
  updatedAt: 0,
};

const fakeUsage = (s: typeof LIVE) => {
  const now = Date.now();
  window.__dexFake = {
    ...window.__dexFake,
    usage_get: { ...s, updatedAt: now, providers: s.providers.map((p) => ({ ...p, measuredAt: p.fiveHour || p.weekly ? now : null })) },
  };
};

test("Kullanım şeridi: dexd görüntüsü 5 saat + haftalık halkalar, olayla güncellenir, Ayarlar → Genel'den kapanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.addInitScript(fakeUsage, LIVE);
  await app.evaluate(() => { localStorage.removeItem("dex.usageStrip"); localStorage.removeItem("dex.usageStripRange"); });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  await expect(strip).toHaveAttribute("data-status", "live");

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

  // Bugünün token'ı; tok/s ve $ yok (gerçek kaynakları yok).
  await expect(strip).toContainText("221.7M");
  await expect(strip).not.toContainText("tok/s");
  await expect(strip).not.toContainText("$");
  // #2139: token hücresi tıklamayla bugün ↔ 7 gün; klavyeyle de; seçim cihazda kalır.
  const tokens = strip.getByRole("button", { name: /Dex ajanları/ });
  await expect(tokens).toHaveAttribute("data-range", "today");
  await expect(tokens).toHaveAttribute("title", /^Bugün Dex ajanları: 221\.700\.000 token .*tıkla: 7 gün$/);
  await tokens.click();
  await expect(tokens).toHaveAttribute("data-range", "week");
  await expect(tokens.locator(".usage-cap")).toHaveText("7 gün");
  await expect(tokens.locator(".usage-big")).toHaveText("1.5B");
  await expect(tokens).toHaveAttribute("aria-label", /^Son 7 gün Dex ajanları: 1\.500\.000\.000 token .*tıkla: bugün$/);
  expect(await app.evaluate(() => localStorage.getItem("dex.usageStripRange"))).toBe("week");
  await tokens.focus();
  await app.keyboard.press("Enter");
  await expect(tokens).toHaveAttribute("data-range", "today");
  await expect(tokens.locator(".usage-cap")).toHaveText("bugün");
  await expect(tokens.locator(".usage-big")).toHaveText("221.7M");
  await app.keyboard.press("Space");
  await expect(tokens).toHaveAttribute("data-range", "week");
  await app.reload();
  await openTeam(app, team.name);
  await expect(tokens).toHaveAttribute("data-range", "week");
  await expect(tokens.locator(".usage-big")).toHaveText("1.5B");
  await tokens.click();
  await expect(tokens).toHaveAttribute("data-range", "today");
  const claude = strip.locator('[data-provider="anthropic"]');
  await expect(claude.locator('.usage-pct[data-window="fiveHour"]')).toHaveText("66%");
  await expect(claude.locator('.usage-pct[data-window="weekly"]')).toHaveText("41%");
  await expect(claude).toHaveAttribute("aria-label", /^Claude\n5 saat: %66 kalan, sıfırlanma .+\nHaftalık: %41 kalan, sıfırlanma /);
  // #2019: iki Claude halkası ayrı kota; ikincisi aynı logoda "2" rozetiyle.
  const claude2 = strip.locator('[data-provider="anthropic-2"]');
  await expect(claude2.locator('.usage-pct[data-window="fiveHour"]')).toHaveText("30%");
  await expect(claude2.locator('.usage-pct[data-window="weekly"]')).toHaveText("70%");
  await expect(claude2).toHaveAttribute("aria-label", /^Claude 2\n5 saat: %30 kalan, sıfırlanma .+\nHaftalık: %70 kalan, sıfırlanma /);
  await expect(claude2.locator(".usage-badge")).toHaveText("2");
  await expect(claude.locator(".usage-badge")).toHaveCount(0);
  // Ton iki pencereden az kalanına göre: GLM'in haftalığı %8 → kritik.
  await expect(strip.locator('[data-provider="glm"]')).toHaveClass(/critical/);
  const muse = strip.locator('[data-provider="muse"]');
  await expect(muse.locator(".usage-pct")).toHaveText(["--", "--"]);
  await expect(muse).toHaveAttribute("aria-label", "Muse Code — şu an veri yok (Muse anahtar ucu kullanım bildirmiyor)");

  // #118: şerit sağa yaslı ama dikeyde ortalı (içerik kısayken üstte değil ortada yüzer).
  const centered = await app.evaluate(() => {
    const r = document.querySelector(".usage-strip")!.getBoundingClientRect();
    const body = document.querySelector(".app-body")!.getBoundingClientRect();
    return { top: r.top - body.top, bottom: body.bottom - r.bottom };
  });
  expect(centered.top).toBeGreaterThan(0);
  expect(Math.abs(centered.top - centered.bottom)).toBeLessThan(2);

  // #118: hover kartı — iki pencere satırı + bugünün token'ı.
  await claude.hover();
  const card = app.locator('[data-card="anthropic"]');
  await expect(card).toBeVisible();
  await expect(card).toContainText("Claude");
  await expect(card.locator('[data-window="fiveHour"]')).toContainText("5 saat");
  await expect(card.locator('[data-window="fiveHour"]')).toContainText("%66 kaldı");
  await expect(card.locator('[data-window="weekly"]')).toContainText("Haftalık");
  await expect(card.locator('[data-window="weekly"]')).toContainText("%41 kaldı");
  await expect(card).toContainText(/Sıfırlanma/);
  await expect(card).toContainText(/önce güncellendi|az önce güncellendi/);
  await expect(card).toContainText("Bugün 200.000.000 token");
  // Kart pencere içinde: sağ kenar şeridin solunda, tamamı görünür alanda.
  const cardBox = await card.boundingBox();
  const stripBox = await strip.boundingBox();
  expect(cardBox!.x + cardBox!.width).toBeLessThanOrEqual(stripBox!.x);
  expect(cardBox!.y).toBeGreaterThanOrEqual(0);
  const vh = await app.evaluate(() => innerHeight);
  expect(cardBox!.y + cardBox!.height).toBeLessThanOrEqual(vh);
  // Ölçümsüz sağlayıcıda kart nedeni gösterir.
  await muse.hover();
  const museCard = app.locator('[data-card="muse"]');
  await expect(museCard).toBeVisible();
  await expect(museCard).toContainText("Muse Code");
  await expect(museCard).toContainText("şu an veri yok — Muse anahtar ucu kullanım bildirmiyor");

  // TM dock'u gibi logolar: maske-boyalı logo; dış yay 5 saat, iç yay haftalık, marka renginde;
  // siyah markalı (z.ai) halka koyu yüzeyde metin rengine düşer; ölçümsüzde dolgu yok.
  const marks = await app.evaluate(() => [...document.querySelectorAll<HTMLElement>(".usage-prov")].map((p) => {
    const mark = p.querySelector<HTMLElement>(".usage-mark")!;
    const fill = (w: string) => {
      const c = p.querySelector(`[data-window="${w}"] .usage-ring-fill`);
      return c ? getComputedStyle(c).stroke : null;
    };
    return { id: p.dataset.provider, mask: getComputedStyle(mark).maskImage, size: Math.round(mark.getBoundingClientRect().width), five: fill("fiveHour"), week: fill("weekly") };
  }));
  expect(marks.map((m) => m.id)).toEqual(["anthropic", "anthropic-2", "glm", "muse"]);
  for (const m of marks) {
    expect(m.mask, m.id).toMatch(/^url\(/);
    expect(m.size, m.id).toBe(15);
  }
  expect(marks[0]).toMatchObject({ five: "rgb(204, 124, 94)", week: "rgb(204, 124, 94)" });
  expect(marks[1]).toMatchObject({ five: "rgb(204, 124, 94)", week: "rgb(204, 124, 94)" });
  expect(marks[2].five, "--text").toBe("rgb(213, 216, 221)");
  expect(marks[3]).toMatchObject({ five: null, week: null });

  // dexd'nin olayı görüntüyü değiştirir (poll yok).
  await app.evaluate((s) => {
    const now = Date.now();
    const payload = { ...s, updatedAt: now + 1, providers: s.providers.map((p) => p.id === "glm" ? { ...p, weekly: { remainingPercent: 50, resetsAt: null }, measuredAt: now } : p) };
    const sock = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
    if (!sock) throw new Error("açık soket yok");
    sock.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "usage-snapshot", payload }) }));
  }, LIVE);
  await expect(strip.locator('[data-provider="glm"] .usage-pct[data-window="weekly"]')).toHaveText("50%");
  await expect(strip.locator('[data-provider="glm"]')).toHaveClass(/ok/);
  // #2139: eski dexd weekTokens göndermez — 7 gün modunda "--".
  await app.evaluate((s) => {
    const { weekTokens: _, ...old } = { ...s, updatedAt: Date.now() + 2 };
    const sock = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
    if (!sock) throw new Error("açık soket yok");
    sock.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "usage-snapshot", payload: old }) }));
  }, LIVE);
  await tokens.click();
  await expect(tokens.locator(".usage-cap")).toHaveText("7 gün");
  await expect(tokens.locator(".usage-big")).toHaveText("--");
  await tokens.click();
  await expect(tokens.locator(".usage-big")).toHaveText("221.7M");

  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Genel" }).click();
  const toggle = app.getByRole("switch", { name: "Kullanım şeridini göster" });
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await expect(strip).toHaveCount(0);
  expect(await app.evaluate(() => localStorage.getItem("dex.usageStrip"))).toBe("off");
  await toggle.check();
  await expect(strip).toBeVisible();
  await app.getByRole("button", { name: "Ayarlar panelini kapat" }).click();
});

// #2019: şeritteki ikinci Claude halkasının hesabı Ayarlar → Sağlayıcılar'dan bağlanır. Giriş gerçek
// `claude auth login`'i dexd'nin `claude-login` PTY'sinde açar (burada spawn sahte: CLI açılmaz);
// bağlıyken e-posta görünür, bağlantı onayla kaldırılır.
test("Claude 2: Ayarlar → Sağlayıcılar'dan bağlanır, girişi terminalde açar, onayla kaldırılır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Claude 2 ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  // Ayarlar takımın dock'unda açılır; başka takımın gizli sahnesindeki panel sayılmaz.
  const stage = app.locator(".team-stage:not(.hidden)");
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await stage.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Sağlayıcılar" }).click();
  // Gerçek dexd (izole ev dizini): hesap bağlı değil.
  const card = stage.locator('[data-setting="provider-anthropic-2"]');
  await expect(card).toContainText("Claude 2");
  await expect(card).toContainText("Bağlı değil");
  await expect(card.getByRole("button", { name: "Bağlantıyı kaldır" })).toHaveCount(0);

  await app.evaluate(() => { window.__dexFake = { ...window.__dexFake, pty_spawn: null }; });
  await card.getByRole("button", { name: "Hesabı bağla" }).click();
  await expect(card.locator(".set-login-term .term-host")).toBeVisible();
  await expect.poll(async () => (await calls(app)).find((c) => c.cmd === "pty_spawn" && c.args.cmd === "claude-login")?.args)
    .toMatchObject({ id: "claude-login-anthropic-2", provider: "anthropic-2" });
  await card.getByRole("button", { name: "Kapat", exact: true }).click();
  await expect(card.locator(".set-login-term")).toHaveCount(0);

  // Bağlı hesap (sahte liste; dexd'nin `providers-changed` olayı kartı tazeler): e-posta, onaylı kaldırma.
  await app.evaluate(() => {
    window.__dexFake = {
      ...window.__dexFake,
      provider_list: [
        { id: "anthropic", label: "Claude", baseUrl: null, hasToken: false, tokenMask: null, models: [] },
        { id: "anthropic-2", label: "Claude 2", baseUrl: null, hasToken: true, tokenMask: null, models: [], account: { connected: true, email: "ikinci@example.com" } },
      ],
      claude_account_remove: null,
    };
    const sock = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
    if (!sock) throw new Error("açık soket yok");
    sock.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "providers-changed", payload: null }) }));
  });
  await expect(card).toContainText("Bağlı · ikinci@example.com");
  await expect(card.getByRole("button", { name: "Yeniden bağla" })).toBeVisible();
  await card.getByRole("button", { name: "Bağlantıyı kaldır" }).click();
  const confirm = app.getByRole("alertdialog", { name: "Claude 2 bağlantısı kaldırılsın mı?" });
  await expect(confirm.getByRole("button", { name: "Vazgeç" })).toBeFocused();
  await confirm.getByRole("button", { name: "Bağlantıyı kaldır" }).click();
  await expect(confirm).toHaveCount(0);
  expect((await calls(app)).filter((c) => c.cmd === "claude_account_remove").at(-1)?.args).toEqual({ id: "anthropic-2" });
  await expect(card).toContainText("Bağlantı kaldırıldı.");
});

// #2034: konum Ayarlar → Genel → Görünüm'den: sağ (varsayılan) / sol / üst başlık; anında uygulanır.
// Başlıkta şerit pencere düğmelerinin hemen solunda, takım çipleri kalan yere daralır; kart aşağı açılır.
test("Kullanım şeridi: konum sağ / sol / üst başlık, 1280 ve 2560 genişlikte çakışmasız", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit konum ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.addInitScript(fakeUsage, LIVE);
  // Çekirdek sürüm bandı (host-warn, z --z-banner) açık: kart sahnedeki bant ve terminalin üstünde kalmalı.
  await app.addInitScript(() => { window.__dexOld = { ...window.__dexOld, commit: "0ld0000000", commitTime: 1 }; });
  await app.evaluate(() => { localStorage.removeItem("dex.usageStrip"); localStorage.removeItem("dex.usageStripPlace"); localStorage.removeItem("dex.usageStripRange"); });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  await expect(strip).toHaveAttribute("data-place", "right");
  await expect(strip).toHaveAttribute("data-status", "live");

  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Genel" }).click();
  const place = app.getByRole("radiogroup", { name: "Kullanım şeridi" });
  await expect(place.getByRole("radio", { name: "Sağ" })).toHaveAttribute("aria-checked", "true");
  // Ayarlar paneli açık kalır: şerit ve kartı panellerin üstünde, seçim anında uygulanır.
  const choose = async (label: string, value: string) => {
    await place.getByRole("radio", { name: label }).click();
    await expect(strip).toHaveAttribute("data-place", value);
    expect(await app.evaluate(() => localStorage.getItem("dex.usageStripPlace"))).toBe(value);
  };
  const geo = () => app.evaluate(() => {
    const r = (q: string) => document.querySelector(q)?.getBoundingClientRect() ?? null;
    const s = document.querySelector(".usage-strip")!;
    return {
      vw: document.documentElement.clientWidth, vh: innerHeight,
      strip: r(".usage-strip")!, body: r(".app-body")!, bar: r(".topbar")!, rack: r(".topbar-rack")!, rackScroll: r(".rack-scroll"),
      ctl: r(".win-ctl")!, brand: r(".brand")!, inBar: !!s.closest(".topbar"), drag: s.hasAttribute("data-tauri-drag-region"),
      overflow: [...s.querySelectorAll<HTMLElement>(".usage-cell, .usage-prov")].some((c) => c.getBoundingClientRect().bottom > s.getBoundingClientRect().bottom + 1 || c.getBoundingClientRect().top < s.getBoundingClientRect().top - 1),
    };
  });
  const cardBelow = async () => {
    const ring = strip.locator('[data-provider="anthropic"]');
    await ring.hover();
    const card = app.locator('[data-card="anthropic"]');
    await expect(card).toBeVisible();
    // #2043: toBeVisible örtülmeyi görmez — kartın ortasında ve üst kenarında (bandın denk geldiği yer)
    // en üstteki öğe kartın kendisi olmalı.
    expect(await card.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return [r.top + r.height / 2, r.top + 4].map((y) => {
        const top = document.elementFromPoint(r.left + r.width / 2, y);
        return top ? el.contains(top) || top.className.toString() : "yok";
      });
    })).toEqual([true, true]);
    const [c, rb] = [await card.boundingBox(), await ring.boundingBox()];
    return { c: c!, ring: rb! };
  };
  // #2139: her konumda token hücresi bugün ↔ 7 gün geçer.
  const flips = async (where: string) => {
    const tokens = strip.getByRole("button", { name: /Dex ajanları/ });
    await tokens.click();
    await expect(tokens.locator(".usage-cap"), where).toHaveText("7 gün");
    await expect(tokens.locator(".usage-big"), where).toHaveText("1.5B");
    await tokens.click();
    await expect(tokens.locator(".usage-cap"), where).toHaveText("bugün");
    await expect(tokens.locator(".usage-big"), where).toHaveText("221.7M");
  };
  await flips("right");

  // Sol: sol kenara yaslı, dikeyde ortalı, sahneyi daraltmaz; kart şeridin sağında.
  await choose("Sol", "left");
  await flips("left");
  let g = await geo();
  expect(g.inBar).toBe(false);
  expect(Math.round(g.strip.left)).toBe(Math.round(g.body.left));
  expect(Math.round(g.strip.width)).toBe(56);
  expect(Math.abs((g.strip.top - g.body.top) - (g.body.bottom - g.strip.bottom))).toBeLessThan(2);
  let { c } = await cardBelow();
  expect(c.x).toBeGreaterThanOrEqual(g.strip.right);
  expect(c.y).toBeGreaterThanOrEqual(0);
  expect(c.y + c.height).toBeLessThanOrEqual(g.vh);
  await app.mouse.move(g.vw / 2, g.vh / 2);

  // Üst başlık: pencere düğmelerinin hemen solunda, başlığa sığar; takım çipleri şeridin solunda biter.
  await choose("Üst başlık", "top");
  await expect(app.locator(".team-stage:not(.hidden) .host-banner")).toBeVisible();
  await flips("top");
  for (const width of [2560, 1280]) {
    await app.setViewportSize({ width, height: 900 });
    g = await geo();
    expect(g.inBar, `${width}`).toBe(true);
    expect(g.drag, `${width}`).toBe(true);
    expect(g.strip.right, `${width}`).toBeLessThanOrEqual(g.ctl.left + 0.5);
    expect(g.ctl.left - g.strip.right, `${width}`).toBeLessThan(12);
    expect(g.strip.top, `${width}`).toBeGreaterThanOrEqual(g.bar.top - 0.5);
    expect(g.strip.bottom, `${width}`).toBeLessThanOrEqual(g.bar.bottom + 0.5);
    expect(g.overflow, `${width}`).toBe(false);
    expect(g.rack.right, `${width}`).toBeLessThanOrEqual(g.strip.left + 0.5);
    expect(g.rack.left, `${width}`).toBeGreaterThanOrEqual(g.brand.right);
    if (g.rackScroll) expect(g.rackScroll.right, `${width}`).toBeLessThanOrEqual(g.strip.left + 0.5);
    // Sahne başlıkta yer kaplayan şeridin altında tam genişlikte; sahnede şerit yok.
    expect(await app.locator(".stage .usage-strip").count()).toBe(0);
    const k = await cardBelow();
    expect(k.c.y, `${width}`).toBeGreaterThanOrEqual(g.bar.bottom);
    expect(k.c.x, `${width}`).toBeGreaterThanOrEqual(0);
    expect(k.c.x + k.c.width, `${width}`).toBeLessThanOrEqual(g.vw);
    expect(k.c.y + k.c.height, `${width}`).toBeLessThanOrEqual(g.vh);
    expect(Math.abs(k.c.x + k.c.width / 2 - (k.ring.x + k.ring.width / 2)), `${width}`).toBeLessThan(2);
    await app.mouse.move(width / 2, 600);
  }
  // Başlıkta değilken takım çipleri pencere düğmelerine kadar uzanır (bugünkü düzen).
  await app.setViewportSize({ width: 1400, height: 900 });
  const rackTop = (await geo()).rack;
  await choose("Sağ", "right");
  g = await geo();
  expect(g.inBar).toBe(false);
  expect(Math.round(g.strip.right)).toBe(g.vw);
  expect(g.rack.width).toBeGreaterThan(rackTop.width);
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
  await app.addInitScript(fakeUsage, LIVE);
  await app.evaluate(() => localStorage.removeItem("dex.usageStrip"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  await expect(strip).toHaveAttribute("data-status", "live");
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

// #314: GPU hücresi kullanım verisinden bağımsız — dexd usage_get bilmiyorken (eski sunucu) de görünür.
test("Kullanım şeridi: kullanım verisi yokken GPU hücresi görünür", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Şerit GPU off ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  const overview = {
    tokenSet: true, connected: true, authorized: true, via: "primary", url: "http://192.0.2.10:8100", models: [],
    status: { owner: "idle", llmModel: null, queued: 1, runningJob: null, dailyUsed: 3, dailyLimit: 120 },
    error: null, errorKind: null, checkedAt: 1,
  };
  await app.addInitScript((o) => { window.__dexFake = { ...window.__dexFake, gpu_overview: o, gpu_request_list: [] }; }, overview);
  await app.addInitScript(() => { window.__dexOld = { hide: ["usage_get"], unknown: ["usage_get"] }; });
  await app.evaluate(() => localStorage.removeItem("dex.usageStrip"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  const gpu = strip.locator(".usage-gpu");
  await expect(strip).toHaveAttribute("data-status", "unsupported");
  await expect(strip).toContainText("sunucu eski");
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
  // #2034: veri yokken başlıkta da yatay ve başlığa sığar.
  await app.evaluate(() => { localStorage.setItem("dex.usageStripPlace", "top"); dispatchEvent(new Event("dex:usage-strip")); });
  await expect(strip).toHaveAttribute("data-place", "top");
  const top = await app.evaluate(() => {
    const s = document.querySelector(".usage-strip")!.getBoundingClientRect();
    const bar = document.querySelector(".topbar")!.getBoundingClientRect();
    const ctl = document.querySelector(".win-ctl")!.getBoundingClientRect();
    const off = document.querySelector(".usage-off")!.getBoundingClientRect();
    return { inBar: s.top >= bar.top - 0.5 && s.bottom <= bar.bottom + 0.5, beforeCtl: s.right <= ctl.left + 0.5, offFits: off.bottom <= s.bottom + 0.5 && off.width > off.height };
  });
  expect(top).toEqual({ inBar: true, beforeCtl: true, offFits: true });
  await expect(gpu).toContainText("3/120");
});
