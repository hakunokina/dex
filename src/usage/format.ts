// Hover kartının Türkçe metinleri (UsageStrip.tsx kullanır; saf fonksiyonlar → birim testli).
import type { ProviderUsage } from "./types";

/** #1861: şeridin iki penceresi, gösterim sırasıyla. */
export const WINDOWS = [["fiveHour", "5 saat"], ["weekly", "Haftalık"]] as const;

/** Halkanın tonu: iki pencereden az kalanına göre; ölçüm yoksa "unknown". */
export function severity(p: ProviderUsage): "unknown" | "critical" | "low" | "ok" {
  const left = WINDOWS.map(([k]) => p[k]?.remainingPercent).filter((n): n is number => n !== undefined);
  if (left.length === 0) return "unknown";
  const min = Math.min(...left);
  return min <= 10 ? "critical" : min <= 25 ? "low" : "ok";
}

/** "5 dk önce güncellendi" / "az önce güncellendi"; null = "". */
export function updatedAgo(updatedAt: number | null, now: number): string {
  if (updatedAt === null) return "";
  const mins = Math.max(0, Math.round((now - updatedAt) / 60_000));
  if (mins < 1) return "az önce güncellendi";
  if (mins < 60) return `${mins} dk önce güncellendi`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} sa önce güncellendi`;
  return `${Math.floor(hours / 24)} gün önce güncellendi`;
}

/** "Sıfırlanma 4 sa 46 dk"; geçersiz ISO = "". */
export function resetShort(iso: string | null, now: number): string {
  if (!iso) return "";
  const at = Date.parse(iso);
  if (!Number.isFinite(at)) return "";
  const mins = Math.max(0, Math.round((at - now) / 60_000));
  const rel =
    mins >= 60 * 24
      ? `${Math.floor(mins / 1440)} gün`
      : mins >= 60
        ? `${Math.floor(mins / 60)} sa ${mins % 60} dk`
        : `${mins} dk`;
  return `Sıfırlanma ${rel}`;
}
