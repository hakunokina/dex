// #209: maliyet arayüzü — görev kartında işin token + $'ı, Kart araç çubuğunda takımın bugün / 7 gün
// toplamı, çip hover'ında ajanın bugünkü kırılımı. Veriyi dexd ancak gerçek transcript'ten üretir;
// burada usage_summary ve görevi bağlayan devretme köprüde hazır cevaplanır (__dexFake).
// Eski dexd (auth.hello listesinde usage_summary yok) → hiçbiri görünmez.
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const T = (input: number, output: number, cacheRead: number, cacheWrite: number, usd: number | null) => ({
  tokens: { input, output, cacheRead, cacheWrite },
  usd,
});

test("Maliyet: görev kartı, Kart toplamı ve çip hover'ı; eski dexd'de gizli", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Maliyet ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  const task = await dexd.call<{ id: number }>("task_create", { task: { title: "maliyetli iş", teamId: team.id } });
  const now = Date.now();
  const fake = {
    delegation_list: [
      {
        id: 501, fromAgent: leader.id, toAgent: worker.id, teamId: team.id, serviceTeamId: null, prompt: "iş",
        status: "done", summary: "bitti", resultPath: null, taskId: task.id, createdAt: now, updatedAt: now,
        deliveredAt: now, readAt: now, seenAt: now,
      },
    ],
    usage_summary: {
      costLabel: "API eşdeğer maliyeti",
      total: T(1000, 2000, 9000, 300, 0.41),
      agents: { [worker.id]: T(1000, 2000, 9000, 300, 0.41) },
      delegations: { "501": T(1000, 2000, 9000, 300, 0.41) },
      warnings: [],
    },
  };
  await app.addInitScript((f) => {
    window.__dexFake = f;
  }, fake);
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  // #2139: Kart araç çubuğunda token/maliyet metni yok (bugün/7 gün kullanım şeridinde).
  const bar = app.getByRole("group", { name: "Kart araçları" });
  await expect(bar.getByRole("button", { name: "Görevler" })).toBeVisible();
  await expect(bar.locator(".office-cost")).toHaveCount(0);
  await expect(bar).not.toContainText("tok");

  // Görev kartı: işin toplamı.
  await bar.getByRole("button", { name: "Görevler" }).click();
  const board = app.getByRole("complementary");
  const card = board.locator(".board-card", { hasText: "maliyetli iş" });
  await expect(card.locator(".board-cost")).toHaveText("12.3K tok · $0.41");
  await expect(card.locator(".board-cost")).toHaveAttribute("title", /API eşdeğer maliyeti/);
  await app.keyboard.press("Control+Backquote");

  // Çip hover'ı: işçinin çipini bul, kartta bugünkü toplam + kırılım.
  const canvas = app.locator("canvas.office");
  const box = (await canvas.boundingBox())!;
  const hover = app.locator(".office-card");
  let found = false;
  for (let y = box.y + 8; y < box.y + box.height && !found; y += 16) {
    for (let x = box.x + 8; x < box.x + box.width && !found; x += 20) {
      await app.mouse.move(x, y);
      await app.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
      found = (await hover.isVisible()) && (await hover.locator("b").textContent()) === worker.name;
    }
  }
  expect(found).toBe(true);
  await expect(hover).toContainText("12.3K tok · $0.41 · API eşdeğeri");
  await expect(hover).toContainText("girdi 1.0K · çıktı 2.0K · cache 9.0K/300");

  // Dar görünüm: araç çubuğu taşmaz.
  await app.setViewportSize({ width: 720, height: 700 });
  await expect.poll(() => bar.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);

  // Eski dexd: hiçbir maliyet görünmez.
  await app.addInitScript(() => {
    window.__dexOld = { hide: ["usage_summary"] };
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await expect(bar.getByRole("button", { name: "Görevler" })).toBeVisible();
  await expect(bar.locator(".office-cost")).toHaveCount(0);
  await bar.getByRole("button", { name: "Görevler" }).click();
  await expect(board.locator(".board-card", { hasText: "maliyetli iş" })).toBeVisible();
  await expect(board.locator(".board-cost")).toHaveCount(0);
});
