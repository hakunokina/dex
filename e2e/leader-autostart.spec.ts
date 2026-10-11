// #27: opening a team starts its offline leader by itself — once; workers wait for a delegation.
// The leader is the fake `claude` (e2e/fake-claude.rs).
import type { Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const starts = async (page: Page, agentId: string) =>
  (await calls(page)).filter((c) => c.cmd === "agent_start" && c.args.id === agentId).length;

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("takım açılınca lider bir kez kendiliğinden başlar; worker'lar başlamaz", async ({ app, dexd }) => {
  const n = test.info().workerIndex;
  const team = await newTeam(dexd, `Oto ${n}`);
  const other = await newTeam(dexd, `Diğer ${n}`);
  const crew = await teamAgents(dexd, team.id);
  const leader = crew.find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);

  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await expect(pane.getByRole("button", { name: "Claude'u başlat" })).toHaveCount(0);
  expect(await starts(app, leader.id)).toBe(1);
  for (const w of crew.filter((a) => a.kind === "worker")) expect(await starts(app, w.id)).toBe(0);

  // Away and back: the running leader is not started again.
  await openTeam(app, other.name);
  await openTeam(app, team.name);
  await app.waitForTimeout(800);
  expect(await starts(app, leader.id)).toBe(1);

  // Full reload: the pane reattaches to the running session (the call log starts empty).
  await app.reload();
  await openTeam(app, team.name);
  await expect((await paneOf(app, leader.name)).locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.waitForTimeout(800);
  expect(await starts(app, leader.id)).toBe(0);
  expect((await dexd.call<{ id: string }[]>("pty.list")).filter((p) => p.id === leader.id)).toHaveLength(1);
});
