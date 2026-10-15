// #151: the local core (`dexd --local`) on screen — down → the team's band with "Çekirdeği başlat";
// another build → "Çekirdek güncel değil"; Ayarlar → Bağlantılar → Sunucular (bu bilgisayar satırı, "Çekirdeği kapat").
// bridge.ts fakes the shell side: LOCAL_DOWN_KEY makes `local_host` answer "down", FAKE_LIFE is
// `core_life` (the worker's dexd is `serve`, which keeps that method to a local endpoint).
import { test, expect, calls, newTeam, openTeam } from "./fixtures";
import { FAKE_LIFE, LOCAL_DOWN_KEY, LOCAL_SLOW_KEY } from "./bridge";

test("yerel çekirdek kapalı: takım bandı, klavyeyle 'Çekirdeği başlat', bağlanınca bant gider", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Çekirdek ${test.info().workerIndex}`);
  await app.reload(); // the team is listed (and cached) while the core runs
  await openTeam(app, team.name);
  await app.evaluate((k) => localStorage.setItem(k, "1"), LOCAL_DOWN_KEY);
  await app.reload();

  // Its tab comes from the cache; the stage is greyed with the band.
  await openTeam(app, team.name);
  const band = app.locator(".team-stage:not(.hidden) .host-banner");
  await expect(band).toContainText("Yerel çekirdek çalışmıyor");
  await app.setViewportSize({ width: 1100, height: 640 });
  expect(await band.evaluate((el) => el.scrollWidth <= el.clientWidth && el.offsetHeight <= 28)).toBe(true);
  await expect(app.locator(".team-stage:not(.hidden)")).toHaveClass(/host-down/);
  const start = band.getByRole("button", { name: "Çekirdeği başlat" });
  await start.focus();
  await expect(start).toBeFocused();
  await app.keyboard.press("Enter");

  await expect(band).toHaveCount(0);
  await expect(app.locator(".team-stage:not(.hidden)")).not.toHaveClass(/host-down/);
  expect((await calls(app)).some((c) => c.cmd === "local_start")).toBe(true);
});

// #203: the shell took a while and the core never came (a stale dexd that exited at once) — the
// stage said nothing for 40 s, then a band without a reason. Now: "açılıyor", then the band + why.
test("çekirdek açılamadı: önce 'açılıyor', sonra bant ve nedeni", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Açılmayan ${test.info().workerIndex}`);
  await app.reload();
  await openTeam(app, team.name);
  const why = String.raw`yerel çekirdek açılırken çıktı (kod 2): bilinmeyen seçenek: --local — C:\Users\x\AppData\Roaming\com.hakunokina.dex\logs\dexd-stderr.log`;
  await app.evaluate(([dk, sk, w]) => (localStorage.setItem(dk, w), localStorage.setItem(sk, "2500")), [LOCAL_DOWN_KEY, LOCAL_SLOW_KEY, why]);
  await app.reload();

  await expect(app.getByRole("status").filter({ hasText: "Yerel çekirdek açılıyor…" })).toBeVisible();
  await openTeam(app, team.name);
  const band = app.locator(".team-stage:not(.hidden) .host-banner");
  await expect(band).toContainText("Yerel çekirdek çalışmıyor");
  await expect(band.locator(".core-down-why")).toHaveAttribute("title", why);
  await expect(app.getByText("Yerel çekirdek açılıyor…")).toHaveCount(0);
  // The long reason is cut on one line; the button stays.
  await app.setViewportSize({ width: 1100, height: 640 });
  const fit = await band.evaluate((el) => ({ sw: el.scrollWidth, cw: el.clientWidth, h: el.offsetHeight, kids: [...el.children].map((c) => [c.className, (c as HTMLElement).offsetWidth, (c as HTMLElement).offsetHeight]) }));
  expect(fit.sw <= fit.cw && fit.h <= 28, JSON.stringify(fit)).toBe(true);
  await expect(band.getByRole("button", { name: "Çekirdeği başlat" })).toBeVisible();
});

test("çekirdek başka derleme: 'Çekirdek güncel değil' bandı, Gizle ile kalkar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Sürüm ${test.info().workerIndex}`);
  await app.addInitScript(() => {
    window.__dexOld = { commit: "0ld0000000", commitTime: 1 };
  });
  await app.reload();
  await openTeam(app, team.name);
  const band = app.locator(".team-stage:not(.hidden) .host-banner");
  await expect(band).toContainText("Çekirdek güncel değil: çalışan çekirdek bu arayüzden eski bir sürüm");
  await expect(band).toContainText("0ld0000000");
  // A warning, not a dead host: the stage stays usable.
  await expect(app.locator(".team-stage:not(.hidden)")).toHaveClass(/host-warn/);
  // The window's narrowest size (tauri.conf minWidth 1100): one line, inside the stage's 28 px.
  await app.setViewportSize({ width: 1100, height: 640 });
  expect(await band.evaluate((el) => el.scrollWidth <= el.clientWidth && el.offsetHeight <= 28)).toBe(true);
  await band.getByRole("button", { name: "Çekirdek sürüm uyarısını gizle" }).click();
  await expect(band).toHaveCount(0);
});

test("Ayarlar → Bağlantılar → Sunucular: bu bilgisayar satırı ve onaylı 'Çekirdeği kapat'", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Ayar çekirdek ${test.info().workerIndex}`);
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Bağlantılar" }).click();
  const servers = app.getByRole("region", { name: "Sunucular", exact: true });
  const core = servers.filter({ hasText: "Bu bilgisayar" });
  await expect(core).toContainText("Bu bilgisayar (yerel çekirdek)");
  await expect(core).toContainText("çalışıyor");
  await expect(core).toContainText(String(FAKE_LIFE.pid));
  await expect(core).toContainText(FAKE_LIFE.commit);
  await expect(core).toContainText(`pencere ${FAKE_LIFE.clients}`);
  await expect(core).toContainText(`ajan ${FAKE_LIFE.agents}`);

  // Vazgeç: hiçbir şey çağrılmaz; onayla: çekirdek kapanır.
  await servers.getByRole("button", { name: "Çekirdeği kapat" }).click();
  const dialog = app.getByRole("alertdialog", { name: "Yerel çekirdek kapatılsın mı?" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Vazgeç", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect((await calls(app)).some((c) => c.cmd === "local_shutdown")).toBe(false);
  await servers.getByRole("button", { name: "Çekirdeği kapat" }).click();
  await app.getByRole("alertdialog", { name: "Yerel çekirdek kapatılsın mı?" }).getByRole("button", { name: "Çekirdeği kapat", exact: true }).click();
  await expect.poll(async () => (await calls(app)).some((c) => c.cmd === "local_shutdown")).toBe(true);
});
