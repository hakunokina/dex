import { test, expect, newTeam, openTeam, teamAgents, paneOf } from "./fixtures";

test("açık worker bittiğinde sola, yeni işte sağa geçer; kapalı/elle taşınan panel ve odak korunur", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 1000, height: 720 });
  const team = await newTeam(dexd, "Worker grupları");
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find(a => a.kind === "leader")!;
  const workers = agents.filter(a => a.kind === "worker");
  await app.reload();
  await openTeam(app, team.name);
  await expect(app.locator("canvas.office")).toBeVisible();
  const setup = await app.evaluate(async ({ teamId, ids, leaderId }) => {
    // Set up two rows and a manually placed worker using the real dock API.
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    const pane = (id: string) => api.getPanel(agentPanelId(id))!;
    const working = pane(ids[0]).group;
    pane(ids[1]).api.moveTo({ group: working, position: "left", skipSetActive: true });
    pane(ids[2]).api.moveTo({ group: pane(leaderId).group, skipSetActive: true });
    api.removePanel(pane(ids[3]));
    pane(leaderId).api.setActive();
    return { working: working.id, done: pane(ids[1]).group.id, other: pane(ids[2]).group.id };
  }, { teamId: team.id, ids: workers.map(a => a.id), leaderId: leader.id });

  const groupOf = (id: string) => app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    return boundApi(teamId)!.getPanel(agentPanelId(id))?.group.id ?? null;
  }, { teamId: team.id, id });
  const focus = (await paneOf(app, leader.name)).getByRole("tab", { name: "Terminal", exact: true });
  await focus.focus();
  await app.keyboard.press("Tab");
  const focused = await app.evaluate(() => {
    document.activeElement?.setAttribute("data-worker-focus", "");
    return !!document.activeElement;
  });
  expect(focused).toBe(true);
  const emit = async (status: string, id = 1) => {
    const before = await app.evaluate(() => window.__dexCalls.filter(c => c.cmd === "delegation_list").length);
    await app.evaluate(({ status, id, workers, leader, team }) => {
      const jobs = [workers[0], workers[2], workers[3]].map((worker, index) => ({
        id: id + index * 100, fromAgent: leader, toAgent: worker, teamId: team,
        serviceTeamId: null, prompt: "test", status, summary: "test", resultPath: null,
        taskId: null, createdAt: 1, updatedAt: id, deliveredAt: 1,
      }));
      window.__dexFake = { ...window.__dexFake, delegation_list: jobs };
      const socket = window.__dexSockets!.filter(s => s.readyState === WebSocket.OPEN).at(-1)!;
      socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "delegations-changed", payload: null }) }));
    }, { status, id, workers: workers.map(a => a.id), leader: leader.id, team: team.id });
    await expect.poll(() => app.evaluate(() => window.__dexCalls.filter(c => c.cmd === "delegation_list").length)).toBeGreaterThan(before);
    await app.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  };
  await emit("running");
  await emit("done");
  await expect.poll(() => groupOf(workers[0].id)).toBe(setup.done);
  await expect(app.locator(".office-fx polyline")).toHaveCount(1);
  await expect(app.locator("[data-worker-focus]")).toBeFocused();
  expect(await groupOf(workers[2].id)).toBe(setup.other);
  expect(await groupOf(workers[3].id)).toBeNull();
  await expect(app.locator(".office-fx")).toHaveCount(0);
  await emit("running", 2);
  await expect.poll(() => groupOf(workers[0].id)).not.toBe(setup.done);
  await expect(app.locator(".office-fx polyline")).toHaveCount(1);
  await expect(app.locator("[data-worker-focus]")).toBeFocused();
  // The only working tab emptied its group; restarting recreates it on the right.
  const order = await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    const panel = api.getPanel(agentPanelId(id))!;
    return panel.group.element.getBoundingClientRect().left;
  }, { teamId: team.id, id: workers[0].id });
  const doneLeft = await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    return boundApi(teamId)!.getPanel(agentPanelId(id))!.group.element.getBoundingClientRect().left;
  }, { teamId: team.id, id: workers[1].id });
  expect(order).toBeGreaterThan(doneLeft);
  await expect(app.locator(".office-fx")).toHaveCount(0);
  await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    api.removePanel(api.getPanel(agentPanelId(id))!);
  }, { teamId: team.id, id: workers[1].id });
  // No finished group remains: create one to the left without stealing keyboard focus.
  const workingId = await groupOf(workers[0].id);
  await emit("failed", 2);
  await expect.poll(() => groupOf(workers[0].id)).not.toBe(workingId);
  await expect(app.locator(".office-fx rect.office-fx-edge")).toHaveCount(1);
  await expect(app.locator("[data-worker-focus]")).toBeFocused();
  await expect(app.locator(".office-fx")).toHaveCount(0);
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill(`Panelde aç: ${workers[1].name}`);
  await app.keyboard.press("Enter");
  await expect.poll(() => groupOf(workers[1].id)).not.toBeNull();
  const manuallyOpened = await groupOf(workers[1].id);
  expect(manuallyOpened).not.toBe(await groupOf(workers[0].id));
  expect(manuallyOpened).not.toBeNull();
  await app.emulateMedia({ reducedMotion: "reduce" });
  await emit("running", 3);
  await expect.poll(() => groupOf(workers[0].id)).toBe(manuallyOpened);
  await expect(app.locator(".office-fx")).toHaveCount(0);
  // Losing the final working group must survive an unrelated bulk revision.
  await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    api.removePanel(api.getPanel(agentPanelId(id))!);
  }, { teamId: team.id, id: workers[1].id });
  await emit("done", 3);
  const finishedGroup = await groupOf(workers[0].id);
  await app.evaluate(async (teamId) => {
    const { closeShell } = await import("/src/layout/panels.ts");
    const { getSnapshot } = await import("/src/layout/store.ts");
    const shell = getSnapshot().teams[teamId].shells[0];
    if (shell) closeShell(teamId, shell.id);
  }, team.id);
  await emit("running", 4);
  await expect.poll(() => groupOf(workers[0].id)).not.toBe(finishedGroup);
  await emit("done", 4);
  const savedGroup = await groupOf(workers[0].id);
  const saved = await app.evaluate(async (teamId) => {
    const { boundApi } = await import("/src/layout/panels.ts");
    const { getSnapshot } = await import("/src/layout/store.ts");
    const state = getSnapshot();
    return { layout: { ...state, version: 2, teams: { ...state.teams, [teamId]: { ...state.teams[teamId], dock: boundApi(teamId)!.toJSON() } } }, jobs: window.__dexFake?.delegation_list };
  }, team.id);
  await dexd.call("layout_save", { layout: saved.layout });
  await app.addInitScript(jobs => { window.__dexFake = { delegation_list: jobs }; }, saved.jobs);
  await app.reload();
  await openTeam(app, team.name);
  await expect(app.locator("canvas.office")).toBeVisible();
  await expect.poll(() => groupOf(workers[0].id)).toBe(savedGroup);
  await emit("running", 5);
  await expect.poll(() => groupOf(workers[0].id)).not.toBe(savedGroup);
  expect(await groupOf(workers[2].id)).toBe(setup.other);
  expect(await groupOf(workers[3].id)).toBeNull();
});

