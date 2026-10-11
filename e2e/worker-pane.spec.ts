// A worker's pane opens watch-only (#22) and follows the patron's draft (#18). The agent is the
// fake `claude` (e2e/fake-claude.rs): it answers every line that reaches its PTY with `RX:<line>`.
import type { Page } from "@playwright/test";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";

/** Keys the patron typed that went to the PTY (xterm's own focus / cursor reports are not keys). */
async function typedIntoPty(page: Page, agentId: string) {
  return (await calls(page))
    .filter((c) => c.cmd === "pty_write" && c.args.id === agentId)
    .map((c) => String(c.args.data))
    .filter((d) => !/^\x1b\[(I|O|\d+;\d+R)$/.test(d));
}

async function startWorker(app: Page, dexd: Dexd, teamName: string) {
  const team = await newTeam(dexd, teamName);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await pane.locator(".xterm-screen").click();
  return { pane, worker };
}

const screen = (dexd: Dexd, agentId: string) => dexd.call<string>("chat_screen", { agentId });
/** Long enough for a key to make the page → dexd → PTY → echo round trip (it takes a few ms). */
const SETTLE = 800;

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("worker pane'i salt izlemede açılır; Yazma ile tuş gider, tekrar basınca kilitlenir", async ({ app, dexd }) => {
  const { pane, worker } = await startWorker(app, dexd, `İzleme ${test.info().workerIndex}`);
  const toggle = pane.getByRole("button", { name: /İzleme|Yazma/ });
  await expect(toggle).toHaveText(/İzleme/);
  await expect(toggle).toHaveAttribute("aria-pressed", "false");

  // Watch-only: typing and Enter never reach the agent.
  await app.keyboard.type("zzq");
  await app.keyboard.press("Enter");
  await app.waitForTimeout(SETTLE);
  expect(await typedIntoPty(app, worker.id)).toEqual([]);
  expect(await screen(dexd, worker.id)).not.toContain("zzq");

  // Write mode: the line reaches the PTY and the agent answers it.
  await toggle.click();
  await expect(toggle).toHaveText(/Yazma/);
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await app.keyboard.type("abc");
  await app.keyboard.press("Enter");
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:abc");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:abc");

  // Pressed again: locked.
  await toggle.click();
  await expect(toggle).toHaveText(/İzleme/);
  const before = (await typedIntoPty(app, worker.id)).length;
  await app.keyboard.type("xyz");
  await app.keyboard.press("Enter");
  await app.waitForTimeout(SETTLE);
  expect((await typedIntoPty(app, worker.id)).slice(before)).toEqual([]);
  expect(await screen(dexd, worker.id)).not.toContain("xyz");
});

test("taslak yazılıp silinince draft bayrağı düşer (#18)", async ({ app, dexd }) => {
  const { pane, worker } = await startWorker(app, dexd, `Taslak ${test.info().workerIndex}`);
  await pane.getByRole("button", { name: "İzleme" }).click();
  const drafts = async () =>
    (await calls(app)).filter((c) => c.cmd === "agent_input" && c.args.id === worker.id).map((c) => c.args.draft);

  await app.keyboard.type("ab");
  await expect.poll(drafts).toEqual([true, true]);
  await app.keyboard.press("Backspace");
  await expect.poll(async () => (await drafts()).at(-1)).toBe(true); // one letter still in the prompt
  await app.keyboard.press("Backspace");
  await expect.poll(async () => (await drafts()).at(-1)).toBe(false);
  expect(await typedIntoPty(app, worker.id)).toEqual(["a", "b", "\x7f", "\x7f"]);
  expect(await screen(dexd, worker.id)).not.toContain("ab");
});

// #142: the local core's methods without `chat_history` (an older dexd) → the pane has no Sohbet tab.
test("eski dexd'de (chat_history yok) Sohbet sekmesi gizli", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Sohbetsiz ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  const tabs = async () => {
    await app.reload();
    await openTeam(app, team.name);
    return (await paneOf(app, worker.name)).getByRole("tablist", { name: "Görünüm" });
  };
  await expect((await tabs()).getByRole("tab", { name: "Sohbet" })).toBeVisible();
  await app.addInitScript(() => {
    window.__dexOld = { hide: ["chat_history"] };
  });
  const old = await tabs();
  await expect(old.getByRole("tab", { name: "Terminal" })).toBeVisible();
  await expect(old.getByRole("tab", { name: "Sohbet" })).toHaveCount(0);
});
