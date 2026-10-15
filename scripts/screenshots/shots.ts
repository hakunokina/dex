// README screenshots: one demo dexd (own temp data/home under .dex/tmp, the demo `claude` of
// demo-claude.rs), made-up team data, the desktop shell in Chromium (e2e/bridge.ts). Each shot is
// checked before it is written: no personal data in the page text (see `audit`), a non-empty render.
import { test as base, expect, type Page } from "@playwright/test";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { homedir, hostname, userInfo } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { installBridge } from "../../e2e/bridge";
import { paneOf } from "../../e2e/fixtures";
import { DEXD, REPO } from "../../e2e/paths";
import { DEMO_CLAUDE } from "./setup";

const OUT = join(REPO, "docs", "screenshots");

interface Demo {
  url: string;
  token: string;
  projects: string;
  home: string;
  call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T>;
}

function rpc(url: string, token: string | null, method: string, params: Record<string, unknown>) {
  return new Promise<unknown>((resolve, reject) => {
    const ws = new WebSocket(url);
    const send = (id: number, m: string, p: Record<string, unknown>) => ws.send(JSON.stringify({ id, method: m, params: p }));
    ws.onopen = () => (token ? send(1, "auth.hello", { protocol: 1, token }) : send(2, method, params));
    ws.onerror = () => reject(new Error(`dexd ${url} açılamadı`));
    ws.onmessage = (m) => {
      const r = JSON.parse(String(m.data));
      if (!("id" in r)) return;
      if (r.error) return ws.close(), reject(new Error(`${method}: ${r.error.message}`));
      if (r.id === 1) return send(2, method, params);
      ws.close();
      resolve(r.result);
    };
  });
}

async function startDemo(): Promise<{ demo: Demo; stop: () => Promise<void> }> {
  const scratch = join(REPO, ".dex", "tmp");
  mkdirSync(scratch, { recursive: true });
  const root = mkdtempSync(join(scratch, "dex-shots-"));
  const [data, home, projects] = ["data", "home", "projects"].map((d) => join(root, d));
  for (const d of [data, projects, join(home, ".claude-route"), join(home, "demo-screens")]) mkdirSync(d, { recursive: true });
  writeFileSync(join(home, ".claude-route", "routes.conf"), `real = ${DEMO_CLAUDE}\n`);
  const env = { ...process.env, USERPROFILE: home, HOME: home, GIT_CEILING_DIRECTORIES: root };
  const args = ["--data-dir", data];
  const child: ChildProcess = spawn(DEXD, ["serve", ...args, "--root", projects, "--bind", "127.0.0.1:0", "--secrets", "file"], { env });
  const exited = new Promise((r) => { child.once("exit", r); child.once("error", r); });
  child.stderr?.resume();
  const url = await new Promise<string>((resolve, reject) => {
    createInterface({ input: child.stdout! }).on("line", (l) => {
      const m = /^dexd: (ws:\/\/\S+)/.exec(l);
      if (m) resolve(m[1]);
    });
    child.once("exit", (code) => reject(new Error(`dexd çıktı (${code})`)));
  });
  const code = execFileSync(DEXD, ["pair", ...args], { env, encoding: "utf8" }).trim();
  const { token } = (await rpc(url, null, "auth.pair", { protocol: 1, code, name: "demo" })) as { token: string };
  const call = <T,>(m: string, p: Record<string, unknown> = {}) => rpc(url, token, m, p) as Promise<T>;
  const stop = async () => {
    const ptys = (await call<{ id: string; alive: boolean }[]>("pty.list").catch(() => [])) ?? [];
    await Promise.all(ptys.filter((p) => p.alive).map((p) => call("pty_kill", { id: p.id }).catch(() => {})));
    child.kill();
    await exited;
    rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  };
  return { demo: { url, token, projects, home, call }, stop };
}

