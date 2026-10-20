import type { Locator, Page } from "@playwright/test";
import { test, expect, newTeam, openTeam, teamAgents } from "./fixtures";

type HeartbeatDot = Element & { heartbeatAnimations?: Animation[] };
/**
 * WAAPI runs on the document timeline, which app.clock does not drive: a 400 ms pulse is gone from
 * getAnimations() after 400 real ms, and one protocol round trip (a clock.runFor, a toHaveClass)
 * can take longer. Keep every pulse the dot ever started, so a test can count them and freeze one.
 */
const keepPulses = (app: Page) => app.addInitScript(() => {
  const animate = Element.prototype.animate;
  Element.prototype.animate = function (this: Element, keyframes, options) {
    const animation = animate.call(this, keyframes, options);
    if (this.classList.contains("dex-heartbeat")) ((this as HeartbeatDot).heartbeatAnimations ??= []).push(animation);
    return animation;
  };
});
/** How many times the dot pulsed so far (finished pulses included). */
const pulses = (dot: Locator) => dot.evaluate(el => (el as HeartbeatDot).heartbeatAnimations?.length ?? 0);

const emit = (app: Page, event: string, payload: unknown) => app.evaluate(([event, payload]) => {
  const socket = window.__dexSockets?.filter(s => s.readyState === WebSocket.OPEN).at(-1);
  if (!socket) throw new Error("açık soket yok");
  socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event, payload }) }));
}, [event, payload] as const);

test("offline ilk hook online olunca bir kez pıtlar", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Bekleyen nabız");
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker")!;
  await app.addInitScript(fake => { window.__dexFake = fake; }, {
    agent_status_list: [{ agentId: worker.id, status: "offline", sessionId: null, engine: null }],
    core_status: { capturedAtMs: Date.now(), agents: [] },
  });
  await keepPulses(app);
  await app.reload();
  await openTeam(app, team.name);
  const tab = app.locator(".team-stage:not(.hidden) .dex-tab").filter({ has: app.locator(".dex-tab-name", { hasText: worker.name }) });
  await tab.click(); // a quiet worker has no top-bar chip
  const dot = tab.locator(".dex-heartbeat");
  await expect(dot).toHaveClass(/offline/);
  await app.clock.install();
  await emit(app, "dex-hook", { agentId: worker.id, event: "SessionStart", status: null, sessionId: null });
  expect(await pulses(dot)).toBe(0);
  await emit(app, "agent-status", { agentId: worker.id, status: "idle" });
  // The tab reads the status on its own 400 ms poll (Rack.tsx useStatuses); the pulse must follow it.
  await expect(dot).toHaveClass(/idle/);
  await expect.poll(() => pulses(dot)).toBe(1);
  await dot.evaluate(el => (el as HeartbeatDot).heartbeatAnimations![0].finish());
  await app.clock.runFor(800);
  await expect.poll(() => dot.evaluate(el => el.getAnimations().length)).toBe(0);
  expect(await pulses(dot)).toBe(1);
});

test("online durum değişimi aynı nabzın 400 ms animasyonunu kesmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Kesilmeyen nabız");
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker")!;
  await app.addInitScript(fake => { window.__dexFake = fake; }, {
    agent_status_list: [{ agentId: worker.id, status: "idle", sessionId: null, engine: null }],
    core_status: { capturedAtMs: Date.now(), agents: [] },
  });
  await keepPulses(app);
  await app.reload();
  await openTeam(app, team.name);
  const tab = app.locator(".team-stage:not(.hidden) .dex-tab").filter({ has: app.locator(".dex-tab-name", { hasText: worker.name }) });
  await tab.click(); // a quiet worker has no top-bar chip
  const dot = tab.locator(".dex-heartbeat");
  await expect(dot).toHaveClass(/idle/);
  await app.clock.install();
  await emit(app, "dex-hook", { agentId: worker.id, event: "UserPromptSubmit", status: "working", sessionId: null });
  await expect.poll(() => pulses(dot)).toBe(1);
  // Freeze the real WAAPI animation mid-way before advancing JS timers (a pulse that already ran
  // out under load is rewound the same way): a status poll must neither cancel nor restart it.
  await dot.evaluate(el => {
    const animation = (el as HeartbeatDot).heartbeatAnimations![0];
    animation.pause();
    animation.currentTime = 100;
  });
  await app.clock.runFor(400);
  await expect(dot).toHaveClass(/working/);
  expect(await dot.evaluate(el => {
    const animation = (el as HeartbeatDot).heartbeatAnimations![0];
    return el.getAnimations()[0] === animation && animation.playState === "paused" && animation.currentTime === 100;
  })).toBe(true);
  await emit(app, "agent-status", { agentId: worker.id, status: "idle" });
  await app.clock.runFor(400);
  await expect(dot).toHaveClass(/idle/);
  expect(await dot.evaluate(el => el.getAnimations()[0]?.playState)).toBe("paused");
  expect(await dot.evaluate(el => el.getAnimations()[0]?.effect?.getTiming().duration)).toBe(400);
  await dot.evaluate(el => {
    const animation = el.getAnimations()[0];
    animation.currentTime = 399;
    animation.play();
  });
  await expect.poll(() => dot.evaluate(el => el.getAnimations().length)).toBe(0);
});

