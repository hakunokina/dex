// #54: kullanım şeridinin sağlayıcı-bağımsız verisi (src-tauri/src/usage.rs UsageSnapshot). Şerit
// kaynağı bilmez: Aşama 1'de Token Monitor hub'ı, Aşama 2'de Dex'in kendi kullanım modülü.

export type UsageStatus = "connecting" | "live" | "offline" | "unauthorized";

export interface ProviderUsage {
  /** claude, codex, zai, muse… */
  id: string;
  /** Birincil pencerede kalan %; null = şu an bildirmiyor ("--"). */
  remainingPercent: number | null;
  /** session (5 saat) | daily | weekly | billing */
  window: string | null;
  /** ISO-8601 */
  resetsAt: string | null;
  /** TM'nin durum etiketi ("High Usage" gibi); hub yollamadıkça boş. */
  statusLabel?: string | null;
}

export interface UsageSnapshot {
  source: string;
  status: UsageStatus;
  /** Çıktı token/sn; `rateAt` anındaki örnek. */
  tokPerSec: number | null;
  /** epoch ms */
  rateAt: number | null;
  todayTokens: number | null;
  todayCostUsd: number | null;
  providers: ProviderUsage[];
  hasSecret: boolean;
  /** epoch ms */
  updatedAt: number | null;
}

export const OFFLINE: UsageSnapshot = {
  source: "",
  status: "offline",
  tokPerSec: null,
  rateAt: null,
  todayTokens: null,
  todayCostUsd: null,
  providers: [],
  hasSecret: false,
  updatedAt: null,
};
