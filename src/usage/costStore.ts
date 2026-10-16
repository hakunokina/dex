// #209: takım başına maliyet özeti (usage_summary, dex-core::cost). Poll yok: izlenen takım bir tur
// bitince (hook idle), devretme değişince ya da sunucu yeniden bağlanınca yenilenir. Eski dexd
// (`auth.hello` listesinde usage_summary yok / "bilinmeyen metot") → "unsupported": UI hiçbir şey göstermez.
import { useEffect, useSyncExternalStore } from "react";
import { callHost, hostSupports, onHostUp, splitId } from "../api/hosts";
import { findAgent } from "../agents";
import { kept, listenKept } from "../hot";
import type { UsageSummary, UsageTotal } from "./costTypes";
import { dayStart } from "./costFormat";

const hot = import.meta.hot;

export interface TeamCost {
  status: "loading" | "ready" | "error" | "unsupported";
  today?: UsageSummary;
  /** Tüm zamanlar — görev kartları devretme toplamını buradan alır. */
  all?: UsageSummary;
  error?: string;
}

const LOADING: TeamCost = { status: "loading" };
const DEBOUNCE_MS = 1500;

const teams = kept(hot, "teams", () => new Map<string, TeamCost>());
const watchers = kept(hot, "watchers", () => new Map<string, number>());
const timers = kept(hot, "timers", () => new Map<string, number>());
const subs = kept(hot, "subs", () => new Set<() => void>());
const emit = () => subs.forEach((f) => f());

function set(teamId: string, v: TeamCost) {
  teams.set(teamId, v);
  emit();
}

async function load(teamId: string) {
  const [host, team] = splitId(teamId);
  if (!hostSupports(host, "usage_summary")) return set(teamId, { status: "unsupported" });
  const now = Date.now();
  const today = dayStart(now);
  const ask = (since?: number) => callHost<UsageSummary>(host, "usage_summary", since === undefined ? { teamId: team } : { teamId: team, since });
  try {
    // Sıralı: ilk çağrı transcript'leri artımsal okur, sonrakiler yalnız SQLite toplar.
    const all = await ask();
    const day = await ask(today);
    set(teamId, { status: "ready", today: day, all });
  } catch (e) {
    const error = String(e);
    if (error.includes("bilinmeyen metot")) return set(teamId, { status: "unsupported" });
    const prev = teams.get(teamId);
    // Son iyi veri kalır; ilk yükleme hatası görünür olur.
    set(teamId, prev?.status === "ready" ? { ...prev, error } : { status: "error", error });
  }
}

function schedule(teamId: string, delay = DEBOUNCE_MS) {
  if (!watchers.get(teamId)) return;
  clearTimeout(timers.get(teamId));
  timers.set(teamId, window.setTimeout(() => {
    timers.delete(teamId);
    void load(teamId);
  }, delay));
}

const refreshHost = (host: string) => {
  for (const id of watchers.keys()) if (splitId(id)[0] === host) schedule(id);
};

// Token'lar tur sonunda transcript'e yazılır: idle'a geçiş = yeni veri.
listenKept<{ status?: string }>(hot, "dex-hook", ({ payload }, host) => {
  if (payload.status === "idle") refreshHost(host);
});
listenKept<unknown>(hot, "delegations-changed", (_e, host) => refreshHost(host));
kept(hot, "costUp", () => onHostUp(refreshHost));

/** Takımı izlemeye al (bileşen ömrü boyunca); ilk izleyici hemen yükler. */
export function watchTeamCost(teamId: string): () => void {
  const n = watchers.get(teamId) ?? 0;
  watchers.set(teamId, n + 1);
  if (!n) schedule(teamId, 0); // önbellekteki eski özet görünür kalır, arkada tazelenir
  return () => {
    const left = (watchers.get(teamId) ?? 1) - 1;
    if (left > 0) watchers.set(teamId, left);
    else watchers.delete(teamId);
  };
}

const subscribe = (f: () => void) => {
  subs.add(f);
  return () => void subs.delete(f);
};

export function useTeamCost(teamId: string | null): TeamCost | null {
  useEffect(() => (teamId ? watchTeamCost(teamId) : undefined), [teamId]);
  return useSyncExternalStore(subscribe, () => (teamId ? teams.get(teamId) ?? LOADING : null));
}

/** Kart çipi / hover: ajanın bugünkü toplamı (izlenen takım yüklüyse). */
export function agentToday(agentId: string): { total: UsageTotal; warnings: string[] } | null {
  const teamId = findAgent(agentId)?.teamId;
  const today = teamId ? teams.get(teamId)?.today : undefined;
  const total = today?.agents[splitId(agentId)[1]];
  return total ? { total, warnings: today!.warnings } : null;
}
