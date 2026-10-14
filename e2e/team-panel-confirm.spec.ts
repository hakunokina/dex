// Takım paneli onayları: üye silme ve "Favoriyi tüm takımlara uygula" alertdialog ister.
import type { Page } from "@playwright/test";
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const groups = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll(".team-stage:not(.hidden) .dv-grid-view .dv-groupview")].map((g) => ({
      tabs: [...g.querySelectorAll(".dv-tab")].map((t) => (t.querySelector(".dex-tab-name") ?? t).textContent?.trim() ?? ""),
    })),
  );
const tabNames = async (page: Page) => (await groups(page)).flatMap((g) => g.tabs);

async function openTeamPanel(page: Page) {
  await page.keyboard.press("Control+Shift+P");
  await page.getByPlaceholder(/Komut ara/).fill("Takım paneli");
  await page.keyboard.press("Enter");
  await expect(visiblePanel(page)).toBeVisible();
}

// Takım panelleri her takımın dock'undadır; gizli takımlarınki de DOM'dadır.
const visiblePanel = (page: Page) => page.locator(".team-stage:not(.hidden) .team-settings");

test("Üye silme: ⋯ menü + onay; Vazgeç tutar, onayla siler", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Sil Onay ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await openTeamPanel(app);
  const panel = visiblePanel(app);

  // Idle bir üye ekle (çalışan ajan zaten silinemez).
  await panel.getByPlaceholder(/Ad \(boş/).fill("Silinecek");
  await panel.getByRole("button", { name: "Ekle" }).click();
  const ops = panel.getByRole("button", { name: "Silinecek işlemleri" });
  await expect(ops).toBeVisible();
  await expect
    .poll(async () => (await teamAgents(dexd, team.id)).map((a) => a.name))
    .toContain("Silinecek");

  // Vazgeç: üye durur.
  await ops.click();
  await panel.getByRole("menuitem", { name: "Üyeyi sil" }).click();
  const dialog = app.getByRole("alertdialog");
  await expect(dialog).toContainText("Silinecek");
  await dialog.getByRole("button", { name: "Vazgeç" }).click();
  await expect
    .poll(async () => (await teamAgents(dexd, team.id)).map((a) => a.name))
    .toContain("Silinecek");
  await expect(panel.getByRole("button", { name: "Silinecek işlemleri" })).toBeVisible();

  // Klavye: ⋯ Enter ile açılır, Escape kapatıp odağı geri verir.
  await panel.getByRole("button", { name: "Silinecek işlemleri" }).focus();
  await app.keyboard.press("Enter");
  await expect(panel.getByRole("menuitem", { name: "Üyeyi sil" })).toBeFocused();
  await app.keyboard.press("Escape");
  await expect(panel.getByRole("button", { name: "Silinecek işlemleri" })).toBeFocused();

  // Onayla: üye gider.
  await panel.getByRole("button", { name: "Silinecek işlemleri" }).click();
  await panel.getByRole("menuitem", { name: "Üyeyi sil" }).click();
  await app.getByRole("alertdialog").getByRole("button", { name: "Üyeyi sil" }).click();
  await expect(panel.getByRole("button", { name: "Silinecek işlemleri" })).toHaveCount(0);
  await expect
    .poll(async () => (await teamAgents(dexd, team.id)).map((a) => a.name))
    .not.toContain("Silinecek");
});

test("Takım paneli 'Favoriyi tüm takımlara uygula' onay ister", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Tümü Onay ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.filter((a) => a.kind === "worker")[0];
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await openTeamPanel(app);
  const panel = visiblePanel(app);

  // Favoriyi bu takımın düzeniyle (yeniden) kaydet — başka spec artığı favori olabilir.
  await panel.getByRole("button", { name: /Bu düzeni favori yap|Favori düzeni güncelle/ }).click();
  await expect(panel.getByRole("button", { name: "Favoriyi tüm takımlara uygula" })).toBeVisible();

  // Düzeni boz: bir worker panelini kapat.
  await app.getByRole("button", { name: `${worker.name} sekmesini kapat` }).click();
  await expect.poll(() => tabNames(app)).not.toEqual(expect.arrayContaining([expect.stringContaining(worker.name)]));

  // Onaysız uygulanmaz: dialog Vazgeç ile kapanır, panel kapalı kalır.
  await panel.getByRole("button", { name: "Favoriyi tüm takımlara uygula" }).click();
  const dialog = app.getByRole("alertdialog");
  await expect(dialog).toContainText("takımın panelleri favori düzene döner");
  await dialog.getByRole("button", { name: "Vazgeç" }).click();
  await expect.poll(() => tabNames(app)).not.toEqual(expect.arrayContaining([expect.stringContaining(worker.name)]));

  // Onayla: uygulanır.
  await panel.getByRole("button", { name: "Favoriyi tüm takımlara uygula" }).click();
  await app.getByRole("alertdialog").getByRole("button", { name: "Tümüne uygula" }).click();
  await expect.poll(() => tabNames(app)).toEqual(expect.arrayContaining([expect.stringContaining(worker.name)]));
});
