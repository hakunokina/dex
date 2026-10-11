// Hafıza haritası paneli: Kart araç çubuğundan açılır; tohumlanmış gerçek veriyle grafik,
// ajan süzgeci, arama, seçim → detay, [[slug]] ile gezinme; boş takım ve eski dexd durumları.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Locator, Page } from "@playwright/test";
import type { OldServer } from "./bridge";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents, type Dexd } from "./fixtures";

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

type Agent = { id: string; slug?: string | null };
type Context = { teamMemoryRoot: string; agentMemoryRoot: string; projectMemoryRoot: string };

/**
 * #838 proje katmanı kökleri (`project_context`): takım ortak `team/<tid>/shared/`, ajan özel
 * `private/<tid>/agents/<slug>/` (app-data), proje ortak `project/<pid>/shared/`. Çıplak [[slug]] yalnız
 * kendi dizininde çözülür; kapsamlar arası bağ nitelikli id ister. MEMORY.md düğüm değildir.
 */
async function seed(dexd: Dexd, teamId: string, projectId: string, worker: Agent, leader: Agent) {
  const ctx = await dexd.call<Context>("project_context", { teamId, projectId });
  const put = (root: string, rel: string, text: string) => {
    mkdirSync(join(root, rel, ".."), { recursive: true });
    writeFileSync(join(root, rel), text);
  };
  const team = (stem: string) => `team/${teamId}/shared/${stem}`;
  put(ctx.teamMemoryRoot, "MEMORY.md", "- indeks, düğüm değil\n");
  put(ctx.teamMemoryRoot, "shared/kurulum.md", "---\nname: Kurulum\ndescription: Sunucu kurulum notları\n---\n\nKurulum adımları burada.\n");
  put(ctx.teamMemoryRoot, "shared/dagitim.md", "---\nname: Dağıtım\nmetadata:\n  type: kural\n---\n\nÖnce [[kurulum]], sonra dağıt.\n");
  put(ctx.agentMemoryRoot, `agents/${worker.slug ?? worker.id}/acik-isler.md`, `---\nname: Açık işler\ndescription: Worker açık işleri\n---\n\n[[${team("dagitim")}]] ve [[yok-boyle]] bekliyor.\n`);
  put(ctx.agentMemoryRoot, `agents/${leader.slug ?? leader.id}/lider-notu.md`, `---\nname: Lider notu\n---\n\nBkz. [[${team("kurulum")}]].\n`);
  put(ctx.projectMemoryRoot, "shared/proje-notu.md", "---\nname: Proje notu\n---\n\nYalnız proje bağlamında.\n");
}

const openMap = async (app: Page, teamName: string, leaderName: string) => {
  await openTeam(app, teamName);
  await paneOf(app, leaderName); // the team's dock is bound
  await app.getByRole("button", { name: "Hafıza haritası", exact: true }).click();
};

/**
 * Yüzen Hafıza panelini kapat ve debounce'lu layout_save'in onu düşürmesini bekle: kayıtlı açık panel
 * sonraki testin reload'unda geri gelir (worker'ın dexd'si ortak) ve onun durum metnini ikiler.
 */
const closeMap = async (app: Page, teamId: string) => {
  const saved = async () => {
    const save = (await calls(app)).filter((c) => c.cmd === "layout_save").at(-1);
    const teams = (save?.args.layout as { teams?: Record<string, { dock: unknown }> } | undefined)?.teams;
    const dock = teams?.[teamId]?.dock as { panels?: Record<string, unknown> } | null | undefined;
    return dock?.panels ? "memory" in dock.panels : "no save";
  };
  await expect.poll(saved).toBe(true);
  await app.getByRole("button", { name: "Hafıza panelini kapat" }).click();
  await expect.poll(saved).toBe(false);
};

/** Sonraki yüklemeden itibaren eski dexd taklidi (bridge.ts `OldServer`); DEX_LOCAL_WS açık/kapalı aynı. */
const setOldServer = (app: Page, old: OldServer) =>
  app.addInitScript((o) => {
    window.__dexOld = o;
  }, old);

/** Düğümün (circle) saydamlığı üst <g>'de: süzgeç/seçim dışı 0.12. */
const opacityOf = (panel: Locator, name: string) =>
  panel.getByRole("button", { name: new RegExp(`^${name}(, \\d+ bağ)?$`) }).locator("..").getAttribute("opacity");