test("geniş düzende yeni worker sağa bölünür ve işi bitince sola taşınır", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 1800, height: 900 });
  const team = await newTeam(dexd, "Geniş worker");
  const workers = (await teamAgents(dexd, team.id)).filter(a => a.kind === "worker");
  await app.reload();
  await openTeam(app, team.name);
  await expect(app.locator("canvas.office")).toBeVisible();
  const opened = await app.evaluate(async ({ teamId, ids }) => {
    const { boundApi, agentPanelId, openAgent, closeShell } = await import("/src/layout/panels.ts");
    const { getSnapshot } = await import("/src/layout/store.ts");
    const api = boundApi(teamId)!;
    for (const shell of getSnapshot().teams[teamId].shells) closeShell(teamId, shell.id);
    const first = api.getPanel(agentPanelId(ids[0]))!;
    for (const id of ids.slice(1)) api.removePanel(api.getPanel(agentPanelId(id))!);
    first.group.api.setSize({ width: 1000 });
    // Initial running event establishes the managed group before the opening splits it.
    const { moveWorkerPanel } = await import("/src/layout/panels.ts");
    moveWorkerPanel(ids[0], false);
    const oldGroup = first.group.id;
    const width = first.group.api.width;
    openAgent(teamId, ids[1]);
    openAgent(teamId, ids[2]); // Keep a working tab so the source group survives the move.
    return { oldGroup, width, newGroup: api.getPanel(agentPanelId(ids[1]))!.group.id };
  }, { teamId: team.id, ids: workers.map(w => w.id) });
  expect(opened.width).toBeGreaterThanOrEqual(840);
  expect(opened.newGroup).not.toBe(opened.oldGroup);
  const finished = await app.evaluate(async ({ teamId, id, firstId, workingId }) => {
    const { boundApi, agentPanelId, moveWorkerPanel } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    const panel = api.getPanel(agentPanelId(id))!;
    moveWorkerPanel(id, true);
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    return { group: panel.group.id, firstLeft: api.getPanel(agentPanelId(firstId))!.group.element.getBoundingClientRect().left, workingLeft: api.getGroup(workingId)!.element.getBoundingClientRect().left, after: panel.group.element.getBoundingClientRect().left };
  }, { teamId: team.id, id: workers[1].id, firstId: workers[0].id, workingId: opened.newGroup });
  expect(finished.group).not.toBe(opened.newGroup);
  expect(finished.after).toBeLessThan(finished.workingLeft);
  expect(finished.after).toBeLessThan(finished.firstLeft);
  const firstFinished = await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId, moveWorkerPanel } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    moveWorkerPanel(id, true);
    return api.getPanel(agentPanelId(id))!.group.id;
  }, { teamId: team.id, id: workers[0].id });
  expect(firstFinished).toBe(finished.group);
  const restarted = await app.evaluate(async ({ teamId, id }) => {
    const { boundApi, agentPanelId, moveWorkerPanel } = await import("/src/layout/panels.ts");
    moveWorkerPanel(id, false);
    return boundApi(teamId)!.getPanel(agentPanelId(id))!.group.id;
  }, { teamId: team.id, id: workers[0].id });
  expect(restarted).toBe(opened.newGroup);
});

