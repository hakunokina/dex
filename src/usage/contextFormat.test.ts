// #212: context göstergesi eşikleri ve metinleri.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { contextFill, contextLevel, contextText, contextTitle, contextTokens } from "./contextFormat.ts";
import type { AgentContext } from "./costTypes.ts";

const ctx = (tokens: number, window: number | null = 1_000_000, updatedAt: number | null = 1_000): AgentContext => ({
  tokens, window, model: "claude-opus-5-5", sessionId: "s", updatedAt,
});

describe("contextLevel", () => {
  it("mutlak eşikler: <250k nötr, ≥250k sarı, ≥500k turuncu, ≥750k kırmızı", () => {
    assert.equal(contextLevel(0), "ok");
    assert.equal(contextLevel(249_999), "ok");
    assert.equal(contextLevel(250_000), "warn");
    assert.equal(contextLevel(499_999), "warn");
    assert.equal(contextLevel(500_000), "high");
    assert.equal(contextLevel(749_999), "high");
    assert.equal(contextLevel(750_000), "critical");
    assert.equal(contextLevel(2_000_000), "critical");
  });
  it("pencereden bağımsız (200k pencerede 190k hâlâ nötr)", () => {
    assert.equal(contextLevel(ctx(190_000, 200_000).tokens), "ok");
  });
});

describe("contextText", () => {
  it("sayı / pencere", () => {
    assert.equal(contextText(ctx(312_400)), "312k / 1M");
    assert.equal(contextText(ctx(1_234_567, 1_048_576)), "1.2M / 1M");
    assert.equal(contextText(ctx(850, 200_000)), "850 / 200k");
  });
  it("pencere bilinmiyor → yalnız sayı; withWindow=false → yalnız sayı", () => {
    assert.equal(contextText(ctx(312_400, null)), "312k");
    assert.equal(contextText(ctx(312_400), false), "312k");
  });
  it("ölçüm yok → —", () => {
    assert.equal(contextText(ctx(0, 1_000_000, null)), "— / 1M");
  });
  it("contextTokens yuvarlama", () => {
    assert.equal(contextTokens(999), "999");
    assert.equal(contextTokens(1_500), "2k");
    assert.equal(contextTokens(1_000_000), "1M");
  });
});

describe("contextFill / contextTitle", () => {
  it("doluluk oranı, taşmada 1, pencere yoksa null", () => {
    assert.equal(contextFill(ctx(250_000)), 0.25);
    assert.equal(contextFill(ctx(3_000_000)), 1);
    assert.equal(contextFill(ctx(250_000, null)), null);
  });
  it("hover: model, pencere, son güncelleme ve eşik ipucu", () => {
    const t = contextTitle(ctx(800_000, 1_000_000, 0), 5 * 60_000);
    assert.match(t, /Model: claude-opus-5-5/);
    assert.match(t, /Pencere: 1M/);
    assert.match(t, /Son güncelleme: 5 dk önce/);
    assert.match(t, /temiz oturum zamanı/);
    assert.match(contextTitle(ctx(0, null, null)), /Pencere: bilinmiyor\nSon güncelleme: henüz ölçüm yok$/);
  });
});
