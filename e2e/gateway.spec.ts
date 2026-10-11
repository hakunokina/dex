import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

test("Gateway: gecikmiş katalog yanıtı yeni giriş ve çıkış durumunu ezmez", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway sıralama");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  const ps = await dexd.call<{ id: string; models: unknown[]; hasToken: boolean }[]>("provider_list");
  await app.evaluate(ps => {
    window.__dexFake = { provider_list: ps.map(p => p.id === "gateway" ? { ...p, hasToken: true, models: [{ id: "old-model", label: "Eski model" }] } : p) };
  }, ps);
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.getByRole("combobox", { name: "Yeni üyenin sağlayıcısı" }).selectOption("gateway");
  await app.getByRole("button", { name: "Girişi yönet", exact: true }).click();
  await app.getByRole("tab", { name: "Gelişmiş", exact: true }).click();
  const chain = app.getByRole("group", { name: "Motor zinciri", exact: true });
  const panel = app.locator(".team-settings").filter({ hasText: team.name });
  await chain.locator("summary").focus();
  await app.keyboard.press("Enter");
  const add = chain.getByRole("button", { name: "Claude Code + OpenAI zincire ekle" });
  await expect(add).toBeEnabled();

  // Hold actual transport responses; release them only after the newer request rendered.
  await app.evaluate(() => {
    const send = WebSocket.prototype.send;
    const held: { socket: WebSocket; id: number; result: unknown }[] = [];
    window.__dexFake!.__held = held;
    WebSocket.prototype.send = function(data) {
      if (typeof data === "string") {
        const message = JSON.parse(data);
        if (message.method === "provider_list" && window.__dexFake!.__hold) {
          held.push({ socket: this, id: message.id, result: structuredClone(window.__dexFake!.provider_list) });
          return;
        }
      }
      return send.call(this, data);
    };
  });
  for (const signedIn of [false, true]) {
    await app.evaluate(() => {
      window.__dexFake!.__hold = true;
      for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "providers-changed", payload: null }) }));
    });
    await expect.poll(() => app.evaluate(() => (window.__dexFake!.__held as unknown[]).length)).toBeGreaterThanOrEqual(2);
    await app.evaluate(signedIn => {
      window.__dexFake!.__hold = false;
      window.__dexFake!.provider_list = (window.__dexFake!.provider_list as { id: string }[]).map(p => p.id === "gateway" ? { ...p, hasToken: signedIn, models: signedIn ? [{ id: "new-model", label: "Yeni model" }] : [] } : p);
      window.dispatchEvent(new CustomEvent("dex:gateway-catalog-changed", { detail: "local" }));
    }, signedIn);
    const models = panel.getByRole("combobox", { name: "Yeni üyenin modeli", includeHidden: true }).locator("option");
    if (signedIn) { await expect(add).toBeEnabled(); await expect(models).toHaveText(["Yeni model"]); }
    else { await expect(add).toBeDisabled(); await expect(chain).toContainText("Giriş yok"); await expect(models).toHaveCount(0); }
    await app.evaluate(async () => {
      const held = window.__dexFake!.__held as { socket: WebSocket; id: number; result: unknown }[];
      for (const { socket, id, result } of held.splice(0)) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ id, result }) }));
      await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    });
    if (signedIn) { await expect(add).toBeEnabled(); await expect(chain).toContainText("Yeni model"); await expect(models).toHaveText(["Yeni model"]); }
    else { await expect(add).toBeDisabled(); await expect(chain).toContainText("Giriş yok"); await expect(models).toHaveCount(0); }
  }
});

