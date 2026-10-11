import { test, expect, newTeam, teamAgents } from "./fixtures";

test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

test("mobil Kart barlar arasını doldurur; dokunma, klavye ve örtülü animasyon", async ({ page, dexd }) => {
  const team = await newTeam(dexd, `Mobil Kart ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const worker = agents.find(a => a.kind === "worker")!;
  const leader = agents.find(a => a.kind === "leader")!;
  // Use awake agents so the existing delayed offline-start bridge cannot reopen
  // a screen while this test closes it to measure the PCB underneath.
  await dexd.call("agent_start", { id: worker.id, options: {} });
  await dexd.call("agent_start", { id: leader.id, options: {} });
  await page.addInitScript(({ url, token }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "he2e01", name: "e2e", url, device: "" }]));
    localStorage.setItem("dex.webtoken.he2e01", token);
  }, { url: dexd.url, token: dexd.token });
  await page.goto("/");
  await page.locator(".m-team-btn").click();
  await page.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();
  const canvas = page.locator("canvas.office");
  await expect(canvas).toBeVisible();

  const scene = () => page.evaluate(async teamId => {
    const { worldFor } = await import("/src/office/worlds.ts");
    const world = worldFor(`he2e01:${teamId}`);
    return {
      time: world.time, w: world.w * world.unit, h: world.h * world.unit,
      chips: world.chips.map(c => ({ id: c.agent.id, x: c.x * world.unit, y: c.y * world.unit, w: c.w * world.unit, h: c.h * world.unit })),
    };
  }, team.id);

  for (const viewport of [{ width: 390, height: 844 }, { width: 412, height: 915 }]) {
    await page.setViewportSize(viewport);
    await expect.poll(async () => {
      const box = (await canvas.boundingBox())!;
      const top = (await page.locator(".m-top").boundingBox())!;
      const tabs = (await page.locator(".m-tabs").boundingBox())!;
      const world = await scene();
      return Math.max(Math.abs(box.x), Math.abs(box.width - viewport.width), Math.abs(box.y - top.y - top.height),
        Math.abs(box.y + box.height - tabs.y), Math.abs(world.w - box.width), Math.abs(world.h - box.height));
    }).toBeLessThan(1);
    const box = (await canvas.boundingBox())!;
    for (const chip of (await scene()).chips) {
      expect(chip.x - chip.w / 2).toBeGreaterThanOrEqual(0);
      expect(chip.x + chip.w / 2).toBeLessThanOrEqual(box.width);
      expect(chip.y - chip.h / 2).toBeGreaterThanOrEqual(0);
      expect(chip.y + chip.h / 2).toBeLessThanOrEqual(box.height);
    }
  }

  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setSafeAreaInsetsOverride", { insets: { top: 24, bottom: 20, left: 12, right: 12 } });
  await expect.poll(async () => (await canvas.boundingBox())!.width).toBe(412 - 24);
  const safeBox = (await canvas.boundingBox())!;
  const safeTop = (await page.locator(".m-top").boundingBox())!;
  const safeTabs = (await page.locator(".m-tabs").boundingBox())!;
  expect(safeBox.x).toBe(12);
  expect(safeBox.y).toBeCloseTo(safeTop.y + safeTop.height, 0);
  expect(safeBox.y + safeBox.height).toBeCloseTo(safeTabs.y, 0);
  await cdp.send("Emulation.setSafeAreaInsetsOverride", { insets: { top: 0, bottom: 0, left: 0, right: 0 } });

  // A keyboard activation uses the same existing mobile agent-screen path as a tap.
  await canvas.focus();
  await page.keyboard.press("Home");
  for (let i = 0; i < agents.length; i++) {
    if ((await canvas.getAttribute("aria-label"))?.startsWith(`${worker.name}.`)) break;
    await page.keyboard.press("ArrowRight");
  }
  await expect(canvas).toHaveAttribute("aria-label", new RegExp(`^${worker.name}\\.`));
  expect(await canvas.evaluate(el => getComputedStyle(el).outlineStyle)).toBe("solid");
  await page.keyboard.press("Enter");
  const screen = page.locator(".m-agent-screen");
  await expect(screen.locator(".m-agent-title b")).toHaveText(worker.name);
  // The PCB is still mounted under the screen; it must stop simulating and drawing.
  const paused = (await scene()).time;
  await page.waitForTimeout(180);
  expect((await scene()).time).toBe(paused);
  await screen.getByRole("button", { name: "Geri", exact: true }).tap();
  await expect(screen).toHaveCount(0);
  await expect.poll(async () => (await scene()).time).toBeGreaterThan(paused);

  const box = (await canvas.boundingBox())!;
  const workerChip = (await scene()).chips.find(c => c.id === `he2e01:${worker.id}`)!;
  await page.mouse.move(box.x + workerChip.x, box.y + workerChip.y);
  await expect(page.locator(".office-card")).toBeVisible();
  await page.touchscreen.tap(box.x + workerChip.x, box.y + workerChip.y);
  await expect(screen.locator(".m-agent-title b")).toHaveText(worker.name);
  await screen.getByRole("button", { name: "Geri", exact: true }).tap();
  await expect(page.locator(".office-card")).toBeHidden();
  const core = (await scene()).chips.find(c => c.id === `he2e01:${leader.id}`)!;
  await page.touchscreen.tap(box.x + core.x, box.y + core.y);
  await expect(screen.locator(".m-agent-title b")).toHaveText(leader.name);
  await screen.getByRole("button", { name: "Geri", exact: true }).tap();
  await expect(page.locator(".office-card")).toBeHidden();
  await page.getByRole("tab", { name: "Ajanlar", exact: true }).tap();
  const offTab = (await scene()).time;
  await page.waitForTimeout(180);
  expect((await scene()).time).toBe(offTab);
  await page.getByRole("tab", { name: "Kart", exact: true }).tap();
  await expect.poll(async () => (await scene()).time).toBeGreaterThan(offTab);
  // Exercise the browser/app visibility event without relying on headless tab focus.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  const background = (await scene()).time;
  await page.waitForTimeout(180);
  expect((await scene()).time).toBe(background);
  await page.evaluate(() => {
    delete (document as unknown as { hidden?: boolean }).hidden;
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(async () => (await scene()).time).toBeGreaterThan(background);
});
