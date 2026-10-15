// #916: süreç paneli — Kart'taki CPU düğmesi açar; ajan başına CPU/RAM, en ağır üstte; alt süreç
// listesi açılır, kapat yalnız çocukta ve onayla; kapalıyken istek yok. Ağaç ancak Windows job'undan
// gelir: agent_processes / kill_agent_process köprüde hazır cevaplanır (__dexFake).
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const proc = (pid: number, x: Record<string, unknown>) => ({ pid, ppid: 1, name: `p${pid}.exe`, cmd: `p${pid}`, cpuPct: 0, memMb: 0, kind: "other", root: false, ...x });

test("Süreç paneli: ağır ajan üstte, alt süreci onayla kapatır, kapalıyken istek yok", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Süreç ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  const fake = {
    agent_processes: {
      // #925: an unreadable tree stays on its agent's row with the reason; the others still list.
      [leader.id]: { processes: [], error: "ajanın süreç listesi (job) okunamadı" },
      [worker.id]: {
        processes: [
          proc(200, { root: true, name: "claude.exe", cpuPct: 0.2, memMb: 150, kind: "node" }),
          proc(201, { name: "chrome.exe", cmd: "chrome.exe --headless", cpuPct: 6.5, memMb: 900, kind: "browser" }),
          proc(202, { ppid: 201, name: "chrome.exe", cmd: "chrome.exe --type=renderer", memMb: 100, kind: "browser" }),
          proc(203, { name: "node.exe", memMb: 90, kind: "browser-tool" }),
          proc(204, { name: "unknown.exe", kind: "future-kind" }),
        ],
      },
    },
    kill_agent_process: null,
  };
  await app.addInitScript((f) => {
    window.__dexFake = f;
  }, fake);
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const stage = app.locator(".team-stage:not(.hidden)");
  // Terminal grubunun başlığındaki aç/kapa düğmesi; Kart araçlarında artık yok.
  await expect(app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Süreçler" })).toHaveCount(0);
  const button = stage.getByRole("button", { name: "Süreçler", exact: true });
  await expect(button).toHaveAttribute("aria-pressed", "false");
  await button.click();
  await expect(button).toHaveAttribute("aria-pressed", "true");
  const group = stage.locator(".dv-groupview").filter({ has: app.getByRole("tab", { name: "Süreçler", exact: true }) });
  await expect(group.getByRole("tab", { name: "Terminal", exact: true })).toHaveCount(1);
  const panel = app.getByRole("complementary", { name: "Süreçler" });
  await expect(panel.locator(".procs-sum")).toHaveText(/1 tarayıcı \(1000 MB\) · 1 tarayıcı aracı \(90 MB\)/);
  const heads = panel.locator(".procs-agent-head");
  await expect(heads).toHaveCount(2);
  await expect(heads.first()).toContainText(worker.name);
  await expect(heads.first()).toContainText("%6.7");
  const failed = panel.locator(".procs-agent").filter({ hasText: leader.name });
  await expect(failed.getByRole("alert")).toHaveText("Süreçleri okunamadı: ajanın süreç listesi (job) okunamadı");

  // Klavye: kapat düğmesi odakta; Tab ile en ağır ajana, Enter ile listesi açılır.
  await expect(panel.getByRole("button", { name: "Süreç panelini kapat" })).toBeFocused();
  await app.keyboard.press("Tab");
  await expect(heads.first()).toBeFocused();
  await app.keyboard.press("Enter");
  await expect(heads.first()).toHaveAttribute("aria-expanded", "true");
  const list = panel.getByRole("list", { name: `${worker.name} süreçleri` });
  await expect(list.getByRole("listitem")).toHaveCount(5);
  const toolBadge = list.locator(".kind-browser-tool");
  await expect(toolBadge).toHaveText("Tarayıcı aracı");
  await expect(list.locator(".kind-other")).toHaveText("Diğer");
  const browserBadge = list.locator(".kind-browser").first();
  expect(await toolBadge.evaluate(el => getComputedStyle(el).color)).not.toBe(await browserBadge.evaluate(el => getComputedStyle(el).color));
  await expect(heads.first().locator(".kind-browser")).toHaveText("1 tarayıcı");
  await expect(heads.first().locator(".kind-browser-tool")).toHaveText("1 tarayıcı aracı");
  await expect(list.getByRole("button", { name: /claude\.exe/ })).toHaveCount(0); // kök: düğme yok
  const kill = list.getByRole("button", { name: "chrome.exe (pid 201) sürecini sonlandır" });

  // Onay reddedilirse istek gitmez; kabulde ajan + pid ile gider.
  app.once("dialog", (d) => void d.dismiss());
  await kill.click();
  expect((await calls(app)).filter((c) => c.cmd === "kill_agent_process")).toHaveLength(0);
  app.once("dialog", (d) => {
    expect(d.message()).toContain("pid 201");
    void d.accept();
  });
  await kill.click();
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "kill_agent_process").map((c) => c.args))
    .toEqual([{ agent: worker.id, pid: 201 }]);
  await expect(heads.first()).toBeFocused(); // odak panelde kalır (satır süreçle gider)

  // Dar görünüm: yatay taşma yok.
  await app.setViewportSize({ width: 420, height: 720 });
  expect(await panel.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);

  // Aç/kapa: açıkken düğme paneli kapatır, tekrar basınca tek örnek olarak geri açılır.
  await button.click();
  await expect(stage.getByRole("tab", { name: "Süreçler", exact: true })).toHaveCount(0);
  await expect(button).toHaveAttribute("aria-pressed", "false");
  await button.focus();
  await app.keyboard.press("Enter");
  await expect(stage.getByRole("tab", { name: "Süreçler", exact: true })).toHaveCount(1);
  await expect(stage.getByRole("tab", { name: "Süreçler", exact: true })).toHaveAttribute("aria-selected", "true");
  // Panelin kapat düğmesi klavyeyle çalışır; kapalıyken yeni istek yok.
  await panel.getByRole("button", { name: "Süreç panelini kapat" }).focus();
  await app.keyboard.press("Enter");
  await expect(panel).toHaveCount(0);
  await expect(button).toBeFocused();
  const n = (await calls(app)).filter((c) => c.cmd === "agent_processes").length;
  await app.waitForTimeout(3500);
  expect((await calls(app)).filter((c) => c.cmd === "agent_processes")).toHaveLength(n);
});

