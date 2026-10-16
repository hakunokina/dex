// The desktop shell against a real dexd (e2e/fixtures.ts). What the patron sees first: the app
// connects, a team is made from the UI, its crew and the task board show up.
import { test, isolatedTest, expect, calls, openTeam, teamAgents, newTeam } from "./fixtures";
import { existsSync } from "node:fs";
import { dirname, basename } from "node:path";

isolatedTest("eğitim profili: klavyeyle takım kurulur, yalnız eğitim rolleri ve iki üye görünür", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("button", { name: "Takım oluştur…" }).click();
  const form = app.getByRole("dialog", { name: "Yeni takım", exact: true });
  await expect(form.getByRole("textbox", { name: "Ad", exact: true })).toBeFocused();
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Ders");
  const profile = form.getByRole("combobox", { name: "Profil", exact: true });
  await expect(profile).toHaveValue("dev");
  await expect(form.getByRole("combobox", { name: /^Worker \d rolü$/ })).toHaveCount(4);
  await expect(form.locator('optgroup[label="Eğitim"]')).toHaveCount(0);
  await profile.focus();
  await app.keyboard.press("ArrowDown");
  await app.keyboard.press("Enter");
  await expect(profile).toHaveValue("edu");
  await expect(form.getByRole("combobox", { name: /^Worker \d rolü$/ })).toHaveCount(0);
  await expect(form.getByText("Öğretmen (lider) + Görsel Anlatıcı", { exact: true })).toBeVisible();
  expect(await form.evaluate(el => el.scrollWidth <= el.clientWidth && el.getBoundingClientRect().right <= window.innerWidth)).toBe(true);
  const create = form.getByRole("button", { name: "Oluştur", exact: true });
  await expect(create).toBeEnabled();
  await create.focus();
  await app.keyboard.press("Tab");
  await expect(form.getByRole("textbox", { name: "Ad", exact: true })).toBeFocused();
  await create.focus();
  await app.keyboard.press("Enter");
  await expect(form).toHaveCount(0);
  const request = (await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args?.team;
  expect(request).toMatchObject({ name: "Ders", profile: "edu" });
  expect(request).not.toHaveProperty("workerRoles");
  expect(request).not.toHaveProperty("members");
  const team = (await dexd.call<{ id: string; name: string; profile: string }[]>("team_list")).find(t => t.name === "Ders")!;
  expect(team.profile).toBe("edu");
  expect((await teamAgents(dexd, team.id)).map(a => a.name).sort()).toEqual(["Chalk-1", "Sage"]);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  const settings = app.locator(".team-settings");
  for (const label of ["Sage rolü", "Chalk-1 rolü", "Yeni üyenin rolü"]) {
    const picker = settings.getByRole("combobox", { name: label, exact: true });
    await expect(picker.locator("optgroup")).toHaveAttribute("label", "Eğitim");
    await expect(picker.locator("option")).toHaveCount(2);
  }
  await settings.getByRole("button", { name: "Takımı kopyala", exact: true }).click();
  const copyForm = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
  await copyForm.getByRole("textbox", { name: "Ad", exact: true }).fill("Ders Kopya");
  const copyCreate = copyForm.getByRole("button", { name: "Oluştur", exact: true });
  await expect(copyCreate).toBeEnabled();
  await copyCreate.focus();
  await app.keyboard.press("Enter");
  await expect(copyForm).toHaveCount(0);
  expect((await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args?.team).toMatchObject({ profile: "edu" });
  const copy = (await dexd.call<{ id: string; name: string; profile: string }[]>("team_list")).find(t => t.name === "Ders Kopya")!;
  expect(copy.profile).toBe("edu");
  expect((await teamAgents(dexd, copy.id)).map(a => a.name).sort()).toEqual(["Chalk-1", "Sage"]);
});

test("uygulama açılır, çekirdeğe bağlanır; UI'dan takım kurulur ve ekibi görünür", async ({ app, dexd }) => {
  const errors: string[] = [];
  app.on("pageerror", (e) => errors.push(e.message));
  // #47: no team yet → no Kart (its "Yeni takım" button); the empty stage offers the create action.
  const create = app.getByRole("status").getByRole("button", { name: "Takım oluştur…" });
  await expect(create).toBeVisible();

  const name = `Açılış ${test.info().workerIndex}`;
  const folder = `Açılış${test.info().workerIndex}`;
  await create.click();
  await app.getByRole("textbox", { name: "Ad" }).fill(name);
  await expect(app.locator(".team-folder-preview")).toContainText(`${folder}Main`);
  await expect(app.getByRole("button", { name: "Oluştur", exact: true })).toBeEnabled();
  await app.getByRole("button", { name: "Oluştur", exact: true }).click();

  await expect(app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name, exact: true })).toBeVisible();
  const team = (await dexd.call<{ id: string; name: string; cwd: string }[]>("team_list")).find((t) => t.name === name);
  expect(team, "takım çekirdekte").toBeTruthy();
  expect(basename(team!.cwd)).toBe(`${folder}Main`);
  expect(basename(dirname(team!.cwd))).toBe(folder);
  expect(existsSync(team!.cwd)).toBe(true);
  expect(existsSync(dirname(team!.cwd))).toBe(true);
  const projects = await dexd.call<{ name: string; rootPath: string }[]>("project_list", { teamId: team!.id });
  expect(projects).toMatchObject([{ name: `${folder}Main`, rootPath: team!.cwd }]);
  const crew = await teamAgents(dexd, team!.id);
  expect(crew.map((a) => a.kind).sort()).toEqual(["leader", "worker", "worker", "worker", "worker"]);
  for (const a of crew) await expect(app.getByRole("region", { name: a.name, exact: true })).toBeVisible();
  // #47: the team's Kart carries the status count (no "bekliyor") and the team/app buttons.
  const kart = app.getByRole("group", { name: "Kart araçları" });
  await expect(kart).toContainText("çalışıyor");
  await expect(kart).toContainText("boşta");
  await expect(kart).not.toContainText("bekliyor");
  for (const b of ["Görevler", "Yeni takım", "Tüm worker'lar", "Takım ayarları", "Ayarlar"]) await expect(kart.getByRole("button", { name: b, exact: true })).toBeVisible();
  // Tüm worker'lar: popover düğmenin yerinde açılır, seçimler içinde, Esc odağı düğmeye döndürür.
  const bulkBtn = kart.getByRole("button", { name: "Tüm worker'lar", exact: true });
  await bulkBtn.click();
  const bulk = app.getByRole("dialog", { name: "Tüm worker'lar", exact: true });
  await expect(bulk.getByRole("combobox", { name: "Worker'ların sağlayıcısı" })).toBeFocused();
  await expect(bulk.getByRole("combobox", { name: "Worker'ların modeli" })).toBeVisible();
  await expect(bulk.getByRole("button", { name: "Uygula", exact: true })).toBeVisible();
  expect(await bulk.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return r.left >= 0 && r.right <= window.innerWidth && r.top >= 0 && r.bottom <= window.innerHeight;
  })).toBe(true);
  await app.keyboard.press("Escape");
  await expect(bulk).toHaveCount(0);
  await expect(bulkBtn).toBeFocused();
  // #2134: Kart dar (≈260px) ve alçak bir paneldeyken de popover kırpılmaz — tamamen pencere
  // içinde kalır, Uygula elementFromPoint ile tıklanabilir ve sonucu gösterir.
  const kartSize = await app.evaluate(async (teamId) => {
    const { boundApi } = await import("/src/layout/panels.ts");
    const office = boundApi(teamId)!.getPanel("office")!;
    office.api.setActive();
    const size = { width: office.group.api.width, height: office.group.api.height };
    office.group.api.setSize({ width: 260, height: 160 });
    return size;
  }, team!.id);
  await expect.poll(() => app.evaluate(async (teamId) => {
    const { boundApi } = await import("/src/layout/panels.ts");
    return boundApi(teamId)!.getPanel("office")!.group.element.getBoundingClientRect().width;
  }, team!.id)).toBeLessThanOrEqual(262);
  await bulkBtn.click();
  await expect(bulk.getByRole("combobox", { name: "Worker'ların sağlayıcısı" })).toBeFocused();
  const apply = bulk.getByRole("button", { name: "Uygula", exact: true });
  await expect(apply).toBeVisible();
  expect(await bulk.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return r.left >= 0 && r.right <= window.innerWidth && r.top >= 0 && r.bottom <= window.innerHeight;
  })).toBe(true);
  expect(await apply.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return hit === el || el.contains(hit);
  })).toBe(true);
  await apply.click();
  await expect(bulk).toContainText(/worker değişti/);
  await app.keyboard.press("Escape");
  await expect(bulk).toHaveCount(0);
  await app.evaluate(async ({ teamId, size }) => {
    const { boundApi } = await import("/src/layout/panels.ts");
    boundApi(teamId)!.getPanel("office")!.group.api.setSize(size);
  }, { teamId: team!.id, size: kartSize });
  // Takım ayarları: Sağlayıcılar bölümü ve Tüm worker'lar yok; Test + Takımı kopyala altta yan yana.
  await kart.getByRole("button", { name: "Takım ayarları", exact: true }).click();
  const settings = app.locator(".team-settings");
  await expect(settings.getByRole("switch", { name: "Tam testler" })).toBeVisible();
  await expect(settings.getByRole("button", { name: "Takımı kopyala", exact: true })).toBeVisible();
  await expect(settings.getByRole("region", { name: "Sağlayıcılar", exact: true })).toHaveCount(0);
  await expect(settings.getByText(/Sağlayıcılar'da yönet/)).toHaveCount(0);
  await expect(settings.getByRole("combobox", { name: "Worker'ların sağlayıcısı" })).toHaveCount(0);
  expect(await settings.evaluate((el) => {
    const test = el.querySelector<HTMLElement>(".team-test-level button")!.getBoundingClientRect();
    const copy = [...el.querySelectorAll<HTMLElement>("button")].find((b) => b.textContent?.trim() === "Takımı kopyala")!.getBoundingClientRect();
    return Math.abs(test.top - copy.top) <= 2 && copy.left >= test.right && test.width > 80 && copy.width > 80;
  })).toBe(true);
  await settings.locator(".close").click();
  // #70/#73: the top bar holds only team tabs | DEX logo | crew chips, plus the window controls.
  const bar = app.locator("header.topbar");
  // Only the leader and the red (working/waiting or job-carrying) workers get a chip; a new team's workers are quiet.
  await expect(bar.locator(".rack-chip")).toHaveCount(crew.filter((a) => a.kind === "leader").length);
  const stray = () => bar.evaluate((h) => [...h.querySelectorAll("button")].filter((b) => !b.closest(".team-tabs, .rack, .win-ctl")).length);
  expect(await stray()).toBe(0);
  await expect(bar.locator(":scope > *")).toHaveCount(3);
  await expect(bar.locator(":scope > .brand")).toHaveText("DEX");
  // The logo sits at the window's centre whatever the tab/chip count; each side scrolls inside itself, the bar never spills.
  const layout = () => app.evaluate(() => {
    const bar = document.querySelector(".topbar")!, logo = document.querySelector(".topbar > .brand svg")!.getBoundingClientRect();
    const brand = document.querySelector(".topbar > .brand")!.getBoundingClientRect();
    return {
      centred: Math.abs((brand.left + brand.right) / 2 - window.innerWidth / 2) <= 2,
      logo: logo.width > 0,
      width: bar.getBoundingClientRect().width <= window.innerWidth,
      bar: bar.scrollWidth <= bar.clientWidth,
      ctl: document.querySelector(".win-ctl")!.getBoundingClientRect().right <= window.innerWidth,
    };
  });
  const ok = { centred: true, logo: true, width: true, bar: true, ctl: true };
  expect(await layout()).toEqual(ok);
  // Narrowest desktop window (tauri.conf minWidth), with more tabs than fit on the left.
  await app.setViewportSize({ width: 1100, height: 720 });
  for (let i = 0; i < 4; i++) await newTeam(dexd, `Uzun takım adı ${test.info().workerIndex}-${i}`);
  await expect(app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name: /^Uzun takım adı/ })).toHaveCount(4);
  expect(await layout()).toEqual(ok);
  expect(errors).toEqual([]);
});

