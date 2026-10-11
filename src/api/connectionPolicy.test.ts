import { test } from "node:test";
import assert from "node:assert/strict";
import { normalizeUrl } from "./connectionPolicy.ts";

test("remote addresses default to WSS", () => {
  for (const address of ["server.example.invalid:7717", "192.168.1.2:7717", "10.0.0.2:7717"]) {
    assert.equal(new URL(normalizeUrl(address)).protocol, "wss:");
  }
});

test("WS stays available on loopback and tailnet", () => {
  for (const host of ["localhost", "LOCALHOST.", "127.0.0.1", "127.255.255.255", "[::1]", "100.64.0.1", "fixture.ts.net"]) {
    for (const address of [`${host}:7717`, `ws://${host}:7717`]) {
      assert.equal(new URL(normalizeUrl(address)).protocol, "ws:", address);
    }
  }
});

test("explicit WS is rejected outside loopback and tailnet", () => {
  for (const host of ["server.example.invalid", "192.0.2.10", "192.168.1.2", "10.0.0.2", "ts.net", "ts.net.attacker.invalid", "localhost.attacker.invalid", "[::]", "[::ffff:192.0.2.10]"]) {
    assert.throws(() => normalizeUrl(`ws://${host}:7717`), /şifresiz bağlantı yalnız yerel\/Tailscale/);
  }
  assert.throws(() => normalizeUrl("ws://127.0.0.1@server.example.invalid:7717"));
});

test("WSS remains allowed; malformed addresses, credentials and other schemes are rejected", () => {
  assert.equal(normalizeUrl(" WSS://server.example.invalid:7717/rpc "), "wss://server.example.invalid:7717/rpc");
  for (const address of ["", "http://server.example.invalid", "https://server.example.invalid", "file:///tmp/test", "wss://synthetic:credential@server.example.invalid", "wss://server.example.invalid/#fragment"]) {
    assert.throws(() => normalizeUrl(address), undefined, address);
  }
});
