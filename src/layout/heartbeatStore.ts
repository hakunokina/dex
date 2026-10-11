import { callHost, onHostUp, qid } from "../api/hosts";
import type { CoreStatus } from "../api/status";
import { kept, listenKept } from "../hot";

const hot = import.meta.hot;
interface Pulse { at: number; sequence: number }
const state = kept(hot, "heartbeat", () => ({
  pulses: new Map<string, Pulse>(),
  revisions: new Map<string, number>(),
  listeners: new Set<() => void>(),
  now: Date.now(),
  timer: null as ReturnType<typeof setInterval> | null,
}));
const changed = () => state.listeners.forEach(fn => fn());
export const heartbeatOf = (id: string) => state.pulses.get(id) ?? null;
export const heartbeatNow = () => state.now;

/** One clock for all mounted tab indicators; hooks still notify immediately. */
export function subscribeHeartbeat(fn: () => void) {
  state.listeners.add(fn);
  if (state.timer === null) {
    state.now = Date.now();
    state.timer = setInterval(() => { state.now = Date.now(); changed(); }, 1000);
  }
  return () => {
    state.listeners.delete(fn);
    if (!state.listeners.size && state.timer !== null) {
      clearInterval(state.timer);
      state.timer = null;
    }
  };
}

listenKept<{ agentId: string }>(hot, "dex-hook", ({ payload }, host) => {
  const id = qid(host, payload.agentId);
  state.revisions.set(id, (state.revisions.get(id) ?? 0) + 1);
  state.pulses.set(id, { at: Date.now(), sequence: (heartbeatOf(id)?.sequence ?? 0) + 1 });
  state.now = Date.now();
  changed();
});
listenKept<{ agentId: string; status: string }>(hot, "agent-status", ({ payload }, host) => {
  if (payload.status === "offline") {
    const id = qid(host, payload.agentId);
    state.revisions.set(id, (state.revisions.get(id) ?? 0) + 1);
    state.pulses.delete(id);
    changed();
  }
});

// Restore silence age on initial connection/reconnect, without replaying a heartbeat.
kept(hot, "heartbeatUp", () => onHostUp(host => {
  const before = new Map(state.revisions);
  void callHost<CoreStatus>(host, "core_status").then(snapshot => {
    const now = Date.now();
    for (const a of snapshot.agents) {
      const id = qid(host, a.agentId);
      if (state.revisions.get(id) !== before.get(id)) continue; // a live hook or exit won the race
      if (a.status === "offline") state.pulses.delete(id);
      else if (a.lastHookAtMs !== null) {
        state.pulses.set(id, { at: now - Math.max(0, snapshot.capturedAtMs - a.lastHookAtMs), sequence: 0 });
      }
    }
    changed();
  }).catch(() => { /* Older/unavailable cores: live hooks remain the source; unknown age stays explicit. */ });
}));
