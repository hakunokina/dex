// #45: "Düzeni sıfırla" builds the default without the rack panel (its chips are in the top bar).
// #50: the root splits into columns — Kart over the workers on the left, the leader full height at
// ~40% on the right with the notes as its tabs; emptying the bottom-right of a two-row layout lets
// the group above it grow to full height. The team tabs run leftwards from the DEX logo.
// #46/#2086: the notes are one free panel — an old two-page layout folds into it, reopened from the palette.
import type { Page } from "@playwright/test";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

/** Docked groups of the team on screen with their box and tab titles. */
const groups = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll(".team-stage:not(.hidden) .dv-grid-view .dv-groupview")].map((g) => {
      const r = g.getBoundingClientRect();
      return {
        left: Math.round(r.left),
        top: Math.round(r.top),
        width: Math.round(r.width),
        height: Math.round(r.height),
        tabs: [...g.querySelectorAll(".dv-tab")].map((t) => t.textContent?.trim() ?? ""),
      };
    }),
  );
const stage = (page: Page) =>
  page.evaluate(() => {
    const r = document.querySelector(".team-stage:not(.hidden) .workspace")!.getBoundingClientRect();
    return { width: Math.round(r.width), height: Math.round(r.height) };
  });

test("Düzeni sıfırla: solda Kart üstte worker'lar altta, sağda lider tam boy ~%40 ve notlar sekmesi, raf paneli yok", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Düzen ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const workers = agents.filter((a) => a.kind === "worker");
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Düzeni sıfırla");
  await app.keyboard.press("Enter");

  const full = await stage(app);
  const has = (name: string) => async () => (await groups(app)).filter((g) => g.tabs.some((t) => t.includes(name))).length;
  await expect.poll(has("Kart")).toBe(1);
  await expect.poll(has("Notlar")).toBe(1);
  for (const w of workers) await expect.poll(has(w.name)).toBe(1);
  expect((await groups(app)).some((g) => g.tabs.some((t) => t.includes("Raf")))).toBe(false);

  const all = await groups(app);
  const lead = all.find((g) => g.tabs.some((t) => t.includes(leader.name)))!;
  expect(lead.width / full.width).toBeGreaterThan(0.37);
  expect(lead.width / full.width).toBeLessThan(0.43);
  expect(lead.height).toBeGreaterThan(full.height - 10);
  expect(lead.tabs).toEqual(expect.arrayContaining(["Notlar", "Sen yokken"]));
  expect(Math.max(...all.map((g) => g.left + g.width))).toBe(lead.left + lead.width);
  const kart = all.find((g) => g.tabs.includes("Kart"))!;
  expect(kart.tabs).toEqual(["Kart", "Takım", "Ayarlar"]);
  expect(kart.left).toBe(Math.min(...all.map((g) => g.left)));
  expect(kart.top).toBe(lead.top);
  for (const w of workers) {
    const g = all.find((g) => g.tabs.some((t) => t.includes(w.name)))!;
    expect(g.top).toBeGreaterThan(kart.top + kart.height - 1);
    expect(g.left + g.width).toBeLessThan(lead.left);
  }

  // Ölçüm: the leader terminal's size once it starts (fake claude).
  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  const resizes = async () => (await calls(app)).filter((c) => c.cmd === "pty_resize" && c.args.id === leader.id);
  await expect.poll(async () => (await resizes()).length).toBeGreaterThan(0);
  const size = (await resizes()).at(-1)!.args;
  console.log("LEADER", JSON.stringify({ cols: size.cols, rows: size.rows }));
});

