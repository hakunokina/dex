import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { execFileSync } from "node:child_process";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list")) await dexd.call("pty_kill", { id: p.id });
});

test("projeler: bağla, düzenle, seç, yeni terminal, arşiv/yeniden aç ve dar klavye akışı", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Projeler");
  const original = (await dexd.call<{ id: string; rootPath: string }[]>("project_list", { teamId: team.id }))[0];
  execFileSync("git", ["init", "-b", "main", original.rootPath]);
  await dexd.call("project_verify", { teamId: team.id, id: original.id });
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await expect(projects).toBeVisible();
  await expect(projects.getByText("git", { exact: true })).toBeVisible();
  await expect(projects.getByRole("button", { name: "Kategori altında klasör aç" })).toHaveCount(0);
  await projects.locator("summary", { hasText: "Kategori kökünü değiştir" }).click();
  const chooseRoot = projects.getByRole("button", { name: "Kategori kökü seç", exact: true });
  await chooseRoot.focus();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "team_update" && c.args.id === team.id && (c.args.patch as { workspaceRoot?: string }).workspaceRoot).length).toBe(1);
  await projects.getByRole("textbox", { name: "Yeni klasör adı" }).fill("İkinci");
  await projects.getByRole("button", { name: "Oluştur ve bağla", exact: true }).click();
  const row = projects.locator(".project-row", { has: app.getByText("İkinci", { exact: true }) });
  await expect(row).toContainText("klasör · worktree/merge yok");
  await row.getByRole("button", { name: "Düzenle" }).click();
  const edit = projects.getByRole("form", { name: "İkinci düzenle" });
  await edit.getByRole("textbox", { name: "Ad", exact: true }).fill("Site");
  await edit.getByRole("button", { name: "Kaydet" }).click();
  const site = projects.locator(".project-row", { has: app.getByText("Site", { exact: true }) });
  await expect(site).toBeVisible();
  const picker = app.getByRole("combobox", { name: "Aktif proje", exact: true });
  await picker.selectOption({ label: "Site" });
  await expect(picker.locator("option:checked")).toHaveText("Site");
  const p2 = (await dexd.call<{ id: string; name: string; rootPath: string }[]>("project_list", { teamId: team.id })).find(p => p.name === "Site")!;
  await app.getByRole("button", { name: "Yeni terminal", exact: true }).first().click();
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "pty_spawn" && c.args.cwd === p2.rootPath).length).toBe(1);
  // Selection must never reset/start a leader behind the patron's back.
  expect((await calls(app)).filter(c => c.cmd === "leader_reset" && c.args.action !== "get")).toHaveLength(0);
  await picker.selectOption(original.id);
  await expect(picker).toHaveValue(original.id);
  // #2051: İptal → arşivlenmez, istek gitmez.
  let asked = false;
  app.once("dialog", d => { asked = true; void d.dismiss(); });
  await site.getByRole("button", { name: "Arşivle" }).click();
  await expect.poll(() => asked).toBe(true);
  await app.waitForTimeout(300);
  expect((await calls(app)).filter(c => c.cmd === "project_archive")).toHaveLength(0);
  await expect(site).toHaveCount(1);
  app.on("dialog", d => d.accept());
  await site.getByRole("button", { name: "Arşivle" }).click();
  await expect(site).toHaveCount(0);
  await projects.getByRole("button", { name: "Arşivdekiler (1)" }).click();
  await expect(site).toContainText("arşivde");
  await site.getByRole("button", { name: "Yeniden aç" }).click();
  await expect(site).not.toContainText("arşivde");
  // The same settings flow remains usable at 390 px and by keyboard.
  await app.setViewportSize({ width: 390, height: 780 });
  await site.getByRole("button", { name: "Düzenle" }).focus();
  await app.keyboard.press("Enter");
  await expect(projects.getByRole("textbox", { name: "Ad", exact: true })).toBeFocused();
  await app.keyboard.press("Escape");
  await expect(projects.locator(".project-edit")).toHaveCount(0);
  await expect(site.getByRole("button", { name: "Düzenle" })).toBeFocused();
  await expect.poll(async () => (await site.boundingBox())!.width).toBeLessThanOrEqual(390);
  await expect(site.getByRole("button", { name: "Düzenle" })).toBeVisible();
});

