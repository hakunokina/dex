// #2258: a leader's pane in its own window. Chromium cannot open a second native window, so the
// pop-out page (`?pane=lead`) is a second page on the same dexd: the main page must drop its PTY
// subscription and show the placeholder; the pop-out page takes the PTY; "Geri al" (and the
// shell's closed event) bring the pane back with a fresh subscription.
import type { Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";
import { installBridge } from "./bridge";

const attaches = async (page: Page, agentId: string) => (await calls(page)).filter((c) => c.cmd === "pty.attach" && c.args.id === agentId);
/** `pty.detach` names a channel; the channel was named with the agent in its `pty.attach`. */
async function detaches(page: Page, agentId: string) {
  const all = await calls(page);
  const channels = new Set(all.filter((c) => c.cmd === "pty.attach" && c.args.id === agentId).map((c) => c.args.channel));
  return all.filter((c) => c.cmd === "pty.detach" && channels.has(c.args.channel));
}

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("lider pane'i pencereye ayrılır: ana pencere aboneliği bırakır, yer tutucu + Geri al; pop-out sayfası PTY'yi alır", async ({ app, dexd }) => {
  const n = test.info().workerIndex;
  const team = await newTeam(dexd, `PiP ${n}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  const region = app.getByRole("region", { name: leader.name, exact: true });
  const detachBtn = region.getByRole("button", { name: `${leader.name}: pencereye ayır` });
  await expect(detachBtn).toHaveAttribute("aria-pressed", "false");
  const attachedBefore = (await attaches(app, leader.id)).length;
  expect(attachedBefore).toBeGreaterThan(0);

  // Detach: the terminal view goes, the placeholder with both actions takes the panel, the subscription is dropped.
  // (Dockview draws panel bodies in an overlay outside their group: the placeholder is found on the page.)
  await detachBtn.click();
  const placeholder = app.getByRole("status").filter({ hasText: "Ayrı pencerede açık" });
  await expect(placeholder).toBeVisible();
  await expect(placeholder.getByRole("button", { name: "Pencereye git" })).toBeVisible();
  await expect(placeholder.getByRole("button", { name: "Geri al" })).toBeVisible();
  await expect(region.getByRole("button", { name: `${leader.name}: ayrı pencereye git` })).toHaveAttribute("aria-pressed", "true");
  await expect(pane).toHaveCount(0);
  await expect.poll(async () => (await detaches(app, leader.id)).length).toBeGreaterThan(0);
  await app.waitForTimeout(600);
  expect((await attaches(app, leader.id)).length).toBe(attachedBefore);

  // The pop-out page: the same bundle routed by the query, one pane, the window's own PTY subscription.
  const lead = await app.context().newPage();
  await lead.addInitScript(installBridge, { url: dexd.url, token: dexd.token, pick: null, localWs: process.env.DEX_LOCAL_WS !== "0" });
  await lead.goto(`/?pane=lead&team=${encodeURIComponent(team.id)}&agent=${encodeURIComponent(leader.id)}`);
  await expect(lead.locator(".lead-title")).toHaveText(`${team.name} · ${leader.name}`);
  await expect(lead.locator("section.pane .xterm-rows")).toContainText("fake-claude hazir");
  await expect.poll(async () => (await attaches(lead, leader.id)).length).toBeGreaterThan(0);
  expect((await calls(lead)).some((c) => c.cmd === "layout_save")).toBe(false);
  const pin = lead.getByRole("button", { name: "Üstte tut" });
  await expect(pin).toHaveAttribute("aria-pressed", "false");
  await pin.click();
  await expect(pin).toHaveAttribute("aria-pressed", "true");
  // Small window: the pane's control row wraps, nothing scrolls sideways; the terminal stays.
  await lead.setViewportSize({ width: 380, height: 260 });
  await expect(lead.getByRole("tab", { name: "Terminal" })).toBeVisible();
  expect(await lead.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await lead.close();

  // Back, by keyboard: the pane returns with a fresh subscription (the repainted snapshot).
  await placeholder.getByRole("button", { name: "Geri al" }).focus();
  await app.keyboard.press("Enter");
  await expect(placeholder).toHaveCount(0);
  const back = await paneOf(app, leader.name);
  await expect(back.locator(".xterm-rows")).toContainText("fake-claude hazir");
  expect((await attaches(app, leader.id)).length).toBe(attachedBefore + 1);
  await expect(detachBtn).toHaveAttribute("aria-pressed", "false");

  // Detached again, then the shell reports the window destroyed (src-tauri/src/popout.rs): same return.
  await detachBtn.click();
  await expect(placeholder).toBeVisible();
  await app.evaluate((label) => (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "dex:lead-closed", payload: label }), `lead-${leader.id}`);
  await expect(placeholder).toHaveCount(0);
  await expect((await paneOf(app, leader.name)).locator(".xterm-rows")).toContainText("fake-claude hazir");
});

// #2260: layout.json is not the truth, the living windows are. The open flip is written at once (no
// 400 ms debounce), and at startup a living `lead-*` window makes its leader popped out even when
// the file never got the record — the pane stays a placeholder, this page takes no subscription.
test("ayır → hemen yenile: yer tutucu kalır, ana sayfa abone olmaz; kayıtsız yaşayan pencere de ayrılmış sayılır", async ({ app, dexd }) => {
  const n = test.info().workerIndex;
  const team = await newTeam(dexd, `PiP yarış ${n}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  const label = `lead-${leader.id}`;
  await app.reload();
  await openTeam(app, team.name);
  await expect((await paneOf(app, leader.name)).locator(".xterm-rows")).toContainText("fake-claude hazir");
  const region = app.getByRole("region", { name: leader.name, exact: true });
  const placeholder = app.getByRole("status").filter({ hasText: "Ayrı pencerede açık" });
  const savedOpen = async (page: Page, open: boolean) =>
    (await calls(page)).some((c) => c.cmd === "layout_save" && (c.args.layout as { popouts?: Record<string, { open?: boolean }> })?.popouts?.[leader.id]?.open === open);

  // Detach, reload as soon as the write is on its way (well inside the old debounce).
  await region.getByRole("button", { name: `${leader.name}: pencereye ayır` }).click();
  await expect.poll(() => savedOpen(app, true)).toBe(true);
  await app.reload();
  await openTeam(app, team.name);
  await expect(placeholder).toBeVisible();
  await app.waitForTimeout(600);
  expect((await attaches(app, leader.id)).length).toBe(0);
  await placeholder.getByRole("button", { name: "Geri al" }).click();
  await expect(placeholder).toHaveCount(0);
  await expect((await paneOf(app, leader.name)).locator(".xterm-rows")).toContainText("fake-claude hazir");
  await expect.poll(() => savedOpen(app, false)).toBe(true);

  // The file says closed (as after a lost write) but the shell lists the window alive: popped out, no subscription,
  // and the record is brought in line at once.
  await app.evaluate((l) => localStorage.setItem("dex.e2e.windows", JSON.stringify([l])), label);
  await app.reload();
  await openTeam(app, team.name);
  await expect(placeholder).toBeVisible();
  await app.waitForTimeout(600);
  expect((await attaches(app, leader.id)).length).toBe(0);
  expect(await savedOpen(app, true)).toBe(true);
  // The window gone: the pane returns with this page's own subscription.
  await app.evaluate(() => localStorage.removeItem("dex.e2e.windows"));
  await placeholder.getByRole("button", { name: "Geri al" }).click();
  await expect(placeholder).toHaveCount(0);
  await expect((await paneOf(app, leader.name)).locator(".xterm-rows")).toContainText("fake-claude hazir");
  expect((await attaches(app, leader.id)).length).toBe(1);
});