const test = base.extend<{ app: Page }, { demo: Demo }>({
  demo: [
    // eslint-disable-next-line no-empty-pattern
    async ({}, use) => {
      const { demo, stop } = await startDemo();
      try {
        await use(demo);
      } finally {
        await stop();
      }
    },
    { scope: "worker" },
  ],
  app: async ({ page, demo }, use) => {
    await page.addInitScript(installBridge, { url: demo.url, token: demo.token, pick: null, localWs: true });
    await page.goto("/");
    await use(page);
    await page.close();
  },
});

// ---- personal-data audit ----
const forbidden = (demo: Demo) => {
  const user = userInfo().username;
  const words = [homedir(), homedir().replace(/\\/g, "/"), REPO, REPO.replace(/\\/g, "/"), demo.home, demo.projects, demo.token, "Users\\", "Users/"]
    .filter((w) => w && w.length >= 3);
  // The account and machine names as whole words ("…larda" is Turkish, not the user).
  const name = (w: string) => new RegExp(`(?<![\\p{L}\\p{N}])${w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\p{L}\\p{N}])`, "iu");
  return {
    words: [...new Set(words)],
    patterns: [name(user), name(hostname()), /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/, /\b[A-Za-z]:[\\/]/, /\/home\//, /sk-[A-Za-z0-9]{8,}/, /ws:\/\/127\.0\.0\.1/],
  };
};

/**
 * The text a viewer can read in the shot: text nodes and form values whose box is inside `clip` and on
 * top there (elementFromPoint: Dockview keeps hidden teams' stages laid out underneath).
 */
const pageText = (page: Page, clip?: { x: number; y: number; width: number; height: number }) =>
  page.evaluate((clip) => {
    const area = clip ?? { x: 0, y: 0, width: innerWidth, height: innerHeight };
    const out: string[] = [];
    const shown = (el: Element, r: DOMRect) => {
      if (r.width < 1 || r.height < 1) return false;
      if (r.right < area.x || r.bottom < area.y || r.left > area.x + area.width || r.top > area.y + area.height) return false;
      const x = Math.min(Math.max(r.left + r.width / 2, area.x + 1), area.x + area.width - 1);
      const y = Math.min(Math.max(r.top + r.height / 2, area.y + 1), area.y + area.height - 1);
      const top = document.elementFromPoint(x, y);
      return !!top && (top === el || el.contains(top) || top.contains(el));
    };
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      const t = n.textContent?.trim();
      const el = n.parentElement;
      if (!t || !el) continue;
      const range = document.createRange();
      range.selectNodeContents(n);
      if ([...range.getClientRects()].some((r) => shown(el, r))) out.push(t);
    }
    for (const el of document.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>("input, textarea")) {
      if (el.value && shown(el, el.getBoundingClientRect())) out.push(el.value);
    }
    return out.join("\n");
  }, clip);

export const audits: Record<string, unknown>[] = [];

