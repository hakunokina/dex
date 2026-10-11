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