// Süreçler Terminal grubunda, Görevler bağımsız çekmecede; gizli sekme yenilemez;
// yenilemede satırlar yer değiştirmez (sıra ilk cevaptaki).
test("Süreç paneli: Görevler bağımsız, gizli sekme yenilemez, yenilemede sıra sabit", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Süreç yuva ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  await app.addInitScript(([l, w]) => {
    window.__dexFake = {
      agent_processes: {
        [l]: { processes: [{ pid: 100, ppid: 1, name: "claude.exe", cmd: "claude", cpuPct: 5, memMb: 200, kind: "node", root: true }] },
        [w]: { processes: [{ pid: 200, ppid: 1, name: "claude.exe", cmd: "claude", cpuPct: 1, memMb: 150, kind: "node", root: true }] },
      },
    };
  }, [leader.id, worker.id]);
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  const bar = app.getByRole("group", { name: "Kart araçları" });
  const procsBtn = app.locator(".team-stage:not(.hidden)").getByRole("button", { name: "Süreçler", exact: true });
  const procs = app.getByRole("complementary", { name: "Süreçler" });
  const board = app.locator("aside.board");
  await bar.getByRole("button", { name: "Görevler" }).click();
  await expect(board).toBeVisible();
  await procsBtn.click();
  await expect(procs).toBeVisible();
  await expect(board).toBeVisible();
  await app.keyboard.press("Escape");
  await expect(board).toHaveCount(0);

  // Yük el değiştirir; sonraki yenilemede satırlar yerinde kalır, sayılar tazelenir.
  const heads = procs.locator(".procs-agent-head");
  await expect(heads.first()).toContainText(leader.name);
  await app.evaluate(([l, w]) => {
    const f = window.__dexFake as { agent_processes: Record<string, { processes: { cpuPct: number }[] }> };
    f.agent_processes[l].processes[0].cpuPct = 1;
    f.agent_processes[w].processes[0].cpuPct = 9;
  }, [leader.id, worker.id]);
  await expect(heads.nth(1)).toContainText("%9.0", { timeout: 8000 });
  await expect(heads.first()).toContainText(leader.name);

  // Görevler bağımsız çekmece olarak kalır; Esc yalnız onu kapatır.
  const boardBtn = bar.getByRole("button", { name: "Görevler" });
  await boardBtn.click();
  await expect(board).toBeVisible();
  await expect(procs).toBeVisible();
  await app.keyboard.press("Escape");
  await expect(board).toHaveCount(0);
  await expect(procs).toBeVisible();

  // Terminal sekmesi görünürken Süreçler yeni istek göndermez.
  const stage = app.locator(".team-stage:not(.hidden)");
  const procsTab = stage.getByRole("tab", { name: "Süreçler", exact: true });
  await stage.locator(".dv-tab").and(stage.getByRole("tab", { name: "Terminal", exact: true })).click();
  await expect(procs).toBeHidden();
  const n = (await calls(app)).filter(c => c.cmd === "agent_processes").length;
  await app.waitForTimeout(3500);
  expect((await calls(app)).filter(c => c.cmd === "agent_processes")).toHaveLength(n);
  await procsTab.click();
  await expect(procs).toBeVisible();
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "agent_processes").length).toBeGreaterThan(n);

  // Favori ve kayıtlı düzen sekmeyi Terminal grubunda tutar.
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Bu düzeni favori yap");
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await calls(app)).filter(c => c.cmd === "layout_save").at(-1)?.args.layout).toMatchObject({ favorite: { from: team.id } });
  await app.reload();
  await openTeam(app, team.name);
  await expect(procsTab).toHaveCount(1);
  const group = stage.locator(".dv-groupview").filter({ has: app.getByRole("tab", { name: "Süreçler", exact: true }) });
  await expect(group.getByRole("tab", { name: "Terminal", exact: true })).toHaveCount(1);
  await stage.getByRole("button", { name: "Süreçler panelini kapat", exact: true }).click();
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Süreç panelini aç");
  await app.keyboard.press("Enter");
  await expect(procsTab).toHaveCount(1);
  await expect(group.getByRole("tab", { name: "Terminal", exact: true })).toHaveCount(1);
});

