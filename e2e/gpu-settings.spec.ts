// #290: Ayarlar → Sunucu → GPU (bağlantı, durum, limitler, modeller) ve patronun GPU izin kartı. Broker’a hiç çıkılmaz: dexd’nin
// gpu_* cevapları köprüde hazır (__dexFake); gpu-requests-changed olayı uygulamanın soketine sahte verilir.
import type { Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

const emit = (app: Page, event: string, payload: unknown) =>
  app.evaluate(([event, payload]) => {
    const s = (window.__dexSockets ?? []).filter((w) => w.readyState === WebSocket.OPEN).at(-1);
    if (!s) throw new Error("açık soket yok");
    s.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event, payload }) }));
  }, [event, payload] as const);

const URL1 = "http://192.0.2.10:8100";
const noToken = {
  tokenSet: false, connected: true, authorized: false, via: "primary", url: URL1, models: [], status: null, error: "token girilmedi", checkedAt: 1,
};
const live = {
  tokenSet: true, connected: true, authorized: true, via: "fallback", url: "http://198.51.100.11:8100",
  models: [
    { name: "FLUX.2-dev", type: "t2i", engine: "comfyui", path: "image/flux2-dev", agent: true, serve: true, disk: true, recipes: ["flux_draft", "flux_final"] },
    { name: "RMBG-2.0", type: "bg-removal", engine: "comfyui", path: "image/rmbg-2", agent: true, serve: true, disk: true, recipes: ["rmbg"] },
    { name: "qwen3-coder-next", type: "llm", engine: "vllm", path: "llm/Qwen3-Coder-Next", agent: false, serve: true, disk: true, recipes: [] },
    // Diskte onlarca kayıt: liste kapalı başlar.
    ...Array.from({ length: 40 }, (_, i) => ({ name: `disk-${i}`, type: "image", engine: null, path: `image/disk-${i}`, agent: false, serve: false, disk: true, recipes: [] })),
  ],
  status: { owner: "llm", llmModel: "qwen3-coder-next", queued: 0, runningJob: null, dailyUsed: 12, dailyLimit: 120 },
  error: null, errorKind: null, checkedAt: 2,
};
const asked = { id: 7, teamId: "default", leader: "timur", leaderName: "Timur", worker: "ayla", workerName: "Ayla",
  types: ["t2i", "bg-removal"], reason: "Ürün kartı için 4 taslak görsel.", budget: 24, status: "pending", grantId: null, createdAt: 1, decidedAt: null };

// Hold one RPC at the socket boundary; release it explicitly, without timing sleeps.
const holdNext = (app: Page, method: string) => app.evaluate((method) => {
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (data) {
    if (typeof data === "string") {
      const request = JSON.parse(data);
      if (request.method === method) {
        WebSocket.prototype.send = send;
        (window as typeof window & { releaseGpu?: () => void }).releaseGpu = () => {
          this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ id: request.id, result: window.__dexFake![method] }) }));
        };
        return;
      }
    }
    return send.call(this, data);
  };
}, method);
const releaseHeld = (app: Page) => app.evaluate(async () => {
  (window as typeof window & { releaseGpu?: () => void }).releaseGpu!();
  await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
});