test("proje hafızası: tek grafikte yalnız seçili proje; oturum başlığı kuyruktan etkilenmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Bellek Proje");
  const p1 = (await dexd.call<{ id: string; name: string }[]>("project_list", { teamId: team.id }))[0];
  const root2 = join(dexd.projects, "memory-second");
  mkdirSync(root2);
  const p2 = await dexd.call<{ id: string; name: string }>("project_create", { teamId: team.id, project: { name: "Başka" }, root: root2 });
  for (const p of [p1, p2]) {
    const context = await dexd.call<{ projectMemoryRoot: string }>("project_context", { teamId: team.id, projectId: p.id });
    mkdirSync(join(context.projectMemoryRoot, "shared"), { recursive: true });
    writeFileSync(join(context.projectMemoryRoot, "shared", "not.md"), `---\nname: ${p.name} notu\n---\n${p.name} gövdesi`);
  }
  const crew = await teamAgents(dexd, team.id);
  const leader = crew.find(a => a.kind === "leader")!;
  const worker = crew.find(a => a.kind === "worker")!;
  // Contract fixture for G1's bound session; all project/memory CRUD calls remain real.
  await app.addInitScript(({ leader, worker, p1, p2, team }) => {
    window.__dexFake = {
      session_projects: { [leader.id]: { projectId: p1.id, delegationId: null }, [worker.id]: { projectId: p1.id, delegationId: 7 } },
      delegation_list: [{ id: 8, toAgent: worker.id, fromAgent: leader.id, teamId: team.id, projectId: p2.id, status: "queued", createdAt: Date.now() }],
      task_list: [{ id: 7, title: "Proje görevi", description: "", status: "todo", agentId: worker.id, teamId: team.id, projectId: p1.id, createdAt: 0, updatedAt: 0 }],
    };
  }, { leader, worker, p1, p2, team });
  await app.reload();
  await openTeam(app, team.name);
  const leaderPane = await paneOf(app, leader.name);
  const workerPane = await paneOf(app, worker.name);
  await expect(workerPane.locator(".project-chip")).toHaveText(p1.name);
  await app.getByRole("combobox", { name: "Aktif proje", exact: true }).selectOption(p2.id);
  await expect(leaderPane).toContainText(`Lider ${p1.name} oturumunda`);
  await expect(leaderPane.getByRole("button", { name: "Yeni projede temiz oturum" })).toBeVisible();
  await expect(workerPane.locator(".project-chip")).toHaveText(p1.name);
  await app.getByRole("button", { name: "Görevler", exact: true }).click();
  await expect(app.locator(".board.open .board-card").filter({ hasText: "Proje görevi" }).locator(".project-chip")).toHaveText(p1.name);
  await app.keyboard.press("Escape");
  await app.getByRole("button", { name: "Hafıza haritası", exact: true }).click();
  const scope = app.getByRole("combobox", { name: "Hafıza bağlamı" });
  await expect(scope).toHaveValue(p2.id);
  await expect(app.getByRole("button", { name: `${p2.name} notu`, exact: true })).toBeVisible();
  await expect(app.getByRole("button", { name: `${p1.name} notu`, exact: true })).toHaveCount(0);
  await scope.selectOption(p1.id);
  const node = app.getByRole("button", { name: `${p1.name} notu`, exact: true });
  await node.focus();
  await app.keyboard.press("Enter");
  await expect(app.locator(".mem-body-md")).toHaveText(`${p1.name} gövdesi`);
  await expect(app.locator(".mem-badge").first()).toHaveText("Proje");
  await expect(app.getByRole("button", { name: `${p2.name} notu`, exact: true })).toHaveCount(0);
  // Back to p2 with its document open: p2 has no bound live session, so it can be archived.
  await scope.selectOption(p2.id);
  const node2 = app.getByRole("button", { name: `${p2.name} notu`, exact: true });
  await node2.focus();
  await app.keyboard.press("Enter");
  await expect(app.locator(".mem-body-md")).toHaveText(`${p2.name} gövdesi`);
  // Archiving while the map is open clears both the explicit scope and its document.
  await app.setViewportSize({ width: 390, height: 780 });
  await scope.focus();
  await expect(scope).toBeFocused();
  await app.getByRole("button", { name: "Takım ayarları" }).focus();
  await app.keyboard.press("Enter");
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  const row = (p: { name: string }) => projects.locator(".project-row", { has: app.getByText(p.name, { exact: true }) });
  app.on("dialog", d => d.accept());
  await row(p2).getByRole("button", { name: "Arşivle" }).focus();
  await app.keyboard.press("Enter");
  await expect(scope).toHaveValue("");
  await expect(node2).toHaveCount(0);
  await expect(app.locator(".mem-body-md")).toHaveCount(0);
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "team_memory_graph" && c.args.teamId === team.id).length).toBeGreaterThan(0);
  // db4fa4e: the fresh leader is bound to the default project p1 — the core refuses to archive it
  // (`project_busy`) and the panel says so; p1 stays open and selectable.
  await expect.poll(async () => (await dexd.call<{ id: string }[]>("pty.list")).some(p => p.id === leader.id)).toBe(true);
  await row(p1).getByRole("button", { name: "Arşivle" }).focus();
  await app.keyboard.press("Enter");
  await expect(projects.getByRole("alert")).toHaveText(/süren iş.*açık oturum var/);
  await expect(row(p1)).not.toContainText("arşivde");
  await expect(scope.locator(`option[value="${p1.id}"]`)).toHaveCount(1);
  await projects.getByRole("button", { name: "Arşivdekiler (1)" }).focus();
  await app.keyboard.press("Enter");
  await row(p2).getByRole("button", { name: "Yeniden aç" }).focus();
  await app.keyboard.press("Enter");
  await expect(scope.locator(`option[value="${p2.id}"]`)).toHaveCount(1);
  await expect(scope).toHaveValue("");
  await expect(node2).toHaveCount(0);
});

