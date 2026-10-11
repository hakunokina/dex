export function heartbeatView(at: number | null, status: string, now: number): { label: string; quiet: boolean } | null {
  if (status === "offline") return null;
  if (at === null) return { label: "—", quiet: false };
  const elapsed = Math.max(0, now - at);
  const seconds = Math.floor(elapsed / 1000);
  return { label: seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}dk`, quiet: status === "working" && elapsed > 120_000 };
}
