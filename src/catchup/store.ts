// #2235: one catchup snapshot per team, shared by the tab badge and the panel page.
// Fetched once when first asked for; refreshed only on host events, coalesced — every
// `catchup` call scans the memory graph, so there is deliberately no polling. The first
// fetch's `since` is kept for the entry's life (snapshot: rows never slide out).
import { useSyncExternalStore } from "react";
import { ackCatchup, fetchCatchup, type Catchup } from "../api/catchup";
import { splitId } from "../api/hosts";
import { kept, listenKept } from "../hot";

export interface CatchupEntry {
  status: "loading" | "ready" | "unsupported" | "error";
  data: Catchup | null;
  error: string | null;
}

const hot = import.meta.hot;
const box = kept(hot, "catchup:entries", () => ({ map: new Map<string, CatchupEntry>() }));
const inflight = kept(hot, "catchup:inflight", () => new Set<string>());
const timers = kept(hot, "catchup:timers", () => new Map<string, ReturnType<typeof setTimeout>>());
const listeners = kept(hot, "catchup:listeners", () => new Set<() => void>());

function publish(teamId: string, entry: CatchupEntry) {
  box.map = new Map(box.map).set(teamId, entry);
  for (const l of listeners) l();
}

async function fetchInto(teamId: string) {
  if (inflight.has(teamId)) return;
  const prev = box.map.get(teamId);
  inflight.add(teamId);
  if (!prev) publish(teamId, { status: "loading", data: null, error: null });
  try {
    const data = await fetchCatchup(teamId, prev?.data?.since);
    publish(teamId, data ? { status: "ready", data, error: null } : { status: "unsupported", data: null, error: null });
  } catch (e) {
    // A failed refresh keeps the snapshot on screen; only a failed first fetch is an error state.
    if (prev?.data) publish(teamId, { status: "ready", data: prev.data, error: String(e) });
    else publish(teamId, { status: "error", data: null, error: String(e) });
  } finally {
    inflight.delete(teamId);
  }
}

/** First fetch for a team (later calls are no-ops; retries after an error go through `retryCatchup`). */
export function ensureCatchup(teamId: string) {
  if (!box.map.has(teamId)) void fetchInto(teamId);
}

export function retryCatchup(teamId: string) {
  box.map.delete(teamId);
  void fetchInto(teamId);
}

/** Event-driven refresh, coalesced per team (the events below can burst). */
const REFRESH_MS = 1500;
export function refreshCatchup(teamId: string) {
  if (!box.map.has(teamId)) return; // never fetched: nothing on screen to keep fresh
  clearTimeout(timers.get(teamId));
  timers.set(teamId, setTimeout(() => {
    timers.delete(teamId);
    void fetchInto(teamId);
  }, REFRESH_MS));
}

const refreshHost = (host: string) => {
  for (const teamId of box.map.keys()) if (splitId(teamId)[0] === host) refreshCatchup(teamId);
};
for (const ev of ["delegations-changed", "questions-changed", "proposals-changed", "gpu-requests-changed", "tasks-changed"]) {
  listenKept<unknown>(hot, ev, (_e, host) => refreshHost(host));
}

/**
 * Advance the cursor to the snapshot the patron actually saw (`shownAt` — its
 * `generatedAt`, pinned by the caller when its wait started, never "now" and never a
 * refresh that landed in the meantime; a job that finished after that snapshot must
 * still show up next time). The list itself stays.
 */
export async function ackShown(teamId: string, shownAt: number): Promise<boolean> {
  if (!box.map.get(teamId)?.data) return false;
  await ackCatchup(teamId, shownAt);
  return true;
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};

export const useCatchup = (teamId: string): CatchupEntry | undefined =>
  useSyncExternalStore(subscribe, () => box.map.get(teamId));