test("varsayılan takım: Main yanında klasör tek çağrıda oluşturulur; dar klavye akışı", async ({ app, dexd }) => {
  const team = await dexd.call<{ id: string; name: string; cwd: string; workspaceRoot: string }>("team_create", { team: { name: "Yeni Kategori", kind: "main" } });
  await app.reload();
  await openTeam(app, team.name);
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await expect(projects.getByText("Kategori kökü kullanılamıyor", { exact: false })).toHaveCount(0);
  const folder = projects.getByRole("textbox", { name: "Yeni klasör adı" });
  await folder.focus();
  await folder.fill("Site");
  const create = projects.getByRole("button", { name: "Oluştur ve bağla", exact: true });
  await create.focus();
  await app.keyboard.press("Enter");
  const row = projects.locator(".project-row").filter({ has: app.getByText("Site", { exact: true }) });
  await expect(row).toBeVisible();
  const site = (await dexd.call<{ name: string; rootPath: string }[]>("project_list", { teamId: team.id })).find(p => p.name === "Site")!;
  expect(dirname(site.rootPath)).toBe(team.workspaceRoot);
  expect(dirname(team.cwd)).toBe(team.workspaceRoot);
  expect(existsSync(site.rootPath)).toBe(true);
  expect((await calls(app)).filter(c => c.cmd === "project_create").at(-1)?.args).toMatchObject({ teamId: team.id, project: { name: "Site" }, createFolder: true });
  expect((await calls(app)).filter(c => c.cmd === "fs.mkdir")).toHaveLength(0);
  await expect(folder).toHaveValue("");
  expect((await projects.boundingBox())!.width).toBeLessThanOrEqual(390);
  await projects.getByRole("button", { name: "Mevcut klasör bağla", exact: true }).click();
  await projects.getByRole("button", { name: "Klasör seç…", exact: true }).click();
  await projects.getByRole("textbox", { name: "Proje adı" }).fill("Mevcut");
  await projects.getByRole("button", { name: "Proje ekle", exact: true }).click();
  await expect(projects.locator(".project-row").filter({ has: app.getByText("Mevcut", { exact: true }) })).toBeVisible();
});