test("Gateway: ChatGPT girişi, hesap kataloğu ve çıkış klavyeyle çalışır", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway giriş");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.evaluate(() => {
    window.__dexFake = {
      gateway_auth_status: { state: "signed_out", activeAccountId: null, accounts: [], models: [], error: null },
      gateway_auth_start: { authorizationUrl: "https://auth.openai.com/oauth/authorize?state=e2e-one-use", expiresAt: Date.now() + 300_000 },
      gateway_auth_logout: { remoteRevoked: false },
      gateway_models: [{ id: "account-model", label: "Hesabın modeli" }],
    };
  });
  await openTeam(app, team.name);
  const terminal = await paneOf(app, leader.name);
  await expect(terminal.locator(".xterm-rows")).toContainText("fake-claude hazir");
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const tab = app.getByRole("tab", { name: "Sağlayıcılar", exact: true });
  await tab.focus();
  await expect(tab).toBeFocused();
  await tab.press("Enter");
  await expect(tab).toHaveAttribute("aria-selected", "true");
  const gateway = app.getByRole("region", { name: "ChatGPT ile giriş (Claude Code + OpenAI)" });
  await expect(gateway).toContainText("Giriş yok");
  const login = gateway.getByRole("button", { name: "ChatGPT ile giriş", exact: true });
  await login.focus();
  await app.evaluate(() => { (window.__dexFake!.gateway_auth_status as { state: string }).state = "signing_in"; });
  await app.keyboard.press("Enter");
  await expect.poll(async () => (await calls(app)).some(c => c.cmd === "plugin:opener|open_url")).toBe(true);
  await expect(gateway).toContainText("Tarayıcıda girişi tamamlayın");
  await app.evaluate(() => {
    window.__dexFake!.gateway_auth_status = {
      state: "ready", activeAccountId: "test-account", accounts: [{ id: "test-account", label: "Test hesabı", email: "test@example.invalid", signedIn: true }],
      models: [], error: null, accessToken: "CANARY-MUST-NOT-RENDER",
    };
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "gateway-auth-changed", payload: null }) }));
  });
  await expect(gateway).toContainText("Bağlı");
  await expect(gateway).toContainText("test@example.invalid");
  await expect(gateway).toContainText("Hesabın modeli");
  await expect(gateway).not.toContainText("CANARY-MUST-NOT-RENDER");
  await expect(gateway.locator("dt", { hasText: "Plan" })).toHaveCount(0);
  await expect(gateway.locator("dt", { hasText: "Jeton süresi" })).toHaveCount(0);
  expect((await calls(app)).some(c => c.cmd === "gateway_models")).toBe(true);
  await app.evaluate(() => {
    window.__dexFake!.gateway_models = [{ id: "renewed-model", label: "Yeni hesap modeli" }];
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "gateway-auth-changed", payload: null }) }));
  });
  await expect(gateway).toContainText("Yeni hesap modeli");
  // The desktop's dock has minimum workspace widths; its supported small-window settings flow is a float.
  await app.getByRole("tab", { name: "Ayarlar", exact: true }).click({ button: "right" });
  await app.getByRole("menuitem", { name: "Yüzdür" }).click();
  await expect(app.getByRole("dialog", { name: "Ayarlar" })).toBeVisible();
  await app.setViewportSize({ width: 420, height: 760 });
  await expect(gateway).toBeVisible();
  expect(await gateway.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  const bounds = await gateway.evaluate(el => { const r = el.getBoundingClientRect(); return { left: r.left, right: r.right, viewport: innerWidth }; });
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(bounds.viewport);
  const logout = gateway.getByRole("button", { name: "Çıkış yap", exact: true });
  await logout.focus();
  await expect(logout).toBeFocused();
  await app.evaluate(() => { window.__dexFake!.gateway_auth_status = { state: "signed_out", activeAccountId: null, accounts: [], models: [], error: null }; });
  await app.keyboard.press("Enter");
  await expect(gateway).toContainText("Giriş yok");
  await expect(gateway).toContainText("ChatGPT");
  await expect(gateway).toContainText("bağlı uygulamalar");
});