test("ajan sekmesi: hook nabzı, ortak sayaç, sessizlik, offline ve azaltılmış hareket", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Nabız");
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker")!;
  const at = Date.now();
  await app.addInitScript(fake => { window.__dexFake = fake; }, {
    agent_status_list: [{ agentId: worker.id, status: "working", sessionId: null, engine: null }],
    core_status: { capturedAtMs: at, agents: [{ agentId: worker.id, status: "working", lastHookAtMs: at - 720000 }] },
  });
  await keepPulses(app);
  await app.reload();
  await openTeam(app, team.name);
  await app.locator(`.rack-chip[data-agent="${worker.id}"]`).click();
  const tab = app.locator(".team-stage:not(.hidden) .dex-tab").filter({ has: app.locator(".dex-tab-name", { hasText: worker.name }) });
  const dot = tab.locator(".dex-heartbeat");
  const age = tab.locator(".dex-heartbeat-age");
  await expect(age).toHaveText("12dk");
  await expect(age).toHaveClass(/quiet/);
  expect(await pulses(dot)).toBe(0); // a restored age is not a heartbeat

  await app.clock.install();
  const hook = { agentId: worker.id, event: "PreToolUse", status: "working", sessionId: null };
  await emit(app, "dex-hook", hook);
  await expect(age).toHaveText("0s");
  await expect(age).not.toHaveClass(/quiet/);
  await expect.poll(() => pulses(dot)).toBe(1);
  await app.clock.fastForward(3000);
  await expect(age).toHaveText("3s");
  // Hooks without a status are still actions; repeated working status is not the trigger.
  await emit(app, "dex-hook", { ...hook, status: null, event: "PostToolUse" });
  await expect(age).toHaveText("0s");
  await app.clock.fastForward(121000);
  await expect(age).toHaveText("2dk");
  await expect(age).toHaveClass(/quiet/);
  await emit(app, "agent-status", { agentId: worker.id, status: "idle" });
  await app.clock.fastForward(1000);
  await expect(age).not.toHaveClass(/quiet/);

  await app.emulateMedia({ reducedMotion: "reduce" });
  await emit(app, "dex-hook", hook);
  await expect(age).toHaveText("0s");
  await expect.poll(() => pulses(dot)).toBe(3);
  expect(await dot.evaluate(el => (el as HeartbeatDot).heartbeatAnimations!.at(-1)!.effect?.getKeyframes().every(f => !f.transform))).toBe(true);
  await app.setViewportSize({ width: 720, height: 700 });
  await expect(age).toBeVisible();
  const close = tab.getByRole("button", { name: `${worker.name} sekmesini kapat` });
  await close.focus();
  await expect(close).toBeFocused();
  await app.keyboard.press("Tab");
  await expect(close).not.toBeFocused();

  await emit(app, "agent-status", { agentId: worker.id, status: "offline" });
  await app.clock.fastForward(1000);
  await expect(age).toHaveCount(0);
  await emit(app, "agent-status", { agentId: worker.id, status: "idle" });
  await app.clock.fastForward(1000);
  await expect(age).toHaveText("—");
});
