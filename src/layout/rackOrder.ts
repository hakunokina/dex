// #285: the rack's worker order — the red ones (working/waiting) step up next to the leader, each
// group keeping its own order; when a worker goes quiet it falls back to its old place.
import type { AgentStatus } from "../agents";

/** A chip lights red in these states (rack.css `.working` / `.waiting`). */
export const isActive = (s: AgentStatus | undefined) => s === "working" || s === "waiting";

/** The top bar shows only the red workers: working/waiting, or carrying the red job count. */
export const rackVisible = <T extends { id: string }>(workers: T[], statuses: Record<string, AgentStatus | undefined>, jobs: (id: string) => number): T[] =>
  workers.filter(w => isActive(statuses[w.id]) || jobs(w.id) > 0);

export function rackOrder<T extends { id: string }>(workers: T[], statuses: Record<string, AgentStatus | undefined>): T[] {
  const active = workers.filter(w => isActive(statuses[w.id]));
  return active.length ? [...active, ...workers.filter(w => !isActive(statuses[w.id]))] : workers;
}