test("Gateway: üye hesap modelini seçer; zincire yalnız açık seçimle eklenir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway seçim");
  const agents = await teamAgents(dexd, team.id);
  const leader = agents.find(a => a.kind === "leader")!;
  const member = agents.find(a => a.kind === "worker")!;
  const allAgents = await dexd.call<{ id: string }[]>("agent_list");
  const ps = await dexd.call<{ id: string; models: unknown[]; hasToken: boolean }[]>("provider_list");
  const chain = await dexd.call<{ order: string[]; disabled: string[]; exhausted: unknown[] }>("chain_get");
  await app.evaluate(({ ps, chain, member, allAgents }) => {
    window.__dexFake = {
      provider_list: ps.map(p => p.id === "gateway" ? { ...p, hasToken: true, models: [{ id: "account-model", label: "Hesabın modeli" }] } : p),
      chain_set_order: { ...chain, order: [...chain.order, "gateway"] },
      gateway_auth_status: { state: "ready", activeAccountId: "account", accounts: [{ id: "account", label: "Test", email: null, signedIn: true }], models: [{ id: "account-model", label: "Hesabın modeli" }], error: null },
      gateway_models: [{ id: "account-model", label: "Hesabın modeli" }],
      agent_update: { ...member, engine: "claude", provider: "gateway", model: "account-model" },
      agent_list: allAgents.map(a => a.id === member.id ? { ...a, engine: "claude", provider: "gateway", model: "account-model" } : a),
    };
  }, { ps, chain, member, allAgents });
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.getByRole("combobox", { name: `${member.name} sağlayıcısı`, exact: true }).selectOption("gateway");
  await expect(app.getByRole("combobox", { name: `${member.name} modeli`, exact: true })).toHaveValue("account-model");
  const patch = (await calls(app)).find(c => c.cmd === "agent_update" && c.args.id === member.id)!;
  expect(patch.args.patch).toEqual({ provider: "gateway", model: "account-model" });
  const provider = app.getByRole("combobox", { name: "Yeni üyenin sağlayıcısı" });
  await provider.selectOption("gateway");
  const models = app.getByRole("combobox", { name: "Yeni üyenin modeli" });
  await expect(models).toHaveValue("account-model");
  await expect(models.locator("option")).toHaveText(["Hesabın modeli"]);
  await app.evaluate(() => {
    window.__dexFake!.provider_list = (window.__dexFake!.provider_list as { id: string; models: unknown[]; hasToken: boolean }[]).map(p => p.id === "gateway" ? { ...p, hasToken: false, models: [] } : p);
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "providers-changed", payload: null }) }));
  });
  await expect(models.locator("option")).toHaveCount(0);
  await expect(app.locator(".team-add").getByRole("button", { name: "Ekle", exact: true })).toBeDisabled();
  await app.evaluate(() => {
    window.__dexFake!.provider_list = (window.__dexFake!.provider_list as { id: string; models: unknown[]; hasToken: boolean }[]).map(p => p.id === "gateway" ? { ...p, hasToken: true, models: [{ id: "account-model", label: "Hesabın modeli" }] } : p);
    window.dispatchEvent(new CustomEvent("dex:gateway-catalog-changed", { detail: "local" }));
  });
  await expect(models.locator("option")).toHaveText(["Hesabın modeli"]);
  await expect(app.getByRole("row", { name: /Claude Code \+ OpenAI/ }).getByRole("button", { name: "Jeton değiştir" })).toHaveCount(0);
  await app.evaluate(() => {
    window.__dexOld = { unknown: ["provider_list"] };
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "providers-changed", payload: null }) }));
  });
  await expect(app.locator(".team-settings").getByRole("alert")).toContainText("Sağlayıcı listesi alınamadı");
  await expect(models.locator("option")).toHaveText(["Hesabın modeli"]);
  await app.evaluate(() => {
    window.__dexOld = undefined;
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "providers-changed", payload: null }) }));
  });
  await expect(app.locator(".team-settings").getByRole("alert")).toHaveCount(0);
  await app.getByRole("button", { name: "Girişi yönet", exact: true }).click();
  await expect(app.getByRole("tab", { name: "Sağlayıcılar", exact: true })).toHaveAttribute("aria-selected", "true");
  await app.getByRole("tab", { name: "Gelişmiş", exact: true }).click();
  const editor = app.getByRole("group", { name: "Motor zinciri", exact: true });
  await editor.locator("summary").click();
  await expect(editor.getByRole("button", { name: "Claude Code + OpenAI zincire ekle" })).toBeVisible();
  expect((await calls(app)).filter(c => c.cmd === "chain_set_order")).toHaveLength(0);
  await editor.getByRole("button", { name: "Claude Code + OpenAI zincire ekle" }).click();
  await expect(editor).toContainText("Hesabın modeli");
  await app.setViewportSize({ width: 420, height: 760 });
  expect(await editor.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  const added = (await calls(app)).find(c => c.cmd === "chain_set_order")!;
  expect(added.args.order).toEqual([...chain.order, "gateway"]);
});

test("Gateway: plan/süre metadatası gösterilir; tarayıcı hatasında giriş yeniden denenebilir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway metadata");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.evaluate(() => {
    window.__dexFake = {
      gateway_auth_status: { state: "ready", activeAccountId: "a", accounts: [{ id: "a", label: "Test hesabı", email: null, signedIn: true, plan: "Plus", expiresAt: 1_800_000_000_000 }], models: [], error: null },
      gateway_models: [],
      gateway_auth_start: { authorizationUrl: "https://auth.openai.com/oauth/authorize", expiresAt: Date.now() + 300_000 },
    };
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (cmd: string, args: Record<string, unknown>) => cmd === "plugin:opener|open_url" ? Promise.reject("opener unavailable") : invoke(cmd, args);
  });
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  await app.getByRole("tab", { name: "Sağlayıcılar", exact: true }).click();
  const gateway = app.getByRole("region", { name: "ChatGPT ile giriş (Claude Code + OpenAI)" });
  await expect(gateway.locator("dt", { hasText: "Plan" })).toBeVisible();
  await expect(gateway).toContainText("Plus");
  await expect(gateway.locator("dt", { hasText: "Jeton süresi" })).toBeVisible();
  await expect(gateway).toContainText("kullanılabilir model yok");
  await app.evaluate(() => { (window.__dexFake!.gateway_auth_status as { state: string }).state = "signing_in"; });
  await gateway.getByRole("button", { name: "Başka hesapla giriş" }).click();
  await expect(gateway.getByRole("alert")).toContainText("Tarayıcı açılamadı");
  await expect(gateway.getByRole("button", { name: "Başka hesapla giriş" })).toBeEnabled();
});

