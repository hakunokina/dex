// #2055: a running job's progress (the worker's own Claude task list, core `delegation_progress`) on the
// board card and in the worker's pane tab. dexd builds it only from real hooks; here the running job and
// its progress are answered by the bridge (__dexFake). An older dexd (no `delegation_progress`) shows none.
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("İlerleme: pano kartında satır, worker sekmesinde yüzde; eski dexd'de gizli", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `İlerleme ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  const task = await dexd.call<{ id: number }>("task_create", { task: { title: "uzun iş", teamId: team.id } });
  const now = Date.now();
  const fake = {
    delegation_list: [
      {
        id: 601, fromAgent: leader.id, toAgent: worker.id, teamId: team.id, serviceTeamId: null, prompt: "iş",
        status: "running", summary: null, resultPath: null, taskId: task.id, createdAt: now, updatedAt: now,
      },
    ],
    delegation_progress: [
      { agentId: worker.id, delegationId: 601, total: 7, done: 3, active: "Testleri koşuyor", updatedAt: now - 2 * 60_000 },
    ],
  };
  await app.addInitScript((f) => {
    window.__dexFake = f;
  }, fake);
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, worker.name);

  const tab = app.locator(".dex-tab", { hasText: worker.name }).getByTestId("tab-progress");
  await expect(tab).toHaveText("%43");
  await expect(tab).toHaveAttribute("title", "İlerleme: %43 · 3/7 · Testleri koşuyor · 2 dk önce");
  await expect(app.locator(".dex-tab", { hasText: leader.name }).getByTestId("tab-progress")).toHaveCount(0);

  const bar = app.getByRole("group", { name: "Kart araçları" });
  await bar.getByRole("button", { name: "Görevler" }).click();
  const card = app.getByRole("complementary").locator(".board-card", { hasText: "uzun iş" });
  await expect(card.getByTestId("job-progress")).toHaveText("%43 · 3/7 · Testleri koşuyor · 2 dk önce");
  await expect(card.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "43");
  await app.keyboard.press("Control+Backquote");

  // Older dexd: no progress anywhere.
  await app.addInitScript(() => {
    delete window.__dexFake!.delegation_progress;
    window.__dexOld = { hide: ["delegation_progress"] };
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, worker.name);
  await expect(app.locator(".dex-tab", { hasText: worker.name })).toBeVisible();
  await expect(app.getByTestId("tab-progress")).toHaveCount(0);
  await bar.getByRole("button", { name: "Görevler" }).click();
  await expect(app.getByRole("complementary").locator(".board-card", { hasText: "uzun iş" })).toBeVisible();
  await expect(app.getByTestId("job-progress")).toHaveCount(0);
});