async function shoot(page: Page, demo: Demo, name: string, clip?: { x: number; y: number; width: number; height: number }) {
  await page.evaluate(() => document.fonts.ready);
  const text = await pageText(page, clip);
  const { words, patterns } = forbidden(demo);
  const lower = text.toLowerCase();
  const hits = [
    ...words.filter((w) => lower.includes(w.toLowerCase())),
    ...patterns.flatMap((p) => (p.exec(text)?.[0] ? [`${p}: ${p.exec(text)![0]}`] : [])),
  ];
  if (hits.length) console.log(name, text.split("\n").filter((l) => /[A-Za-z]:[\\/]|Users/.test(l)));
  expect(hits, `${name}: kişisel veri`).toEqual([]);
  // No stray toast / alert / band in a README shot.
  const notices = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>(".toast, [role=alert], .host-banner, .model-news")].filter((e) => e.checkVisibility() && e.innerText.trim()).map((e) => e.innerText.slice(0, 80)));
  expect(notices, `${name}: bildirim`).toEqual([]);
  const png = await page.screenshot({ clip, animations: "allow" });
  // WebP (the Kart's texture does not compress as PNG): highest quality under the size limit, encoded in
  // a blank page; plus the non-empty check: brightness spread over a downscaled copy (a blank frame has none).
  const blank = await page.context().newPage();
  const out = await blank.evaluate(async ({ b64, limit }) => {
    const img = new Image();
    img.src = `data:image/png;base64,${b64}`;
    await img.decode();
    const full = document.createElement("canvas");
    full.width = img.width;
    full.height = img.height;
    full.getContext("2d")!.drawImage(img, 0, 0);
    let webp = "";
    let quality = 0.92;
    for (; quality >= 0.5; quality -= 0.04) {
      webp = full.toDataURL("image/webp", quality);
      if ((webp.length - 23) * 0.75 < limit) break;
    }
    const c = document.createElement("canvas");
    c.width = 160;
    c.height = 90;
    const g = c.getContext("2d")!;
    g.drawImage(img, 0, 0, 160, 90);
    const d = g.getImageData(0, 0, 160, 90).data;
    const lum: number[] = [];
    for (let i = 0; i < d.length; i += 4) lum.push(0.2126 * d[i] + 0.7152 * d[i + 1] + 0.0722 * d[i + 2]);
    const mean = lum.reduce((a, b) => a + b, 0) / lum.length;
    const sd = Math.sqrt(lum.reduce((a, b) => a + (b - mean) ** 2, 0) / lum.length);
    return { webp: webp.slice(webp.indexOf(",") + 1), quality, mean, sd, width: img.width, height: img.height };
  }, { b64: png.toString("base64"), limit: 400 * 1024 });
  await blank.close();
  const bytes = Buffer.from(out.webp, "base64");
  writeFileSync(join(OUT, `${name}.webp`), bytes);
  audits.push({ shot: name, ok: true, hits, bytes: bytes.length, quality: +out.quality.toFixed(2), size: `${out.width}x${out.height}`, mean: Math.round(out.mean), spread: Math.round(out.sd) });
  expect(out.sd, `${name}: boş görüntü`).toBeGreaterThan(8);
  expect(bytes.length, `${name}: boyut`).toBeLessThan(400 * 1024);
}

export { test, expect, shoot, OUT };
export type { Demo };

// ---- demo data ----
const C = { o: "\x1b[38;5;209m", g: "\x1b[38;5;114m", d: "\x1b[38;5;244m", b: "\x1b[1m", c: "\x1b[38;5;117m", y: "\x1b[38;5;221m", r: "\x1b[0m" };
const banner = (role: string, model: string) =>
  `${C.o}╭──────────────────────────────────────────────╮${C.r}\n${C.o}│${C.r} ${C.o}✻${C.r} ${C.b}Claude Code${C.r}  ${C.d}${model} · ${role}${C.r}\n${C.o}╰──────────────────────────────────────────────╯${C.r}\n\n`;
