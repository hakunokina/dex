import { test } from "node:test";
import assert from "node:assert/strict";
import { configPatch, connectionLine, gpuStripCell, limitsPatch, modelGroups, parseBudget, waitingToastText, type GpuModel, type GpuOverview } from "./view.ts";

const o = (p: Partial<GpuOverview>): GpuOverview => ({
  tokenSet: true, connected: true, authorized: true, via: "primary", url: "http://10.0.0.1:8100",
  models: [], status: null, error: null, checkedAt: 0, ...p,
});

test("connectionLine: bağlı, hangi adres yanıt verdi", () => {
  assert.deepEqual(connectionLine(o({})), { tone: "ok", text: "bağlı (birincil: http://10.0.0.1:8100)" });
  assert.equal(connectionLine(o({ via: "fallback", url: "http://10.0.0.2:8100" })).text, "bağlı (yedek: http://10.0.0.2:8100)");
});

test("connectionLine: bağlanamadı / token yok / token yanlış", () => {
  assert.deepEqual(connectionLine(o({ connected: false, authorized: false, url: null, error: "zaman aşımı" })), { tone: "down", text: "bağlanamadı — zaman aşımı" });
  assert.equal(connectionLine(o({ tokenSet: false, authorized: false })).tone, "warn");
  assert.match(connectionLine(o({ tokenSet: false, authorized: false })).text, /^token yok/);
  assert.match(connectionLine(o({ authorized: false, errorKind: "auth", error: "GPU broker token'ı yok ya da geçersiz" })).text, /^token yanlış/);
});

test("connectionLine: modeller 500/zaman aşımı → token değil broker hatası, hata metni görünür (#305)", () => {
  const l = connectionLine(o({ authorized: false, errorKind: "service", error: "internal: vllm yanıt vermedi" }));
  assert.equal(l.tone, "down");
  assert.doesNotMatch(l.text, /token/);
  assert.match(l.text, /modeller alınamadı \(birincil: http:\/\/10\.0\.0\.1:8100\) — internal: vllm yanıt vermedi$/);
  // Hata türü bildirmeyen dexd de "token yanlış" demez.
  assert.doesNotMatch(connectionLine(o({ authorized: false, error: "GPU broker HTTP 500 (hata)" })).text, /token yanlış/);
  // Modeller geldi, /v1/status düştü: bağlı ama uyarı + hata.
  const s = connectionLine(o({ errorKind: "service", error: "GPU broker HTTP 502 (hata)" }));
  assert.deepEqual(s, { tone: "warn", text: "bağlı (birincil: http://10.0.0.1:8100) — durum alınamadı: GPU broker HTTP 502 (hata)" });
});

test("modelGroups: ajana açık / servis (patronun) / yalnız disk, ayrık", () => {
  const m = (name: string, agent: boolean, serve: boolean): GpuModel => ({ name, type: "t2i", engine: null, path: null, agent, serve, disk: true, recipes: [] });
  const g = modelGroups([m("a", true, true), m("s", false, true), m("d1", false, false), m("d2", false, false)]);
  assert.deepEqual([g.agent, g.served, g.diskOnly].map((x) => x.map((y) => y.name)), [["a"], ["s"], ["d1", "d2"]]);
});

test("parseBudget: 1–240, boş ya da bozuk → liderin istediği", () => {
  assert.equal(parseBudget("", 24), 24);
  assert.equal(parseBudget("abc", 24), 24);
  assert.equal(parseBudget("0", 24), 1);
  assert.equal(parseBudget("500", 24), 240);
  assert.equal(parseBudget("12.4", 24), 12);
});

test("parseBudget: broker limiti, boş ve bozuk değerler de sınırı aşamaz", () => {
  assert.equal(parseBudget("500", 24, 300), 300);
  assert.equal(parseBudget("", 24, 8), 8);
  assert.equal(parseBudget("abc", 24, 8), 8);
});

test("limitsPatch: boş alan gönderilmez, pozitif tam sayı ve istek başına ≤4", () => {
  assert.deepEqual(limitsPatch({ daily_image_limit: "300", max_pending_jobs: "", max_count_per_request: "2" }), { daily_image_limit: 300, max_count_per_request: 2 });
  for (const raw of ["0", "-1", "1.5", "abc", "5"]) {
    assert.throws(() => limitsPatch({ daily_image_limit: "300", max_pending_jobs: "8", max_count_per_request: raw }));
  }
  assert.throws(() => limitsPatch({ daily_image_limit: "", max_pending_jobs: "", max_count_per_request: "" }));
});

test("limitsPatch: yalnız yüklenen değerden farklı alanlar gönderilir", () => {
  const saved = { daily_image_limit: 300, max_pending_jobs: 8, max_count_per_request: 2 };
  assert.deepEqual(limitsPatch({ daily_image_limit: "300", max_pending_jobs: "10", max_count_per_request: "2" }, saved), { max_pending_jobs: 10 });
  assert.throws(() => limitsPatch({ daily_image_limit: "300", max_pending_jobs: "8", max_count_per_request: "2" }, saved));
});

test("configPatch: yalnız değişen alan; boş token kayıtlıyı korur", () => {
  const saved = { primary: "http://a:8100", fallback: "", tokenSet: true };
  assert.deepEqual(configPatch(saved, { primary: "http://a:8100", fallback: "", token: "" }), {});
  assert.deepEqual(configPatch(saved, { primary: " http://b:8100 ", fallback: "http://c:8100", token: " s3cr " }), { primary: "http://b:8100", fallback: "http://c:8100", token: "s3cr" });
});

test("gpuStripCell: token yok → yok; sahip, kuyruk, günlük", () => {
  const st = { owner: "llm", llmModel: "qwen3", queued: 2, runningJob: null, dailyUsed: 9, dailyLimit: 120 };
  assert.equal(gpuStripCell(null), null);
  assert.equal(gpuStripCell(o({ tokenSet: false, authorized: false })), null);
  assert.deepEqual(gpuStripCell(o({ status: st })), { owner: "LLM", queued: 2, daily: "9/120", tone: "ok", title: "GPU: LLM'in (qwen3) · kuyrukta 2 iş · bugün 9/120 görsel" });
  assert.equal(gpuStripCell(o({ status: { ...st, owner: null } }))!.owner, "boş");
  assert.equal(gpuStripCell(o({ status: { ...st, owner: "idle" } }))!.owner, "boş");
  assert.equal(gpuStripCell(o({ status: { ...st, owner: "broker" } }))!.owner, "iş");
  assert.equal(gpuStripCell(o({ status: { ...st, runningJob: "j_1" } }))!.owner, "iş");
  assert.equal(gpuStripCell(o({ status: { ...st, dailyUsed: 120 } }))!.tone, "warn");
  const down = gpuStripCell(o({ connected: false, authorized: false, status: null, error: "zaman aşımı" }))!;
  assert.deepEqual([down.owner, down.daily, down.tone], ["--", "--", "down"]);
  assert.match(down.title, /bağlanamadı/);
});

test("waitingToastText: ad + LLM boşalınca, varsa en erken süre", () => {
  assert.deepEqual(waitingToastText("Pixel-1", 365), { title: "Pixel-1 GPU bekliyor", body: "LLM'in boşa çıkınca başlayacak (en erken ~6 dk)" });
  assert.equal(waitingToastText("Pixel-1", null).body, "LLM'in boşa çıkınca başlayacak");
});
