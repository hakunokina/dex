type Job = { id: number; toAgent: string; status: string };
type Box = { left: number; top: number; width: number; height: number };
export type WorkerRows = { working: Set<string>; done?: string };
export function workerGroups(office: Box, groups: readonly (Box & { id: string })[]): WorkerRows {
  if (!office.width || !office.height) return { working: new Set() };
  const below = groups.filter(g => g.width > 0 && g.height > 0 && Math.abs(g.top - office.top - office.height) <= 8
    && g.left < office.left + office.width && g.left + g.width > office.left);
  const working = below.sort((a, b) => b.left - a.left)[0];
  if (!working) return { working: new Set() };
  const done = groups.find(g => g.id !== working.id && g.width > 0 && g.height > 0
    && Math.abs(g.left + g.width - working.left) <= 8 && Math.abs(g.top - working.top) <= 8);
  return { working: new Set([working.id]), ...(done ? { done: done.id } : {}) };
}
/**
 * #2267: the patron's rule is positional — the group under the Kart is the working row, its left
 * neighbour the finished row. A recorded role (panel params / this session) is kept while it agrees
 * with that geometry; a record that puts the "done" row under the Kart (a working row that emptied and
 * went away, or a stale save) is dropped and the geometry decides. No geometry (hidden team, nothing
 * under the Kart) → the record stands. Returns `recorded` itself when nothing changes.
 */
export function reconcileWorkerRows(recorded: WorkerRows, geo: WorkerRows | undefined): WorkerRows {
  const under = geo && [...geo.working][0];
  if (!under) return recorded;
  if (recorded.working.has(under)) {
    if (recorded.done || !geo.done || recorded.working.has(geo.done)) return recorded;
    return { working: recorded.working, done: geo.done };
  }
  const done = geo.done ?? (recorded.done !== under ? recorded.done : undefined);
  return { working: new Set([under]), ...(done ? { done } : {}) };
}
/**
 * What a job transition does to the worker's panel: a closed panel opens in the working row when its
 * job starts (#2267); an open one moves between the rows it is in; a placement outside both rows is
 * the patron's and stays. `groupId` null = the panel is closed.
 */
export function workerStep(groupId: string | null, rows: WorkerRows, finished: boolean): "open" | "working" | "done" | null {
  if (groupId === null) return finished ? null : "open";
  if (finished) return rows.working.has(groupId) ? "done" : null;
  return groupId === rows.done ? "working" : null;
}
export function workerMoves(before: readonly Job[], after: readonly Job[]): { agentId: string; finished: boolean }[] {
  const previous = new Map(before.map(job => [job.id, job]));
  const moves: { agentId: string; finished: boolean }[] = [];
  for (const agentId of new Set(after.map(job => job.toAgent))) {
    const jobs = after.filter(job => job.toAgent === agentId);
    const running = jobs.filter(job => job.status === "running");
    if (running.length) {
      if (running.some(job => previous.get(job.id)?.status !== "running")) moves.push({ agentId, finished: false });
    } else if (jobs.some(job => {
      const old = previous.get(job.id);
      return old && (old.status === "running" || old.status === "queued") && ["done", "done_no_report", "failed"].includes(job.status);
    })) moves.push({ agentId, finished: true });
  }
  return moves;
}
/** A move's circuit trace is drawn over the whole window: only for the team on screen, never a hidden one. */
export const movePlaysFx = (teamId: string, activeTeamId: string, reducedMotion: boolean) => teamId === activeTeamId && !reducedMotion;
