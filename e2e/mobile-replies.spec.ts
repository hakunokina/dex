// #1719 Phone: the top bar's "Echo cevapları" opens the replies list over the app, it follows the
// system light/dark scheme, and the system back gesture closes it.
import { test, expect, newTeam } from "./fixtures";

test.use({ viewport: { width: 390, height: 780 }, hasTouch: true, isMobile: true });

test("Echo cevapları listesi açılır, açık/koyu temayı izler, geri kapatır", async ({ page, dexd }) => {
  const team = await newTeam(dexd, `Mobil cevaplar ${test.info().workerIndex}`);
  await page.addInitScript(
    ({ url, token }) => {
      localStorage.setItem("dex.hosts", JSON.stringify([{ id: "he2e01", name: "e2e", url, device: "" }]));
      localStorage.setItem("dex.webtoken.he2e01", token);
    },
    { url: dexd.url, token: dexd.token },
  );
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await page.locator(".m-team-btn").click();
  await page.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();

  await page.getByRole("button", { name: "Echo cevapları", exact: true }).tap();
  const list = page.getByRole("dialog", { name: "Echo cevapları" });
  await expect(list).toBeVisible();
  await expect(list.locator(".m-hint")).toContainText("Henüz cevap yok");

  const bg = () => list.evaluate((el) => getComputedStyle(el).backgroundColor);
  const dark = await bg();
  await page.emulateMedia({ colorScheme: "light" });
  await expect.poll(bg).not.toBe(dark);

  await page.goBack();
  await expect(list).toHaveCount(0);
  await expect(page.locator(".m-top")).toBeVisible();
});