export const SCREENS: Record<string, string> = {
  Core:
    banner("Lead", "Opus 5.5") +
    `${C.d}>${C.r} Ödeme sayfasını yenile: tek adımlı checkout, kayıtlı kartlar, 3-D Secure.\n\n` +
    `${C.o}●${C.r} Planı üç işe böldüm ve devrettim:\n` +
    `  ${C.d}⎿${C.r}  ${C.c}Kernel-1${C.r}  /api/checkout uç noktası + kayıtlı kart tablosu (migration)\n` +
    `  ${C.d}⎿${C.r}  ${C.c}Pixel-1${C.r}   CheckoutForm: tek adım, kart seçici, hata durumları\n` +
    `  ${C.d}⎿${C.r}  ${C.c}Probe-1${C.r}   uçtan uca ödeme senaryoları + 3-D Secure regresyonu\n\n` +
    `${C.o}●${C.r} ${C.b}dex${C.r} - delegate ${C.d}(Kernel-1, #412)${C.r}  ${C.g}✓ başladı${C.r}\n` +
    `${C.o}●${C.r} ${C.b}dex${C.r} - delegate ${C.d}(Pixel-1, #413)${C.r}   ${C.g}✓ başladı${C.r}\n` +
    `${C.o}●${C.r} ${C.b}dex${C.r} - delegate ${C.d}(Probe-1, #414)${C.r}   ${C.g}✓ başladı${C.r}\n\n` +
    `${C.o}●${C.r} Scout-1 dünkü araştırmayı teslim etti: Stripe Payment Element\n  3-D Secure akışını kendi yönetiyor; özel modal gerekmiyor. Plana işledim.\n\n` +
    `${C.d}>${C.r} `,
  "Kernel-1":
    banner("Backend Engineer", "Sonnet 5.5") +
    `${C.o}●${C.r} ${C.b}Read${C.r}(src/routes/checkout.ts)\n  ${C.d}⎿  148 satır${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Write${C.r}(migrations/0042_saved_cards.sql)\n  ${C.d}⎿  ${C.g}+31${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Edit${C.r}(src/routes/checkout.ts)\n  ${C.d}⎿  ${C.g}+64${C.r} ${C.d}-22${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Bash${C.r}(cargo test -p api checkout)\n  ${C.d}⎿  test result: ${C.g}ok${C.d}. 18 passed; 0 failed${C.r}\n\n` +
    `${C.y}✶${C.r} Idempotency anahtarını ekliyor… ${C.d}(2m 14s · ↓ 3.1k token)${C.r}\n`,
  "Pixel-1":
    banner("Frontend Engineer", "Sonnet 5.5") +
    `${C.o}●${C.r} ${C.b}Read${C.r}(src/checkout/CheckoutForm.tsx)\n  ${C.d}⎿  212 satır${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Edit${C.r}(src/checkout/CheckoutForm.tsx)\n  ${C.d}⎿  ${C.g}+88${C.r} ${C.d}-41${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Write${C.r}(src/checkout/SavedCards.tsx)\n  ${C.d}⎿  ${C.g}+57${C.r}\n` +
    `${C.o}●${C.r} ${C.b}Bash${C.r}(npx tsc --noEmit)\n  ${C.d}⎿  ${C.g}temiz${C.r}\n\n` +
    `${C.y}✶${C.r} Kart seçicinin klavye erişimini bağlıyor… ${C.d}(1m 02s)${C.r}\n`,
  "Probe-1":
    banner("QA Engineer", "Sonnet 5.5") +
    `${C.o}●${C.r} Plan: mutlu yol · kayıtlı kart · 3-D Secure red · çift tıklama\n` +
    `${C.o}●${C.r} ${C.b}Bash${C.r}(npx playwright test e2e/checkout.spec.ts --workers=1)\n` +
    `  ${C.d}⎿${C.r}  ${C.g}✓${C.r} mutlu yol ${C.d}(3.2s)${C.r}\n  ${C.d}⎿${C.r}  ${C.g}✓${C.r} kayıtlı kartla öde ${C.d}(2.7s)${C.r}\n  ${C.d}⎿${C.r}  ${C.g}✓${C.r} 3-D Secure reddi ${C.d}(4.1s)${C.r}\n\n` +
    `${C.y}✶${C.r} Çift tıklama senaryosunu yazıyor… ${C.d}(48s)${C.r}\n`,
  "Scout-1":
    banner("Explorer", "Sonnet 5.5") +
    `${C.o}●${C.r} Rapor teslim edildi ${C.d}(#409 · Stripe Payment Element)${C.r}\n\n${C.d}>${C.r} `,
};
export const BUSY = ["Kernel-1", "Pixel-1", "Probe-1"];

interface Agent { id: string; name: string; kind: string; teamId: string; roleSlug?: string }

