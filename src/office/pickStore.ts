// #2443: Kart'ta worker seçim modu — "Tüm worker'lar" popover'ı açar/kapatır, Office tıklama ve
// sürüklemeyle doldurur ve seçilileri vurgular. `teamId` null = mod kapalı.
import { useSyncExternalStore } from "react";

export interface PickState { teamId: string | null; ids: ReadonlySet<string> }

let state: PickState = { teamId: null, ids: new Set() };
const listeners = new Set<() => void>();

function emit(next: PickState) {
  state = next;
  for (const l of listeners) l();
}

export const subscribePick = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
export const getPick = () => state;
export const usePick = () => useSyncExternalStore(subscribePick, getPick);
/** Is the Kart of `teamId` picking workers now? */
export const picking = (teamId: string) => state.teamId === teamId;

export const startPick = (teamId: string, ids: Iterable<string>) => emit({ teamId, ids: new Set(ids) });
export const endPick = () => { if (state.teamId) emit({ teamId: null, ids: new Set() }); };

export function togglePick(id: string) {
  const ids = new Set(state.ids);
  if (!ids.delete(id)) ids.add(id);
  emit({ ...state, ids });
}

export function addPicks(add: string[]) {
  if (!add.length) return;
  emit({ ...state, ids: new Set([...state.ids, ...add]) });
}