test("eski uzak host: project_list var, yeni klasör yeteneği yok; mkdir ve root ile bağlar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Eski Klasör Yeteneği");
  const category = join(dexd.projects, "legacy-category");
  mkdirSync(category);
  await dexd.call("team_update", { id: team.id, patch: { workspaceRoot: category } });
  await app.addInitScript(({ url, token }) => {
    window.__dexOld = { hide: ["team_folder_preview", "team_clone_template"] };
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "remote", name: "Uzak", url, device: "e2e" }]));
    localStorage.setItem("dex.e2e.token.remote", token);
  }, { url: dexd.url, token: dexd.token });
  await app.reload();
  await app.locator('.team-tab[title^="Uzak:"]').filter({ hasText: team.name }).click();
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await projects.getByRole("textbox", { name: "Yeni klasör adı" }).fill("Eski Site");
  await projects.getByRole("button", { name: "Oluştur ve bağla", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(projects.locator(".project-row").filter({ has: app.getByText("Eski Site", { exact: true }) })).toBeVisible();
  const recorded = await calls(app);
  const created = recorded.filter(c => c.cmd === "project_create").at(-1)!;
  expect(created.args).toEqual({ teamId: team.id, project: { name: "Eski Site" }, root: join(category, "Eski Site"), branch: null });
  const mkdir = recorded.findIndex(c => c.cmd === "fs.mkdir");
  expect(mkdir).toBeGreaterThanOrEqual(0);
  expect(mkdir).toBeLessThan(recorded.indexOf(created));
  expect(recorded[mkdir].args).toMatchObject({ parent: category, name: "Eski Site" });
  expect(recorded.some(c => c.cmd === "team_folder_preview")).toBe(false);
  expect(existsSync(join(category, "Eski Site"))).toBe(true);
  expect((await projects.boundingBox())!.width).toBeLessThanOrEqual(390);
});

test("eski host yalnız project_list metot kanıtıyla eski klasör UI'sını korur", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Eski Proje");
  await app.addInitScript(() => { window.__dexOld = { hide: ["project_list"], unknown: ["project_list"] }; });
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await expect(app.locator(".team-settings:visible").getByText("Çalışma dizini", { exact: true })).toBeVisible();
  await expect(app.getByRole("region", { name: "Projeler", exact: true })).toHaveCount(0);
  await expect(app.getByRole("combobox", { name: "Aktif proje", exact: true })).toHaveCount(0);
});

