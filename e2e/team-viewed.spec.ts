import { isolatedTest as test, expect, calls, newTeam, openTeam, teamAgents } from "./fixtures";

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("takım görüntülenmesi: klavye, dar görünüm, odak, görünürlük, reconnect ve çalışan sayısı", async ({ app, dexd }) => {
  const first = await newTeam(dexd, "Viewed A");
  const second = await newTeam(dexd, "Viewed B");
  await app.reload();
  await app.bringToFront();
  await openTeam(app, first.name);
  const viewedCalls = async () => (await calls(app)).filter(c => c.cmd === "team_viewed");
  await expect.poll(async () => (await viewedCalls()).at(-1)?.args).toEqual({ teamId: first.id, viewed: true });
  await app.evaluate(() => { window.__dexCalls.length = 0; });
  await app.setViewportSize({ width: 390, height: 844 });
  const secondTab = app.locator(".team-tab").filter({ hasText: second.name });
  await secondTab.focus();
  await expect(secondTab).toBeFocused();
  await secondTab.press("Enter");
  await expect(secondTab).toHaveClass(/active/);
  await expect.poll(async () => (await viewedCalls()).map(c => c.args)).toEqual([
    { teamId: first.id, viewed: false }, { teamId: second.id, viewed: true },
  ]);
  await app.evaluate(() => {
    Object.defineProperty(document, "hasFocus", { configurable: true, value: () => false });
    window.dispatchEvent(new Event("blur"));
  });
  await expect.poll(async () => (await viewedCalls()).at(-1)?.args).toEqual({ teamId: second.id, viewed: false });
  await app.evaluate(() => {
    Object.defineProperty(document, "hasFocus", { configurable: true, value: () => true });
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("focus"));
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(async () => (await viewedCalls()).length).toBe(4);
  await app.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(async () => (await viewedCalls()).at(-1)?.args).toEqual({ teamId: second.id, viewed: false });
  await app.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(async () => (await viewedCalls()).length).toBe(6);
  await app.evaluate(() => { for (const ws of window.__dexSockets ?? []) ws.close(); });
  await expect.poll(async () => (await viewedCalls()).length).toBeGreaterThan(6);
  expect((await viewedCalls()).at(-1)?.args).toEqual({ teamId: second.id, viewed: true });
  const leader = (await teamAgents(dexd, second.id)).find(a => a.kind === "leader")!;
  // An open but idle session is excluded; the leader counts once it starts working.
  await expect(secondTab.locator(".team-live")).toHaveCount(0);
  await app.evaluate(agentId => {
    const socket = window.__dexSockets?.filter(s => s.readyState === WebSocket.OPEN).at(-1);
    if (!socket) throw new Error("açık soket yok");
    socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "agent-status", payload: { agentId, status: "working" } }) }));
  }, leader.id);
  await expect(secondTab.locator(".team-live")).toHaveText("1");
  await dexd.call("pty_kill", { id: leader.id });
  await expect(secondTab.locator(".team-live")).toHaveCount(0);
});

test("uzak takım ham id gönderir; unknown_method sessizce atlanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Remote viewed");
  const warnings: string[] = [];
  app.on("console", msg => { if (msg.type() === "warning") warnings.push(msg.text()); });
  await app.addInitScript(({ url, token }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "remote", name: "Test host", url, device: "e2e" }]));
    localStorage.setItem("dex.e2e.token.remote", token);
    window.__dexOld = { unknown: ["team_viewed"] };
  }, { url: dexd.url, token: dexd.token });
  await app.reload();
  await app.bringToFront();
  const tab = app.locator(".team-tab").filter({ hasText: team.name }).filter({ has: app.locator(".team-host") });
  await tab.click();
  await expect(tab).toHaveClass(/active/);
  // The old local team's false uses another transport and may arrive after the remote true.
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "team_viewed" && c.args.teamId === team.id).map(c => c.args))
    .toContainEqual({ teamId: team.id, viewed: true });
  await app.evaluate(() => {
    Object.defineProperty(document, "hasFocus", { configurable: true, value: () => false });
    window.dispatchEvent(new Event("blur"));
  });
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "team_viewed").at(-1)?.args)
    .toEqual({ teamId: team.id, viewed: false });
  expect(warnings.filter(w => w.includes("görüntülenme"))).toEqual([]);
});