test("GPU broker adresi değişince limitler yenilenir ve yalnız değişen Kuyruk gönderilir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "GPU broker değişimi");
  await app.addInitScript((url) => {
    localStorage.setItem("dex.settingsTab", "gpu");
    window.__dexFake = {
      gpu_config_get: { primary: url, fallback: "", tokenSet: true },
      gpu_config_set: { primary: "http://broker-b:8100", fallback: "", tokenSet: true },
      gpu_overview: { tokenSet: true, connected: false, authorized: false, models: [], status: null },
      gpu_limits_get: { daily_image_limit: 120, max_pending_jobs: 5, max_count_per_request: 4 },
      gpu_limits_set: { daily_image_limit: 300, max_pending_jobs: 10, max_count_per_request: 2 },
      gpu_request_list: [],
    };
  }, URL1);
  await app.reload();
  await openTeam(app, team.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const conn = app.getByRole("region", { name: "GPU bağlantısı" });
  const limits = app.getByRole("region", { name: "GPU limitleri" });
  await expect(limits.getByLabel("Günlük görsel")).toHaveValue("120");
  await limits.getByLabel("Kuyruk").fill("6");
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  await limits.getByLabel("Yönetici şifresi").fill("1234567");
  await conn.getByLabel("Birincil adres").fill("http://broker-b:8100");
  await holdNext(app, "gpu_config_set");
  await conn.getByRole("button", { name: "Kaydet ve bağlan", exact: true }).click();
  await expect(limits.getByRole("button", { name: "Onayla" })).toBeDisabled();
  await app.evaluate(() => { window.__dexFake!.gpu_limits_get = { daily_image_limit: 300, max_pending_jobs: 8, max_count_per_request: 2 }; });
  await releaseHeld(app);
  await expect(limits.getByLabel("Yönetici şifresi")).toHaveCount(0);
  await expect(limits.getByLabel("Günlük görsel")).toHaveValue("300");
  await expect(limits.getByLabel("İstek başına")).toHaveValue("2");
  await limits.getByLabel("Kuyruk").fill("10");
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  await limits.getByLabel("Yönetici şifresi").fill("1234567");
  await limits.getByRole("button", { name: "Onayla" }).click();
  await expect(limits.getByRole("status")).toHaveText("Limitler kaydedildi.");
  expect((await calls(app)).filter((c) => c.cmd === "gpu_limits_set").map((c) => c.args)).toEqual([
    { adminToken: "1234567", limits: { max_pending_jobs: 10 } },
  ]);
});

test("GPU geciken limit yanıtı onaylanan kartı geri getirmez", async ({ app }) => {
  await app.addInitScript(() => { window.__dexFake = { gpu_request_list: [], gpu_limits_get: { daily_image_limit: 300 }, gpu_request_decide: {} }; });
  await app.reload();
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "gpu_request_list").length).toBeGreaterThan(0);
  await app.evaluate((r) => { window.__dexFake!.gpu_request_list = [r]; }, asked);
  await emit(app, "gpu-requests-changed", asked);
  const card = app.getByRole("alertdialog", { name: "Timur GPU izni istiyor" });
  await expect(card).toBeVisible();
  await holdNext(app, "gpu_limits_get");
  await emit(app, "gpu-requests-changed", asked);
  await expect.poll(() => app.evaluate(() => typeof (window as typeof window & { releaseGpu?: () => void }).releaseGpu)).toBe("function");
  await card.getByRole("button", { name: "Onayla" }).click();
  await app.evaluate(() => { window.__dexFake!.gpu_request_list = []; });
  await emit(app, "gpu-requests-changed", { ...asked, status: "approved" });
  await expect(card).toHaveCount(0);
  await releaseHeld(app);
  await expect(card).toHaveCount(0);
});

test("GPU geciken limit yanıtı kaldırılan hostun kartını geri getirmez", async ({ app, dexd }) => {
  await app.addInitScript(({ url, token, request }) => {
    localStorage.setItem("dex.hosts", JSON.stringify([{ id: "gpu-remote", name: "GPU remote", url, device: "test" }]));
    localStorage.setItem("dex.e2e.token.gpu-remote", token);
    window.__dexFake = { gpu_request_list: [request], gpu_limits_get: { daily_image_limit: 300 } };
  }, { url: dexd.url, token: dexd.token, request: asked });
  await app.reload();
  const cards = app.getByRole("alertdialog", { name: "Timur GPU izni istiyor" });
  await expect(cards).toHaveCount(2);
  await holdNext(app, "gpu_limits_get");
  await emit(app, "gpu-requests-changed", asked);
  await expect.poll(() => app.evaluate(() => typeof (window as typeof window & { releaseGpu?: () => void }).releaseGpu)).toBe("function");
  await app.evaluate(async () => {
    const path = "/src/api/hosts.ts";
    const { removeHost } = await import(path);
    await removeHost("gpu-remote");
  });
  await expect(cards).toHaveCount(1);
  await releaseHeld(app);
  await expect(cards).toHaveCount(1);
});