test("Gateway: boş katalog seçimi engeller; kota ve erişim hataları görünür", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway hatalar");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  await app.evaluate(() => {
    window.__dexFake = { gateway_auth_status: { state: "error", activeAccountId: null, accounts: [], models: [], error: "gateway_auth_rate_limited" } };
  });
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.getByRole("combobox", { name: "Yeni üyenin sağlayıcısı" }).selectOption("gateway");
  await expect(app.getByRole("combobox", { name: "Yeni üyenin modeli" }).locator("option")).toHaveCount(0);
  await expect(app.locator(".team-add").getByRole("button", { name: "Ekle", exact: true })).toBeDisabled();
  await app.getByRole("button", { name: "Girişi yönet", exact: true }).click();
  const gateway = app.getByRole("region", { name: "ChatGPT ile giriş (Claude Code + OpenAI)" });
  await expect(gateway.getByRole("alert")).toContainText("Kota");
  await app.evaluate(() => {
    (window.__dexFake!.gateway_auth_status as { error: string }).error = "gateway_account_ineligible";
    for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event: "gateway-auth-changed", payload: null }) }));
  });
  await expect(gateway.getByRole("alert")).toContainText("erişim");
  await expect(gateway.locator("dt", { hasText: "Plan" })).toHaveCount(0);
  await expect(gateway.locator("dt", { hasText: "Jeton süresi" })).toHaveCount(0);
});

test("Gateway: kalıcı auth hatası açık giriş ve takım panellerini yeniler", async ({ app, dexd }) => {
  const team = await newTeam(dexd, "Gateway auth olayları");
  const leader = (await teamAgents(dexd, team.id)).find(a => a.kind === "leader")!;
  const ps = await dexd.call<{ id: string }[]>("provider_list");
  await app.evaluate(ps => {
    window.__dexFake = {
      provider_list: ps.map(p => p.id === "gateway" ? { ...p, hasToken: true, models: [{ id: "model", label: "Hesap modeli" }] } : p),
      gateway_auth_status: { state: "ready", activeAccountId: "a", accounts: [{ id: "a", label: "Test", email: null, signedIn: true }], models: [], error: null },
      gateway_models: [{ id: "model", label: "Hesap modeli" }],
    };
  }, ps);
  await openTeam(app, team.name);
  await paneOf(app, leader.name);
  await app.getByRole("button", { name: "Takım ayarları" }).click();
  await app.getByRole("combobox", { name: "Yeni üyenin sağlayıcısı" }).selectOption("gateway");
  const panel = app.locator(".team-settings").filter({ hasText: team.name });
  const models = panel.getByRole("combobox", { name: "Yeni üyenin modeli", includeHidden: true }).locator("option");
  await expect(models).toHaveText(["Hesap modeli"]);
  await app.getByRole("button", { name: "Girişi yönet", exact: true }).click();
  const gateway = app.getByRole("region", { name: "ChatGPT ile giriş (Claude Code + OpenAI)" });
  await expect(gateway).toContainText("Bağlı");
  await expect(gateway).toContainText("Hesap modeli");
  await app.evaluate(() => {
    window.__dexFake!.gateway_auth_status = { state: "error", activeAccountId: null, accounts: [{ id: "a", label: "Test", email: null, signedIn: false }], models: [], error: "gateway_auth_rejected" };
    window.__dexFake!.provider_list = (window.__dexFake!.provider_list as { id: string }[]).map(p => p.id === "gateway" ? { ...p, hasToken: false, models: [] } : p);
    for (const event of ["gateway-auth-changed", "providers-changed"]) {
      for (const socket of window.__dexSockets ?? []) socket.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ event, payload: null }) }));
    }
  });
  await expect(gateway).toContainText("Giriş yok");
  await expect(gateway.getByRole("alert")).toContainText("erişim");
  await expect(gateway).not.toContainText("Hesap modeli");
  await expect(gateway.getByRole("button", { name: "Çıkış yap", exact: true })).toHaveCount(0);
  await expect(models).toHaveCount(0);
  await expect(panel.locator(".team-add").getByRole("button", { name: "Ekle", exact: true, includeHidden: true })).toBeDisabled();
  await expect(panel.getByRole("row", { name: /Claude Code \+ OpenAI/, includeHidden: true })).toContainText("giriş yok");
});
