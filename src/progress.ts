// #2055: workers' task-list progress per host (core `delegation-progress` / `delegation_progress`).
// Shown on the board card, under the worker's label in the office and in its pane tab. Never sent
// to the leader.
import { useSyncExternalStore } from "react";
import { callHost, onHostUp, qid } from "./api/hosts";
import { kept, listenKept } from "./hot";
import type { Delegation } from "./delegations";
import { jobProgress, progressKey, workerProgress as findWorkerProgress, type Progress } from "./progressModel";

const hot = import.meta.hot;
const all = kept(hot, "all", () => new Map<string, Progress>());
const box = kept(hot, "version", () => ({ v: 0 }));
const listeners = kept(hot, "listeners", () => new Set<() => void>());

function changed() {
  box.v++;
  for (const l of listeners) l();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
/** Re-renders on every progress change (read it with `progressOf` / `workerProgress`). */
export const useProgress = () => useSyncExternalStore(subscribe, () => box.v);

const put = (host: string, p: Progress) => all.set(progressKey(host, p.delegationId), { ...p, agentId: qid(host, p.agentId) });

async function reload(host: string) {
  const list = await callHost<Progress[]>(host, "delegation_progress");
  for (const k of [...all.keys()]) if (k.startsWith(`${host}:`)) all.delete(k);
  for (const p of list) put(host, p);
  changed();
}

listenKept<Progress>(hot, "delegation-progress", ({ payload }, host) => {
  put(host, payload);
  changed();
});

// An older host has no `delegation_progress`: no progress, nothing else changes.
kept(hot, "progressUp", () => onHostUp((host) => void reload(host).catch(() => {})));

export const progressOf = (d: Pick<Delegation, "id" | "host" | "status" | "toAgent">) => jobProgress(d, all);
export const workerProgress = (agentId: string, list: readonly Delegation[]) => findWorkerProgress(agentId, list, all);
