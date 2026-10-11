// #44: Ayarlar opens on the tab the caller asks for — the palette's limits command lands on Ajanlar,
// also when Ayarlar is already open on another tab.
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Palet 'kaynak sınırları' Ayarlar'ı Ajanlar sekmesinde açar, açık paneli de oraya geçirir", async ({ app, dexd }) => {
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
  await expect(palette.getByRole("option").first()).toContainText("Ayarlar → Ajanlar");
  await app.keyboard.press("Enter");
  await expect(tabs.getByRole("tab", { name: "Ajanlar" })).toHaveAttribute("aria-selected", "true");
  await expect(app.getByRole("tabpanel", { name: "Ajanlar" })).toBeVisible();
  expect(await app.evaluate(() => localStorage.getItem("dex.settingsTab"))).toBe("agents");
});