test("Notlar tek panel: eski iki sayfalı kayıtlı düzen tek Notlar'a göçer, boş grup kalmaz, kapatılınca paletten döner", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Notlar ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  // Before #2086: Kart | [leader, Ortak not] with "Takım notu" split off under it in a group of its own.
  const panels = Object.fromEntries([
    { id: "office", contentComponent: "office", tabComponent: "fixed", title: "Kart" },
    { id: `agent:${leader.id}`, contentComponent: "agent", tabComponent: "agent", title: leader.name, params: { agentId: leader.id } },
    { id: "notes:shared", contentComponent: "notes", tabComponent: "notes", title: "Ortak not", params: { page: "shared" } },
    { id: "notes:team", contentComponent: "notes", tabComponent: "notes", title: "Takım notu", params: { page: "team" } },
  ].map((p) => [p.id, p]));
  const leaf = (id: string, views: string[], size: number) => ({ type: "leaf", data: { id, views, activeView: views[0] }, size });
  const dock = {
    grid: {
      orientation: "HORIZONTAL", width: 1400, height: 800,
      root: { type: "branch", size: 800, data: [
        leaf("1", ["office"], 800),
        { type: "branch", size: 600, data: [leaf("2", [`agent:${leader.id}`, "notes:shared"], 500), leaf("3", ["notes:team"], 300)] },
      ] },
    },
    panels, activeGroup: "3",
  };
  await app.goto("about:blank"); // no page left to save its own layout over this one
  await dexd.call("layout_save", { layout: { version: 2, active: team.id, teams: { [team.id]: { dock, shells: [] } }, sessions: {} } });
  await app.goto("/");
  await paneOf(app, leader.name);

  const notesTabs = async () => (await groups(app)).map((g) => g.tabs.filter((t) => t.includes("Notlar"))).filter((t) => t.length);
  await expect.poll(notesTabs).toEqual([["Notlar"]]);
  const all = await groups(app);
  // Kart | leader + Notlar + Sen yokken (#2235 bir kez eklenir) — the emptied group is gone; #2266 adds the empty terminal group once, under the Kart.
  expect(all.map((g) => g.tabs.length)).toEqual([1, 0, 3]);
  expect(all[2].tabs).toEqual([leader.name, "Notlar", "Sen yokken"].map((t) => expect.stringContaining(t)));
  expect(all[1].top).toBeGreaterThan(all[0].top + all[0].height - 1);
  expect(all[1].left).toBe(all[0].left);
  const stageEl = app.locator(".team-stage:not(.hidden)");
  await stageEl.locator(".dv-tab", { hasText: "Notlar" }).click();
  await expect(stageEl.getByRole("heading", { name: /Ortak not/ })).toBeVisible();
  await expect(stageEl.getByRole("heading", { name: /Takım notu/ })).toBeVisible();

  // Closable like any panel; the palette brings it back.
  await app.getByRole("button", { name: "Notlar sekmesini kapat" }).click();
  await expect.poll(notesTabs).toEqual([]);
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Notları aç");
  await app.keyboard.press("Enter");
  await expect.poll(notesTabs).toEqual([["Notlar"]]);
});

test("Terminal: yeni takım sol altta bir terminalle açılır; hepsi kapanınca pane ve düğmeler kalır, yenisi aynı gruba açılır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Terminal ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  const rack = app.getByRole("group", { name: "Takım ajanları" });
  await expect(rack.getByRole("button", { name: new RegExp(leader.name) })).toBeVisible();
  const stageEl = app.locator(".team-stage:not(.hidden)");
  const headerNew = stageEl.locator(".dex-group-action[aria-label='Yeni terminal']");
  const headerProcs = stageEl.locator(".dex-group-action[aria-label='Süreçler']");
  const emptyPane = stageEl.locator(".shell-empty");

  // #65: the default screen has a shell row at the bottom left, under the workers, with its "Yeni terminal".
  const shellGroups = async () => (await groups(app)).filter((g) => g.tabs.includes("Terminal"));
  await expect.poll(async () => (await shellGroups()).map((g) => g.tabs.length)).toEqual([1]);
  const all = await groups(app);
  const [sh] = await shellGroups();
  const kart = all.find((g) => g.tabs.includes("Kart"))!;
  expect(sh.left).toBe(kart.left);
  expect(sh.top + sh.height).toBe(Math.max(...all.map((g) => g.top + g.height)));
  await expect(headerNew).toHaveCount(1);
  await expect(rack.getByText("Terminal")).toHaveCount(0);

  // Keyboard: the header action is a real button; the new shell tabs onto the same group.
  await headerNew.focus();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await shellGroups()).map((g) => g.tabs.length)).toEqual([2]);

  // #2266: all closed — the group stays put at its size, header buttons and the empty pane in it.
  const close = app.getByRole("button", { name: "Terminali kapat" });
  while (await close.count()) await close.first().click();
  await expect.poll(shellGroups).toEqual([]);
  const box = (g: { left: number; top: number; width: number; height: number }) => [g.left, g.top, g.width, g.height];
  await expect.poll(async () => (await groups(app)).filter((g) => g.tabs.length === 0).map(box)).toEqual([box(sh)]);
  await expect(headerNew).toHaveCount(1);
  await expect(headerProcs).toHaveCount(1);
  await expect(emptyPane).toBeVisible();
  const open = emptyPane.getByRole("button", { name: "Terminal aç" });
  await expect(open).toBeVisible();
  // Keyboard: the empty pane's button opens the shell into this very group.
  await open.focus();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await shellGroups()).map((g) => g.tabs.length)).toEqual([1]);
  expect(box((await shellGroups())[0])).toEqual(box(sh));
  await expect(emptyPane).toHaveCount(0);

  // The pane survives a reload empty, and the header's "Yeni terminal" refills it in place.
  const saves = async () => (await calls(app)).filter((c) => c.cmd === "layout_save").length;
  const saved = await saves();
  await close.first().click();
  await expect.poll(shellGroups).toEqual([]);
  await expect(emptyPane).toBeVisible();
  await expect.poll(saves).toBeGreaterThan(saved); // the debounced layout write landed before the reload
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await expect(stageEl.locator(".shell-empty")).toBeVisible();
  await expect.poll(async () => (await groups(app)).filter((g) => g.tabs.length === 0).map(box)).toEqual([box(sh)]);
  await stageEl.locator(".dex-group-action[aria-label='Yeni terminal']").click();
  await expect.poll(async () => (await shellGroups()).map((g) => g.tabs.length)).toEqual([1]);
  expect(box((await shellGroups())[0])).toEqual(box(sh));
  // Süreçler opens into the same group too.
  await stageEl.locator(".dex-group-action[aria-label='Süreçler']").click();
  await expect.poll(async () => (await shellGroups()).map((g) => g.tabs)).toEqual([["Terminal", "Süreçler"]]);

  // Close them: a live PowerShell holds the team workdir and the fixture could not remove its temp root.
  while (await close.count()) await close.first().click();
  await expect.poll(shellGroups).toEqual([]);
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "pty_kill").length).toBe(1);
});

