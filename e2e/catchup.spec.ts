// #2235: "Sen yokken" — Core grubunda Notlar'ın yaninda sekme, rozet yalnız bekleyen sayısı,
// üç bölüm, "+N daha", satır kaynağın panelini açar, "Okudum" imleci gösterilen verinin
// generatedAt'ine ilerletir (Date.now() değil). Veri __dexFake ile (gerçek dexd bu listeyi
// bir testte üretemez); kapatılan sekme geri gelmez, paletten döner.
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents, type Agent } from "./fixtures";

const AT = Date.now();
const GEN = AT - 5_000;

const question = (leader: Agent, worker: Agent) => ({
  id: 12, delegationId: 236, delegationStatus: "running", worker: worker.id, workerName: worker.name, leader: leader.id,
  leaderName: leader.name, teamId: leader.teamId, teamName: "Catchup takımı", question: "Hangi sırayla ilerleyeyim?",
  options: [], context: null, status: "escalated", note: null, answer: null,
  answeredBy: null, leaderDeliveredAt: 1, answerDeliveredAt: null, createdAt: AT - 60_000, updatedAt: AT - 60_000,
});

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Sen yokken: sekme + rozet, üç bölüm, Okudum generatedAt'i ack'ler, satır kaynağı açar, kapatınca kalır ve paletten döner", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Catchup ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;

  const done = Array.from({ length: 8 }, (_, i) => ({
    kind: "delegation_done", id: `delegation_done:${100 + i}`, title: `Bitmiş iş ${i + 1}`, at: AT - (i + 2) * 60_000, ref: 100 + i,
  }));
  await app.addInitScript((f) => { window.__dexFake = f; }, {
    question_list: [question(leader, worker)],
    proposal_list: [],
    catchup: {
      teamId: team.id, since: AT - 86_400_000, cursor: null, generatedAt: GEN,
      waiting: { items: [{ kind: "question_escalated", id: "question_escalated:12", title: "Hangi sırayla ilerleyeyim?", at: AT - 60_000, ref: 12 }], more: 0 },
      changed: { items: done, more: 0 },
      memories: { items: [{ kind: "memory", id: "memory:shared/not", title: "not — tek satır açıklama", at: AT - 120_000, ref: "shared/not" }], more: 0 },
    },
    catchup_ack: { seenAt: GEN },
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const stage = app.locator(".team-stage:not(.hidden)");
  const coreGroup = stage.locator(".dv-groupview").filter({ has: app.locator(".dex-tab-name", { hasText: leader.name }) });
  await expect.poll(() => coreGroup.locator(".dex-tab-name").allTextContents())
    .toEqual([leader.name, "Notlar", "Sen yokken"]);

  // Badge: only the waiting count (1), not the 9 others.
  const tab = stage.locator(".dv-tab").filter({ has: app.locator(".dex-tab-name", { hasText: "Sen yokken" }) });
  await expect(tab.locator(".catchup-badge")).toHaveText("1");
  await expect(tab.locator(".catchup-badge")).toHaveAttribute("aria-label", "1 seni bekliyor");

  await tab.click();
  const page = stage.locator(".catchup");
  await expect(page.getByRole("heading", { name: "Seni bekleyenler" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Sen yokken değişenler" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Yeni", exact: true })).toBeVisible();
  await expect(page.getByText("not — tek satır açıklama")).toBeVisible();

  // "Okudum" acks the shown snapshot's generatedAt — never a later "now".
  const okudum = page.getByRole("button", { name: "Okudum", exact: true });
  await okudum.focus();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "catchup_ack").map((c) => c.args))
    .toEqual([{ teamId: team.id, at: GEN }]);
  await expect(page.getByRole("button", { name: /Okundu/ })).toBeDisabled();
  // The list stays put after the ack.
  await expect(page.getByText("Hangi sırayla ilerleyeyim?")).toBeVisible();

  // Folded at 6: "+2 daha" unfolds the rest.
  await expect(page.locator(".catchup-row", { hasText: "Bitmiş iş 7" })).toHaveCount(0);
  await page.getByRole("button", { name: "+2 daha" }).click();
  await expect(page.locator(".catchup-row", { hasText: "Bitmiş iş 8" })).toBeVisible();

  // The waiting row opens its source: the asking worker's panel.
  const row = page.getByRole("button", { name: /Soru sana çıktı: Hangi sırayla ilerleyeyim\?/ });
  await row.focus();
  await app.keyboard.press("Enter");
  await expect(app.getByRole("region", { name: worker.name, exact: true })).toBeVisible();

  // Narrow: no sideways scroll inside the panel.
  await tab.click();
  await app.setViewportSize({ width: 480, height: 800 });
  await expect(page).toBeVisible();
  expect(await page.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(0);
  await app.setViewportSize({ width: 1400, height: 900 });

  // Closed by the patron, it stays closed across a reload; the palette brings it back.
  await stage.getByRole("button", { name: "Sen yokken sekmesini kapat", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect.poll(() => coreGroup.locator(".dex-tab-name").allTextContents()).toEqual([leader.name, "Notlar"]);
  // The debounced layout_save must carry the closed panel and the offered mark together
  // (polling the mark alone would pass on a pre-close save and the reload would resurrect the tab).
  await expect.poll(async () => {
    const save = (await calls(app)).filter((c) => c.cmd === "layout_save").at(-1);
    const t = (save?.args.layout as { teams?: Record<string, { dock?: { panels?: Record<string, unknown> }; catchup?: boolean }> })?.teams?.[team.id];
    return { open: !!t?.dock?.panels && "catchup" in t.dock.panels, offered: t?.catchup };
  }).toEqual({ open: false, offered: true });
  await app.reload();
  await openTeam(app, team.name);
  // The group's active tab is wherever the patron left it; bring Core forward first.
  await coreGroup.locator(".dv-tab").filter({ has: app.locator(".dex-tab-name", { hasText: leader.name }) }).click();
  await paneOf(app, leader.name);
  await expect.poll(() => coreGroup.locator(".dex-tab-name").allTextContents()).toEqual([leader.name, "Notlar"]);
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Sen yokken özetini aç");
  await app.keyboard.press("Enter");
  await expect.poll(() => coreGroup.locator(".dex-tab-name").allTextContents())
    .toEqual([leader.name, "Notlar", "Sen yokken"]);
});
