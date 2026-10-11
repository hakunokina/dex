export const SOUND_LABELS = {
  complete: "Tur tamamlandı",
  attention: "Patrona soru / üye önerisi",
  warning: "Takıldı / izin bekliyor / kota tükendi",
  delegation: "İş devredildi (siuv)",
  worker: "Worker işi bitti",
};
export type SoundKind = keyof typeof SOUND_LABELS;
interface DelegationSoundRow { key: string; status: string }
export function createDelegationSoundTracker() {
  const hosts = new Map<string, Map<string, string>>();
  return {
    seed(host: string, rows: readonly DelegationSoundRow[]) {
      if (!hosts.has(host)) hosts.set(host, new Map(rows.map((d) => [d.key, d.status])));
    },
    change(host: string, row: DelegationSoundRow): SoundEvent | null {
      const statuses = hosts.get(host);
      if (!statuses) return null; // no initial snapshot yet: events are history too
      const previous = statuses.get(row.key);
      statuses.set(row.key, row.status);
      if (previous === undefined && (row.status === "queued" || row.status === "running")) return "delegation_started";
      if (previous !== undefined && previous !== "done" && row.status === "done") return "worker_done";
      return null;
    },
  };
}
const EVENT_SOUNDS = {
  team_turn_done: "complete",
  question_escalated: "attention",
  member_proposed: "attention",
  agent_stalled: "warning",
  agent_waiting: "warning",
  chain_exhausted: "warning",
  delegation_started: "delegation",
  worker_done: "worker",
} as const satisfies Record<string, SoundKind>;
export type SoundEvent = keyof typeof EVENT_SOUNDS;
export function soundForEvent(event: string): SoundKind | null {
  return Object.prototype.hasOwnProperty.call(EVENT_SOUNDS, event) ? EVENT_SOUNDS[event as SoundEvent] : null;
}
export interface SoundSettings {
  enabled: boolean;
  volume: number;
  events: Record<SoundKind, boolean>;
}
export function createSoundGate() {
  const last = new Map<SoundKind, number>();
  return (kind: SoundKind, settings: SoundSettings, now: number) => {
    if (!settings.enabled || settings.volume === 0 || !settings.events[kind]) return false;
    const previous = last.get(kind);
    if (previous !== undefined && now - previous < 800) return false;
    last.set(kind, now);
    return true;
  };
}
export function defaultSoundSettings(): SoundSettings {
  return { enabled: true, volume: 0.5, events: { complete: true, attention: true, warning: true, delegation: true, worker: false } };
}
export function normalizeSoundSettings(raw: unknown): SoundSettings {
  const next = defaultSoundSettings();
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return next;
  const value = raw as Partial<SoundSettings>;
  if (typeof value.enabled === "boolean") next.enabled = value.enabled;
  if (typeof value.volume === "number" && Number.isFinite(value.volume)) next.volume = Math.max(0, Math.min(1, value.volume));
  for (const kind of Object.keys(SOUND_LABELS) as SoundKind[]) {
    if (typeof value.events?.[kind] === "boolean") next.events[kind] = value.events[kind];
  }
  return next;
}
