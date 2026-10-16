import { test, expect } from "./fixtures";

test.use({ viewport: { width: 360, height: 740 }, hasTouch: true, isMobile: true });

// #2159: sunucu "Kaldır" onayı Dex'in kendi diyaloğu — Android WebView'in yerel confirm kutusu değil.
test("mobil sunucu kaldırma onayı uygulama içi diyalogda; İptal korur, Kaldır siler", async ({ page, dexd }) => {
  await page.addInitScript(({ url, token }) => {
    if (sessionStorage.getItem("seeded")) return; // reloads keep what the test removed
    sessionStorage.setItem("seeded", "1");
    localStorage.setItem("dex.hosts", JSON.stringify([
      { id: "he2e01", name: "e2e", url, device: "" },
      { id: "he2e02", name: "ikinci", url, device: "" },
    ]));
    localStorage.setItem("dex.webtoken.he2e01", token);
    localStorage.setItem("dex.webtoken.he2e02", token);
  }, { url: dexd.url, token: dexd.token });
  page.on("dialog", (d) => { throw new Error(`yerel ${d.type()} kutusu açıldı: ${d.message()}`); });
  await page.goto("/");
  await page.getByRole("button", { name: "Sunucular" }).click();
  const row = page.locator(".m-host", { hasText: "ikinci" });
  const remove = row.getByRole("button", { name: "Kaldır" });

  await remove.click();
  const dialog = page.getByRole("alertdialog", { name: '"ikinci" kaldırılsın mı?' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("sunucuda çalışmaya devam eder");
  const cancel = dialog.getByRole("button", { name: "İptal" });
  await expect(cancel).toBeFocused(); // destructive: focus opens on İptal
  for (const b of await dialog.getByRole("button").all()) expect((await b.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  const box = (await dialog.locator(".set-dialog").boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(360);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(row).toBeVisible();
  await expect(remove).toBeFocused();

  await remove.click();
  await dialog.getByRole("button", { name: "Kaldır" }).click();
  await expect(dialog).toBeHidden();
  await expect(row).toBeHidden();
  await expect(page.locator(".m-host", { hasText: "e2e" })).toBeVisible();
});
