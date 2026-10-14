// Görev #32: Ctrl+wheel sets one shared, persisted terminal font size (Ctrl+0 resets it); the
// leader's "Temiz oturum" asks for a hand-over note, then restarts it once with `fresh`. The leader is
// the fake `claude` (e2e/fake-claude.rs).
import type { Locator, Page } from "@playwright/test";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const fontPx = (pane: Locator) => pane.locator(".xterm-rows").evaluate((el) => parseFloat(getComputedStyle(el).fontSize));

async function ctrlWheel(page: Page, pane: Locator, deltaY: number) {
  const box = (await pane.locator(".xterm-screen").boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, deltaY);
  await page.keyboard.up("Control");
}

const starts = async (page: Page, agentId: string) =>
  (await calls(page)).filter((c) => c.cmd === "agent_start" && c.args.id === agentId);

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Ctrl+tekerlek yazı boyutunu değiştirir, PTY'yi yeniden boyutlar, reload sonrası korunur; Ctrl+0 sıfırlar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Yazı ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  let pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  // The status poll (≤400 ms) lags the first output; until then the start button covers the grid's middle.
  await expect(pane.locator(".launch")).toBeHidden();
  const base = await fontPx(pane);
  const zoom = app.evaluate(() => window.devicePixelRatio);

  await ctrlWheel(app, pane, -100);
  await ctrlWheel(app, pane, -100);
  await expect.poll(() => fontPx(pane)).toBe(base + 2);
  expect(await app.evaluate(() => window.devicePixelRatio)).toBe(await zoom); // no page zoom
  // The bigger grid reached the PTY.
  await expect
    .poll(async () => (await calls(app)).filter((c) => c.cmd === "pty_resize" && c.args.id === leader.id).at(-1)?.args.cols)
    .toBe((await dexd.call<{ id: string; cols: number }[]>("pty.list")).find((p) => p.id === leader.id)!.cols);

  await app.reload();
  await openTeam(app, team.name);
  pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  expect(await fontPx(pane)).toBe(base + 2);

  await pane.locator(".xterm-screen").click();
  await app.keyboard.press("Control+0");
  await expect.poll(() => fontPx(pane)).toBe(base);
});

// Görev #51: the reset first asks the leader for a hand-over note (the `[Dex-Sistem]` prompt reaches the
// PTY); the fake cannot call `reset_ready` (core unit tests cover it), so the patron's "Şimdi sıfırla"
// finishes it here — and "İptal" keeps the session.
test("lider Temiz oturum: önce devir notu istemi gider; İptal oturumu korur, Şimdi sıfırla temiz açar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Temiz ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  const rows = pane.locator(".xterm-rows");
  await expect(rows).toContainText("fake-claude hazir");
  await expect(pane.locator(".launch")).toBeHidden();
  const button = pane.getByRole("button", { name: "Temiz oturum" });
  const strip = pane.locator(".pane-handover");
  const resets = async () => (await calls(app)).filter((c) => c.cmd === "leader_reset" && c.args.action === "begin");
  const session = async () => (await dexd.call<{ id: string; startedAt: number }[]>("pty.list")).filter((p) => p.id === leader.id);

  app.once("dialog", (d) => {
    expect(d.message()).toContain("konuşma geçmişi yeni oturuma taşınmaz");
    void d.dismiss();
  });
  await button.click();
  await app.waitForTimeout(500);
  expect(await resets()).toHaveLength(0);

  // Accept → the prompt goes into the idle leader; the pane says so and the button waits.
  const first = (await session())[0].startedAt;
  app.once("dialog", (d) => void d.accept());
  await button.click();
  await expect(strip).toContainText("Devir notu yazılıyor");
  await expect(rows).toContainText("RX:[Dex-Sistem] Oturumun sıfırlanacak");
  await expect(button).toBeDisabled();
  await expect(dexd.call("leader_reset", { id: leader.id, action: "begin" })).rejects.toThrow(/zaten sürüyor/);

  await strip.getByRole("button", { name: "İptal" }).click();
  await expect(strip).toBeHidden();
  await expect(button).toBeEnabled();
  expect((await session())[0].startedAt).toBe(first);

  // Again: the first prompt left the leader working (the fake sends no Stop), so this one waits its
  // turn. Reset without waiting for the note: one fresh session, painted clean.
  app.once("dialog", (d) => void d.accept());
  await button.click();
  await expect(strip).toContainText("istemi sırada");
  await strip.getByRole("button", { name: "Şimdi sıfırla" }).click();
  await expect(strip).toBeHidden();
  await expect.poll(async () => (await session())[0]?.startedAt ?? first).not.toBe(first);
  await expect(rows).toContainText("fake-claude hazir");
  await expect(rows).not.toContainText("RX:[Dex]");
  await expect(rows).not.toContainText("oturum kapandı");
  await expect(button).toBeEnabled();
  expect(await session()).toHaveLength(1);
  expect(await starts(app, leader.id)).toHaveLength(1); // the core restarted it, not the pane
});
