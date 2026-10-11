// #118: hover kartı metinleri.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { WINDOW_SHORT, resetShort, updatedAgo } from "./format.ts";

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

describe("WINDOW_SHORT", () => {
  it("TM kind → Türkçe", () => {
    assert.equal(WINDOW_SHORT.session, "Oturum");
    assert.equal(WINDOW_SHORT.weekly, "Haftalık");
  });
});