test("Üst çubuk: takım sekmeleri DEX logosunun solundan başlar, 1. takım logoya bitişik, sonrakiler sola", async ({ app, dexd }) => {
  await newTeam(dexd, `Sekme A ${test.info().workerIndex}`);
  await newTeam(dexd, `Sekme B ${test.info().workerIndex}`);
  await app.reload();
  const tabs = app.getByRole("navigation", { name: "Takımlar" }).getByRole("button");
  await expect.poll(() => tabs.count()).toBeGreaterThanOrEqual(2);
  const boxes = await tabs.evaluateAll((els) => els.map((e) => e.getBoundingClientRect()).map((r) => ({ left: r.left, right: r.right })));
  const logo = (await app.locator(".topbar .brand").boundingBox())!;
  // DOM order = team order; the first one ends right before the logo (the bar's 12 px gap), each next one further left.
  expect(logo.x - boxes[0].right).toBeGreaterThanOrEqual(0);
  expect(logo.x - boxes[0].right).toBeLessThan(16);
  for (let i = 1; i < boxes.length; i++) expect(boxes[i].right).toBeLessThanOrEqual(boxes[i - 1].left);
});

test("#75 Üst çubuk: logo en büyük, takım sekmesi ve ilk çip logoya eşit uzak, pane sekme satırı çubuğa yapışık değil", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Aralık ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  const measure = () =>
    app.evaluate(() => {
      const box = (el: Element | null) => el!.getBoundingClientRect();
      const brand = document.querySelector(".topbar > .brand")!;
      document.querySelector(".team-tabs-scroll")!.scrollLeft = 0; // earlier tests' teams may have scrolled the strip
      const tab = box(document.querySelector(".team-tabs .team-tab")); // first team: the one against the logo
      const chip = box(document.querySelector(".topbar .rack-chip")); // its working/waiting overlay sits on the border box
      const b = box(brand);
      const ink = b.right + parseFloat(getComputedStyle(brand).marginRight); // minus DEX's trailing letter-space
      const bar = box(document.querySelector(".topbar"));
      const groupTop = Math.min(...[...document.querySelectorAll(".team-stage:not(.hidden) .dv-grid-view .dv-groupview")].map((g) => box(g).top));
      const fs = (sel: string) => parseFloat(getComputedStyle(document.querySelector(sel)!).fontSize);
      return {
        left: b.left - tab.right,
        right: chip.left - ink,
        centred: Math.abs((b.left + ink) / 2 - window.innerWidth / 2),
        below: groupTop - bar.bottom,
        logo: fs(".topbar > .brand"),
        names: Math.max(fs(".team-tabs .team-tab"), fs(".topbar .rack-name")),
        img: box(brand.querySelector("svg")).height,
        overflow: document.querySelector(".topbar")!.scrollWidth > document.querySelector(".topbar")!.clientWidth,
      };
    });
  for (const density of ["compact", "normal"]) {
    await app.evaluate((d) => localStorage.setItem("dex.density", d), density);
    await app.setViewportSize({ width: 1100, height: 720 });
    await app.reload();
    await expect(app.locator("html")).toHaveAttribute("data-density", density);
    await openTeam(app, team.name);
    await paneOf(app, leader.name);
    const m = await measure();
    console.log("TOPBAR", density, JSON.stringify(m));
    expect(Math.abs(m.left - m.right)).toBeLessThanOrEqual(1);
    expect(m.left).toBeGreaterThan(0);
    expect(m.centred).toBeLessThanOrEqual(2);
    expect(m.below).toBeGreaterThanOrEqual(6);
    expect(m.logo).toBeGreaterThan(m.names);
    expect(m.img).toBeGreaterThan(18);
    expect(m.overflow).toBe(false);
  }
  await app.evaluate(() => localStorage.removeItem("dex.density"));
});