/** Team "Orbit" (lead + 4 workers + the Echo assistant), its board, jobs, progress and usage. */
async function seed(app: Page, demo: Demo) {
  const cwd = join(demo.projects, "orbit-api");
  mkdirSync(cwd, { recursive: true });
  const team = await demo.call<{ id: string; name: string }>("team_create", { team: { name: "Orbit", kind: "main", cwd } });
  await demo.call("agent_create", { agent: { teamId: team.id, name: "Echo-1", kind: "worker", model: "claude-sonnet-5-5", roleSlug: "assistant" } });
  const agents = (await demo.call<Agent[]>("agent_list")).filter((a) => a.teamId === team.id);
  const by = (n: string) => agents.find((a) => a.name === n)!;
  for (const a of agents) {
    writeFileSync(join(demo.home, "demo-screens", `${a.id}.ans`), SCREENS[a.name] ?? SCREENS["Scout-1"]);
    if (BUSY.includes(a.name)) writeFileSync(join(demo.home, "demo-screens", `${a.id}.busy`), "");
  }
  const task = (title: string, status: string, agent?: string) =>
    demo.call<{ id: number }>("task_create", { task: { title, teamId: team.id, status, agentId: agent ? by(agent).id : null } });
  const t1 = await task("Checkout API: /api/checkout + kayıtlı kartlar", "doing", "Kernel-1");
  const t2 = await task("CheckoutForm: tek adım, kart seçici", "doing", "Pixel-1");
  const t3 = await task("Ödeme e2e + 3-D Secure regresyonu", "doing", "Probe-1");
  const t4 = await task("Stripe Payment Element araştırması", "done", "Scout-1");
  await task("Fatura PDF'i: KDV satırı", "todo");
  await task("Sipariş e-postası şablonu", "todo");
  await task("Webhook imza doğrulaması", "todo");
  await task("Sepet terk bildirimi (A/B)", "todo");
  await task("Eski ödeme sayfasını kaldır", "done");
  // A task's status may be ignored on create: set it explicitly.
  for (const [t, status] of [[t1, "doing"], [t2, "doing"], [t3, "doing"], [t4, "done"]] as const) await demo.call("task_update", { id: t.id, patch: { status } }).catch(() => {});
  const now = Date.now();
  const leader = by("Core");
  const job = (id: number, to: string, taskId: number, status: string, prompt: string, extra: object = {}) => ({
    id, fromAgent: leader.id, toAgent: by(to).id, teamId: team.id, serviceTeamId: null, prompt, status, summary: null, resultPath: null,
    taskId, createdAt: now - 25 * 60_000, updatedAt: now - 60_000, ...extra,
  });
  const fake = {
    delegation_list: [
      job(412, "Kernel-1", t1.id, "running", "Checkout API"),
      job(413, "Pixel-1", t2.id, "running", "CheckoutForm"),
      job(414, "Probe-1", t3.id, "running", "Ödeme e2e"),
      job(409, "Scout-1", t4.id, "done", "Stripe araştırması", { summary: "bitti: Payment Element 3-D Secure'u kendi yönetiyor", readAt: now - 3_600_000, seenAt: now - 3_600_000 }),
    ],
    delegation_progress: [
      { agentId: by("Kernel-1").id, delegationId: 412, total: 8, done: 6, active: "Idempotency anahtarı", updatedAt: now - 40_000 },
      { agentId: by("Pixel-1").id, delegationId: 413, total: 6, done: 3, active: "Kart seçici erişimi", updatedAt: now - 70_000 },
      { agentId: by("Probe-1").id, delegationId: 414, total: 5, done: 3, active: "Çift tıklama senaryosu", updatedAt: now - 20_000 },
    ],
    usage_get: {
      todayTokens: 48_300_000, updatedAt: now,
      providers: [
        { id: "anthropic", fiveHour: { remainingPercent: 62, resetsAt: new Date(now + 2.4 * 3_600_000).toISOString() }, weekly: { remainingPercent: 71, resetsAt: new Date(now + 3.5 * 86_400_000).toISOString() }, note: null, measuredAt: now, todayTokens: 41_200_000 },
        { id: "glm", fiveHour: { remainingPercent: 94, resetsAt: null }, weekly: { remainingPercent: 88, resetsAt: null }, note: null, measuredAt: now, todayTokens: 7_100_000 },
      ],
    },
  };
  const total = (input: number, output: number, cacheRead: number, usd: number) => ({ tokens: { input, output, cacheRead, cacheWrite: Math.round(cacheRead / 9) }, usd });
  const perAgent: Record<string, ReturnType<typeof total>> = {
    [leader.id]: total(310_000, 92_000, 6_400_000, 7.84), [by("Kernel-1").id]: total(220_000, 61_000, 4_100_000, 2.91),
    [by("Pixel-1").id]: total(180_000, 54_000, 3_300_000, 2.37), [by("Probe-1").id]: total(140_000, 38_000, 2_600_000, 1.82),
    [by("Scout-1").id]: total(95_000, 21_000, 1_200_000, 0.96), [by("Echo-1").id]: total(12_000, 4_000, 90_000, 0.11),
  };
  const sum = Object.values(perAgent).reduce((a, b) => total(a.tokens.input + b.tokens.input, a.tokens.output + b.tokens.output, a.tokens.cacheRead + b.tokens.cacheRead, +(a.usd! + b.usd!).toFixed(2)));
  const ctx = (tokens: number) => ({ tokens, window: 1_000_000, model: "claude-sonnet-5-5", sessionId: null, updatedAt: now - 30_000 });
  Object.assign(fake, {
    model_news: [],
    chat_history: { hasMore: false, cursor: null, messages: CHAT.map((m, i) => ({ id: `m${i}`, seq: i + 1, ts: now - (CHAT.length - i) * 95_000, ...m })) },
    usage_summary: {
      costLabel: "API eşdeğer maliyeti", total: sum, agents: perAgent, warnings: [],
      delegations: { 412: perAgent[by("Kernel-1").id], 413: perAgent[by("Pixel-1").id], 414: perAgent[by("Probe-1").id], 409: perAgent[by("Scout-1").id] },
    },
    team_context: {
      [leader.id]: { ...ctx(184_000), model: "claude-opus-5-5" }, [by("Kernel-1").id]: ctx(96_000), [by("Pixel-1").id]: ctx(71_000),
      [by("Probe-1").id]: ctx(52_000), [by("Scout-1").id]: ctx(33_000), [by("Echo-1").id]: ctx(8_000),
    },
  });
  await app.addInitScript((f) => { window.__dexFake = { ...window.__dexFake, ...f }; }, fake);
  // The demo dexd measures real quota in the background: its `usage-snapshot` would replace the demo one.
  await app.addInitScript(() => {
    const Bridged = window.WebSocket;
    window.WebSocket = class extends Bridged {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        this.addEventListener("message", (ev) => {
          if (typeof ev.data === "string" && ev.data.includes('"usage-snapshot"') && ev.isTrusted) ev.stopImmediatePropagation();
        });
      }
    };
  });
  return { team, agents, by };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** A floating card / dialog wholly inside the window and on top at its centre (not cut, not covered). */
