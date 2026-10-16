// #43 follow-up: a host whose dexd predates `team_delete_info` answers `unknown_method`. The delete
// dialog says so and falls back to the plain `team_delete {id}`, which an old server refuses while an
// agent runs. The old server is stubbed in the page: `team_delete_info` fails with its exact text.
import { test, expect, answerConfirm, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("eski sunucu: bilgi yerine not, çalışan ajan varken anlaşılır hata, yoksa stopAgents'sız silme", async ({ app, dexd }) => {
  const n = test.info().workerIndex;
  const team = await newTeam(dexd, `Eski ${n}`);
  await newTeam(dexd, `Kalan ${n}`); // the last local team cannot be deleted
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  // bridge.ts `OldServer`: the same answer whether the call goes through `invoke` or the app's
  // own socket (#150: the default — an `invoke` stub would never see it).
  await app.addInitScript(() => {
    window.__dexOld = { unknown: ["team_delete_info"] };
  });
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");

  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Takım ayarları", exact: true }).click();
  const del = app.getByRole("button", { name: "Takımı sil", exact: true });
  const err = app.locator(".team-err");
  // #2051: İptal → takım silinmez, istek gitmez.
  await del.click();
  await answerConfirm(app, false);
  await app.waitForTimeout(300);
  expect((await calls(app)).filter((c) => c.cmd === "team_delete")).toHaveLength(0);
  await del.click();
  const message = await answerConfirm(app, true);
  await expect(err).toContainText("sunucudaki ajanları durdurup tekrar deneyin ya da sunucuyu güncelleyin");
  expect(message).toContain("Bu sunucu eski sürüm; çalışan ajan bilgisi alınamadı.");
  expect(message).not.toContain("durdurulup takım silinecek");
  expect((await dexd.call<{ id: string }[]>("team_list")).some((t) => t.id === team.id)).toBe(true);

  // The leader stopped (by hand on the server): the plain delete goes through, without stopAgents.
  await dexd.call("pty_kill", { id: leader.id });
  await expect(pane.getByRole("button", { name: "Claude'u başlat" })).toBeVisible();
  await del.click();
  await answerConfirm(app, true);
  await expect.poll(async () => (await dexd.call<{ id: string }[]>("team_list")).some((t) => t.id === team.id)).toBe(false);
  const deletes = (await calls(app)).filter((c) => c.cmd === "team_delete");
  expect(deletes).toEqual([{ cmd: "team_delete", args: { id: team.id, stopAgents: false } }]);
});
