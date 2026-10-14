import { test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Ses ayarları: varsayılanlar, klavye, dinleme ve kalıcılık dar görünümde çalışır", async ({ app, dexd }) => {
  // Dar görünüm baştan: yerleşim 640'a göre kurulur, sonradan küçültmenin geçici taşması odağı kaydırmaz.
  await app.setViewportSize({ width: 640, height: 800 });
  const team = await newTeam(dexd, "Ses ayarları");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.addInitScript(() => {
    const original = AudioContext.prototype.createOscillator;
    (window as unknown as { soundStarts: number }).soundStarts = 0;
    AudioContext.prototype.createOscillator = function () {
      (window as unknown as { soundStarts: number }).soundStarts++;
      return original.call(this);
    };
  });
  await app.evaluate(() => {
    localStorage.removeItem("dex.sounds");
    localStorage.setItem("dex.settingsTab", "notifications");
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const sounds = app.getByRole("region", { name: "Olay sesleri", exact: true });
  await expect(sounds).toBeVisible();
  await expect(sounds.getByRole("switch", { name: "Sesleri aç" })).toBeChecked();
  await expect(sounds.getByRole("switch", { name: "Worker işi bitti" })).not.toBeChecked();
  for (const name of ["Tur tamamlandı", "Patrona soru / üye önerisi", "Takıldı / izin bekliyor / kota tükendi", "İş devredildi (siuv)"]) {
    await expect(sounds.getByRole("switch", { name, exact: true })).toBeChecked();
  }
  const volume = sounds.getByRole("slider", { name: "Ses düzeyi" });
  await volume.focus();
  await app.keyboard.press("Home");
  await app.keyboard.press("ArrowRight");
  await expect(volume).toHaveValue("5");
  const worker = sounds.getByRole("switch", { name: "Worker işi bitti" });
  await worker.focus();
  await app.keyboard.press("Space");
  await expect(worker).toBeChecked();
  await app.keyboard.press("Tab");
  await expect(sounds.getByRole("button", { name: "Worker işi bitti sesini dinle", exact: true })).toBeFocused();
  const before = await app.evaluate(() => (window as unknown as { soundStarts: number }).soundStarts);
  const preview = sounds.getByRole("button", { name: "İş devredildi (siuv) sesini dinle", exact: true });
  await preview.focus();
  await app.keyboard.press("Enter");
  await expect.poll(() => app.evaluate(() => (window as unknown as { soundStarts: number }).soundStarts)).toBeGreaterThan(before);
  await expect(sounds.getByRole("alert")).toHaveCount(0);
  expect(await sounds.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  expect(await sounds.evaluate(el => {
    const rect = el.getBoundingClientRect();
    return rect.left >= 0 && rect.right <= innerWidth;
  })).toBe(true);
  const enabled = sounds.getByRole("switch", { name: "Sesleri aç" });
  await enabled.focus();
  await app.keyboard.press("Space");
  await expect(enabled).not.toBeChecked();
  await app.reload();
  await openTeam(app, team.name);
  // Reload restores the panel as a dock tab; select it before reading device preferences.
  await app.getByRole("tab", { name: "Ayarlar", exact: true }).click();
  await expect(sounds).toBeVisible();
  await expect(enabled).not.toBeChecked();
  await expect(volume).toHaveValue("5");
  await expect(worker).toBeChecked();
});

test("Geçici ses aygıtı hatası başarılı dinlemeden sonra temizlenir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Ses aygıtı");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.addInitScript(() => {
    const native = AudioContext;
    const probe = window as unknown as { soundDeviceFailed: boolean };
    probe.soundDeviceFailed = true;
    window.AudioContext = class extends native {
      get state(): AudioContextState { return probe.soundDeviceFailed ? "suspended" : super.state; }
      resume() { return probe.soundDeviceFailed ? Promise.reject(new Error("Ses aygıtı geçici olarak kullanılamıyor.")) : super.resume(); }
    };
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Bildirimler" }).click();
  const sounds = app.getByRole("region", { name: "Olay sesleri", exact: true });
  await expect(sounds.getByRole("alert")).toHaveText("Ses aygıtı geçici olarak kullanılamıyor.");
  await app.evaluate(() => { (window as unknown as { soundDeviceFailed: boolean }).soundDeviceFailed = false; });
  await sounds.getByRole("button", { name: "Tur tamamlandı sesini dinle", exact: true }).click();
  await expect(sounds.getByRole("status").filter({ hasText: "Tur tamamlandı çalındı." })).toHaveText("Tur tamamlandı çalındı.");
  await expect(sounds.getByRole("alert")).toHaveCount(0);
});

test("Ses olayları odaktan bağımsız çalar; tekrar ve kapatma sessiz kalır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Ses olayları");
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find(a => a.kind === "leader")!;
  const worker = agents.find(a => a.kind === "worker")!;
  await app.addInitScript(() => {
    const original = AudioContext.prototype.createOscillator;
    (window as unknown as { soundStarts: number }).soundStarts = 0;
    AudioContext.prototype.createOscillator = function () {
      (window as unknown as { soundStarts: number }).soundStarts++;
      return original.call(this);
    };
  });
  await app.reload();
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tablist", { name: "Ayar bölümleri" }).getByRole("tab", { name: "Bildirimler" }).click();
  const sounds = app.getByRole("region", { name: "Olay sesleri", exact: true });
  await expect(sounds).toBeVisible();
  const result = await app.evaluate(({ teamId, leaderId, workerId }) => {
    // Feed the normal host transport, leaving the sound implementation and Web Audio real.
    const sockets = (window.__dexSockets ?? []).filter(s => s.readyState === WebSocket.OPEN);
    if (!sockets.length) throw new Error("Yerel host soketi yok");
    const emit = (event: string, payload: unknown) => {
      for (const socket of sockets) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event, payload }) }));
    };
    Object.defineProperty(document, "hasFocus", { configurable: true, value: () => false });
    const count = () => (window as unknown as { soundStarts: number }).soundStarts;
    const before = count();
    const done = { teamId, leaderId, timestamp: Date.now() };
    emit("team_turn_done", done);
    emit("team_turn_done", done);
    const complete = count() - before;
    emit("dex-chain-wait", { agentId: leaderId, text: "Kota tükendi" });
    emit("dex-chain-wait", { agentId: leaderId, text: "Kota tükendi" });
    const warning = count() - before - complete;
    const delegation = { id: 900001, fromAgent: leaderId, toAgent: workerId, teamId, serviceTeamId: null,
      status: "queued", prompt: "Ses testi", summary: null, taskId: null, resultPath: null, createdAt: Date.now(), updatedAt: Date.now() };
    emit("delegations-changed", delegation);
    emit("delegations-changed", { ...delegation, status: "running" });
    const started = count() - before - complete - warning;
    emit("delegations-changed", { ...delegation, status: "done" });
    const workerDone = count() - before - complete - warning - started;
    return { complete, warning, started, workerDone };
  }, { teamId: team.id, leaderId: leader.id, workerId: worker.id });
  expect(result).toEqual({ complete: 2, warning: 1, started: 1, workerDone: 0 });
  await sounds.getByRole("switch", { name: "Sesleri aç" }).uncheck();
  const muted = await app.evaluate(() => {
    const before = (window as unknown as { soundStarts: number }).soundStarts;
    for (const socket of window.__dexSockets ?? []) {
      if (socket.readyState === WebSocket.OPEN) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "team_turn_done", payload: {} }) }));
    }
    return (window as unknown as { soundStarts: number }).soundStarts - before;
  });
  expect(muted).toBe(0);
  // Audition works even with the master and the event switch off.
  const before = await app.evaluate(() => (window as unknown as { soundStarts: number }).soundStarts);
  for (const name of ["Tur tamamlandı", "Patrona soru / üye önerisi", "Takıldı / izin bekliyor / kota tükendi", "İş devredildi (siuv)", "Worker işi bitti"]) {
    await sounds.getByRole("button", { name: `${name} sesini dinle`, exact: true }).click();
    await expect(sounds.getByRole("status").filter({ hasText: `${name} çalındı.` })).toHaveText(`${name} çalındı.`);
  }
  expect(await app.evaluate(() => (window as unknown as { soundStarts: number }).soundStarts)).toBe(before + 7);
  await app.evaluate(() => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = function (key, value) {
      if (key === "dex.sounds") throw new DOMException("Blocked", "QuotaExceededError");
      return original.call(this, key, value);
    };
  });
  await sounds.getByRole("switch", { name: "Sesleri aç" }).click();
  await expect(sounds.getByRole("switch", { name: "Sesleri aç" })).not.toBeChecked();
  await expect(sounds.getByRole("alert")).toHaveText("Ses ayarları kaydedilemedi. Cihaz depolamasını kontrol edin.");
});
