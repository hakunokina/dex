import { test } from "node:test";
import assert from "node:assert/strict";
import { createDelegationSoundTracker, createSoundGate, defaultSoundSettings, normalizeSoundSettings, soundForEvent } from "./model.ts";

test("raporsuz bitişten geçerli rapora geçiş worker sesini bir kez üretir", () => {
  const sounds = createDelegationSoundTracker();
  sounds.seed("local", [{ key: "local:1", status: "running" }]);
  assert.equal(sounds.change("local", { key: "local:1", status: "done_no_report" }), null);
  assert.equal(sounds.change("local", { key: "local:1", status: "done" }), "worker_done");
  assert.equal(sounds.change("local", { key: "local:1", status: "done" }), null);
});

test("sesler açık başlar; worker bitişi sessiz, ses düzeyi ölçülüdür", () => {
  const settings = defaultSoundSettings();
  assert.equal(settings.enabled, true);
  assert.equal(settings.volume, 0.5);
  assert.deepEqual(settings.events, { complete: true, attention: true, warning: true, delegation: true, worker: false });
});

test("başlangıçtaki done kaydı okundu diye yeniden yayınlanınca ses üretmez", () => {
  const sounds = createDelegationSoundTracker();
  sounds.seed("local", [{ key: "local:old", status: "done" }]);
  assert.equal(sounds.change("local", { key: "local:old", status: "done" }), null);
  assert.equal(sounds.change("local", { key: "local:old", status: "done" }), null);
});

test("siuv yalnız yeni aktif devretmede çalar; geçmiş ve yeniden yayınlar sessizdir", () => {
  const sounds = createDelegationSoundTracker();
  sounds.seed("local", [
    { key: "local:running", status: "running" },
    { key: "local:queued", status: "queued" },
    { key: "local:done", status: "done" },
  ]);
  assert.equal(sounds.change("local", { key: "local:running", status: "running" }), null);
  assert.equal(sounds.change("local", { key: "local:queued", status: "running" }), null);
  assert.equal(sounds.change("local", { key: "local:done", status: "running" }), null);
  assert.equal(sounds.change("local", { key: "local:new", status: "queued" }), "delegation_started");
  assert.equal(sounds.change("local", { key: "local:new", status: "running" }), null);
  assert.equal(sounds.change("local", { key: "local:new", status: "running" }), null);
  assert.equal(sounds.change("local", { key: "local:new-running", status: "running" }), "delegation_started");
  for (const status of ["done", "done_no_report", "failed"]) {
    assert.equal(sounds.change("local", { key: `local:unseen-${status}`, status }), null);
  }
});

test("ilk liste gelmeden ve başka hostun geçmiş kayıtlarında ses üretilmez", () => {
  const sounds = createDelegationSoundTracker();
  assert.equal(sounds.change("remote", { key: "remote:1", status: "running" }), null);
  assert.equal(sounds.change("remote", { key: "remote:2", status: "done" }), null);
  sounds.seed("local", []);
  sounds.seed("remote", [{ key: "remote:1", status: "running" }, { key: "remote:2", status: "done" }]);
  assert.equal(sounds.change("remote", { key: "remote:1", status: "running" }), null);
  assert.equal(sounds.change("remote", { key: "remote:2", status: "done" }), null);
  assert.equal(sounds.change("local", { key: "local:1", status: "running" }), "delegation_started");
  assert.equal(sounds.change("remote", { key: "remote:1", status: "done" }), "worker_done");
});

test("liste yenilemesi olaydan önce bitişi tüketmez; yalnız done geçişi çalar", () => {
  const sounds = createDelegationSoundTracker();
  sounds.seed("local", [{ key: "local:1", status: "running" }]);
  sounds.seed("local", [{ key: "local:1", status: "done" }]);
  assert.equal(sounds.change("local", { key: "local:1", status: "done" }), "worker_done");
  assert.equal(sounds.change("local", { key: "local:1", status: "done" }), null);
  assert.equal(sounds.change("local", { key: "local:2", status: "queued" }), "delegation_started");
  assert.equal(sounds.change("local", { key: "local:2", status: "failed" }), null);
  assert.equal(sounds.change("local", { key: "local:2", status: "done_no_report" }), null);
});

test("aynı ses kısa pencerede tek kez; farklı sesler ve pencere sonu serbest", () => {
  const accept = createSoundGate();
  const settings = defaultSoundSettings();
  assert.equal(accept("attention", settings, 0), true);
  assert.equal(accept("attention", settings, 500), false);
  assert.equal(accept("delegation", settings, 500), true);
  assert.equal(accept("attention", settings, 800), true);
  assert.equal(accept("warning", { ...settings, enabled: false }, 1000), false);
  assert.equal(accept("warning", { ...settings, volume: 0 }, 1000), false);
  assert.equal(accept("worker", settings, 1000), false);
  assert.equal(accept("worker", { ...settings, events: { ...settings.events, worker: true } }, 1000), true);
  assert.equal(accept("warning", settings, 1000), true); // muted events do not consume the window
});

test("bildirim olayları birbirinden ayırt edilen seslere eşlenir", () => {
  assert.deepEqual([
    "team_turn_done", "question_escalated", "member_proposed", "agent_stalled", "agent_waiting", "chain_exhausted", "delegation_started", "worker_done",
  ].map(soundForEvent), ["complete", "attention", "attention", "warning", "warning", "warning", "delegation", "worker"]);
  assert.equal(soundForEvent("dex-chain"), null); // successful fallback is not an exhausted chain
});

test("bozuk veya eski kayıt varsayılanlarla tamamlanır; düzey sınırlandırılır", () => {
  for (const raw of [null, false, [], "broken"]) assert.deepEqual(normalizeSoundSettings(raw), defaultSoundSettings());
  assert.deepEqual(normalizeSoundSettings({ enabled: false, volume: 2, events: { attention: false, worker: true, warning: "no" } }), {
    enabled: false, volume: 1, events: { complete: true, attention: false, warning: true, delegation: true, worker: true },
  });
  assert.equal(normalizeSoundSettings({ volume: -1 }).volume, 0);
  assert.equal(normalizeSoundSettings({ volume: NaN }).volume, 0.5);
  const a = defaultSoundSettings();
  a.events.complete = false;
  assert.equal(defaultSoundSettings().events.complete, true);
});
