// #411: Kart'ta liderin etiketi ("Ana Core") ve köşe imzası.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { chipNameStyle, coreLabel, OFFLINE_NAME_ALPHA, PREFIX_MAX, SIGNATURE_MAX, teamSignature } from "./label.ts";

describe("coreLabel", () => {
  it("takım adının ilk kelimesi liderin adının başına", () => {
    assert.equal(coreLabel("Ana Takım", "Core"), "Ana Core");
    assert.equal(coreLabel("Jez Dev", "Core"), "Jez Core");
    assert.equal(coreLabel("  Jez   Dev  ", "Core"), "Jez Core");
  });
  it("tek kelimelik takım adı olduğu gibi", () => {
    assert.equal(coreLabel("Coucou", "Core"), "Coucou Core");
  });
  it("ad yoksa ya da aynı kelimeyse yalnız liderin adı", () => {
    assert.equal(coreLabel("", "Core"), "Core");
    assert.equal(coreLabel("   ", "Core"), "Core");
    assert.equal(coreLabel(undefined, "Core"), "Core");
    assert.equal(coreLabel("core ekibi", "Core"), "Core");
  });
  it("uzun kelime kısaltılır", () => {
    const label = coreLabel("Süperuzunkelimelitakım X", "Core");
    assert.equal(label, "Süperuzunke… Core");
    assert.equal([...label.split(" ")[0]].length, PREFIX_MAX);
  });
  it("12 karakterlik kelime kesilmez", () => {
    assert.equal(coreLabel("Abcdefghijkl", "Core"), "Abcdefghijkl Core");
  });
});

describe("teamSignature", () => {
  it("boşluklar tek, uzun ad kısaltılır, boş ad imzasız", () => {
    assert.equal(teamSignature(" Ana   Takım "), "Ana Takım");
    assert.equal(teamSignature(""), "");
    assert.equal(teamSignature(null), "");
    const long = teamSignature("a".repeat(40));
    assert.equal([...long].length, SIGNATURE_MAX);
    assert.ok(long.endsWith("…"));
  });
});

describe("chipNameStyle", () => {
  it("lider her durumda takım renginde; offline yalnız soluk", () => {
    assert.deepEqual(chipNameStyle(true, false), { color: "teamHi", alpha: null });
    assert.deepEqual(chipNameStyle(true, true), { color: "teamHi", alpha: OFFLINE_NAME_ALPHA });
  });
  it("worker'ın rengi ve opaklığı değişmez", () => {
    assert.deepEqual(chipNameStyle(false, false), { color: "text", alpha: null });
    assert.deepEqual(chipNameStyle(false, true), { color: "text", alpha: null });
  });
  it("offline soluklukta kırmızı --team-hi kapalı çipte ≥4.5:1", () => {
    const rgb = (h: string) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
    const mix = (a: number[], b: number[], t: number) => a.map((v, i) => v * t + b[i] * (1 - t));
    const lum = (c: number[]) => {
      const [r, g, b] = c.map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    // App.css: --pcb-board, --pcb-chip-off (çip 0.55 opak), --red-hi (en koyu --team-hi).
    const bg = mix(rgb("#131618"), rgb("#15191a"), 0.55);
    const fg = mix(rgb("#ff5c5f"), bg, OFFLINE_NAME_ALPHA);
    assert.ok((lum(fg) + 0.05) / (lum(bg) + 0.05) >= 4.5);
  });
});
