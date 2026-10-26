// #2443: "Tüm worker'lar" popover'ında kapsam — uzmanlığa göre ve Kart'ta tıklama/sürükleme ile
// seçilen worker'lar; Uygula yalnız onları değiştirir, lidere ve kapsam dışına dokunmaz.
import type { Page } from "@playwright/test";
import { test, expect, newTeam, openTeam } from "./fixtures";

interface Rec { id: string; name: string; kind: string; teamId: string; model: string }
const crewOf = async (dexd: { call<T>(m: string, p?: unknown): Promise<T> }, teamId: string) =>
  (await dexd.call<Rec[]>("agent_list")).filter((a) => a.teamId === teamId);

/** A chip's box on screen (CSS px), from the team's World — the Kart draws with scale = unit, no offset. */
async function chipBox(app: Page, teamId: string, name: string) {
  return app.evaluate(async ({ teamId, name }) => {
    const { worldFor } = await import("/src/office/worlds.ts");
    const world = worldFor(teamId);
    const chip = world.chips.find((c) => c.agent.name === name)!;
    const r = document.querySelector("canvas.office")!.getBoundingClientRect();
    return { x: r.left + chip.x * world.unit, y: r.top + chip.y * world.unit, w: chip.w * world.unit, h: chip.h * world.unit };
  }, { teamId, name });
}

