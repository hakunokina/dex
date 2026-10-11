// #62: one PTY, two clients — the desktop shell (bridge → dexd) and the phone (a second browser
// context, mobile viewport, its own dexd connection). The client the patron types into or focuses
// owns the PTY's size; the other draws its terminal at that size instead of its own.
import { devices, type Locator } from "@playwright/test";
import { test, expect, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";

const HOST = "he2e62";
const screen = (dexd: Dexd, agentId: string) => dexd.call<string>("chat_screen", { agentId });
const ptyCols = async (dexd: Dexd, id: string) =>
  (await dexd.call<{ id: string; cols: number }[]>("pty.list")).find((p) => p.id === id)?.cols ?? 0;
/** The desktop xterm's drawn width over its pane's: ~1 at the pane's own size, small at a phone's. */
const drawnShare = (pane: Locator) =>
  pane.evaluate((el) => {
    const host = el.querySelector(".term-host")!.getBoundingClientRect().width;
    return el.querySelector(".xterm-screen")!.getBoundingClientRect().width / host;
  });

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("mobil yazınca PTY telefon boyutuna geçer, masaüstü bozulmadan uyar; masaüstü odaklanınca geri alır", async ({ app, dexd, browser }) => {
  const team = await newTeam(dexd, `Boyut ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  // A wide worker pane, many more columns than the phone; tall enough that the default's shell row
  // (#65) leaves the workers two columns, not four.
  await app.setViewportSize({ width: 2400, height: 1400 });
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await expect.poll(() => ptyCols(dexd, worker.id)).toBeGreaterThan(80);
  const wide = await ptyCols(dexd, worker.id);

  // The phone opens the running session: it only watches, the PTY keeps the desktop's size.
  const phone = await browser.newContext({ ...devices["Pixel 7"], baseURL: test.info().project.use.baseURL });
  const mobile = await phone.newPage();
  await mobile.addInitScript(
    ({ host, url, token }) => {
      localStorage.setItem("dex.hosts", JSON.stringify([{ id: host, name: "e2e", url, device: "" }]));
      localStorage.setItem(`dex.webtoken.${host}`, token);
    },
    { host: HOST, url: dexd.url, token: dexd.token },
  );
  await mobile.goto("/");
  await mobile.locator(".m-team-btn").click();
  await mobile.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();
  await mobile.getByRole("tab", { name: "Ajanlar" }).click();
  await mobile.getByRole("button", { name: new RegExp(worker.name) }).click();
  // Sohbet is the default view; the terminal (and its PTY attach) only opens on request.
  await mobile.getByRole("button", { name: "Terminal", exact: true }).tap();
  await expect(mobile.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await mobile.waitForTimeout(800);
  expect(await ptyCols(dexd, worker.id)).toBe(wide);

  // The patron types on the phone: the PTY narrows, the desktop draws it at the left, intact.
  const line = mobile.getByRole("textbox", { name: "Terminale yaz" });
  await line.tap();
  await mobile.keyboard.type("mobil");
  await mobile.getByRole("button", { name: "Gönder" }).tap();
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:mobil");
  await expect.poll(() => ptyCols(dexd, worker.id)).toBeLessThan(wide * 0.75);
  await expect.poll(() => drawnShare(pane)).toBeLessThan(0.8);
  await expect(pane.locator(".xterm-rows")).toContainText("RX:mobil");

  // Back on the desktop: focusing the pane takes the size back, typing goes through at full width.
  await pane.getByRole("button", { name: /İzleme|Yazma/ }).click();
  await pane.locator(".xterm-screen").click();
  await expect.poll(() => ptyCols(dexd, worker.id)).toBe(wide);
  await expect.poll(() => drawnShare(pane)).toBeGreaterThan(0.9);
  await app.keyboard.type("masa");
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:masa");
  expect(await ptyCols(dexd, worker.id)).toBe(wide);

  // The phone, still watching, now draws the desktop's columns; its own refit does not narrow it.
  await mobile.setViewportSize({ width: 380, height: 600 });
  await mobile.waitForTimeout(800);
  expect(await ptyCols(dexd, worker.id)).toBe(wide);
  await phone.close();
});