test("eski host (project_list yok): ana takım klasör seçilmeden oluşturulamaz", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Eski Kurulum ${test.info().workerIndex}`);
  await app.addInitScript(() => { window.__dexOld = { hide: ["project_list"] }; });
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Yeni takım", exact: true }).click();
  await app.getByRole("textbox", { name: "Ad" }).fill("Klasörsüz olmaz");
  await expect(app.getByText("klasör seçilmedi", { exact: true })).toBeVisible();
  const create = app.getByRole("button", { name: "Oluştur", exact: true });
  await expect(create).toBeDisabled();
  await app.getByRole("button", { name: "Klasör seç…" }).click();
  await expect(create).toBeEnabled();
  await app.getByRole("button", { name: "Vazgeç", exact: true }).click();
});

test("eski host (profile yok): Eğitim engellenir, geliştirici oluşturma korunur", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Eski Profil");
  const teams = await dexd.call<Record<string, unknown>[]>("team_list");
  const oldTeams = teams.map(({ profile: _profile, ...t }) => t);
  await app.addInitScript(({ oldTeams, url, token }) => {
    // The old core accepts team_create but ignores profile, returning a development team.
    window.__dexFake = { team_list: oldTeams, team_create: oldTeams[0] };
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "old-profile", name: "Eski uzak host", url, device: "test" }]));
    localStorage.setItem("dex.e2e.token.old-profile", token);
  }, { oldTeams, url: dexd.url, token: dexd.token });
  await app.reload();
  await app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name: new RegExp(`^${team.name}`) }).first().click();
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Yeni takım", exact: true }).focus();
  await app.keyboard.press("Enter");
  const form = app.getByRole("dialog", { name: "Yeni takım", exact: true });
  const host = form.getByRole("combobox", { name: "Takımın host'u", exact: true });
  await expect(host.locator('option[value="old-profile"]')).toBeEnabled();
  await host.selectOption("old-profile");
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Eski Ders");
  const profile = form.getByRole("combobox", { name: "Profil", exact: true });
  await profile.focus();
  await app.keyboard.press("ArrowDown");
  await app.keyboard.press("Enter");
  await expect(profile).toHaveValue("edu");
  await expect(form.getByRole("alert")).toContainText("Eğitim için bu hostun çekirdeğini güncelleyin.");
  const create = form.getByRole("button", { name: "Oluştur", exact: true });
  await expect(create).toBeDisabled();
  // Even implicit/programmatic submission must stop before the mutating request.
  await form.evaluate(el => (el as HTMLFormElement).requestSubmit());
  await expect(form.getByRole("alert")).toBeVisible();
  expect((await calls(app)).filter(c => c.cmd === "team_create")).toHaveLength(0);
  expect(await form.evaluate(el => el.scrollWidth <= el.clientWidth && el.getBoundingClientRect().right <= window.innerWidth)).toBe(true);
  await profile.selectOption("dev");
  await expect(form.getByRole("alert")).toHaveCount(0);
  await expect(create).toBeEnabled();
  await create.focus();
  await app.keyboard.press("Enter");
  await expect(form).toHaveCount(0);
  expect((await calls(app)).filter(c => c.cmd === "team_create")).toHaveLength(1);
});

for (const failure of ["team_list", "role_list", "eksik eğitim rolleri"]) {
  isolatedTest(`eğitim desteği belirsiz (${failure}): takım oluşturulmaz`, async ({ app }) => {
    await expect(app.getByRole("button", { name: "Takım oluştur…" })).toBeVisible();
    await app.evaluate(failure => {
      window.__dexFake = { team_list: [], ...(failure === "eksik eğitim rolleri" ? { role_list: [] } : {}) };
      if (failure !== "eksik eğitim rolleri") {
        delete window.__dexFake[failure];
        window.__dexOld = { unknown: [failure] };
      }
    }, failure);
    await app.getByRole("button", { name: "Takım oluştur…" }).click();
    const form = app.getByRole("dialog", { name: "Yeni takım", exact: true });
    await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Belirsiz Ders");
    await form.getByRole("combobox", { name: "Profil", exact: true }).selectOption("edu");
    await expect(form.getByRole("alert")).toContainText("çekirdeğini güncelleyin");
    await expect(form.getByRole("button", { name: "Oluştur", exact: true })).toBeDisabled();
    await form.evaluate(el => (el as HTMLFormElement).requestSubmit());
    expect((await calls(app)).filter(c => c.cmd === "team_create")).toHaveLength(0);
  });
}

test("görev panosu: görev oluşturulur, listede ve çekirdekte görünür", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Pano ${test.info().workerIndex}`);
  await app.reload();
  await openTeam(app, team.name);

  // #70: the board button sits in the Kart; ` still toggles the board.
  const boardBtn = app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Görevler" });
  await boardBtn.click();
  await expect(boardBtn).toHaveAttribute("aria-pressed", "true");
  const board = app.getByRole("complementary");
  const input = board.getByRole("textbox", { name: "Yeni görev… (Enter)" });
  await expect(input).toBeVisible();
  const title = `e2e görev ${Date.now()}`;
  await input.fill(title);
  await input.press("Enter");

  await expect(board.getByText(title)).toBeVisible();
  await expect(input).toHaveValue("");
  const tasks = await dexd.call<{ title: string; teamId: string | null; status: string }[]>("task_list", { teamId: team.id });
  expect(tasks.find((t) => t.title === title)).toMatchObject({ teamId: team.id, status: "todo" });
  await app.keyboard.press("Control+Backquote");
  await expect(input).toBeHidden();
  await expect(boardBtn).toHaveAttribute("aria-pressed", "false");
});

