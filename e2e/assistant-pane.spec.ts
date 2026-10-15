import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Asistan: üye ekleme, Core grubunda 4. sekme, doğrudan yazım, dar görünüm ve kaldırma", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Asistan paneli");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  const region = (name: string) => app.getByRole("region", { name, exact: true });
  const groups = () => app.locator('.team-stage:not(.hidden) .dv-grid-view .dv-groupview').evaluateAll(elements => elements.map(el => {
    const { x, y, width, height } = el.getBoundingClientRect();
    const names = [...el.querySelectorAll(".dex-tab-name")].map(tab => tab.textContent).sort().join("|");
    return { names, x, y, width, height };
  }).sort((a, b) => a.names.localeCompare(b.names)));
  const stage = app.locator(".team-stage:not(.hidden)");
  const tabName = (name: string) => app.locator(".dex-tab-name", { hasText: new RegExp(`^${name}$`) });
  const tabOf = (name: string) => stage.locator(".dv-tab").filter({ has: tabName(name) });
  // Core's group, tabs in order.
  const coreGroup = () => stage.locator(".dv-groupview").filter({ has: tabName(leader.name) });
  const coreTabs = () => coreGroup().locator(".dex-tab-name").allTextContents();
  const saveLayout = () => app.evaluate(async teamId => {
    const { boundApi } = await import("/src/layout/panels.ts");
    const { getSnapshot } = await import("/src/layout/store.ts");
    const state = getSnapshot();
    return { ...state, version: 2, teams: { ...state.teams, [teamId]: { ...state.teams[teamId], dock: boundApi(teamId)!.toJSON() } } };
  }, team.id);
  const before = await coreGroup().boundingBox();
  const beforeGroups = await groups();
  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Yeni takım", exact: true }).click();
  const newTeamForm = app.getByRole("dialog", { name: "Yeni takım", exact: true });
  const crew = newTeamForm.getByRole("combobox", { name: /^Worker \d rolü$/ });
  await expect(crew).toHaveCount(4);
  await expect(crew.locator('option[value="assistant"]')).toHaveCount(0);
  await app.keyboard.press("Escape");
  await app.locator('.team-stage:not(.hidden) .dv-tab').filter({ hasText: /^Takım$/ }).click();
  const settings = app.locator('.team-stage:not(.hidden) .team-settings');
  const role = settings.getByRole("combobox", { name: "Yeni üyenin rolü", exact: true });
  await expect(role.locator('optgroup[label="Kişisel"] option[value="assistant"]')).toHaveCount(1);
  await role.selectOption("assistant");
  await settings.getByRole("button", { name: "Ekle", exact: true }).click();
  await expect.poll(async () => (await dexd.call<{ id: string; name: string; teamId: string; roleSlug: string }[]>("agent_list"))
    .find(a => a.teamId === team.id && a.roleSlug === "assistant")?.name).toBe("Echo-1");
  expect((await calls(app)).some(c => c.cmd === "agent_create" && (c.args.agent as { roleSlug?: string })?.roleSlug === "assistant")).toBe(true);
  await expect(role.locator('option[value="assistant"]')).toBeDisabled();
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker" && a.name !== "Echo-1")!;
  await expect(settings.getByRole("combobox", { name: `${worker.name} rolü`, exact: true }).locator('option[value="assistant"]')).toBeDisabled();
  await expect(settings.getByRole("combobox", { name: `${leader.name} rolü`, exact: true }).locator('option[value="assistant"]')).toHaveCount(0);
  await app.locator('.team-stage:not(.hidden) .dv-tab').filter({ hasText: /^Kart$/ }).click();
  // #1840: Echo joins Core's tabs last, without a column of its own and without taking Core's place.
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar", "Echo-1"]);
  await expect(region(leader.name)).toBeVisible();
  await expect(region("Echo-1")).toBeHidden();
  expect((await groups()).length).toBe(beforeGroups.length);
  expect(Math.abs((await coreGroup().boundingBox())!.width - before!.width)).toBeLessThan(2);
  await tabOf("Echo-1").click();
  let pane = await paneOf(app, "Echo-1");
  await expect(pane.getByRole("button", { name: "İzleme", exact: true })).toHaveCount(0);
  await pane.getByRole("button", { name: "Claude'u başlat", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  const echo = (await teamAgents(dexd, team.id)).find(a => a.name === "Echo-1")!;
  await expect.poll(async () => (await dexd.call<{ agentId: string; status: string }[]>("agent_status_list"))
    .find(a => a.agentId === echo.id)?.status).toBe("idle");
  await dexd.call("inbox_submit", { teamId: team.id, clientId: "assistant-pane-share", text: "PAYLASILAN_LINK" });
  await expect(pane.locator(".xterm-rows")).toContainText(/RX:Patron paylaştı:\s*PAYLASILAN_LINK/);
  await pane.locator(".xterm-helper-textarea").focus();
  await app.keyboard.type("PATRON_PANEL");
  await app.keyboard.press("Enter");
  await expect(pane.locator(".xterm-rows")).toContainText("RX:PATRON_PANEL");
  await app.reload();
  await openTeam(app, team.name);
  // A narrow window keeps Core's tabs, with no sideways scroll.
  await app.setViewportSize({ width: 720, height: 700 });
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar", "Echo-1"]);
  await expect.poll(() => app.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await tabOf("Echo-1").click();
  pane = await paneOf(app, "Echo-1");
  await pane.getByRole("tab", { name: "Sohbet", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(pane.getByRole("textbox", { name: "Ajana mesaj gönder", exact: true })).toBeVisible();
  await pane.getByRole("tab", { name: "Terminal", exact: true }).focus();
  await app.keyboard.press("Enter");
  await app.setViewportSize({ width: 1400, height: 900 });
  await dexd.call("pty_kill", { id: echo.id });
  await app.locator('.team-stage:not(.hidden) .dv-tab').filter({ hasText: /^Takım$/ }).click();
  const row = settings.locator("tr").filter({ hasText: "Echo-1" });
  await expect(row.getByRole("button", { name: "Echo-1 işlemleri" })).toBeEnabled();
  await row.getByRole("button", { name: "Echo-1 işlemleri" }).click();
  await row.getByRole("menuitem", { name: "Üyeyi sil" }).click();
  await app.getByRole("alertdialog").getByRole("button", { name: "Üyeyi sil" }).click();
  await expect(region("Echo-1")).toHaveCount(0);
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar"]);
  const after = (await coreGroup().boundingBox())!;
  expect(Math.abs(after.width - before!.width)).toBeLessThan(2);
  expect(Math.abs(after.x - before!.x)).toBeLessThan(2);
  const afterGroups = await groups();
  expect(afterGroups.map(g => g.names)).toEqual(beforeGroups.map(g => g.names));
  // Proportional resizing can round individual worker columns; their row/column order stays intact.
  const order = (layout: Awaited<ReturnType<typeof groups>>) => layout.map(g => layout.map(other => [
    Math.sign(g.x - other.x), Math.sign(g.y - other.y),
  ]));
  expect(order(afterGroups)).toEqual(order(beforeGroups));
  // A newly added assistant opens by itself as Core's last tab; closed by the patron, it stays closed
  // through reloads, and only the rack brings it back.
  await role.selectOption("assistant");
  await settings.getByRole("button", { name: "Ekle", exact: true }).click();
  let next = { id: "", name: "" };
  await expect.poll(async () => {
    next = (await dexd.call<{ id: string; name: string; teamId: string; roleSlug: string }[]>("agent_list"))
      .find(a => a.teamId === team.id && a.roleSlug === "assistant") ?? next;
    return next.id;
  }).not.toBe("");
  await app.locator('.team-stage:not(.hidden) .dv-tab').filter({ hasText: /^Kart$/ }).click();
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar", next.name]);
  await tabOf(next.name).click();
  await paneOf(app, next.name);
  await app.locator(".team-stage:not(.hidden)").getByRole("button", { name: `${next.name} sekmesini kapat`, exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(region(next.name)).toHaveCount(0);
  // The debounced layout_save must carry the closed pane and the assistant it was opened for.
  await expect.poll(async () => {
    const save = (await calls(app)).filter(c => c.cmd === "layout_save").at(-1);
    const t = (save?.args.layout as { teams?: Record<string, { dock?: { panels?: Record<string, unknown> }; assistant?: string }> })?.teams?.[team.id];
    return { open: !!t?.dock?.panels && `agent:${next.id}` in t.dock.panels, assistant: t?.assistant };
  }).toEqual({ open: false, assistant: next.id });
  await app.reload();
  await openTeam(app, team.name);
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar"]);
  await expect(region(next.name)).toHaveCount(0);
  await app.getByRole("group", { name: "Takım ajanları" }).locator(`[data-agent="${next.id}"]`).focus();
  await app.keyboard.press("Enter");
  await paneOf(app, next.name);
  // A layout saved before #1840 kept Echo in its own column right of Core: it joins Core's tabs once,
  // Core gets its width back; moved by the patron afterwards, it stays where it was put.
  const move = (legacy: boolean) => app.evaluate(async ({ teamId, leaderId, echoId, legacy }) => {
    const { boundApi, agentPanelId } = await import("/src/layout/panels.ts");
    const api = boundApi(teamId)!;
    const lead = api.getPanel(agentPanelId(leaderId))!;
    const share = lead.group.api.width / api.width;
    const echo = api.getPanel(agentPanelId(echoId))!;
    echo.api.moveTo({ group: lead.group, position: "right", skipSetActive: true });
    if (legacy) echo.api.updateParameters({ assistant: undefined, assistantDirection: "right", assistantLeader: lead.id, assistantLeadShare: share });
    return echo.group !== lead.group;
  }, { teamId: team.id, leaderId: leader.id, echoId: next.id, legacy });
  const wide = (await coreGroup().boundingBox())!.width;
  expect(await move(true)).toBe(true);
  await dexd.call("layout_save", { layout: await saveLayout() });
  await app.reload();
  await openTeam(app, team.name);
  await expect.poll(coreTabs).toEqual([leader.name, "Notlar", next.name]);
  // Within a sash of the width it had before the split.
  await expect.poll(async () => Math.abs((await coreGroup().boundingBox())!.width - wide)).toBeLessThan(4);
  expect(await move(false)).toBe(true);
  await dexd.call("layout_save", { layout: await saveLayout() });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, next.name);
  expect(await coreTabs()).toEqual([leader.name, "Notlar"]);
});