async function inView(page: Page, selector: string) {
  const box = await page.evaluate((q) => {
    const el = document.querySelector(q)!;
    const r = el.getBoundingClientRect();
    const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return { inside: r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight, onTop: !!top && el.contains(top) };
  }, selector);
  expect(box, selector).toEqual({ inside: true, onTop: true });
}

test("README ekran görüntüleri", async ({ app, demo }) => {
  mkdirSync(OUT, { recursive: true });
  const { agents } = await seed(app, demo);
  await app.reload();
  await app.getByRole("navigation", { name: "Takımlar" }).getByRole("button", { name: /^Orbit/ }).click();
  // Start every offline agent (the lead autostarts) with the demo `claude`.
  for (let i = 0; i < 8; i++) {
    const b = app.getByRole("button", { name: "Claude'u başlat" }).first();
    if (!(await b.isVisible().catch(() => false))) break;
    await b.click();
    await sleep(400);
  }
  // The team's plain shell panel shows its folder in the prompt: not in the shots.
  const shellTab = app.locator(".team-stage:not(.hidden) .dv-tab").filter({ hasText: /^Terminal$/ });
  await shellTab.hover();
  await shellTab.locator(".dv-default-tab-action, [aria-label*='apat']").first().click();
  await expect(shellTab).toHaveCount(0);
  await sleep(3000);
  const stage = app.locator(".team-stage:not(.hidden)");
  const quiet = () => app.mouse.move(800, 30);

  // 1. Workspace: Kart (A · Doku), the lead and the workers' panes, three of them working.
  await expect(app.locator(".dex-tab", { hasText: "Kernel-1" }).getByTestId("tab-progress")).toHaveText("%75");
  // Working for real (the demo `claude`'s UserPromptSubmit hook), the Kart lit by WebGL2 (not its 2D fallback).
  await expect.poll(async () => {
    const st = await demo.call<{ agentId: string; status: string }[]>("agent_status_list");
    return Object.fromEntries(agents.map((a) => [a.name, st.find((s) => s.agentId === a.id)?.status ?? "offline"]));
  }).toEqual({ Core: "idle", "Pixel-1": "working", "Kernel-1": "working", "Probe-1": "working", "Scout-1": "idle", "Echo-1": "idle" });
  await expect(stage.locator("canvas.office-lit")).not.toHaveCSS("visibility", "hidden");
  expect(agents.find((a) => a.name === "Echo-1")?.roleSlug, "Echo").toBe("assistant");
  await quiet();
  await sleep(1500);
  await shoot(app, demo, "01-workspace");

  // 2. Kart close-up: the panel maximized (tab menu → Büyüt).
  const kartTab = stage.locator(".dv-tab").filter({ hasText: /^Kart$/ });
  await kartTab.click({ button: "right" });
  await app.getByRole("menuitem", { name: "Büyüt" }).click();
  const canvas = stage.locator("canvas.office");
  await expect.poll(async () => (await canvas.boundingBox())?.height ?? 0).toBeGreaterThan(700);
  await quiet();
  await sleep(1500);
  await shoot(app, demo, "02-kart");
  await kartTab.click({ button: "right" });
  await app.getByRole("menuitem", { name: "Geri yükle" }).click();

  // 3. Tasks board with job progress.
  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Görevler" }).click();
  const board = app.getByRole("complementary").locator(".board-card", { hasText: "Checkout API" });
  await expect(board.getByTestId("job-progress")).toContainText("%75");
  await quiet();
  await sleep(600);
  await shoot(app, demo, "03-tasks-board");
  await app.getByRole("group", { name: "Kart araçları" }).getByRole("button", { name: "Görevler" }).click();
  await expect(app.getByRole("complementary").locator(".board-card")).toHaveCount(0);

  // 4. Usage strip and its hover card (the Kart in front again: the team panel shows folders).
  await kartTab.click();
  const strip = app.getByRole("complementary", { name: "Kullanım şeridi" });
  await expect(strip).toHaveAttribute("data-status", "live");
  await expect(strip.locator('[data-provider="anthropic"] .usage-pct[data-window="fiveHour"]')).toHaveText("62%");
  await strip.locator('[data-provider="anthropic"]').hover();
  await expect(app.locator('[data-card="anthropic"]')).toBeVisible();
  await inView(app, '[data-card="anthropic"]');
  await sleep(400);
  await shoot(app, demo, "04-usage-strip");
  await quiet();

  // 5. Settings → Sağlayıcılar and Kota zinciri.
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const sections = stage.getByRole("tablist", { name: "Ayar bölümleri" });
  await sections.getByRole("tab", { name: "Sağlayıcılar" }).click();
  await expect(stage.locator('[data-setting="provider-anthropic"]')).toBeVisible();
  await quiet();
  await sleep(500);
  await shoot(app, demo, "05-settings-providers");
  await sections.getByRole("tab", { name: "Kota zinciri" }).click();
  await sleep(500);
  await shoot(app, demo, "06-settings-chain");
  await app.getByRole("button", { name: "Ayarlar panelini kapat" }).click();
  await kartTab.click();

  // 6. Team note and the "Dex istemleri" overlay.
  await stage.locator(".dv-tab", { hasText: "Takım notu" }).click();
  const sys = stage.getByLabel("Lider sistem istemine ekle");
  await sys.fill("Planı küçük, geri alınabilir adımlara böl. Ödeme koduna dokunan her işe Probe'u ekle.");
  await stage.getByLabel("Her mesaja ekle").fill("Kısa yaz; kararı ve gerekçesini tek satırda ver.");
  await stage.getByLabel("Her mesaja ekle").blur();
  await quiet();
  await sleep(500);
  await shoot(app, demo, "07-team-note");
  await stage.getByRole("button", { name: "Dex istemleri" }).click();
  await expect(app.getByRole("dialog", { name: "Dex istemleri" })).toBeVisible();
  await inView(app, '[role="dialog"]');
  await sleep(500);
  await shoot(app, demo, "08-dex-prompts");
  await app.keyboard.press("Escape");

  // 7. Chat view of the lead's session.
  await stage.locator(".dv-tab", { hasText: "Core" }).click();
  const core = await paneOf(app, "Core");
  await core.getByRole("tab", { name: "Sohbet", exact: true }).click();
  await expect(core.locator(".chat-host")).toContainText("tek adımlı checkout");
  await quiet();
  await sleep(600);
  await shoot(app, demo, "09-chat");

  writeFileSync(join(REPO, ".dex", "tmp", "shots-audit.json"), JSON.stringify(audits, null, 1));
});