test("görev panosu: kartta ham markdown yok, Sürüyor başlığı dağılımı gösterir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Pano Sade ${test.info().workerIndex}`);
  const created = await dexd.call<{ id: number }>("task_create", { task: {
    teamId: team.id, title: "Markdown görev", status: "doing",
    description: "## Patronun sözü\n**Kalın** ve `kod` ile ilk cümle. İkinci cümle.",
  } });
  await app.reload();
  await openTeam(app, team.name);
  await app.setViewportSize({ width: 390, height: 780 });
  const boardBtn = app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Görevler" });
  await boardBtn.click();
  const board = app.getByRole("complementary");
  await expect(board.getByText("Markdown görev")).toBeVisible();
  // Ham markdown görünmez; ilk anlamlı cümleler sade görünür.
  await expect(board.getByText("## Patronun sözü")).toHaveCount(0);
  await expect(board.getByText("**Kalın**")).toHaveCount(0);
  await expect(board.getByText("Kalın ve kod ile ilk cümle. İkinci cümle.")).toBeVisible();
  // #2104: devretmesiz görevde pano görevinin kendi numarası (T#); başlık dağılımı gösterir.
  await expect(board.locator(".board-num")).toHaveText(`T#${created.id}`);
  await expect(board.locator('[data-status="doing"] header')).toContainText("0 çalışıyor · 0 sırada · 1 bekliyor");
});

