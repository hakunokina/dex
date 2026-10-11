import { test } from "node:test";
import assert from "node:assert/strict";
import { browserTokensAllowed, webTokenCommand } from "./webTokens.ts";

function storage() {
  const values = new Map<string, string>();
  return {
    getItem: (k: string) => values.get(k) ?? null,
    setItem: (k: string, v: string) => { values.set(k, v); },
    removeItem: (k: string) => { values.delete(k); },
  } as Storage;
}

test("production public/LAN browser cannot persist or read a device token", async () => {
  for (const origin of ["https://server.example.invalid", "http://192.168.1.2", "http://10.0.0.2", "https://ts.net.attacker.invalid", "null", "tauri://localhost"]) {
    const s = storage();
    s.setItem("dex.webtoken.test", "synthetic-device-token");
    assert.equal(browserTokensAllowed(false, origin), false, origin);
    assert.equal(await webTokenCommand("host_token_get", { id: "test" }, s, false, origin), null);
    await assert.rejects(webTokenCommand("host_token_set", { id: "test", token: "replacement" }, s, false, origin), /tarayıcı.*yerel\/Tailscale/);
    assert.equal(s.getItem("dex.webtoken.test"), "synthetic-device-token");
    await webTokenCommand("host_token_delete", { id: "test" }, s, false, origin);
    assert.equal(s.getItem("dex.webtoken.test"), null);
  }
});

test("DEV or production loopback/tailnet browser retains pairing storage", async () => {
  for (const [dev, origin] of [[true, "https://server.example.invalid"], [false, "http://localhost:1420"], [false, "http://127.0.0.1"], [false, "http://[::1]"], [false, "http://100.64.0.1"], [false, "https://fixture.ts.net"]] as const) {
    const s = storage();
    assert.equal(browserTokensAllowed(dev, origin), true);
    await webTokenCommand("host_token_set", { id: "test", token: "synthetic-device-token" }, s, dev, origin);
    assert.equal(await webTokenCommand("host_token_get", { id: "test" }, s, dev, origin), "synthetic-device-token");
    await webTokenCommand("host_token_delete", { id: "test" }, s, dev, origin);
    assert.equal(s.getItem("dex.webtoken.test"), null);
  }
});