test("GPU bağlantısı: anahtar kaydedilince bağlı + modeller; lider isteği kartla onaylanır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `GPU ${test.info().workerIndex}`);
  const request = { ...asked, teamId: team.id };
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.addInitScript((f) => { window.__dexFake = f; }, {
    gpu_config_get: { primary: URL1, fallback: "", tokenSet: false },
    gpu_config_set: { primary: URL1, fallback: "http://198.51.100.11:8100", tokenSet: true },
    gpu_overview: noToken,
    gpu_limits_get: { daily_image_limit: 300, max_pending_jobs: 8, max_count_per_request: 4 },
    gpu_limits_set: { daily_image_limit: 300, max_pending_jobs: 8, max_count_per_request: 2 },
    gpu_request_list: [],
    gpu_request_decide: { ...request, status: "approved", grantId: 3 },
  });
  await app.evaluate(() => localStorage.setItem("dex.settingsTab", "gpu"));
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);

  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const conn = app.getByRole("region", { name: "GPU bağlantısı" });
  const status = app.getByRole("region", { name: "GPU durumu" });
  const models = app.getByRole("region", { name: "GPU modelleri" });
  await expect(status.getByRole("status").filter({ hasText: "token yok" })).toBeVisible();
  await expect(app.getByRole("list", { name: "GPU modelleri" })).toHaveCount(0);

  await conn.getByLabel("Yedek adres").fill("http://198.51.100.11:8100");
  const token = conn.getByLabel("GPU anahtarı");
  await expect(token).toHaveAttribute("type", "password");
  await token.fill("gizli-token");
  await app.evaluate((o) => { window.__dexFake!.gpu_overview = o; }, live);
  await conn.getByRole("button", { name: "Kaydet ve bağlan", exact: true }).click();

  await expect(status.getByRole("status").filter({ hasText: "bağlı (yedek: http://198.51.100.11:8100)" })).toBeVisible();
  const set = (await calls(app)).filter((c) => c.cmd === "gpu_config_set").map((c) => c.args);
  expect(set).toEqual([{ config: { fallback: "http://198.51.100.11:8100", token: "gizli-token" } }]);
  // Üç grup: ajana açık, servis (patronun), yalnız disk (kapalı, sayılı).
  await expect(models.getByRole("heading", { name: /^Ajana açık \(2\)/ })).toBeVisible();
  await expect(models.getByRole("list", { name: "Ajana açık" }).getByRole("listitem")).toHaveText([/FLUX\.2-dev\s*t2i/, /RMBG-2\.0\s*bg-removal/]);
  await expect(models.getByRole("heading", { name: /^Servis ediliyor \(1\)/ })).toBeVisible();
  await expect(models.getByRole("list", { name: "Servis ediliyor" }).getByRole("listitem")).toHaveText([/qwen3-coder-next\s*llm/]);
  const disk = models.locator("summary").filter({ hasText: "Yalnız diskte (40)" });
  await expect(disk).toBeVisible();
  await expect(models.getByRole("list", { name: "Yalnız diskte" })).toBeHidden();
  await disk.focus();
  await app.keyboard.press("Enter");
  await expect(models.getByRole("list", { name: "Yalnız diskte" }).getByRole("listitem")).toHaveCount(40);
  await expect(models.getByRole("list", { name: "Yalnız diskte" }).getByRole("listitem").first()).toHaveText(/disk-0\s*image/);
  await expect(status).toContainText("bugün 12/120");
  // Token kayıtlı: alan kapanır, "Değiştir" yeniden açar.
  await expect(conn.getByText("kayıtlı", { exact: true })).toBeVisible();
  await conn.getByRole("button", { name: "Değiştir" }).click();
  await expect(conn.getByLabel("GPU anahtarı")).toHaveValue("");

  const limits = app.getByRole("region", { name: "GPU limitleri" });
  await expect(limits.getByLabel("Günlük görsel")).toHaveValue("300");
  await limits.getByLabel("İstek başına").fill("2");
  await limits.getByLabel("Sebep").fill("Deneme");
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  const admin = limits.getByLabel("Yönetici şifresi");
  await expect(admin).toHaveAttribute("type", "password");
  await admin.fill("1234567");
  await limits.getByRole("button", { name: "Onayla" }).click();
  await expect(limits.getByRole("status")).toHaveText("Limitler kaydedildi.");
  await expect(admin).toHaveCount(0);
  expect((await calls(app)).filter((c) => c.cmd === "gpu_limits_set").map((c) => c.args)).toEqual([
    { adminToken: "1234567", limits: { max_count_per_request: 2 }, reason: "Deneme" },
  ]);
  await limits.getByLabel("Kuyruk").fill("9");
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  await expect(admin).toHaveValue("");
  await limits.getByRole("button", { name: "Vazgeç" }).click();
  // Uç henüz yoksa hata satırı; başarısız istek de şifreyi bırakmaz.
  await app.evaluate(() => { window.__dexOld = { unknown: ["gpu_limits_set"] }; });
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  await admin.fill("7654321");
  await limits.getByRole("button", { name: "Onayla" }).click();
  await expect(limits.getByRole("alert")).toContainText("bilinmeyen metot: gpu_limits_set");
  await expect(admin).toHaveCount(0);
  await limits.getByRole("button", { name: "Kaydet", exact: true }).click();
  await expect(admin).toHaveValue("");
  await limits.getByRole("button", { name: "Vazgeç" }).click();
  await app.evaluate(() => { window.__dexOld = undefined; });

  // #305: broker yanıt veriyor ama model listesi 500 → "token yanlış" değil, broker hatası görünür.
  await app.evaluate((o) => { window.__dexFake!.gpu_overview = o; },
    { ...live, authorized: false, models: [], status: null, error: "internal: vllm yanıt vermedi", errorKind: "service" });
  await conn.getByRole("button", { name: "Yenile", exact: true }).click();
  const line = status.getByRole("status").filter({ hasText: "modeller alınamadı" });
  await expect(line).toContainText("internal: vllm yanıt vermedi");
  await expect(line).not.toContainText("token");
  await expect(app.getByRole("list", { name: "Ajana açık" })).toHaveCount(0);

  // Lider gpu_request yaptı: sağ altta kart; bütçe değişir, klavyeyle Onayla.
  await app.setViewportSize({ width: 420, height: 800 });
  await app.evaluate((r) => { window.__dexFake!.gpu_request_list = [r]; }, request);
  await emit(app, "gpu-requests-changed", request);
  const card = app.getByRole("alertdialog", { name: "Timur GPU izni istiyor" });
  await expect(card).toBeVisible();
  await expect(card).toContainText("Ayla");
  await expect(card.getByRole("heading")).toContainText(team.name);
  await expect(card).toContainText("t2i");
  await expect(card).toContainText("Ürün kartı için 4 taslak görsel.");
  const box = await card.boundingBox();
  expect(box!.x + box!.width).toBeLessThanOrEqual(420);
  const budget = card.getByLabel("GPU bütçesi");
  await expect(budget).toHaveValue("24");
  await expect(budget).toHaveAttribute("max", "300");
  await budget.fill("30");
  await app.keyboard.press("Tab");
  await expect(card.getByRole("button", { name: "Reddet" })).toBeFocused();
  await app.keyboard.press("Tab");
  await expect(card.getByRole("button", { name: "Onayla" })).toBeFocused();
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await calls(app)).filter((c) => c.cmd === "gpu_request_decide").map((c) => c.args)).toEqual([{ id: 7, approve: true, budget: 30 }]);

  // dexd kararı yayınlar; liste boşalır, kart kalkar.
  await app.evaluate(() => { window.__dexFake!.gpu_request_list = []; });
  await emit(app, "gpu-requests-changed", { ...request, status: "approved", grantId: 3 });
  await expect(card).toHaveCount(0);
});