test("Süreç paneli: Terminal yokken açılır, gizli takım istek atmaz, pane kimlikleri benzersiz", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Süreç kapsam 1");
  const other = await newTeam(dexd, "Süreç kapsam 2");
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker")!;
  await app.addInitScript((id) => {
    window.__dexFake = { layout_load: null, agent_processes: { [id]: { processes: [{ pid: 200, ppid: 1, name: "claude.exe", cmd: "claude", cpuPct: 1, memMb: 150, kind: "node", root: true }] } } };
  }, worker.id);
  await app.reload();
  await openTeam(app, team.name);
  const stage = app.locator(".team-stage:not(.hidden)");
  await stage.getByRole("button", { name: "Terminali kapat", exact: true }).click();
  // Terminal yok → başlık düğmesi yok; palet açar.
  await expect(stage.getByRole("button", { name: "Süreçler", exact: true })).toHaveCount(0);
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Süreç panelini aç");
  await app.keyboard.press("Enter");
  await expect(stage.getByRole("tab", { name: "Süreçler", exact: true })).toHaveCount(1);
  await expect(stage.locator(".procs-agent-head")).toHaveCount(1);
  await openTeam(app, other.name);
  const n = (await calls(app)).filter(c => c.cmd === "agent_processes").length;
  await app.waitForTimeout(3500);
  expect((await calls(app)).filter(c => c.cmd === "agent_processes")).toHaveLength(n);
  await app.keyboard.press("Control+Shift+P");
  await app.getByPlaceholder(/Komut ara/).fill("Süreç panelini aç");
  await app.keyboard.press("Enter");
  await expect(stage.locator(".procs-agent-head")).toHaveCount(1);
  const ids = await app.locator(".procs-agent-head").evaluateAll(els => els.map(el => el.id));
  expect(ids).toHaveLength(2);
  expect(new Set(ids).size).toBe(2);
});
