// #209: Pano/Kart maliyet metinleri.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { breakdownLine, costBreakdown, costText, dayStart, sumDelegations, tokensText, usdText } from "./costFormat.ts";
import type { UsageSummary, UsageTotal } from "./costTypes.ts";

const total = (input: number, output: number, cacheRead: number, cacheWrite: number, usd: number | null): UsageTotal => ({
  tokens: { input, output, cacheRead, cacheWrite },
  usd,
});
const summary = (delegations: Record<string, UsageTotal>, warnings: string[] = []): UsageSummary => ({
  costLabel: "API eşdeğer maliyeti",
  total: total(0, 0, 0, 0, 0),
  agents: {},
  delegations,
  warnings,
});

describe("tokensText / usdText", () => {
  it("şeritle aynı birimler", () => {
    assert.equal(tokensText(850), "850");
    assert.equal(tokensText(12_345), "12.3K");
    assert.equal(tokensText(221_700_000), "221.7M");
  });
  it("küçük ve büyük dolar", () => {
    assert.equal(usdText(0), "$0.00");
    assert.equal(usdText(0.004), "<$0.01");
    assert.equal(usdText(0.414), "$0.41");
    assert.equal(usdText(123.4), "$123");
  });
});

describe("costText", () => {
  it("token + $", () => assert.equal(costText(total(1000, 2000, 9000, 300, 0.41)), "12.3K tok · $0.41"));
  it("fiyat bilinmiyorsa yalnız token", () => assert.equal(costText(total(1000, 0, 0, 0, null)), "1.0K tok"));
  it("token yoksa boş", () => {
    assert.equal(costText(total(0, 0, 0, 0, 0)), "");
    assert.equal(costText(null), "");
  });
  it("çip için birimsiz", () => assert.equal(costText(total(1000, 0, 0, 0, 1), false), "1.0K · $1.00"));
});

describe("kırılım", () => {
  it("tek satır", () => assert.equal(breakdownLine(total(1200, 3000, 40_000, 2000, 0).tokens), "girdi 1.2K · çıktı 3.0K · cache 40.0K/2.0K"));
  it("ipucu, bilinmeyen fiyat ve uyarı", () => {
    const text = costBreakdown(total(1, 1, 0, 0, null), ["x"]);
    assert.match(text, /\$ bilinmiyor/);
    assert.match(text, /API eşdeğer maliyeti/);
    assert.match(text, /Eksik olabilir: 1 transcript/);
  });
});

describe("sumDelegations", () => {
  const s = summary({ "7": total(100, 50, 0, 0, 0.5), "8": total(10, 5, 1, 1, 0.25), "9": total(1, 0, 0, 0, null), "10": total(0, 0, 0, 0, null) });
  it("görevin devretmelerini toplar", () => assert.deepEqual(sumDelegations([7, 8], s), total(110, 55, 1, 1, 0.75)));
  it("fiyatsız bir devretme $'ı bilinmez yapar", () => assert.equal(sumDelegations([7, 9], s)!.usd, null));
  it("token'sız fiyatsız kayıt $'ı bozmaz", () => assert.equal(sumDelegations([7, 10], s)!.usd, 0.5));
  it("kayıt yoksa null", () => {
    assert.equal(sumDelegations([99], s), null);
    assert.equal(sumDelegations([7], null), null);
  });
});

it("dayStart yerel gece yarısı", () => {
  const at = new Date(2026, 9, 5, 17, 30).getTime();
  assert.equal(dayStart(at), new Date(2026, 9, 5).getTime());
});
