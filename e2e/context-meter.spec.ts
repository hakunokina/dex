// #212: pane üst çubuğunda güncel context doluluğu (team_context + agent-context olayı). Lider: "312k / 1M"
// + ince çubuk, worker: yalnız sayı. Renk mutlak eşikle (≥250k sarı, ≥500k turuncu, ≥750k kırmızı);
// kırmızıda "Temiz oturum" vurgulanır. dexd ölçümü gerçek transcript'ten üretir — burada team_context
// köprüde hazır cevaplanır (__dexFake). Eski dexd (team_context yok) → gösterge yok.
// Aynı spec #209'un lider çipi maliyetini de doğrular (Kart hover'ı).
import type { Page } from "@playwright/test";
import { test, expect, newTeam, openTeam, paneOf, teamAgents, type Agent } from "./fixtures";

const ctx = (tokens: number) => ({ tokens, window: 1_000_000, model: "claude-opus-5-5", sessionId: "s", updatedAt: Date.now() - 120_000 });
const T = (input: number, output: number, usd: number) => ({ tokens: { input, output, cacheRead: 0, cacheWrite: 0 }, usd });

async function load(app: Page, team: { name: string }, leader: Agent, fake: Record<string, unknown>) {
  await app.addInitScript((f) => {
    window.__dexFake = f;
  }, fake);
  await app.reload();
  await openTeam(app, team.name);
  return paneOf(app, leader.name);
}

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Context göstergesi: 3 eşik rengi, kırmızıda Temiz oturum vurgusu, worker'da sayı; eski dexd'de gizli", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Context ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  const usage_summary = {
    costLabel: "API eşdeğer maliyeti",
    total: T(1000, 2000, 0.41),
    agents: { [leader.id]: T(1000, 2000, 0.41) },
    delegations: {},
    warnings: [],
  };

  // Lider kendiliğinden temiz başlar → dexd gerçek agent-context olayıyla ölçümü sıfırlar (hazır cevabı ezer).
  let pane = await load(app, team, leader, { team_context: { [leader.id]: ctx(312_400) } });
  await expect.poll(async () => (await dexd.call<{ id: string }[]>("pty.list")).some((p) => p.id === leader.id)).toBe(true);
  await expect(pane.locator(".ctx-meter")).toHaveText("— / 1M");
  await expect(pane.locator(".ctx-meter")).toHaveAttribute("title", /henüz ölçüm yok/);

  // Oturum sürerken yeniden açılış: olay yok, hazır cevap görünür. Sarı (lider 312k) + turuncu (worker 600k, yalnız sayı).
  pane = await load(app, team, leader, { usage_summary, team_context: { [leader.id]: ctx(312_400), [worker.id]: ctx(600_000) } });
  const meter = pane.locator(".ctx-meter");
  await expect(meter).toHaveText("312k / 1M");
  await expect(meter).toHaveAttribute("data-level", "warn");
  await expect(meter.locator(".ctx-bar > span")).toHaveAttribute("style", /width: 31\.2%/);
  await expect(meter).toHaveAttribute("title", /Model: claude-opus-5-5\nPencere: 1M\nSon güncelleme: 2 dk önce/);
  await expect(meter).toHaveCSS("color", "rgb(245, 195, 91)");
  await expect(pane.getByRole("button", { name: "Temiz oturum" })).not.toHaveClass(/urgent/);
  // Klavye: gösterge odaklanır, açıklama etikette.
  await meter.focus();
  await expect(meter).toBeFocused();
  await expect(meter).toHaveAttribute("aria-label", /Context doluluğu\. Context: 312\.400 token/);

  const wpane = await paneOf(app, worker.name);
  const wmeter = wpane.locator(".ctx-meter");
  await expect(wmeter).toHaveText("600k");
  await expect(wmeter).toHaveAttribute("data-level", "high");
  await expect(wmeter.locator(".ctx-bar")).toHaveCount(0);
  await expect(wmeter).toHaveCSS("color", "rgb(242, 137, 58)");

  // #209: lider (Core) çipinin hover'ında bugünkü maliyet.
  const canvas = app.locator("canvas.office");
  const box = (await canvas.boundingBox())!;
  const hover = app.locator(".office-card");
  let found = false;
  for (let y = box.y + 8; y < box.y + box.height && !found; y += 16) {
    for (let x = box.x + 8; x < box.x + box.width && !found; x += 20) {
      await app.mouse.move(x, y);
      await app.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
      found = (await hover.isVisible()) && (await hover.locator("b").textContent()) === leader.name;
    }
  }
  expect(found).toBe(true);
  await expect(hover).toContainText("3.0K tok · $0.41");

  // Kırmızı (lider 800k): Temiz oturum vurgulu.
  pane = await load(app, team, leader, { usage_summary, team_context: { [leader.id]: ctx(800_000) } });
  await expect(pane.locator(".ctx-meter")).toHaveText("800k / 1M");
  await expect(pane.locator(".ctx-meter")).toHaveAttribute("data-level", "critical");
  await expect(pane.locator(".ctx-meter")).toHaveAttribute("title", /750k üstü — temiz oturum zamanı/);
  await expect(pane.getByRole("button", { name: "Temiz oturum" })).toHaveClass(/urgent/);

  // Dar görünüm: üst çubuk taşmaz.
  await app.setViewportSize({ width: 720, height: 700 });
  await expect.poll(() => pane.locator(".pane-view-toggle").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await expect(pane.getByRole("button", { name: "Temiz oturum" })).toBeVisible();

  // Eski dexd: gösterge yok, düğme sade.
  await app.addInitScript(() => {
    window.__dexOld = { hide: ["team_context"] };
  });
  pane = await load(app, team, leader, { team_context: { [leader.id]: ctx(800_000) } });
  await expect(pane.getByRole("button", { name: "Temiz oturum" })).toBeVisible();
  await expect(pane.locator(".ctx-meter")).toHaveCount(0);
  await expect(pane.getByRole("button", { name: "Temiz oturum" })).not.toHaveClass(/urgent/);
});