test("Tüm worker'lar: kapsam uzmanlığa göre ve Kart'ta seçilenlerle sınırlanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Kapsam ${test.info().workerIndex}`);
  const before = await crewOf(dexd, team.id);
  const lead = before.find((a) => a.kind === "leader")!;
  const named = (stem: string) => before.find((a) => a.name.startsWith(`${stem}-`))!;
  await app.reload();
  await openTeam(app, team.name);

  const kart = app.getByRole("group", { name: "Kart araçları" });
  await kart.getByRole("button", { name: "Tüm worker'lar", exact: true }).click();
  const bulk = app.getByRole("dialog", { name: "Tüm worker'lar", exact: true });
  const model = bulk.getByRole("combobox", { name: "Worker'ların modeli" });
  await expect(model).toBeVisible();
  const fresh = (await model.locator("option").evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value)))
    .filter((v) => !before.some((a) => a.model === v));
  expect(fresh.length, "işçilerin şu anki modelinden farklı iki model").toBeGreaterThanOrEqual(2);
  const [target, second] = fresh;
  await model.selectOption(target);

  // Başlık kapsam seçicidir: üç seçenek.
  await bulk.getByRole("button", { name: "Kapsam: Tüm worker'lar. Değiştir" }).click();
  for (const o of ["Tüm worker'lar", "Uzmanlığa göre", "Seçili worker'lar"]) {
    await expect(bulk.getByRole("button", { name: new RegExp(`^${o}`) })).toBeVisible();
  }

  // Uzmanlığa göre: Kernel + Probe → Tamam → başlık özeti → Uygula yalnız ikisini değiştirir.
  await bulk.getByRole("button", { name: /^Uzmanlığa göre/ }).click();
  await bulk.getByRole("checkbox", { name: /^Kernel/ }).check();
  await bulk.getByRole("checkbox", { name: /^Probe/ }).check();
  await bulk.getByRole("button", { name: "Tamam (2)", exact: true }).click();
  await expect(bulk.getByRole("button", { name: "Kapsam: Kernel, Probe · 2 worker. Değiştir" })).toBeFocused();
  await expect(model).toHaveValue(target); // model seçimi kapsam değişince kaybolmaz
  await bulk.getByRole("button", { name: "Uygula", exact: true }).click();
  await expect(bulk).toContainText("2 worker değişti");
  let after = await crewOf(dexd, team.id);
  const modelOf = (id: string) => after.find((a) => a.id === id)!.model;
  expect(modelOf(named("Kernel").id)).toBe(target);
  expect(modelOf(named("Probe").id)).toBe(target);
  expect(modelOf(named("Pixel").id)).toBe(named("Pixel").model);
  expect(modelOf(lead.id)).toBe(lead.model);

  // Seçili worker'lar: Kart seçim moduna geçer; tıklama tek worker'ı, sürükleme alanı seçer.
  await bulk.getByRole("button", { name: /^Kapsam: Kernel, Probe/ }).click();
  await bulk.getByRole("button", { name: /^Seçili worker'lar/ }).click();
  // Popover kenara çekilir (Kart'ı örtmesin); seçim çubuğu Kart'ın araç satırına oturur.
  await expect(bulk).toBeHidden();
  const bar = app.getByRole("group", { name: "Seçili worker'lar" });
  await expect(bar.getByRole("button", { name: "Vazgeç" })).toBeFocused();
  const status = bar.getByRole("status");
  await expect(status).toHaveText("Henüz seçim yok");
  const pixel = await chipBox(app, team.id, named("Pixel").name);
  await app.mouse.click(pixel.x, pixel.y);
  await expect(status).toHaveText("1 worker seçili");
  await expect(app.locator(".office-card")).toBeHidden(); // seçim modunda hover kartı yok
  await expect(bar).toBeVisible(); // Kart'a tıklamak seçimden çıkarmaz
  // Lidere tıklamak seçmez.
  const core = await chipBox(app, team.id, lead.name);
  await app.mouse.click(core.x, core.y);
  await expect(status).toHaveText("1 worker seçili");
  // Scout'u sürükleyerek alana al.
  const scout = await chipBox(app, team.id, named("Scout").name);
  await app.mouse.move(scout.x - scout.w / 2 - 6, scout.y - scout.h / 2 - 6);
  await app.mouse.down();
  await app.mouse.move(scout.x, scout.y, { steps: 4 });
  await app.mouse.move(scout.x + 2, scout.y + 2, { steps: 2 });
  await app.mouse.up();
  await expect(status).toHaveText("2 worker seçili");

  // Esc seçimden çıkar (popover açık kalır, kapsam değişmez); yeniden girince önceki seçim yok.
  await app.keyboard.press("Escape");
  await expect(bar).toHaveCount(0);
  await expect(bulk.getByRole("button", { name: /^Kapsam: Kernel, Probe/ })).toBeFocused();
  await bulk.getByRole("button", { name: /^Kapsam: Kernel, Probe/ }).click();
  await bulk.getByRole("button", { name: /^Seçili worker'lar/ }).click();
  await expect(status).toHaveText("Henüz seçim yok");
  // Klavye: Kart'ta ok tuşu + Boşluk seçimi aç/kapa yapar (lider ilk çiptir, seçilmez).
  const canvas = app.locator("canvas.office");
  await canvas.focus();
  await app.keyboard.press("Space");
  await expect(status).toHaveText("Henüz seçim yok");
  await app.keyboard.press("ArrowRight");
  await app.keyboard.press("Space");
  await expect(status).toHaveText("1 worker seçili");
  await app.keyboard.press("Space");
  await expect(status).toHaveText("Henüz seçim yok");
  await app.mouse.click(pixel.x, pixel.y);
  await app.mouse.move(scout.x - scout.w / 2 - 6, scout.y - scout.h / 2 - 6);
  await app.mouse.down();
  await app.mouse.move(scout.x + 2, scout.y + 2, { steps: 4 });
  await app.mouse.up();
  await bar.getByRole("button", { name: "Tamam (2)", exact: true }).click();
  await expect(bulk.getByRole("button", { name: "Kapsam: Seçili · 2 worker. Değiştir" })).toBeFocused();

  await model.selectOption(second);
  await bulk.getByRole("button", { name: "Uygula", exact: true }).click();
  await expect(bulk).toContainText("2 worker değişti");
  after = await crewOf(dexd, team.id);
  expect(modelOf(named("Pixel").id)).toBe(second);
  expect(modelOf(named("Scout").id)).toBe(second);
  expect(modelOf(named("Kernel").id)).toBe(target);
  expect(modelOf(lead.id)).toBe(lead.model);

  // Dar Kart (≈260px): seçim çubuğunun düğmeleri sığar, metin kısalır.
  await app.evaluate(async (teamId) => {
    const { boundApi } = await import("/src/layout/panels.ts");
    boundApi(teamId)!.getPanel("office")!.group.api.setSize({ width: 260 });
  }, team.id);
  await expect.poll(() => app.locator(".office-bar").evaluate((el) => el.getBoundingClientRect().width)).toBeLessThanOrEqual(262);
  await bulk.getByRole("button", { name: /^Kapsam: Seçili/ }).click();
  await bulk.getByRole("button", { name: /^Seçili worker'lar/ }).click();
  const barBox = (await bar.boundingBox())!;
  for (const name of ["Tamam (2)", "Vazgeç"]) {
    const b = (await bar.getByRole("button", { name, exact: true }).boundingBox())!;
    expect(b.x).toBeGreaterThanOrEqual(barBox.x);
    expect(b.x + b.width).toBeLessThanOrEqual(barBox.x + barBox.width + 0.5);
    expect(b.height).toBeGreaterThanOrEqual(24);
  }
  await app.keyboard.press("Escape");
  await expect(bar).toHaveCount(0);
  await app.keyboard.press("Escape");
  await expect(bulk).toHaveCount(0);
});
