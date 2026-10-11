// #247: which leaders' Core strips are open and on screen right now. The corner decision lists hide
// those leaders' cards (the balloon carries them); compact / hidden / off-screen → the corner shows them.
// One registration per mounted strip (a leader may have more than one pane).
// #252: the office registers the leaders whose big Core is on screen; their balloon opens there (above
// Core) and the strip keeps only its sentence and `? n`. Off-screen office → the strip's balloon again.

import { useSyncExternalStore } from "react";
import type { StripMode } from "./coreState";

/** A strip covers its leader's cards only while open and visible (team tab active, dock tab in front). */
export const stripCovers = (mode: StripMode, inView: boolean) => mode === "open" && inView;

/** #252 (leader: a card never shows twice): the strip's own balloon — only while no on-screen office Core carries this leader's cards. */
export const stripBalloonOpen = (mode: StripMode, cards: number, deferred: boolean, inOffice: boolean) =>
  mode !== "hidden" && cards > 0 && !deferred && !inOffice;

const shown = new Map<object, string>();
const offices = new Map<object, readonly string[]>();
let officeSnap: ReadonlySet<string> = new Set();
let snapshot: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();

function publish() {
  officeSnap = new Set([...offices.values()].flat());
  snapshot = new Set([...shown.values(), ...officeSnap]);
  for (const l of listeners) l();
}

/** `leader` = the strip covers that leader's cards now; `null` = it does not (or it unmounted). */
export function setCoreShown(token: object, leader: string | null) {
  if ((shown.get(token) ?? null) === leader) return;
  if (leader === null) shown.delete(token);
  else shown.set(token, leader);
  publish();
}

/** #252: `leaders` = the office's big Cores on screen now; `null` = the office is hidden (or unmounted). */
export function setOfficeShown(token: object, leaders: readonly string[] | null) {
  const was = offices.get(token);
  if ((was ?? null) === leaders || (was && leaders && was.join("\n") === leaders.join("\n"))) return;
  if (leaders === null) offices.delete(token);
  else offices.set(token, leaders);
  publish();
}

/** Leaders whose cards a balloon (strip or office) carries — the corner lists hide those. */
export const coveredLeaders = () => snapshot;
export const officeLeaders = () => officeSnap;
export const subscribeCoreShown = (l: () => void) => (listeners.add(l), () => void listeners.delete(l));
/** The leaders whose open, on-screen strip or office Core carries their cards (the corner lists hide those). */
export const useCoveredLeaders = () => useSyncExternalStore(subscribeCoreShown, coveredLeaders);
export const useOfficeLeaders = () => useSyncExternalStore(subscribeCoreShown, officeLeaders);

// ---- Esc deferral, per leader: shared by the strip's `? n` and whichever balloon is open ----

let deferred: ReadonlyMap<string, ReadonlySet<string>> = new Map();
const deferListeners = new Set<() => void>();

/** `keys` = the cards on hand when Esc was pressed; `null` = open again. */
export function setDeferred(leader: string, keys: readonly string[] | null) {
  const next = new Map(deferred);
  if (keys === null) {
    if (!next.delete(leader)) return;
  } else next.set(leader, new Set(keys));
  deferred = next;
  for (const l of deferListeners) l();
}

/** The balloon stays shut until a card arrives that was not on hand at the deferral. */
export const isDeferred = (leader: string, keys: readonly string[]) => {
  const set = deferred.get(leader);
  return !!set && keys.every((k) => set.has(k));
};
export const deferredMap = () => deferred;
export const useDeferredMap = () =>
  useSyncExternalStore((l) => (deferListeners.add(l), () => void deferListeners.delete(l)), deferredMap);
