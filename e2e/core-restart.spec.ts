// #206: the local core restarted (tray / Ayarlar → Çekirdek): the old `dexd --local` went with its
// PTYs and the app's socket comes back to the new one. Each test restarts its isolated dexd with
// the same data and endpoint: in-memory PTY replay must go with the old process too.
// Today nobody starts the leader again: its pane must not keep the old Claude's last frame (cursor
// on the status line, nothing to type into) but show a clean terminal, why, and ▶. When the core
// does start it again (`--resume`, before or after the socket is back), the pane shows that new
// session at its own size with the keys in it — no team switch needed.
import type { Locator, Page } from "@playwright/test";
import { isolatedTest as test, expect, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";
import { LOCAL_DOWN_KEY } from "./bridge";

const ptyOf = async (dexd: Dexd, id: string) =>
  (await dexd.call<{ id: string; cols: number; rows: number; startedAt: number }[]>("pty.list")).find((p) => p.id === id);
/** The xterm's rows, whether it fits its pane and whether the patron's keys go to it. */
const view = (pane: Locator) =>
  pane.evaluate((el) => {
    const screen = el.querySelector(".xterm-screen")!.getBoundingClientRect();
    const host = el.querySelector(".term-host")!.getBoundingClientRect();
    const at = document.activeElement;
    return {
      rows: el.querySelectorAll(".xterm-rows > div").length,
      fits: screen.height <= host.height + 1 && screen.width <= host.width + 1,
      focused: !!at && el.contains(at) && at.classList.contains("xterm-helper-textarea"),
    };
  });

/** `start`: the core starts the leader again before / after the app is back on it, or nobody does. */
async function restartCore(app: Page, dexd: Dexd, id: string, start: "before" | "after" | null) {
  await app.evaluate((k) => {
    localStorage.setItem(k, "1");
  }, LOCAL_DOWN_KEY);
  const n = await app.evaluate(() => window.__dexSockets!.length);
  await dexd.restart();
  await expect.poll(async () => (await ptyOf(dexd, id)) ?? null).toBeNull();
  if (start === "before") await dexd.call("agent_start", { id, options: {} });
  await app.evaluate((k) => localStorage.removeItem(k), LOCAL_DOWN_KEY);
  await expect.poll(() => app.evaluate(() => window.__dexSockets!.at(-1)!.readyState), { timeout: 15_000 }).toBe(1);
  expect(await app.evaluate(() => window.__dexSockets!.length)).toBeGreaterThan(n);
  if (start === "after") {
    await app.waitForTimeout(500);
    await dexd.call("agent_start", { id, options: {} });
  }
}

/** A team whose leader's (fake) Claude runs in its pane and took a line; the pane has the keys. */
async function running(app: Page, dexd: Dexd, name: string) {
  const team = await newTeam(dexd, name);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await pane.locator(".xterm-screen").click();
  await app.keyboard.type("once");
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:once");
  const before = (await ptyOf(dexd, leader.id))!;
  expect(await view(pane)).toEqual({ rows: before.rows, fits: true, focused: true });
  return { leader, pane, before };
}

async function typesInto(app: Page, pane: Locator, line: string) {
  await app.keyboard.type(line);
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText(`RX:${line}`);
}

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("çekirdek yeniden başlayınca kapanan lider pane'i temiz boş durum + neden + ▶ gösterir; ▶ klavyeyle açar", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 1280, height: 720 });
  const { leader, pane, before } = await running(app, dexd, `Kapanan ${test.info().workerIndex}`);

  await restartCore(app, dexd, leader.id, null);
  await expect(pane.getByRole("status").filter({ hasText: "Çekirdek yeniden başladı — oturum kapandı" })).toBeVisible();
  const launch = pane.getByRole("button", { name: "Claude'u başlat" });
  await expect(launch).toBeVisible();
  // Not the old session's frame, not a "closed" line drawn into it.
  await expect(pane.locator(".xterm-rows")).not.toContainText("RX:once");
  await expect(pane.locator(".xterm-rows")).not.toContainText("oturum kapandı");

  await launch.focus();
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await expect(pane.getByRole("status").filter({ hasText: "Çekirdek yeniden başladı" })).toHaveCount(0);
  await expect.poll(async () => (await ptyOf(dexd, leader.id))?.rows).toBe(before.rows);
  await expect.poll(() => view(pane)).toEqual({ rows: before.rows, fits: true, focused: true });
  await typesInto(app, pane, "sonra");
});

// A wide pane: the core's own start (120×40) is narrower than it — nothing but the pane can size it.
for (const start of ["before", "after"] as const)
  test(`çekirdek lideri yeniden açınca (${start === "before" ? "bağlanmadan önce" : "bağlandıktan sonra"}) pane yeni oturumu kendi boyutunda gösterir, yazılabilir`, async ({ app, dexd }) => {
    await app.setViewportSize({ width: 2400, height: 1400 });
    const { leader, pane, before } = await running(app, dexd, `Açılan ${test.info().workerIndex} ${start}`);

    await restartCore(app, dexd, leader.id, start);
    await expect.poll(async () => (await ptyOf(dexd, leader.id))?.startedAt).not.toBe(before.startedAt);
    await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
    await expect(pane.locator(".xterm-rows")).not.toContainText("RX:once");
    await expect.poll(async () => { const p = await ptyOf(dexd, leader.id); return [p?.cols, p?.rows]; }).toEqual([before.cols, before.rows]);
    await expect.poll(() => view(pane)).toEqual({ rows: before.rows, fits: true, focused: true });
    await typesInto(app, pane, "sonra");
  });
