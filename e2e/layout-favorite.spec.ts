// Patron düzeni (favori): kaydet → yeni takım favoriyle açılır → tek/tüm takıma uygula.
// Kart favori varken sabitlenir (× yok, kilit simgesi).
import type { Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const groups = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll(".team-stage:not(.hidden) .dv-grid-view .dv-groupview")].map((g) => ({
      // Ajan sekmesinde nabız yaşı ("0s") ayrı öğe: ad `.dex-tab-name`'den, yaşsız.
      tabs: [...g.querySelectorAll(".dv-tab")].map((t) => (t.querySelector(".dex-tab-name") ?? t).textContent?.trim() ?? ""),
    })),
  );
const tabNames = async (page: Page) => (await groups(page)).flatMap((g) => g.tabs);
const palette = async (page: Page, cmd: string) => {
  await page.keyboard.press("Control+Shift+P");
  await page.getByPlaceholder(/Komut ara/).fill(cmd);
  await page.keyboard.press("Enter");
};
const savedFavorite = async (page: Page, teamId: string) => {
  const save = (await calls(page)).filter((c) => c.cmd === "layout_save").at(-1);
  return (save?.args.layout as { favorite?: { from?: string; leaders?: string[] } | null } | undefined)?.favorite ?? null;
};
const stage = (page: Page) => page.locator(".team-stage:not(.hidden)");

test("Favori: kaydet → Kart kilitlenir → yeni takım favoriyle ve kendi ajanlarıyla açılır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Fav Kaynak ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  // Favori yokken Kart sıradan bir paneldir (× var) — bugünkü davranış.
  await expect(app.getByRole("button", { name: "Kart panelini kapat" })).toHaveCount(1);

  await palette(app, "Bu düzeni favori yap");
  await expect.poll(() => savedFavorite(app, team.id)).not.toBeNull();
  const fav = (await savedFavorite(app, team.id))!;
  expect(fav.from).toBe(team.id);
  expect(fav.leaders).toEqual([leader.id]);

  // Kart sabit: × gitti, kilit geldi; reload sonrası da sürer.
  await expect.poll(() => stage(app).locator(".dex-tab-lock").count()).toBe(1);
  await expect(app.getByRole("button", { name: "Kart panelini kapat" })).toHaveCount(0);
  // Taşınamaz: Kart sekmesini lider grubuna sürüklemek hiçbir şeyi değiştirmez.
  const kartGroup = async () => (await groups(app)).find((g) => g.tabs.includes("Kart"))?.tabs;
  const before = await kartGroup();
  const kartTab = (await stage(app).locator(".dv-tab", { hasText: "Kart" }).boundingBox())!;
  const leadTab = (await stage(app).locator(".dv-tab", { hasText: leader.name }).boundingBox())!;
  await app.mouse.move(kartTab.x + kartTab.width / 2, kartTab.y + kartTab.height / 2);
  await app.mouse.down();
  await app.mouse.move(leadTab.x + 40, leadTab.y + 200, { steps: 12 });
  await app.mouse.up();
  expect(await kartGroup()).toEqual(before);
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await expect.poll(() => stage(app).locator(".dex-tab-lock").count()).toBe(1);

  // Yeni takım favoriyle açılır: Kart + kendi lideri + kendi worker'ları, kaynak ajan yok.
  const team2 = await newTeam(dexd, `Fav Yeni ${test.info().workerIndex}`);
  const agents2 = await teamAgents(dexd, team2.id);
  const leader2 = agents2.find((a) => a.kind === "leader")!;
  const workers2 = agents2.filter((a) => a.kind === "worker");
  await app.reload();
  await openTeam(app, team2.name);
  await stage(app).locator(".dv-tab", { hasText: leader2.name }).click();
  await paneOf(app, leader2.name);
  const tabs = await tabNames(app);
  expect(tabs).toEqual(expect.arrayContaining(["Kart", leader2.name, ...workers2.slice(0, 2).map((w) => expect.stringContaining(w.name))]));
  // Erişilebilir ad yalnız ajan adı; görünür nabız yaşı ona bitişmez.
  await expect(stage(app).getByRole("tab", { name: leader2.name, exact: true })).toHaveCount(1);
  // Adlar takımlar arasında aynı (Core, Kernel-1…): eşlemeyi kaydedilen dock'taki id'lerle doğrula.
  const savedIds = async () => {
    const save = (await calls(app)).filter((c) => c.cmd === "layout_save").at(-1);
    const dock = (save?.args.layout as { teams?: Record<string, { dock?: { panels?: Record<string, unknown> } }> } | undefined)?.teams?.[team2.id]?.dock;
    return Object.keys(dock?.panels ?? {}).filter((id) => id.startsWith("agent:")).sort();
  };
  await expect.poll(savedIds).toEqual(agents2.map((a) => `agent:${a.id}`).sort());
  for (const a of agents) expect(await savedIds()).not.toContain(`agent:${a.id}`);
});

test("Favori: kapanan worker paneli 'bu takıma uygula' ile geri gelir, 'tüm takımlara' hepsine uygular", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Fav Uygula ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.filter((a) => a.kind === "worker")[0];
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await palette(app, "Bu düzeni favori yap");
  await expect.poll(() => savedFavorite(app, team.id)).not.toBeNull();

  // Düzeni boz: bir worker panelini kapat.
  await app.getByRole("button", { name: `${worker.name} sekmesini kapat` }).click();
  await expect.poll(() => tabNames(app)).not.toEqual(expect.arrayContaining([expect.stringContaining(worker.name)]));

  await palette(app, "Favori düzeni bu takıma uygula");
  await expect.poll(() => tabNames(app)).toEqual(expect.arrayContaining([expect.stringContaining(worker.name)]));

  // İkinci takımda da boz, tümüne uygula.
  const team2 = await newTeam(dexd, `Fav Tümü ${test.info().workerIndex}`);
  const worker2 = (await teamAgents(dexd, team2.id)).filter((a) => a.kind === "worker")[0];
  await app.reload();
  await openTeam(app, team2.name);
  await paneOf(app, (await teamAgents(dexd, team2.id)).find((a) => a.kind === "leader")!.name);
  await app.getByRole("button", { name: `${worker2.name} sekmesini kapat` }).click();
  await expect.poll(() => tabNames(app)).not.toEqual(expect.arrayContaining([expect.stringContaining(worker2.name)]));

  await palette(app, "Favori düzeni tüm takımlara uygula");
  await expect.poll(() => tabNames(app)).toEqual(expect.arrayContaining([expect.stringContaining(worker2.name)]));
  await openTeam(app, team.name);
  await expect.poll(() => tabNames(app)).toEqual(expect.arrayContaining([expect.stringContaining(worker.name), "Kart"]));
});