test("hafıza paneli: sayılar, ajan süzgeci, arama, detay, slug gezinmesi", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Hafıza ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const worker = agents.find((a) => a.kind === "worker")!;
  const leader = agents.find((a) => a.kind === "leader")!;
  const project = (await dexd.call<{ id: string }[]>("project_list", { teamId: team.id }))[0];
  await seed(dexd, team.id, project.id, worker, leader);
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  const panel = app.locator(".mem-map");
  await expect(panel).toBeVisible();
  // Bağlam önce takımın varsayılan projesi: takım + özel + proje ortak hafızası.
  const scope = app.getByRole("combobox", { name: "Hafıza bağlamı" });
  await expect(scope).toHaveValue(project.id);
  await expect(panel.getByText("6 hafıza · 4 bağ", { exact: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Proje notu", exact: true })).toBeVisible();
  // Açık seçim: projesiz takım bağlamı proje hafızasını göstermez.
  await scope.selectOption("");
  await expect(scope).toHaveValue("");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "team_memory_graph" && c.args.teamId === team.id).length).toBeGreaterThan(0);
  // 4 dosya (MEMORY.md hariç) + çözülemeyen [[yok-boyle]] = 5 düğüm;
  // dagitim→kurulum, acik-isler→dagitim, acik-isler→yok-boyle, lider-notu→kurulum = 4 bağ.
  await expect(panel.getByText("5 hafıza · 4 bağ", { exact: true })).toBeVisible();
  await expect(panel.locator(".mem-node")).toHaveCount(5);
  await expect(panel.locator(".mem-edge")).toHaveCount(4);
  await expect(panel.getByRole("button", { name: "Proje notu", exact: true })).toHaveCount(0);
  const all = ["Kurulum", "Dağıtım", "Açık işler", "Lider notu", "yok-boyle"];
  for (const n of all) expect(await opacityOf(panel, n)).toBe("1");

  // Ajan süzgeci: yalnız worker'ın hafızası tam görünür, diğerleri solar; ikinci tık kaldırır.
  const filter = panel.getByRole("group", { name: "Ajan süzgeci" });
  const workerBtn = filter.getByRole("button", { name: `${worker.name} 1`, exact: true });
  await expect(filter.getByRole("button", { name: `${leader.name} 1`, exact: true })).toBeVisible();
  await workerBtn.click();
  await expect.poll(() => opacityOf(panel, "Açık işler")).toBe("1");
  for (const n of all.filter((x) => x !== "Açık işler")) expect(await opacityOf(panel, n)).toBe("0.12");
  await workerBtn.click();
  for (const n of all) await expect.poll(() => opacityOf(panel, n)).toBe("1");

  // Arama ad + description'da: "notları" yalnız Kurulum'un açıklamasında.
  const search = panel.getByRole("searchbox", { name: "Hafızalarda ara" });
  await search.fill("notları");
  await expect.poll(() => opacityOf(panel, "Kurulum")).toBe("1");
  for (const n of all.filter((x) => x !== "Kurulum")) expect(await opacityOf(panel, n)).toBe("0.12");
  await search.fill("zzz-hicbirsey-zzz");
  for (const n of all) await expect.poll(() => opacityOf(panel, n)).toBe("0.12");
  await search.fill("");

  // Klavyeyle Dağıtım'ı seç: başlık, tip rozeti, gövde, giden/gelen bağlar.
  const detail = panel.getByLabel("Hafıza detayı");
  await panel.getByRole("button", { name: "Dağıtım, 2 bağ", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(detail.locator("h2")).toHaveText("Dağıtım");
  await expect(detail.locator(".mem-badge")).toHaveText(["Takım", "kural"]);
  await expect(detail.locator(".mem-body-md")).toContainText("sonra dağıt.");
  const links = detail.locator(".mem-links");
  await expect(links.getByRole("heading", { name: "Bağlı hafızalar" })).toBeVisible();
  await expect(links.locator("div", { has: app.getByRole("heading", { name: "Giden" }) }).getByRole("button")).toHaveText(["Kurulum"]);
  await expect(links.locator("div", { has: app.getByRole("heading", { name: "Gelen" }) }).getByRole("button")).toHaveText(["Açık işler"]);
  // Seçimin komşusu olmayan düğüm solar.
  expect(await opacityOf(panel, "Lider notu")).toBe("0.12");

  // Gövdedeki [[kurulum]] → Kurulum (sunucunun çözdüğü kenardan; id "team/<tid>/shared/kurulum", ad "Kurulum").
  await detail.locator(".mem-body-md").getByRole("button", { name: "kurulum", exact: true }).click();
  await expect(detail.locator("h2")).toHaveText("Kurulum");
  await expect(detail.locator(".mem-body-md")).toContainText("Kurulum adımları burada.");
  // Sıra sunucunun id sırası (sözleşme değil): küme olarak bak.
  const incoming = links.locator("div", { has: app.getByRole("heading", { name: "Gelen" }) }).getByRole("button");
  await expect.poll(async () => (await incoming.allTextContents()).sort()).toEqual(["Dağıtım", "Lider notu"]);
  // Kapsamlar arası nitelikli bağ (özel → takım) da gövdeden gezilir.
  await incoming.filter({ hasText: "Lider notu" }).click();
  await expect(detail.locator("h2")).toHaveText("Lider notu");
  await detail.locator(".mem-body-md").getByRole("button", { name: `team/${team.id}/shared/kurulum`, exact: true }).click();
  await expect(detail.locator("h2")).toHaveText("Kurulum");

  // Bağlı hafızalar listesinden geç: Açık işler (ajan rozeti) → kopuk bağ.
  await panel.getByRole("button", { name: "Açık işler, 2 bağ", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(detail.locator(".mem-badge")).toHaveText(["Özel", worker.name]);
  await links.getByRole("button", { name: "yok-boyle", exact: true }).click();
  await expect(detail.locator("h2")).toHaveText("yok-boyle");
  await expect(detail.getByText("Bu hafıza dosyası bulunamadı (bağlantı kopuk).")).toBeVisible();
  await closeMap(app, team.id);
});

test("hafıza paneli: hafızasız takım boş durum", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Bos Hafiza ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  await expect(app.getByText("Henüz hafıza yok.")).toBeVisible();
  await expect(app.getByRole("button", { name: "Yenile", exact: true })).toBeVisible();
  await closeMap(app, team.id);
});

const UNSUPPORTED = "Bu sunucu hafıza haritasını desteklemiyor.";
const PROJECT_MEMORY = ["team_memory_graph", "project_memory_graph"];

// #142: sunucu metot listesini veriyor ve hafıza metodu listede yok → hiç çağrılmadan aynı not.
// Proje katmanlı host (`project_list` var) takım/proje hafıza metotlarını, eski host `memory_*`'ı kullanır.
test("hafıza paneli: metot listesinde yoksa çağırmadan anlaşılır not (proje katmanlı host)", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Listesiz Proje Hafiza ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await setOldServer(app, { hide: [...PROJECT_MEMORY, "team_memory_read", "project_memory_read"] });
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  const scope = app.getByRole("combobox", { name: "Hafıza bağlamı" });
  await expect(scope).not.toHaveValue("");
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await scope.selectOption("");
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await expect(app.locator(".mem-map")).toHaveCount(0);
  expect((await calls(app)).filter((c) => [...PROJECT_MEMORY, "memory_graph"].includes(c.cmd))).toHaveLength(0);
  await closeMap(app, team.id);
});

test("hafıza paneli: metot listesinde yoksa çağırmadan anlaşılır not (eski host)", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Listesiz Hafiza ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await setOldServer(app, { hide: ["project_list", "memory_graph", "memory_read"] });
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await expect(app.getByRole("combobox", { name: "Hafıza bağlamı" })).toHaveCount(0);
  await expect(app.locator(".mem-map")).toHaveCount(0);
  expect((await calls(app)).filter((c) => c.cmd === "memory_graph")).toHaveLength(0);
  await closeMap(app, team.id);
});

test("hafıza paneli: bilinmeyen takım/proje hafıza metodu anlaşılır not (proje katmanlı host)", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Eski Proje Hafiza ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await setOldServer(app, { unknown: PROJECT_MEMORY });
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  const scope = app.getByRole("combobox", { name: "Hafıza bağlamı" });
  const called = (m: string) => async () => (await calls(app)).filter((c) => c.cmd === m && c.args.teamId === team.id).length;
  await expect.poll(called("project_memory_graph")).toBeGreaterThan(0);
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await scope.selectOption("");
  await expect.poll(called("team_memory_graph")).toBeGreaterThan(0);
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await expect(app.locator(".mem-map")).toHaveCount(0);
  await closeMap(app, team.id);
});

test("hafıza paneli: eski dexd (bilinmeyen metot) anlaşılır not", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Eski Hafiza ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await setOldServer(app, { unknown: ["project_list", "memory_graph"] });
  await app.setViewportSize({ width: 1600, height: 900 });
  await app.reload();
  await openMap(app, team.name, leader.name);
  await expect(app.getByText(UNSUPPORTED)).toBeVisible();
  await expect(app.getByRole("combobox", { name: "Hafıza bağlamı" })).toHaveCount(0);
  await expect(app.locator(".mem-map")).toHaveCount(0);
  await closeMap(app, team.id);
});