test("takımı kopyala: panel ve sekme menüsü, düzenlenebilir üyeler, fresh klasör/proje/hafıza", async ({ app, dexd }) => {
  const source = await newTeam(dexd, "Kaynak");
  await dexd.call("agent_create", { agent: { teamId: source.id, name: "Özel Üye", kind: "worker", model: "claude-sonnet-5-5", roleSlug: "frontend" } });
  const original = await teamAgents(dexd, source.id);
  const template = await dexd.call<{ kind: string; members: { name: string; kind: string; model: string; engine: string; roleSlug: string }[] }>("team_clone_template", { teamId: source.id });
  await app.reload();
  await openTeam(app, source.name);
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.locator(".team-settings").getByRole("button", { name: "Takımı kopyala", exact: true }).click();
  const form = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
  await expect(form.getByRole("textbox", { name: "Ad", exact: true })).toHaveValue("");
  await expect(form.locator(".team-clone-member")).toHaveCount(original.length);
  for (const [i, member] of template.members.entries()) {
    await expect(form.getByRole("textbox", { name: `Üye ${i + 1} adı`, exact: true })).toHaveValue(member.name);
    await expect(form.getByRole("combobox", { name: `Üye ${i + 1} modeli`, exact: true })).toHaveValue(member.model);
  }
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Kopya");
  await form.getByRole("textbox", { name: "Üye 2 adı", exact: true }).fill("Yeni Üye");
  await form.getByRole("combobox", { name: "Üye 2 rolü", exact: true }).selectOption("backend");
  // #1713: tek motor (Claude) kaldı — motor seçicisi yok, sağlayıcı ve model seçimi duruyor.
  await expect(form.getByRole("combobox", { name: /motoru$/ })).toHaveCount(0);
  await form.getByRole("combobox", { name: "Üye 2 modeli", exact: true }).selectOption("claude-opus-5-5");
  await expect(form.getByRole("combobox", { name: "Üye 2 modeli", exact: true })).toHaveValue("claude-opus-5-5");
  expect((await form.boundingBox())!.width).toBeLessThanOrEqual(390);
  await form.getByRole("button", { name: "Oluştur", exact: true }).focus();
  await app.keyboard.press("Tab");
  await expect(form.getByRole("textbox", { name: "Ad", exact: true })).toBeFocused();
  await app.keyboard.press("Shift+Tab");
  await expect(form.getByRole("button", { name: "Oluştur", exact: true })).toBeFocused();
  await app.keyboard.press("Enter");
  await expect(form).toHaveCount(0);
  const teams = await dexd.call<{ id: string; name: string; cwd: string; memoryDir: string; workspaceRoot: string }[]>("team_list");
  const copy = teams.find(t => t.name === "Kopya")!;
  expect(copy).toBeTruthy();
  expect(copy.id).not.toBe(source.id);
  expect(basename(copy.cwd)).toBe("KopyaMain");
  expect(basename(copy.workspaceRoot)).toBe("Kopya");
  expect(copy.memoryDir).not.toBe(teams.find(t => t.id === source.id)!.memoryDir);
  const crew = await dexd.call<{ id: string; teamId: string; name: string; engine: string; roleSlug: string; model: string }[]>("agent_list");
  const copied = crew.filter(a => a.teamId === copy.id);
  expect(copied).toHaveLength(original.length);
  expect(copied.every(a => !original.some(o => o.id === a.id))).toBe(true);
  expect(copied.find(a => a.name === "Yeni Üye")).toMatchObject({ engine: "claude", roleSlug: "backend", model: "claude-opus-5-5" });
  expect(await dexd.call("project_list", { teamId: copy.id })).toMatchObject([{ name: "KopyaMain", rootPath: copy.cwd }]);
  expect((await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args).toMatchObject({ team: { name: "Kopya", cwd: null, members: expect.any(Array) } });
  const tab = app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name: /^Kaynak( \d+)?$/ });
  await tab.focus();
  await app.keyboard.press("Shift+F10");
  const menu = app.getByRole("menu", { name: "Takım işlemleri" });
  await expect(menu.getByRole("menuitem", { name: "Takımı kopyala" })).toBeFocused();
  await app.keyboard.press("Enter");
  await expect(form.locator(".team-clone-member")).toHaveCount(original.length);
  await app.keyboard.press("Escape");
  await expect(form).toHaveCount(0);
  await expect(tab).toBeFocused();
});

