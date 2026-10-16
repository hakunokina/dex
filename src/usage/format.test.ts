// #118: hover kartı metinleri.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { resetShort, severity, tokenCell, updatedAgo } from "./format.ts";

describe("updatedAgo", () => {
  it("null boş döner", () => assert.equal(updatedAgo(null, 1_000), ""));
  it("1 dk altı 'az önce'", () => assert.equal(updatedAgo(1_000, 30_000), "az önce güncellendi"));
  it("dakika", () => assert.equal(updatedAgo(0, 5 * 60_000), "5 dk önce güncellendi"));
  it("saat", () => assert.equal(updatedAgo(0, 3 * 3_600_000), "3 sa önce güncellendi"));
  it("gün", () => assert.equal(updatedAgo(0, 2 * 86_400_000), "2 gün önce güncellendi"));
});

describe("resetShort", () => {
  it("null/bozuk boş döner", () => {
    assert.equal(resetShort(null, 0), "");
    assert.equal(resetShort("xxx", 0), "");
  });
  it("saat + dakika", () => {
    const iso = new Date(4 * 3_600_000 + 46 * 60_000).toISOString();
    assert.equal(resetShort(iso, 0), "Sıfırlanma 4 sa 46 dk");
  });
  it("dakika", () => {
    const iso = new Date(35 * 60_000).toISOString();
    assert.equal(resetShort(iso, 0), "Sıfırlanma 35 dk");
  });
});

describe("severity", () => {
  const p = (five: number | null, week: number | null) => ({
    id: "glm", note: null, measuredAt: null, todayTokens: null,
    fiveHour: five === null ? null : { remainingPercent: five, resetsAt: null },
    weekly: week === null ? null : { remainingPercent: week, resetsAt: null },
  });
  it("ölçüm yoksa unknown", () => assert.equal(severity(p(null, null)), "unknown"));
  it("az kalan pencere belirler", () => {
    assert.equal(severity(p(90, 8)), "critical");
    assert.equal(severity(p(20, 90)), "low");
    assert.equal(severity(p(null, 60)), "ok");
  });
});

describe("tokenCell (#2139)", () => {
  const snap = { providers: [], todayTokens: 1_500, weekTokens: 9_000, updatedAt: 0 };
  it("bugün", () => {
    const c = tokenCell(snap, "today");
    assert.deepEqual([c.cap, c.tokens], ["bugün", 1_500]);
    assert.match(c.title, /^Bugün Dex ajanları: 1\.500 token .*tıkla: 7 gün$/);
  });
  it("7 gün", () => {
    const c = tokenCell(snap, "week");
    assert.deepEqual([c.cap, c.tokens], ["7 gün", 9_000]);
    assert.match(c.title, /^Son 7 gün Dex ajanları: 9\.000 token .*tıkla: bugün$/);
  });
  it("eski dexd: weekTokens yok → null", () => {
    const { weekTokens: _, ...old } = snap;
    assert.equal(tokenCell(old, "week").tokens, null);
    assert.match(tokenCell(old, "week").title, /: -- token/);
  });
});
