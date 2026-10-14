import { test } from "node:test";
import assert from "node:assert/strict";
import { providerScopeKey } from "./providerScope.ts";

test("aynı sağlayıcı farklı sunucuda farklı karttır (taslak taşınmaz)", () => {
  const a = providerScopeKey("local", "openai");
  const b = providerScopeKey("uzak-sunucu", "openai");
  assert.notEqual(a, b);
});

test("aynı sunucu + sağlayıcı kararlıdır (gereksiz remount yok)", () => {
  assert.equal(providerScopeKey("local", "openai"), providerScopeKey("local", "openai"));
});

test("aynı sunucuda farklı sağlayıcı farklı karttır", () => {
  assert.notEqual(providerScopeKey("local", "openai"), providerScopeKey("local", "local"));
});