test("mevcut takım önerilen kategori kökünü Kullan ile kabul eder", async ({ app, dexd }) => {
  const workspace = join(dexd.projects, "Studio");
  const cwd = join(workspace, "StudioMain");
  mkdirSync(cwd, { recursive: true });
  const team = await dexd.call<{ id: string; name: string; suggestedWorkspaceRoot: string }>("team_create", { team: { name: "Eski Studio", kind: "main", cwd } });
  expect(team.suggestedWorkspaceRoot).toBe(workspace);
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await expect(projects.getByText(`Kategori kökü: ${workspace}`)).toBeVisible();
  await expect(projects.getByText("Kategori kökü kullanılamıyor", { exact: false })).toHaveCount(0);
  await expect(projects.getByRole("button", { name: "Oluştur ve bağla" })).toBeDisabled();
  await projects.getByRole("button", { name: "Kullan", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(projects.getByRole("button", { name: "Kullan", exact: true })).toHaveCount(0);
  await projects.getByRole("textbox", { name: "Yeni klasör adı" }).fill("Site");
  await projects.getByRole("button", { name: "Oluştur ve bağla" }).click();
  await expect(projects.locator(".project-row").filter({ has: app.getByText("Site", { exact: true }) })).toBeVisible();
  expect((await dexd.call<{ name: string; rootPath: string }[]>("project_list", { teamId: team.id })).find(p => p.name === "Site")!.rootPath).toBe(join(workspace, "Site"));
});

test("uzak host: aynı ham kimlikler proje seçimi ve hafıza çağrısında niteliklenir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Uzak Proje");
  const root = join(dexd.projects, "remote-second");
  mkdirSync(root);
  const p2 = await dexd.call<{ id: string }>("project_create", { teamId: team.id, project: { name: "Uzak İkinci" }, root });
  await app.addInitScript(({ url, token }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "remote", name: "Uzak", url, device: "e2e" }]));
    localStorage.setItem("dex.e2e.token.remote", token);
  }, { url: dexd.url, token: dexd.token });
  await app.reload();
  await app.locator('.team-tab[title^="Uzak:"]').filter({ hasText: team.name }).click();
  const picker = app.getByRole("combobox", { name: "Aktif proje", exact: true });
  await picker.selectOption(`remote:${p2.id}`);
  await expect(picker).toHaveValue(`remote:${p2.id}`);
  const selection = (await calls(app)).filter(c => c.cmd === "project_set_default").at(-1)!;
  expect(selection.args).toEqual({ teamId: team.id, id: p2.id });
  await app.getByRole("button", { name: "Hafıza haritası", exact: true }).click();
  await expect(app.getByRole("combobox", { name: "Hafıza bağlamı" })).toHaveValue(`remote:${p2.id}`);
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "project_memory_graph" && c.args.teamId === team.id && c.args.projectId === p2.id).length).toBeGreaterThan(0);
});

test("proje liste hatası görünür; eski klasör UI'sına sessiz dönüş yok", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Hatalı Proje");
  await app.addInitScript(() => { window.__dexFake = { project_list: null }; });
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await expect(projects.getByRole("alert")).toContainText("Projeler okunamadı");
  await expect(projects.getByRole("button", { name: "Yeniden dene" })).toBeVisible();
  await expect(app.locator(".team-settings:visible").getByText("Çalışma dizini", { exact: true })).toHaveCount(0);
});

test("uzak takım: home kategorisinde scoped klasör gezme ve oluşturma", async ({ app, dexd }) => {
  const team = await dexd.call<{ id: string; name: string; workspaceRoot: string }>("team_create", { team: { name: "Uzak Kategori", kind: "main" } });
  await app.addInitScript(({ url, token }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "remote", name: "Uzak", url, device: "e2e" }]));
    localStorage.setItem("dex.e2e.token.remote", token);
  }, { url: dexd.url, token: dexd.token });
  await app.reload();
  await app.locator('.team-tab[title^="Uzak:"]').filter({ hasText: team.name }).click();
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const projects = app.getByRole("region", { name: "Projeler", exact: true });
  await projects.getByRole("button", { name: "Mevcut klasör bağla", exact: true }).click();
  await projects.getByRole("button", { name: "Klasör seç…", exact: true }).click();
  const picker = app.getByRole("dialog", { name: "Uzak klasörleri" });
  await expect(picker.locator(".remote-folder-head code")).toHaveText(team.workspaceRoot);
  await picker.getByRole("button", { name: "Yeni klasör", exact: true }).click();
  await picker.getByRole("textbox", { name: "Yeni klasörün adı" }).fill("Site");
  await picker.getByRole("button", { name: "Oluştur", exact: true }).click();
  await expect(picker.locator(".remote-folder-head code")).toHaveText(join(team.workspaceRoot, "Site"));
  await picker.getByRole("button", { name: "Bu klasörü seç" }).click();
  await projects.getByRole("button", { name: "Proje ekle", exact: true }).click();
  await expect(projects.locator(".project-row").filter({ has: app.getByText("Site", { exact: true }) })).toBeVisible();
  expect((await calls(app)).filter(c => c.cmd === "fs.list").at(-1)?.args.teamId).toBe(team.id);
  expect((await calls(app)).filter(c => c.cmd === "fs.mkdir").at(-1)?.args).toEqual({ teamId: team.id, parent: team.workspaceRoot, name: "Site" });
});
