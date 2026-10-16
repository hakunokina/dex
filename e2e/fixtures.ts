// Per worker: one isolated dexd (own temp data dir, own HOME so `claude` resolves to the fake one,
// own --root), paired automatically. Per test: a page whose "local core" is that dexd (bridge.ts).
// isolatedTest gives each test its own core too, so teams/layout cannot leak from earlier specs.
import { test as base, expect, type Page, type TestInfo } from "@playwright/test";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { installBridge, type RecordedCall } from "./bridge";
import { DEXD, FAKE_CLAUDE, REPO } from "./paths";

export interface Dexd {
  url: string;
  token: string;
  /** The dexd `--root`: team folders must be inside it. */
  projects: string;
  /** Its `--data-dir` (the core's own files: `dex.live.json`, …). */
  data: string;
  /**
   * Stop the process and start a fresh core with the same data, endpoint and pairing. `clean`: as
   * after a clean exit (no live snapshot left); `crash`: as after a crash — the new core resumes
   * the agents its live snapshot names (#1875).
   */
  restart(how?: "clean" | "crash"): Promise<void>;
  /** A core method straight over the protocol (seeding, backend-side checks). */
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

async function startDexd(): Promise<{ dexd: Dexd; stop: () => Promise<void> }> {
  const scratch = join(REPO, ".dex", "tmp");
  mkdirSync(scratch, { recursive: true });
  const root = mkdtempSync(join(scratch, "dex-e2e-"));
  const [data, home, projects] = ["data", "home", "projects"].map((d) => join(root, d));
  for (const d of [data, projects, join(home, ".claude-route")]) mkdirSync(d, { recursive: true });
  // providers::resolve_claude: `real =` in the root section of routes.conf wins.
  writeFileSync(join(home, ".claude-route", "routes.conf"), `real = ${FAKE_CLAUDE}\n`);
  // Scratch lives inside the checkout: test folders must not discover its parent .git.
  const env = { ...process.env, USERPROFILE: home, HOME: home, GIT_CEILING_DIRECTORIES: root };
  const args = ["--data-dir", data];
  let child: ChildProcess;
  let exited: Promise<unknown>;
  // Agent processes live in dexd's job object and go with it; the DB may stay locked a moment.
  // Shells are outside that job: one left running holds its team folder and the rmSync fails (EPERM).
  // #65: every new team opens with a shell, so kill whatever is still running first.
  let paired: { url: string; token: string } | null = null;
  const stopProcess = async (shellsOnly = false) => {
    if (paired) {
      const { url, token } = paired;
      const ptys = (await rpc(url, token, "pty.list", {}).catch(() => [])) as { id: string; alive: boolean; kind: string }[];
      await Promise.all(ptys.filter((p) => p.alive && (!shellsOnly || p.kind === "shell")).map((p) => rpc(url, token, "pty_kill", { id: p.id }).catch(() => {})));
    }
    child.kill();
    await exited;
  };
  const stop = async () => {
    await stopProcess();
    rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  };
  const launch = (bind: string) => {
    child = spawn(DEXD, ["serve", ...args, "--root", projects, "--bind", bind, "--secrets", "file"], { env });
    exited = new Promise((r) => { child.once("exit", r); child.once("error", r); });
    child.stderr?.resume();
    return new Promise<string>((resolve, reject) => {
      const lines = createInterface({ input: child.stdout! });
      lines.on("line", (l) => {
        const m = /^dexd: (ws:\/\/\S+)/.exec(l);
        if (m) resolve(m[1]);
      });
      child.once("error", reject);
      child.once("exit", (code) => reject(new Error(`dexd çıktı (${code})`)));
    });
  };
  const url = await launch("127.0.0.1:0").catch(async (e) => {
    await stop();
    throw e;
  });
  const code = execFileSync(DEXD, ["pair", ...args], { env, encoding: "utf8" }).trim();
  const { token } = (await rpc(url, null, "auth.pair", { protocol: 1, code, name: "e2e" })) as { token: string };
  paired = { url, token };
  const restart = async (how: "clean" | "crash" = "clean") => {
    // Shells are outside the core's job; agents must die with the actual process, not pty_kill.
    await stopProcess(true);
    // `dexd serve` exits cleanly on Ctrl+C only, which cannot be sent to a child here: the kill is a
    // crash to the core. What a clean exit leaves different (Core::shutdown) is no live snapshot.
    if (how === "clean") rmSync(join(data, "dex.live.json"), { force: true });
    const restartedUrl = await launch(new URL(url).host);
    if (restartedUrl !== url) throw new Error(`dexd restart endpoint changed: ${restartedUrl}`);
    // The persisted pairing still authenticates to the new process; no browser reload is needed.
    await rpc(url, token, "server_info", {});
  };
  return { dexd: { url, token, projects, data, restart, call: (m, p = {}) => rpc(url, token, m, p) as Promise<never> }, stop };
}

// eslint-disable-next-line no-empty-pattern
async function dexdFixture({}: object, use: (dexd: Dexd) => Promise<void>) {
  const { dexd, stop } = await startDexd();
  try {
    await use(dexd);
  } finally {
    await stop();
  }
}

async function appFixture({ page, dexd }: { page: Page; dexd: Dexd }, use: (page: Page) => Promise<void>, info: TestInfo) {
  // What the native folder picker "returns" in this test (TeamBar → Klasör seç…).
  const pick = join(dexd.projects, `pick-${info.workerIndex}-${Date.now()}`);
  mkdirSync(pick, { recursive: true });
  await page.addInitScript(installBridge, { url: dexd.url, token: dexd.token, pick, localWs: process.env.DEX_LOCAL_WS !== "0" });
  await page.goto("/");
  try {
    await use(page);
  } finally {
    // Stop UI requests before the test-scoped core kills PTYs and removes their folders.
    await page.close();
  }
}

export const test = base.extend<{ app: Page }, { dexd: Dexd }>({
  dexd: [dexdFixture, { scope: "worker" }],
  app: appFixture,
});

/** For specs that require a fresh team list and layout, independent of earlier worker tests. */
export const isolatedTest = base.extend<{ app: Page; dexd: Dexd }>({
  dexd: dexdFixture,
  app: appFixture,
});

export { expect };

export const calls = (page: Page) => page.evaluate(() => window.__dexCalls as RecordedCall[]);

/** #2145: answers askConfirm's in-app dialog (destructive: opens on İptal) and returns its text. */
export async function answerConfirm(page: Page, ok: boolean): Promise<string> {
  const dialog = page.getByRole("alertdialog");
  await expect(dialog.getByRole("button", { name: "İptal", exact: true })).toBeFocused();
  const text = await dialog.innerText();
  await dialog.getByRole("button", { name: ok ? "Tamam" : "İptal", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  return text;
}

/** Selects a team by its tab in the top bar (its name may carry a status badge: "Oto 0 1", #27). */
export const openTeam = (page: Page, name: string) =>
  page
    .getByRole("navigation", { name: "Takımlar" })
    .getByRole("button", { name: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}( \\d+)?$`) })
    .click();

export interface Agent {
  id: string;
  name: string;
  kind: "leader" | "worker";
  teamId: string;
}
export const teamAgents = async (dexd: Dexd, teamId: string) =>
  (await dexd.call<Agent[]>("agent_list")).filter((a) => a.teamId === teamId);

let seq = 0;
/** A project team straight in the core (1 lead + 4 workers, as the UI makes it); reload to see it. */
export async function newTeam(dexd: Dexd, name: string) {
  const cwd = join(dexd.projects, `${name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}-${process.pid}-${++seq}`);
  mkdirSync(cwd, { recursive: true });
  return dexd.call<{ id: string; name: string }>("team_create", { team: { name, kind: "main", cwd } });
}

/**
 * An agent's terminal pane. Dockview draws panel bodies in an overlay outside their group, so the
 * pane is the one on top at the middle of the group titled with the agent's name (other teams'
 * stages stay laid out underneath); it gets a test-only marker.
 */
export async function paneOf(page: Page, agentName: string) {
  const panel = page.getByRole("region", { name: agentName, exact: true }).getByRole("tabpanel");
  await expect(panel).toBeVisible();
  const marker = `pane-${agentName}`;
  await expect
    .poll(() =>
      panel.evaluate((el, m) => {
        const r = el.getBoundingClientRect();
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2)?.closest("section.pane");
        for (const p of document.querySelectorAll(`section.pane[data-e2e="${m}"]`)) if (p !== hit) p.removeAttribute("data-e2e");
        hit?.setAttribute("data-e2e", m);
        return !!hit;
      }, marker),
    )
    .toBe(true);
  return page.locator(`section.pane[data-e2e="${marker}"]`);
}
