// #38: Windows Terminal copy/paste in a pane — Ctrl+C copies a selection (else ^C to the PTY),
// Ctrl+V / Ctrl+Shift+V paste; a watch-only worker pane (#22) copies but never pastes. The agent is
// the fake `claude` (e2e/fake-claude.rs): it answers every line that reaches its PTY with `RX:<line>`.
import type { Locator, Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";

async function typedIntoPty(page: Page, agentId: string) {
  return (await calls(page))
    .filter((c) => c.cmd === "pty_write" && c.args.id === agentId)
    .map((c) => String(c.args.data))
    .filter((d) => !/^\x1b\[(I|O|\d+;\d+R)$/.test(d));
}

async function startWorker(app: Page, dexd: Dexd, teamName: string) {
  await app.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  const team = await newTeam(dexd, teamName);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await expect(pane.locator(".launch")).toBeHidden();
  return { pane, worker };
}

const screen = (dexd: Dexd, agentId: string) => dexd.call<string>("chat_screen", { agentId });
const setClipboard = (page: Page, text: string) => page.evaluate((t) => navigator.clipboard.writeText(t), text);
const readClipboard = (page: Page) => page.evaluate(() => navigator.clipboard.readText());
const SETTLE = 800;

/** Double-clicks the first word of the on-screen row starting with `line` (default the fake's banner): an xterm selection. */
async function selectBanner(page: Page, pane: Locator, line = "fake-claude hazir") {
  // The pane must show the row first: the core's screen runs ahead of xterm's paint, and a short pane scrolls.
  await expect(pane.locator(".xterm-rows")).toContainText(line);
  const row = pane.locator(".xterm-rows > div").filter({ hasText: new RegExp(`^${line}`) }).first();
  const box = (await row.boundingBox())!; // the screen layer above the rows takes the mouse
  await page.mouse.dblclick(box.x + 8, box.y + box.height / 2);
  await expect(pane.locator(".xterm-selection div").first()).toBeVisible();
}

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Ctrl+C seçimi kopyalar, seçimsiz ^C gönderir; Ctrl+V ve Ctrl+Shift+V yapıştırır (#38)", async ({ app, dexd }) => {
  const { pane, worker } = await startWorker(app, dexd, `Pano ${test.info().workerIndex}`);
  await pane.getByRole("button", { name: "İzleme" }).click();
  await setClipboard(app, "eski");

  // Selection + Ctrl+C: copied, selection cleared, nothing reaches the PTY.
  await selectBanner(app, pane);
  await app.keyboard.press("Control+C");
  await expect.poll(() => readClipboard(app)).toBe("fake-claude");
  await expect(pane.locator(".xterm-selection div")).toHaveCount(0);
  expect(await typedIntoPty(app, worker.id)).toEqual([]);

  // Ctrl+V: the clipboard reaches the agent and counts as a draft (#18) until Enter.
  const drafts = async () =>
    (await calls(app)).filter((c) => c.cmd === "agent_input" && c.args.id === worker.id).map((c) => c.args.draft);
  await setClipboard(app, "yapis");
  await app.keyboard.press("Control+V");
  await expect.poll(async () => (await drafts()).at(-1)).toBe(true);
  await app.keyboard.press("Enter");
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:yapis");
  await expect.poll(async () => (await drafts()).at(-1)).toBe(false);

  await setClipboard(app, "ikinci");
  await app.keyboard.press("Control+Shift+V");
  await app.keyboard.press("Enter");
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:ikinci");
  expect((await typedIntoPty(app, worker.id)).join("")).not.toContain("\x16");

  // Selection + Ctrl+Shift+C copies too. The banner may have scrolled off a short pane: the last answer is on screen.
  await selectBanner(app, pane, "RX:ikinci");
  await app.keyboard.press("Control+Shift+C");
  await expect.poll(() => readClipboard(app)).toBe("RX:ikinci");

  // No selection: Ctrl+C is the interrupt.
  await app.keyboard.press("Control+C");
  await expect.poll(async () => (await typedIntoPty(app, worker.id)).at(-1)).toBe("\x03");
});

test("izleme modunda Ctrl+C kopyalar, Ctrl+V PTY'ye gitmez (#22, #38)", async ({ app, dexd }) => {
  const { pane, worker } = await startWorker(app, dexd, `Pano izleme ${test.info().workerIndex}`);
  await expect(pane.getByRole("button", { name: /İzleme/ })).toHaveAttribute("aria-pressed", "false");

  await selectBanner(app, pane);
  await app.keyboard.press("Control+C");
  await expect.poll(() => readClipboard(app)).toBe("fake-claude");

  await setClipboard(app, "kilitli");
  await app.keyboard.press("Control+V");
  await app.keyboard.press("Control+Shift+V");
  await app.keyboard.press("Enter");
  await app.waitForTimeout(SETTLE);
  expect(await typedIntoPty(app, worker.id)).toEqual([]);
  expect(await screen(dexd, worker.id)).not.toContain("kilitli");
});