for (const provider of ["anthropic", "local"]) {
  test(`takımı kopyala: ${provider} üyesi canlı local modellerini seçer`, async ({ app, dexd }) => {
    const source = await dexd.call<{ id: string; name: string }>("team_create", { team: {
      name: `Local Kaynak ${provider}`, kind: "main", members: [
        { name: "Lead", kind: "leader", engine: "claude", provider, model: provider === "local" ? "qwen-a" : "claude-sonnet-5-5" },
      ],
    } });
    await app.addInitScript(() => {
      window.__dexFake = {
        provider_list: [
          { id: "anthropic", label: "Anthropic", baseUrl: null, hasToken: false, tokenMask: null, models: [{ id: "claude-sonnet-5-5", label: "Sonnet" }] },
          { id: "local", label: "Local", baseUrl: "http://127.0.0.1:4000", hasToken: true, tokenMask: null, models: [] },
        ],
        provider_local_status: { baseUrl: "http://127.0.0.1:4000", registry: false, models: [
          { id: "qwen-a", backend: null, removable: false }, { id: "qwen-b", backend: null, removable: false },
        ] },
      };
    });
    await app.reload();
    await openTeam(app, source.name);
    await app.setViewportSize({ width: 390, height: 780 });
    const tab = app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name: new RegExp(`^${source.name}`) });
    await tab.focus();
    await app.keyboard.press("Shift+F10");
    await app.keyboard.press("Enter");
    const form = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
    await form.getByRole("textbox", { name: "Ad", exact: true }).fill(`Local Kopya ${provider}`);
    const picker = form.getByRole("combobox", { name: "Üye 1 sağlayıcısı", exact: true });
    await expect(picker.locator('option[value="local"]')).toHaveCount(1);
    if (provider !== "local") await picker.selectOption("local");
    const models = form.getByRole("combobox", { name: "Üye 1 modeli", exact: true });
    await expect(models.locator("option")).toHaveText(["qwen-a", "qwen-b"]);
    await expect(models).toHaveValue("qwen-a");
    const create = form.getByRole("button", { name: "Oluştur", exact: true });
    await expect(create).toBeEnabled();
    await models.focus();
    await app.keyboard.press("ArrowDown");
    await app.keyboard.press("Enter");
    await expect(models).toHaveValue("qwen-b");
    await expect(create).toBeEnabled();
    expect((await form.boundingBox())!.width).toBeLessThanOrEqual(390);
    await create.focus();
    await app.keyboard.press("Enter");
    await expect(form).toHaveCount(0);
    expect((await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args).toMatchObject({ team: {
      members: [{ provider: "local", model: "qwen-b" }],
    } });
  });
}

