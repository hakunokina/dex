import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Asistan: üye ekleme, Core yanında panel, doğrudan yazım, dar görünüm ve kaldırma", async ({ app, dexd }) => {
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
  const before = await region(leader.name).boundingBox();
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
  let pane = await paneOf(app, "Echo-1");
  await expect(pane.getByRole("button", { name: "İzleme", exact: true })).toHaveCount(0);
  const leadBox = (await region(leader.name).boundingBox())!;
  const echoBox = (await region("Echo-1").boundingBox())!;
  expect(echoBox.x).toBeGreaterThanOrEqual(leadBox.x + leadBox.width - 1);
  expect(Math.abs(echoBox.y - leadBox.y)).toBeLessThan(2);
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
  pane = await paneOf(app, "Echo-1");
  await app.setViewportSize({ width: 720, height: 700 });
  await expect.poll(async () => {
    const lead = (await region(leader.name).boundingBox())!;
    const echo = (await region("Echo-1").boundingBox())!;
    return { below: echo.y >= lead.y + lead.height - 1, lead, echo };
  }).toMatchObject({ below: true });
  await expect.poll(() => app.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await pane.getByRole("tab", { name: "Sohbet", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(pane.getByRole("textbox", { name: "Ajana mesaj gönder", exact: true })).toBeVisible();
  await pane.getByRole("tab", { name: "Terminal", exact: true }).focus();
  await app.keyboard.press("Enter");
  await app.setViewportSize({ width: 1400, height: 900 });
  await dexd.call("pty_kill", { id: echo.id });
  await app.locator('.team-stage:not(.hidden) .dv-tab').filter({ hasText: /^Takım$/ }).click();
  const row = settings.locator("tr").filter({ hasText: "Echo-1" });
  await expect(row.getByRole("button", { name: "Üyeyi sil" })).toBeEnabled();
  await row.getByRole("button", { name: "Üyeyi sil" }).click();
  await expect(region("Echo-1")).toHaveCount(0);
  const after = (await region(leader.name).boundingBox())!;
  expect(Math.abs(after.width - before!.width)).toBeLessThan(2);
  expect(Math.abs(after.x - before!.x)).toBeLessThan(2);
  const afterGroups = await groups();
  expect(afterGroups.map(g => g.names)).toEqual(beforeGroups.map(g => g.names));
  // Proportional resizing can round individual worker columns; their row/column order stays intact.
  const order = (layout: Awaited<ReturnType<typeof groups>>) => layout.map(g => layout.map(other => [
    Math.sign(g.x - other.x), Math.sign(g.y - other.y),
  ]));
  expect(order(afterGroups)).toEqual(order(beforeGroups));
});