test("İki satırlı kayıtlı düzen: sağ-alt grup boşalınca üstündeki lider tam boya uzar, başka grup panel kaybetmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Köşe ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const [w1, w2, w3, w4] = agents.filter((a) => a.kind === "worker");
  // Two-row layout: [Kart | leader + notes] over [w1, w2 | w3, w4] — the root split in rows.
  const agent = (a: { id: string; name: string }) => ({ id: `agent:${a.id}`, contentComponent: "agent", tabComponent: "agent", title: a.name, params: { agentId: a.id } });
  const panels = Object.fromEntries([
    { id: "office", contentComponent: "office", tabComponent: "fixed", title: "Kart" },
    agent(leader), agent(w1), agent(w2), agent(w3), agent(w4),
    { id: "notes:shared", contentComponent: "notes", tabComponent: "notes", title: "Ortak not", params: { page: "shared" } },
    { id: "notes:team", contentComponent: "notes", tabComponent: "notes", title: "Takım notu", params: { page: "team" } },
  ].map((p) => [p.id, p]));
  const leaf = (id: string, views: string[], size: number) => ({ type: "leaf", data: { id, views, activeView: views[0] }, size });
  const dock = {
    grid: {
      orientation: "VERTICAL", width: 1400, height: 800,
      root: { type: "branch", size: 1400, data: [
        { type: "branch", size: 450, data: [leaf("1", ["office"], 700), leaf("2", [`agent:${leader.id}`, "notes:shared", "notes:team"], 700)] },
        { type: "branch", size: 350, data: [leaf("3", [`agent:${w1.id}`, `agent:${w2.id}`], 700), leaf("4", [`agent:${w3.id}`, `agent:${w4.id}`], 700)] },
      ] },
    },
    panels, activeGroup: "2",
  };
  await app.goto("about:blank"); // no page left to save its own layout over this one
  await dexd.call("layout_save", { layout: { version: 2, active: team.id, teams: { [team.id]: { dock, shells: [] } }, sessions: {} } });
  await app.goto("/");
  await paneOf(app, leader.name);

  const tabsOf = async (name: string) => (await groups(app)).find((g) => g.tabs.some((t) => t.includes(name)))!;
  const full = await stage(app);
  expect((await tabsOf(leader.name)).height).toBeLessThan(full.height * 0.7);

  // Empty the bottom-right group by closing its two panels.
  for (const w of [w3, w4]) await app.getByRole("button", { name: `${w.name} sekmesini kapat` }).click();
  await expect.poll(async () => (await tabsOf(leader.name)).height).toBeGreaterThan(full.height - 10);
  const all = await groups(app);
  const lead = await tabsOf(leader.name);
  // #2086: the saved two pages came back as the one "Notlar" tab in their place; #2235 adds "Sen yokken" once.
  expect(lead.tabs).toEqual([leader.name, "Notlar", "Sen yokken"].map((t) => expect.stringContaining(t)));
  expect(Math.max(...all.map((g) => g.left + g.width))).toBe(lead.left + lead.width);
  // The rest stays as it was, on the left: Kart over w1 + w2 (#2266: plus the empty terminal group added once under them).
  expect(all).toHaveLength(4);
  const kart = await tabsOf("Kart");
  const bottom = await tabsOf(w1.name);
  const shell = all.find((g) => g.tabs.length === 0)!;
  expect(shell.top).toBeGreaterThan(bottom.top + bottom.height - 1);
  expect(shell.left + shell.width).toBeLessThan(lead.left);
  expect(kart.tabs).toEqual(["Kart"]);
  expect(bottom.tabs).toEqual([w1.name, w2.name].map((t) => expect.stringContaining(t)));
  expect(bottom.top).toBeGreaterThan(kart.top);
  expect(bottom.left + bottom.width).toBeLessThan(lead.left);
});
