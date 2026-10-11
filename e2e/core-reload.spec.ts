// #150 (B): the core is its own process (`dexd --local`); the window is only a client. Reloading
// the UI (as a `tauri dev` rebuild or closing and reopening the window does) leaves the agent's
// PTY running: the new page attaches to the same session and sees what it printed before.
import { test, expect, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";

const pty = async (dexd: Dexd, id: string) =>
  (await dexd.call<{ id: string; alive: boolean; startedAt: number }[]>("pty.list")).find((p) => p.id === id);

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("arayüz yeniden yüklenince ajanın PTY'si yaşar, yeni sayfa aynı oturuma bağlanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Yenile ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  let pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  // A worker pane opens watch-only; "İzleme" → write.
  await pane.getByRole("button", { name: /İzleme|Yazma/ }).click();
  await pane.locator(".xterm-screen").click();
  await app.keyboard.type("once");
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:once");
  const before = await pty(dexd, worker.id);
  expect(before?.alive).toBe(true);

  await app.reload();
  await openTeam(app, team.name);
  pane = await paneOf(app, worker.name);
  // The same session: its screen comes back from the snapshot, nothing restarted it.
  await expect(pane.locator(".xterm-rows")).toContainText("RX:once");
  await expect(pane.getByRole("button", { name: "Claude'u başlat" })).toHaveCount(0);
  expect(await pty(dexd, worker.id)).toEqual(before);
  await pane.getByRole("button", { name: /İzleme|Yazma/ }).click();
  await pane.locator(".xterm-screen").click();
  await app.keyboard.type("sonra");
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:sonra");
});