const CHAT = [
  { role: "user", text: "Ödeme sayfasını yenile: tek adımlı checkout, kayıtlı kartlar, 3-D Secure." },
  { role: "assistant", text: "Önce mevcut akışa bakıyorum; checkout şu an üç adım ve kart bilgisi her seferinde yeniden isteniyor." },
  { role: "assistant", text: "", tool: { name: "Read", input: "src/routes/checkout.ts", result: "148 satır", ok: true } },
  { role: "assistant", text: "", tool: { name: "Grep", input: "savedCard|paymentMethod", result: "6 eşleşme, 3 dosya", ok: true } },
  {
    role: "assistant",
    text: [
      "Plan üç iş:",
      "",
      "1. **Kernel-1** — `/api/checkout` uç noktası, kayıtlı kart tablosu (migration), idempotency anahtarı",
      "2. **Pixel-1** — `CheckoutForm`: tek adım, kart seçici, hata durumları",
      "3. **Probe-1** — uçtan uca senaryolar + 3-D Secure regresyonu",
      "",
      "Scout-1'in raporuna göre Payment Element 3-D Secure'u kendi yönetiyor; özel modal yazmıyoruz.",
    ].join("\n"),
  },
  { role: "assistant", text: "", tool: { name: "dex · delegate", input: "Kernel-1 · Checkout API (#412)", result: "başladı", ok: true } },
  { role: "assistant", text: "", tool: { name: "dex · delegate", input: "Pixel-1 · CheckoutForm (#413)", result: "başladı", ok: true } },
  { role: "assistant", text: "", tool: { name: "dex · delegate", input: "Probe-1 · Ödeme e2e (#414)", result: "başladı", ok: true } },
  { role: "user", text: "Kayıtlı kartları silme de olsun." },
  { role: "assistant", text: "Ekledim: Pixel-1'in işine kart silme (onaylı) ve Probe-1'in planına silme sonrası ödeme senaryosu. Kernel-1'in uç noktası `DELETE /api/cards/:id` ile genişledi." },
];
