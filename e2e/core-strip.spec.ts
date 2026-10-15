// Core lives in the office; leader panes retain only their session controls.
import type { Page } from "@playwright/test";
import { isolatedTest as test, expect, calls, newTeam, openTeam, paneOf, teamAgents, type Agent } from "./fixtures";

const emit = (app: Page, event: string, payload: unknown) =>
  app.evaluate(([event, payload]) => {
    const s = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
    if (!s) throw new Error("açık soket yok");
    s.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event, payload }) }));
  }, [event, payload] as const);

const AT = Date.now();
const proposal = (leader: Agent, id: number, status = "pending") => ({
  id, teamId: leader.teamId, teamName: "Core takımı", leader: leader.id, leaderName: leader.name, roleSlug: "qa", roleName: "QA",
  name: null, model: "claude-opus-5-5", provider: "anthropic", reason: "Checkout akışında regresyon yakalamak için.",
  status, agentId: null, createdAt: AT - 1000 * id,
});
const question = (leader: Agent, worker: Agent) => ({
  id: 12, delegationId: 236, delegationStatus: "running", worker: worker.id, workerName: worker.name, leader: leader.id,
  leaderName: leader.name, teamId: leader.teamId, teamName: "Core takımı", question: "Hangi sırayla ilerleyeyim?",
  options: ["Önce testler", "Önce arayüz", "Paralel"], context: null, status: "escalated", note: null, answer: null,
  answeredBy: null, leaderDeliveredAt: 1, answerDeliveredAt: null, createdAt: AT, updatedAt: AT,
});
const hideOffice = (app: Page) => app.addStyleTag({ content: ".office-stage { display: none !important; }" });

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Lider pane: Core şeridi yok; sekme durumu, oturum kontrolleri ve worker izleme korunur", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Core header ${test.info().workerIndex}`);
  // Other teams have the same agent names and remain mounted behind the active team's stage.
  await newTeam(dexd, `Other Core ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  await expect(pane.locator(".core-strip, .core-balloon, .pane-core")).toHaveCount(0);
  await expect(pane.getByRole("tab", { name: "Terminal", exact: true })).toBeVisible();
  await expect(pane.getByRole("tab", { name: "Sohbet", exact: true })).toBeVisible();
  await expect(pane.getByRole("button", { name: "Temiz oturum", exact: true })).toBeVisible();
  await expect(pane.locator(".ctx-meter")).toBeVisible();
  await expect.poll(async () => (await dexd.call<{ id: string }[]>("pty.list")).some((p) => p.id === leader.id)).toBe(true);
  await emit(app, "agent-status", { agentId: leader.id, status: "waiting", sessionId: null, engine: null });
  const tabs = app.locator(".team-stage:not(.hidden) .dex-tab");
  const tab = tabs.filter({ has: app.locator(".dex-tab-name", { hasText: leader.name }) });
  await expect(tab).toHaveAttribute("title", `${leader.name} · terminalde izin/girdi bekliyor`);
  await expect(tab.locator(".dex-heartbeat")).toHaveClass(/waiting/);
  await expect(tab.getByRole("button", { name: `${leader.name} sekmesini kapat`, exact: true })).toBeVisible();

  const wpane = await paneOf(app, worker.name);
  await expect(wpane.getByRole("button", { name: "İzleme", exact: true })).toBeVisible();
  await expect(wpane.getByRole("button", { name: "Temiz oturum", exact: true })).toHaveCount(0);
  await expect(wpane.locator(".core-strip")).toHaveCount(0);
  await emit(app, "agent-status", { agentId: worker.id, status: "waiting", sessionId: null, engine: null });
  await expect(tabs.filter({ has: app.locator(".dex-tab-name", { hasText: worker.name }) }))
    .toHaveAttribute("title", `${worker.name} · bekliyor`);

  await app.setViewportSize({ width: 720, height: 700 });
  await expect.poll(() => pane.locator(".pane-view-toggle").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await pane.getByRole("tab", { name: "Sohbet", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(pane.getByRole("tab", { name: "Sohbet", exact: true })).toHaveAttribute("aria-selected", "true");
  await expect(pane.locator(".chat-host")).toBeVisible();
  await pane.getByRole("tab", { name: "Terminal", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(pane.locator(".term-host")).toBeVisible();
  await pane.getByRole("button", { name: "Temiz oturum", exact: true }).focus();
  await expect(pane.getByRole("button", { name: "Temiz oturum", exact: true })).toBeFocused();
});

test("Ofis gizliyken kararlar köşe listesinde; lider pane'inde Core geri gelmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Köşe ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  // #2048: a leader's reassign of a job not started yet is a card of the same list.
  const move = { ...proposal(leader, 2), roleSlug: "", roleName: "", model: "", provider: "", change: "reassign",
    reason: "İş #7: Ödeme sayfası\nŞu an: Pixel-1\nİşlem: aktarma → Pixel-2\nGerekçe: Pixel-1 meşgul" };
  await app.addInitScript((f) => { window.__dexFake = f; }, { proposal_list: [proposal(leader, 1), move], question_list: [] });
  await app.reload();
  await hideOffice(app);
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  const corner = app.getByRole("complementary", { name: "Patron onayları" });
  await expect(pane.locator(".core-strip, .core-balloon, .pane-core")).toHaveCount(0);
  await expect(corner).toBeVisible();
  await expect(corner).toContainText("Checkout akışında regresyon");
  await corner.getByRole("button", { name: /Ekle/ }).click();
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "proposal_decide").map((c) => c.args)).toEqual([{ id: 1, approve: true, name: null }]);
  const card = corner.getByRole("alertdialog", { name: `${leader.name} iş aktarma onayı istiyor` });
  await expect(card.getByRole("heading")).toContainText("İş aktarma");
  await expect(card).toContainText("aktarma → Pixel-2");
  await expect(card.getByRole("textbox")).toHaveCount(0);
  await card.getByRole("button", { name: "Onayla", exact: true }).click();
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "proposal_decide").map((c) => c.args).at(-1)).toEqual({ id: 2, approve: true, name: null });
});

test("Ofis Core'u: karar kartları, klavye ve dar görünüm; gizlenince köşe listesi devralır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Core XL ${test.info().workerIndex}`);
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find((a) => a.kind === "leader")!;
  const worker = agents.find((a) => a.kind === "worker")!;
  await app.addInitScript((f) => { window.__dexFake = f; }, {
    proposal_list: [proposal(leader, 1), proposal(leader, 2)], proposal_decide: null,
    question_list: [question(leader, worker)], question_answer: null,
  });
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  const balloon = app.locator(".office-box .core-balloon");
  const canvas = app.locator("canvas.office");
  const corner = app.getByRole("complementary", { name: "Patron onayları" });
  await expect(balloon).toBeVisible();
  await expect(balloon.locator(".core-pager")).toHaveText("1/3");
  await expect(pane.locator(".core-strip, .core-balloon")).toHaveCount(0);
  await expect(corner).toHaveCount(0);
  await pane.locator(".term-host").click();
  await app.keyboard.type("re");
  expect((await calls(app)).filter((c) => c.cmd === "proposal_decide")).toHaveLength(0);
  await balloon.getByRole("button", { name: /Ekle/ }).focus();
  await app.keyboard.press("e");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "proposal_decide").map((c) => c.args)).toEqual([{ id: 2, approve: true, name: null }]);
  await app.evaluate((p) => { window.__dexFake!.proposal_list = p; }, [proposal(leader, 1), proposal(leader, 2, "approved")]);
  await emit(app, "proposals-changed", null);
  await expect(balloon.locator(".core-pager")).toHaveText("1/2");
  await balloon.getByRole("button", { name: /Reddet/ }).focus();
  await app.keyboard.press("R");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "proposal_decide").map((c) => c.args)).toEqual([
    { id: 2, approve: true, name: null }, { id: 1, approve: false, name: null },
  ]);
  await app.evaluate((p) => { window.__dexFake!.proposal_list = p; }, [proposal(leader, 1, "rejected"), proposal(leader, 2, "approved")]);
  await emit(app, "proposals-changed", null);
  await expect(balloon.getByRole("alertdialog")).toContainText("Hangi sırayla ilerleyeyim?");
  await balloon.getByRole("button", { name: /Önce testler/ }).focus();
  await app.keyboard.press("2");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "question_answer").map((c) => c.args)).toEqual([{ id: 12, text: "Önce arayüz" }]);
  await app.evaluate(() => { window.__dexFake!.question_list = []; });
  await emit(app, "questions-changed", null);
  await expect(balloon).toHaveCount(0);

  await app.setViewportSize({ width: 720, height: 700 });
  await app.evaluate((p) => { window.__dexFake!.proposal_list = p; }, [proposal(leader, 3)]);
  await emit(app, "proposals-changed", null);
  await expect(balloon).toBeVisible();
  await expect.poll(async () => {
    const [b, c] = [(await balloon.boundingBox())!, (await canvas.boundingBox())!];
    return b.x >= c.x - 1 && b.x + b.width <= c.x + c.width + 1 && b.y + b.height <= c.y + c.height / 2;
  }).toBe(true);
  const style = await hideOffice(app);
  await expect(corner).toBeVisible();
  await expect(balloon).toHaveCount(0);
  await expect(pane.locator(".core-balloon")).toHaveCount(0);
  await style.evaluate((el) => el.remove());
  await expect(balloon).toBeVisible();
  await expect(corner).toHaveCount(0);
  await balloon.getByRole("button", { name: /Ekle/ }).focus();
  await app.keyboard.press("Escape");
  await expect(balloon).toHaveCount(0);
  const badge = app.getByRole("button", { name: "1 bekleyen kararı aç" });
  await expect(badge).toBeVisible();
  await expect(badge).toHaveText("? 1");
  await expect(corner).toHaveCount(0);
  const [badgeBox, canvasBox] = [(await badge.boundingBox())!, (await canvas.boundingBox())!];
  expect(badgeBox.width).toBeGreaterThanOrEqual(24);
  expect(badgeBox.height).toBeGreaterThanOrEqual(24);
  expect(badgeBox.x).toBeGreaterThanOrEqual(canvasBox.x);
  expect(badgeBox.x + badgeBox.width).toBeLessThanOrEqual(canvasBox.x + canvasBox.width);
  await badge.click();
  await expect(balloon).toBeVisible();
  await expect(badge).toHaveCount(0);
  for (const key of ["Enter", "Space"]) {
    await balloon.getByRole("button", { name: /Ekle/ }).focus();
    await app.keyboard.press("Escape");
    await badge.focus();
    await expect(badge).toBeFocused();
    await app.keyboard.press(key);
    await expect(balloon).toBeVisible();
  }
  for (const key of ["Enter", "Space"]) {
    await balloon.getByRole("button", { name: /Ekle/ }).focus();
    await app.keyboard.press("Escape");
    await canvas.focus();
    await app.keyboard.press("Home");
    await app.keyboard.press(key);
    await expect(balloon).toBeVisible();
    await expect(badge).toHaveCount(0);
  }
  await balloon.getByRole("button", { name: /Ekle/ }).focus();
  await app.keyboard.press("Escape");
  const deferredStyle = await hideOffice(app);
  await expect(corner).toBeVisible();
  await expect(badge).toHaveCount(0);
  await deferredStyle.evaluate((el) => el.remove());
  await expect(badge).toBeVisible();
  await expect(corner).toHaveCount(0);
  await badge.click();
  await expect(balloon).toBeVisible();
  await balloon.getByRole("button", { name: /Ekle/ }).click();
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "proposal_decide").at(-1)?.args)
    .toEqual({ id: 3, approve: true, name: null });
  await app.evaluate(() => { window.__dexFake!.proposal_list = []; });
  await emit(app, "proposals-changed", null);
  await expect(balloon).toHaveCount(0);
  await expect(badge).toHaveCount(0);
});