test("başka takım ekrandayken worker taşınır ama devre efekti oynamaz", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 1000, height: 720 });
  const main = await newTeam(dexd, "Efekt ana");
  const other = await newTeam(dexd, "Efekt jezdev");
  const worker = (await teamAgents(dexd, main.id)).find(a => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, main.name);
  await expect(app.locator("canvas.office")).toBeVisible();
  await openTeam(app, other.name);
  await expect(app.locator(".team-stage:not(.hidden) canvas.office")).toBeVisible();
  const move = (finished: boolean) => app.evaluate(async ({ teamId, id, finished }) => {
    const { boundApi, agentPanelId, moveWorkerPanel } = await import("/src/layout/panels.ts");
    const pane = () => boundApi(teamId)!.getPanel(agentPanelId(id))!;
    const before = pane().group.id;
    moveWorkerPanel(id, finished);
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    return { moved: pane().group.id !== before, fx: document.querySelectorAll(".office-fx polyline").length };
  }, { teamId: main.id, id: worker.id, finished });
  // The hidden team's layout still follows the job; only the window-wide trace stays off.
  expect(await move(true)).toEqual({ moved: true, fx: 0 });
  await openTeam(app, main.name);
  await expect(app.locator(".team-stage:not(.hidden) canvas.office")).toBeVisible();
  expect(await move(false)).toEqual({ moved: true, fx: 1 });
  await expect(app.locator(".office-fx")).toHaveCount(0);
});

