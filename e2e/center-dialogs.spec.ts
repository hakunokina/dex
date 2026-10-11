// #49: a floated Ayarlar (it is a tab beside the Kart by default) reopens in the middle of the Dex window, fit a small
// window with a 16 px margin, and a float the window shrinks under doesn't spill out of it.
import type { Locator, Page } from "@playwright/test";
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

async function centerOffset(app: Page, el: Locator) {
  const b = (await el.boundingBox())!;
  const vp = app.viewportSize()!;
  return { dx: b.x + b.width / 2 - vp.width / 2, dy: b.y + b.height / 2 - vp.height / 2 };
}

async function insideWindow(app: Page, el: Locator) {
  const b = (await el.boundingBox())!;
  const vp = app.viewportSize()!;
  expect(b.x).toBeGreaterThanOrEqual(0);
  expect(b.y).toBeGreaterThanOrEqual(0);
  expect(b.x + b.width).toBeLessThanOrEqual(vp.width);
  expect(b.y + b.height).toBeLessThanOrEqual(vp.height);
}

/** Ayarlar is a tab of the Kart group by default; "Yüzdür" in its tab menu makes it the floating panel. */
async function floatAyarlar(app: Page) {
  await app.getByRole("tab", { name: "Ayarlar", exact: true }).click({ button: "right" });
  await app.getByRole("menuitem", { name: "Yüzdür" }).click();
  const ayarlar = app.getByRole("dialog", { name: "Ayarlar" });
  await expect(ayarlar).toBeVisible();
  return ayarlar;
}

test("Yüzen Ayarlar küçük pencereye sığar, Yeni takım pencerenin ortasında açılır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Orta ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.setViewportSize({ width: 1280, height: 800 });
  await openTeam(app, team.name);
  await paneOf(app, leader.name); // the team's dock is bound

  const ayarlar = await floatAyarlar(app);

  // The window shrinks under the open float: it shrinks with it. (A float made with "Yüzdür" sits where
  // dockview puts it, so only a float centred at open or on reload is asserted to stay inside.)
  await app.setViewportSize({ width: 560, height: 360 });
  await expect.poll(async () => (await ayarlar.boundingBox())!.height).toBeLessThanOrEqual(360 - 32);

  await app.setViewportSize({ width: 1280, height: 800 });
  await app.getByRole("button", { name: "Yeni takım", exact: true }).click();
  const form = app.locator("form.team-pop");
  await expect(form).toBeVisible();
  await expect(form.getByLabel("Ad")).toBeFocused();
  const c = await centerOffset(app, form);
  expect(Math.abs(c.dx)).toBeLessThanOrEqual(2);
  expect(Math.abs(c.dy)).toBeLessThanOrEqual(2);

  await app.setViewportSize({ width: 480, height: 360 });
  await insideWindow(app, form);
  const b = (await form.boundingBox())!;
  expect(b.height).toBeLessThanOrEqual(360 - 32 + 1);
  await form.press("Escape");
  await expect(form).toHaveCount(0);
});

test("Kayıtlı düzendeki Ayarlar yeniden açılışta yine pencerenin ortasından başlar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Orta yeniden ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.setViewportSize({ width: 1280, height: 800 });
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  const ayarlar = await floatAyarlar(app);
  await app.waitForTimeout(800); // layout_save is debounced (400 ms): the float is saved

  // Saved at the 1280 px window's middle; a narrower window must not reopen it there.
  await app.setViewportSize({ width: 900, height: 700 });
  await app.reload();
  await openTeam(app, team.name);
  await expect(ayarlar).toBeVisible();
  await expect.poll(async () => Math.abs((await centerOffset(app, ayarlar)).dx)).toBeLessThanOrEqual(2);
  await expect.poll(async () => Math.abs((await centerOffset(app, ayarlar)).dy)).toBeLessThanOrEqual(2);
});
