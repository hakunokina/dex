// Hover kartının Türkçe metinleri (UsageStrip.tsx kullanır; saf fonksiyonlar → birim testli).
// Pencere adları TM hub'ının `kind` alanından gelir (session/daily/weekly/billing).

export const WINDOW_SHORT: Record<string, string> = {
  session: "Oturum",
  daily: "Günlük",
  weekly: "Haftalık",
  billing: "Dönem",
};

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
