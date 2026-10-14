import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { test, expect, newTeam, openTeam, teamAgents, type Dexd } from "./fixtures";
import { DEXD, VITE_PORT } from "./paths";

const pairCode = (dexd: Dexd) => execFileSync(DEXD, ["pair", "--data-dir", join(dexd.projects, "..", "data")], { encoding: "utf8" }).trim();

test("masaüstü Sunucular LAN WS'yi reddeder, loopback eşlemesi klavyeyle çalışır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Sunucu güvenliği");
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tab", { name: "Bağlantılar", exact: true }).click();
  const hosts = app.getByRole("region", { name: "Sunucular", exact: true });
  const address = hosts.getByLabel("Adres", { exact: true });
  await expect(address).toHaveAttribute("placeholder", /^wss:\/\//);
  await expect(address).toHaveAccessibleDescription(/yerel\/Tailscale/);
  await address.fill("ws://192.168.1.2:7717");
  await hosts.getByLabel("Eşleme kodu", { exact: true }).fill(pairCode(dexd));
  await hosts.getByRole("button", { name: "Sunucu ekle", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(hosts.getByRole("alert")).toContainText("şifresiz bağlantı yalnız yerel/Tailscale");
  await hosts.getByLabel("Ad", { exact: true }).fill("Loopback");
  await address.fill(dexd.url.replace(/^ws:\/\//, ""));
  await hosts.getByRole("button", { name: "Sunucu ekle", exact: true }).focus();
  await app.keyboard.press("Enter");
  await expect(hosts.locator(".set-item").filter({ hasText: "Loopback" }).getByRole("status")).toHaveText("bağlı");
  const duplicate = await app.evaluate(async () => {
    const path = "/src/api/hosts.ts";
    const { remoteHosts, addHost } = await import(path);
    const host = remoteHosts().find((h: { name: string }) => h.name === "Loopback");
    // Pre-upgrade records have no canonical trailing slash.
    host.url = host.url.replace(/\/$/, "");
    try { await addHost("duplicate", host.url, "unused-synthetic-code", "test"); }
    catch (e) { return String(e); }
    return "unexpected pairing";
  });
  expect(duplicate).toBe("bu adres zaten ekli");
});

test("mobil dar görünümde güvensiz adres soket açmaz, Tailscale WS eşlemesi korunur", async ({ page, dexd }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  // Route a tailnet address to the isolated real core. This exercises pairing/auth without
  // requiring the test machine to join a tailnet or sending the code onto an external network.
  await page.addInitScript(({ url }) => {
    const NativeWs = window.WebSocket;
    window.WebSocket = class extends NativeWs {
      constructor(address: string | URL, protocols?: string | string[]) {
        const target = String(address);
        super(target === "ws://100.64.0.1:7717/" ? url : target, protocols);
      }
    };
  }, { url: dexd.url });
  const sockets: string[] = [];
  // Vite's HMR socket is independent of the app's pairing/host transport.
  page.on("websocket", ws => { if (new URL(ws.url()).port !== String(VITE_PORT)) sockets.push(ws.url()); });
  await page.goto("/");
  const address = page.getByLabel("Adres", { exact: true });
  await expect(address).toHaveAccessibleDescription(/yerel\/Tailscale/);
  await page.getByLabel("Ad", { exact: true }).fill("Tailnet");
  await page.keyboard.press("Tab");
  await expect(address).toBeFocused();
  await page.keyboard.type("ws://10.0.0.2:7717");
  await page.keyboard.press("Tab");
  await expect(page.getByLabel("Eşleme kodu", { exact: true })).toBeFocused();
  await page.keyboard.type(pairCode(dexd));
  await page.keyboard.press("Tab");
  const add = page.getByRole("button", { name: "Sunucu ekle", exact: true });
  await expect(add).toBeFocused();
  expect(await add.evaluate(el => getComputedStyle(el).boxShadow)).not.toBe("none");
  const box = (await add.boundingBox())!;
  expect(box.height).toBeGreaterThanOrEqual(44);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("alert")).toContainText("şifresiz bağlantı yalnız yerel/Tailscale");
  expect(sockets).toHaveLength(0);
  // Existing persisted addresses and direct transport/pair calls must share the same gate.
  const denied = await page.evaluate(async () => {
    const path = "/src/api/transport.ts";
    const { WsTransport, pairDevice } = await import(path);
    const states: string[] = [];
    const t = new WsTransport("ws://server.example.invalid:7717", "synthetic-device-token", (s: string) => states.push(s));
    const errors: string[] = [];
    try { await t.call("server_info"); } catch (e) { errors.push(String(e)); }
    try { await pairDevice("ws://192.168.1.2:7717", "synthetic-code", "test"); } catch (e) { errors.push(String(e)); }
    t.close();
    return { states, errors };
  });
  expect(denied.states).toContain("blocked");
  expect(denied.errors).toHaveLength(2);
  for (const error of denied.errors) expect(error).toContain("şifresiz bağlantı yalnız yerel/Tailscale");
  expect(sockets).toHaveLength(0);
  await address.fill("100.64.0.1:7717");
  await add.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Sunucular", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Sunucular", exact: true }).click();
  const host = page.locator(".m-host").filter({ hasText: "Tailnet" });
  await expect(host.locator(".m-host-status")).toHaveText("bağlı");
  await expect(host.locator("code")).toHaveText("ws://100.64.0.1:7717/");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("mobil sohbet render hatası ekrana yayılmaz; tekrar dene ve geri klavyeyle çalışır", async ({ page, dexd }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  const team = await newTeam(dexd, "Sohbet hata sınırı");
  const worker = (await teamAgents(dexd, team.id)).find(a => a.kind === "worker")!;
  await page.addInitScript(({ url, token }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "he2e01", name: "e2e", url, device: "" }]));
    localStorage.setItem("dex.webtoken.he2e01", token);
  }, { url: dexd.url, token: dexd.token });
  await page.route("**/src/chat/ChatView.tsx*", route => route.fulfill({ contentType: "application/javascript", body: 'export default function ChatView() { throw new Error("synthetic render failure"); }' }));
  await page.goto("/");
  await page.locator(".m-team-btn").click();
  await page.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();
  await page.getByRole("tab", { name: "Ajanlar", exact: true }).click();
  await page.getByRole("button", { name: new RegExp(worker.name) }).click();
  const screen = page.locator(".m-agent-screen");
  await expect(screen.getByRole("alert")).toContainText("Bu panel açılamadı");
  const retry = screen.getByRole("button", { name: "Tekrar dene", exact: true });
  await retry.focus();
  await page.keyboard.press("Enter");
  await expect(screen.getByRole("alert")).toBeVisible();
  await screen.getByRole("button", { name: "Geri", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(screen).toHaveCount(0);
  await expect(page.locator(".m-agents")).toBeVisible();
});
