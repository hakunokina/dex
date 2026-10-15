// #411: Kart'ta uyanık bir worker'a tıklanınca çipten açılan paneye kırmızı devre izi gider (tek overlay,
// tıklamayı engellemez) ve kaybolur; panel zaten görünürse yalnız kenar vurgusu. Zamanlama test edilmez.
import type { Page } from "@playwright/test";
import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

/** The worker chip's centre on the Kart, found through its hover card. */
async function chipAt(app: Page, name: string) {
  const box = (await app.locator("canvas.office").boundingBox())!;
  const hover = app.locator(".office-card");
  for (let y = box.y + 8; y < box.y + box.height; y += 14) {
    for (let x = box.x + 8; x < box.x + box.width; x += 18) {
      await app.mouse.move(x, y);
      await app.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
      if ((await hover.isVisible()) && (await hover.locator("b").textContent()) === name) return { x, y };
    }
  }
  throw new Error(`${name} çipi bulunamadı`);
}

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Kart'tan worker paneli: devre izi overlay'i oluşur ve kaybolur; görünür panelde yalnız vurgu", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Devre ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");

  const fx = app.locator(".office-fx");
  const at = await chipAt(app, worker.name);

  // Panel açık ve görünür: iz yok, panelin kenarı kısa süre parlar.
  await app.mouse.click(at.x, at.y);
  await expect(fx.locator("rect.office-fx-edge")).toHaveCount(1);
  await expect(fx.locator("polyline")).toHaveCount(0);
  await expect(fx).toHaveCSS("pointer-events", "none");
  await expect(fx).toHaveCount(0);

  // Panel kapalı: tıklayınca açılır, çipten panele iz gider ve katman kendini kaldırır.
  await app.getByRole("button", { name: `${worker.name} sekmesini kapat` }).click();
  await expect(app.getByRole("region", { name: worker.name, exact: true })).toHaveCount(0);
  await app.mouse.click(at.x, at.y);
  await expect(fx.locator("polyline.office-fx-trace")).toHaveCount(1);
  await expect(fx.locator("polyline.office-fx-trace")).toHaveCSS("fill", "rgb(3, 4, 5)");
  await expect(app.getByRole("region", { name: worker.name, exact: true }).getByRole("tabpanel")).toBeVisible();
  await expect(fx).toHaveCount(0);
});

