// #54/#1861: kullanım şeridinin sağlayıcı-bağımsız verisi (crates/dex-core/src/usage.rs UsageSnapshot).
// dexd üretir (usage_get + usage-snapshot olayı); şerit kaynağı bilmez.

export interface UsageWindow {
  /** Kalan % (0-100). */
  remainingPercent: number;
  /** RFC 3339 */
  resetsAt: string | null;
}

export interface ProviderUsage {
  /** Dex sağlayıcı kimliği: anthropic, glm, muse. */
  id: string;
  /** null = taze ölçüm yok ("--"); nedeni `note`. */
  fiveHour: UsageWindow | null;
  weekly: UsageWindow | null;
  note: string | null;
  /** epoch ms; gösterilen ölçümün anı */
  measuredAt: number | null;
  /** Bugün bu sağlayıcıdaki Dex ajanlarının token'ı. */
  todayTokens: number | null;
}

export interface UsageSnapshot {
  providers: ProviderUsage[];
  /** Bugün tüm sağlayıcılarda Dex ajanlarının token'ı (input + output + cache). */
  todayTokens: number | null;
  /** epoch ms */
  updatedAt: number;
}

/** Şeridin bağlantı durumu: veri yok (sunucu kapalı / eski dexd) ya da canlı. */
export type UsageStatus = "connecting" | "live" | "offline" | "unsupported";