test("takımı kopyala: eski hosttan gelen Codex üyesi Claude'a çevrilir", async ({ app, dexd }) => {
  const source = await newTeam(dexd, "Codex Kaynak");
  await app.addInitScript(() => {
    window.__dexFake = {
      provider_list: [{ id: "anthropic", label: "Anthropic", baseUrl: null, hasToken: false, tokenMask: null, models: [{ id: "claude-sonnet-5-5", label: "Sonnet" }] }],
      team_clone_template: { kind: "main", profile: "dev", members: [{ name: "Eski", kind: "leader", engine: "codex", provider: null, model: "gpt-5.5" }] },
    };
  });
  await app.reload();
  await openTeam(app, source.name);
  await app.setViewportSize({ width: 390, height: 780 });
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.locator(".team-settings").getByRole("button", { name: "Takımı kopyala", exact: true }).click();
  const form = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
  await expect(form.getByText("Codex üyeleri Claude'a çevrildi")).toBeVisible();
  await expect(form.getByRole("combobox", { name: "Üye 1 sağlayıcısı", exact: true })).toHaveValue("anthropic");
  await expect(form.getByRole("combobox", { name: "Üye 1 modeli", exact: true })).toHaveValue("claude-sonnet-5-5");
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Codex Kopya");
  await form.getByRole("button", { name: "Oluştur", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(form).toHaveCount(0);
  expect((await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args).toMatchObject({ team: {
    members: [{ name: "Eski", engine: null, provider: "anthropic", model: "claude-sonnet-5-5" }],
  } });
});

test("takımı kopyala: katalog gecikirken de Codex üyesi gönderilmez", async ({ app, dexd }) => {
  const source = await newTeam(dexd, "Codex Gecikme");
  await app.addInitScript(() => {
    window.__dexFake = {
      team_clone_template: { kind: "main", profile: "dev", members: [{ name: "Eski", kind: "leader", engine: "codex", provider: null, model: "gpt-5.5" }] },
    };
  });
  await app.reload();
  await openTeam(app, source.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  // The catalog never arrives for the clone form.
  await app.evaluate(() => { window.__dexFake!.provider_list = new Promise(() => {}); });
  await app.locator(".team-settings").getByRole("button", { name: "Takımı kopyala", exact: true }).click();
  const form = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
  await expect(form.getByText("Model kataloğu yükleniyor…")).toBeVisible();
  await expect(form.getByText("Codex üyeleri Claude'a çevrildi")).toBeVisible();
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Codex Gecikme Kopya");
  await expect(form.getByText("Model kataloğu yükleniyor…")).toBeVisible();
  await expect(form.getByRole("alert")).toHaveCount(0);
  await form.getByRole("button", { name: "Oluştur", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect((await calls(app)).filter(c => c.cmd === "team_create").at(-1)?.args).toMatchObject({ team: {
    members: [{ name: "Eski", engine: null, provider: "anthropic", model: "claude-fable-5-1" }],
  } });
});

test("kopya şablonu hatası görünür ve yeniden denenebilir", async ({ app, dexd }) => {
  const source = await newTeam(dexd, "Kopya Hata");
  await app.addInitScript(() => {
    window.__dexOld = { unknown: ["team_clone_template"] };
  });
  await app.reload();
  await openTeam(app, source.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.locator(".team-settings").getByRole("button", { name: "Takımı kopyala", exact: true }).click();
  const form = app.getByRole("dialog", { name: "Takımı kopyala", exact: true });
  await expect(form.getByRole("alert")).toContainText("team_clone_template");
  await form.getByRole("textbox", { name: "Ad", exact: true }).fill("Hatalı Kopya");
  await expect(form.getByRole("button", { name: "Oluştur", exact: true })).toBeDisabled();
  await app.evaluate(() => { window.__dexOld = {}; });
  await form.getByRole("button", { name: "Yeniden dene", exact: true }).click();
  await expect(form.locator(".team-clone-member")).toHaveCount((await teamAgents(dexd, source.id)).length);
  await expect(form.getByRole("button", { name: "Oluştur", exact: true })).toBeEnabled();
  await app.keyboard.press("Escape");
  await expect(form).toHaveCount(0);
});
