// #212: pane üst çubuğundaki context doluluk göstergesi (agent_context/team_context). Saf → birim testli.
// Renk eşikleri mutlak token sayısıdır, yüzde değil: patron "Temiz oturum" zamanını pencereden bağımsız görsün.
import type { AgentContext } from "./costTypes";

export type ContextLevel = "ok" | "warn" | "high" | "critical";

/** Alt sınırlar (dahil), büyükten küçüğe. */
export const CONTEXT_LEVELS: [number, ContextLevel][] = [[750_000, "critical"], [500_000, "high"], [250_000, "warn"]];

export function contextLevel(tokens: number): ContextLevel {
  for (const [min, level] of CONTEXT_LEVELS) if (tokens >= min) return level;
  return "ok";
}

/** "312k", "1M", "1.2M", "850" — gösterge dar, ondalık yalnız M'de. */
export function contextTokens(n: number): string {
  if (n >= 1e6) return `${Number((n / 1e6).toFixed(1))}M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}k`;
  return String(Math.round(n));
}

/** "312k / 1M"; pencere bilinmiyorsa yalnız sayı; ölçüm yoksa "—". */
export function contextText(c: AgentContext, withWindow = true): string {
  const used = c.updatedAt === null ? "—" : contextTokens(c.tokens);
  return withWindow && c.window ? `${used} / ${contextTokens(c.window)}` : used;
}

/** Çubuğun doluluğu 0..1; pencere bilinmiyorsa null (tahmini pencereyle yüzde hesaplanmaz). */
export const contextFill = (c: AgentContext): number | null => (c.window ? Math.min(1, c.tokens / c.window) : null);

const LEVEL_HINT: Record<ContextLevel, string | null> = {
  ok: null,
  warn: "250k üstü — temiz oturum düşünülebilir",
  high: "500k üstü — temiz oturum önerilir",
  critical: "750k üstü — temiz oturum zamanı",
};

/** Hover/aria metni: doluluk, model, pencere, son güncelleme. */
export function contextTitle(c: AgentContext, now = Date.now()): string {
  const updated = c.updatedAt === null ? "henüz ölçüm yok" : `${ago(now - c.updatedAt)} (${new Date(c.updatedAt).toLocaleTimeString()})`;
  return [
    `Context: ${c.updatedAt === null ? "—" : c.tokens.toLocaleString("tr-TR")} token`,
    `Model: ${c.model || "—"}`,
    `Pencere: ${c.window ? contextTokens(c.window) : "bilinmiyor"}`,
    `Son güncelleme: ${updated}`,
    c.updatedAt === null ? null : LEVEL_HINT[contextLevel(c.tokens)],
  ].filter(Boolean).join("\n");
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return "az önce";
  if (s < 3600) return `${Math.floor(s / 60)} dk önce`;
  return `${Math.floor(s / 3600)} sa önce`;
}
