// #209: Pano/Kart maliyet metinleri (usage_summary). Saf fonksiyonlar → birim testli; sayı biçimi
// kullanım şeridiyle aynı (221.7M, $94.0 gibi).
import type { UsageSummary, UsageTokens, UsageTotal } from "./costTypes";

export const COST_HINT = "API eşdeğer maliyeti — abonelik faturası değil";

export const tokenSum = (t: UsageTokens) => t.input + t.output + t.cacheRead + t.cacheWrite;

export function tokensText(n: number): string {
  const units: [number, string][] = [[1e9, "B"], [1e6, "M"], [1e3, "K"]];
  for (const [v, u] of units) if (n >= v) return `${(n / v).toFixed(1)}${u}`;
  return String(Math.round(n));
}

export function usdText(usd: number): string {
  if (usd > 0 && usd < 0.01) return "<$0.01";
  return `$${usd >= 100 ? usd.toFixed(0) : usd.toFixed(2)}`;
}

/** "12.3K tok · $0.41"; fiyat bilinmiyorsa yalnız token; hiç token yoksa "". `unit: false` = çip için "12.3K · $0.41". */
export function costText(total: UsageTotal | null | undefined, unit = true): string {
  if (!total || !tokenSum(total.tokens)) return "";
  const tok = `${tokensText(tokenSum(total.tokens))}${unit ? " tok" : ""}`;
  return total.usd === null ? tok : `${tok} · ${usdText(total.usd)}`;
}

/** Kırılım: girdi/çıktı, cache, $ (tek satıra ya da alt alta). */
export function breakdownParts(total: UsageTotal): string[] {
  const t = total.tokens;
  return [
    `Girdi ${tokensText(t.input)} · Çıktı ${tokensText(t.output)}`,
    `Cache okuma ${tokensText(t.cacheRead)} · yazma ${tokensText(t.cacheWrite)}`,
    total.usd === null ? "$ bilinmiyor (fiyatı girilmemiş model)" : usdText(total.usd),
  ];
}

/** Hover'ın tek satırı: "girdi 1.2K · çıktı 3.0K · cache 40.0K/2.0K". */
export const breakdownLine = (t: UsageTokens) =>
  `girdi ${tokensText(t.input)} · çıktı ${tokensText(t.output)} · cache ${tokensText(t.cacheRead)}/${tokensText(t.cacheWrite)}`;

export const warningText = (warnings: string[]) => (warnings.length ? `Eksik olabilir: ${warnings.length} transcript okunamadı` : null);

/** Hover kırılımı + ipucu, alt alta. */
export function costBreakdown(total: UsageTotal, warnings: string[] = []): string {
  return [...breakdownParts(total), COST_HINT, warningText(warnings)].filter(Boolean).join("\n");
}

const ZERO: UsageTotal = { tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, usd: 0 };

/** Görevin devretmelerinin toplamı; tokenı olan herhangi birinin fiyatı yoksa usd null. Hiç kayıt yoksa null. */
export function sumDelegations(ids: number[], summary: UsageSummary | null | undefined): UsageTotal | null {
  if (!summary) return null;
  let out: UsageTotal | null = null;
  for (const id of ids) {
    const d = summary.delegations[String(id)];
    if (!d || !tokenSum(d.tokens)) continue;
    const acc: UsageTotal = out ?? ZERO;
    out = {
      tokens: {
        input: acc.tokens.input + d.tokens.input,
        output: acc.tokens.output + d.tokens.output,
        cacheRead: acc.tokens.cacheRead + d.tokens.cacheRead,
        cacheWrite: acc.tokens.cacheWrite + d.tokens.cacheWrite,
      },
      usd: acc.usd === null || d.usd === null ? null : acc.usd + d.usd,
    };
  }
  return out;
}

/** Yerel gece yarısı (epoch ms) — "bugün"ün başı. */
export function dayStart(now: number): number {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}
