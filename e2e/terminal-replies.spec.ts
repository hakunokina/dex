// #71: dex-core answers an agent PTY's terminal queries itself (ConPTY's start-up `ESC[6n`, DA1,
// OSC 10/11). The pane's xterm answering them too was a second answer the CLI read as input — real
// Claude got a "C" in its prompt after every Dex restart. The fake `claude` (e2e/fake-claude.rs) runs
// under ConPTY like the real one, so its start-up query reaches the pane.
import type { Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const writes = async (page: Page, agentId: string) =>
  (await calls(page)).filter((c) => c.cmd === "pty_write" && c.args.id === agentId).map((c) => String(c.args.data));
const ANSWER = /^(\x1b\[\d+;\d+R|\x1b\[\?[\d;]*c|\x1b\]1[01];)/;
const SETTLE = 1500;

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("ajan pane'i çekirdeğin cevapladığı sorgulara cevap yazmaz; yeniden yüklemede PTY'ye bayt gitmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Cevap ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.waitForTimeout(SETTLE);
  expect((await writes(app, worker.id)).filter((d) => ANSWER.test(d))).toEqual([]);

  // Reload: the pane reattaches to the running session from the snapshot and sends nothing.
  await app.reload();
  await openTeam(app, team.name);
  const again = await paneOf(app, worker.name);
  await expect(again.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.waitForTimeout(SETTLE);
  expect(await writes(app, worker.id)).toEqual([]);
  expect(await dexd.call<string>("chat_screen", { agentId: worker.id })).not.toContain("RX:");
});
