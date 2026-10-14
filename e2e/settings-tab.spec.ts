// Ayarlar grup gezinmeyle açılır; paletin limits komutu Sunucu › Kaynak sınırları
// sayfasına indirir — açık panel de oraya geçer.
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Palet 'kaynak sınırları' Ayarlar'ı Kaynak sınırları sayfasında açar, açık paneli de oraya geçirir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Ayar sekmesi ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.evaluate(() => localStorage.removeItem("dex.settingsTab"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name); // the team's dock is bound

  const ayarlar = app.getByRole("button", { name: "Ayarlar", exact: true });
  await expect(ayarlar).toHaveAttribute("title", "Ayarlar");
  await ayarlar.click();
  const tabs = app.getByRole("tablist", { name: "Ayar bölümleri" });
  await expect(tabs.getByRole("tab", { name: "Genel" })).toHaveAttribute("aria-selected", "true");

  await app.keyboard.press("Control+Shift+P");
  const palette = app.getByRole("dialog", { name: "Komut paleti" });
  await palette.getByRole("combobox").fill("kaynak sınırları");
  await expect(palette.getByRole("option").first()).toContainText("Kaynak sınırları");
  await palette.getByRole("option", { name: /Ayarlar → Sunucu: kaynak sınırları/ }).click();
  await expect(tabs.getByRole("tab", { name: "Kaynak sınırları" })).toHaveAttribute("aria-selected", "true");
  await expect(app.getByRole("tabpanel", { name: "Kaynak sınırları" })).toBeVisible();
  expect(await app.evaluate(() => localStorage.getItem("dex.settingsTab"))).toBe("resources");
});