test("Simbiyot: dar Kart klavye ile worker açar; Core çizim bütçesi", async ({ app, dexd }) => {
  await app.setViewportSize({ width: 800, height: 700 });
  const team = await newTeam(dexd, `Simbiyot ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const worker = agents.find(a => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.getByRole("button", { name: `${worker.name} sekmesini kapat` }).click();
  const canvas = app.locator("canvas.office");
  await canvas.focus();
  await expect(canvas).toBeFocused();
  await app.keyboard.press("Home");
  expect(await canvas.evaluate(el => getComputedStyle(el).outlineStyle)).toBe("solid");
  for (let i = 0; i < agents.length; i++) {
    if ((await canvas.getAttribute("aria-label"))?.startsWith(`${worker.name}.`)) break;
    await app.keyboard.press("ArrowRight");
  }
  await expect(canvas).toHaveAttribute("aria-label", new RegExp(`^${worker.name}\\.`));
  await app.keyboard.press("Enter");
  await expect(app.getByRole("region", { name: worker.name, exact: true }).getByRole("tabpanel")).toBeVisible();
  await expect(app.locator(".office-fx")).toHaveCount(0);
  const contacts = await app.evaluate(async (teamId) => {
    const { World } = await import("/src/office/world.ts");
    const { membersOf } = await import("/src/team/store.ts");
    const { readOfficeTheme } = await import("/src/office/theme.ts");
    const { readPalette } = await import("/src/office/companion/coreEngine.ts");
    const agents = [...membersOf(teamId)];
    const leader = agents.find(a => a.kind === "leader")!;
    const worker = agents.find(a => a.kind === "worker")!;
    const otherLeader = { ...leader, id: `${leader.id}-other` };
    agents.push(otherLeader);
    let jobs: { id: number; fromAgent: string; toAgent: string; status: string }[] = [];
    const data = {
      status: () => "idle", delegations: () => jobs, active: j => j.status === "running",
      result: () => null, question: () => null, stalled: () => null, progress: () => null, slow: () => false,
      start: () => false, cost: () => "", proposals: () => [], escalated: () => 0,
      workerAsks: () => 0, steps: () => [], agentError: () => null,
    };
    const world = new World(agents, data);
    world.resize(480, 320);
    world.update(0.01);
    const surface = document.createElement("canvas");
    surface.width = 480; surface.height = 320;
    const ctx = surface.getContext("2d")!;
    const originalFill = ctx.fill.bind(ctx);
    const originalGradient = ctx.createRadialGradient.bind(ctx);
    let ribbons = 0;
    let pieces: { x: number; y: number }[] = [];
    ctx.createRadialGradient = (...args) => {
      if (args[5] < 20) pieces.push({ x: args[3], y: args[4] });
      return originalGradient(...args);
    };
    ctx.fill = (...args) => { if (ctx.fillStyle === "#030405") ribbons++; originalFill(...args); };
    const draw = () => {
      ribbons = 0;
      pieces = [];
      world.draw(ctx, null, readOfficeTheme(document.documentElement), readPalette());
      return ribbons;
    };
    const idle = draw();
    const idlePieces = pieces;
    world.activate(worker.id); world.update(0.3);
    const clicked = draw();
    world.update(1.5);
    const retracted = draw();
    const movedPieces = pieces;
    jobs = [{ id: 770, fromAgent: otherLeader.id, toAgent: worker.id, status: "running" }];
    world.update(0.3);
    world.update(0.3); // The event starts at reach=0; the next frame extends it.
    const delegated = draw();
    world.reduced = true;
    const reduced = draw();
    const reducedPieces = pieces.length;
    const historical = new World(agents, data);
    historical.update(0.3);
    ribbons = 0;
    historical.draw(ctx, null, readOfficeTheme(document.documentElement), readPalette());
    return { contacts: { idle, clicked, retracted, delegated, reduced, historical: ribbons }, idlePieces, movedPieces, reducedPieces };
  }, team.id);
  expect(contacts.contacts).toEqual({ idle: 0, clicked: 1, retracted: 0, delegated: 1, reduced: 0, historical: 0 });
  expect(contacts.idlePieces.length).toBeGreaterThan(0);
  expect(contacts.movedPieces).not.toEqual(contacts.idlePieces);
  expect(contacts.reducedPieces).toBe(0);
  const budget = await app.evaluate(async () => {
    const { drawCoreXL, drawCoreFragments, readPalette } = await import("/src/office/companion/coreEngine.ts");
    const { FragmentMotion, fragmentPaths } = await import("/src/office/symbiote.ts");
    const { LOOKS } = await import("/src/office/companion/coreState.ts");
    const surface = document.createElement("canvas");
    surface.width = surface.height = 320;
    const ctx = surface.getContext("2d")!;
    const palette = readPalette();
    const body = { x: 160, y: 160, s: 120 };
    const measure = (id: string, fragments: boolean) => {
      const samples = [];
      const motion = new FragmentMotion();
      for (let i = 0; i < 240; i++) {
        const start = performance.now();
        ctx.clearRect(0, 0, 320, 320);
        if (fragments) {
          motion.step(id, 0.016, false);
          const paths = fragmentPaths(body, [], [], { x: 0, y: 0, w: 320, h: 320 });
          drawCoreFragments(ctx, body, motion.sample(paths, body, false), motion.phase, palette, "#71777b");
        }
        drawCoreXL(ctx, body.x, body.y, body.s, LOOKS[id], {
          t: i * 16, age: i * 16, from: null, pupil: { x: 0, y: 0 },
          lid: 0, hover: 0, reduced: false, provider: null, boot: 0,
          boardBg: "#15191a", edge: "#71777b", mono: "monospace",
        }, palette);
        samples.push(performance.now() - start);
      }
      samples.sort((a, b) => a - b);
      return { mean: samples.reduce((a, b) => a + b, 0) / samples.length, p95: samples[Math.floor(samples.length * 0.95)] };
    };
    return Object.fromEntries(Object.keys(LOOKS).map(id => [id, { core: measure(id, false), combined: measure(id, true) }]));
  });
  console.log("SYMBIOTE_DRAW_MS", JSON.stringify(budget));
  // CPU draw submission budget; this deliberately excludes GPU raster/presentation time.
  for (const sample of Object.values(budget)) expect(sample.combined.mean).toBeLessThan(4);
});

test("prefers-reduced-motion: devre izi çizilmez", async ({ app, dexd }) => {
  await app.emulateMedia({ reducedMotion: "reduce" });
  const team = await newTeam(dexd, `Devre Sakin ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, worker.name);
  await pane.getByRole("button", { name: "Claude'u başlat" }).click();
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.getByRole("button", { name: `${worker.name} sekmesini kapat` }).click();
  const at = await chipAt(app, worker.name);
  await app.mouse.click(at.x, at.y);
  await expect(app.getByRole("region", { name: worker.name, exact: true }).getByRole("tabpanel")).toBeVisible();
  await expect(app.locator(".office-fx")).toHaveCount(0);
  await app.emulateMedia({ reducedMotion: null });
});
