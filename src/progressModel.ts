// #2055: a worker's progress on its running job, from its own Claude task list (core `progress.rs`).
// Pure: which job's progress counts, the percentage and the one-line text. No list → no progress.
import type { Delegation } from "./delegations";

/** Core payload (`delegation-progress` / `delegation_progress`), `agentId` qualified. */
export interface Progress {
  agentId: string;
  delegationId: number;
  total: number;
  done: number;
  /** The step in progress, or null. */
  active: string | null;
  /** Unix ms of the last list change. */
  updatedAt: number;
}

type Job = Pick<Delegation, "id" | "host" | "status" | "toAgent">;

export const progressKey = (host: string, delegationId: number) => `${host}:${delegationId}`;

/** Only a running job shows progress; an empty list is none. */
export function jobProgress(d: Job, all: ReadonlyMap<string, Progress>): Progress | null {
  if (d.status !== "running") return null;
  const p = all.get(progressKey(d.host, d.id));
  return p && p.total > 0 ? p : null;
}

/** The worker's newest running job's progress. */
export function workerProgress(agentId: string, list: readonly Job[], all: ReadonlyMap<string, Progress>): Progress | null {
  let job: Job | null = null;
  for (const d of list) if (d.toAgent === agentId && d.status === "running" && (!job || d.id > job.id)) job = d;
  return job && jobProgress(job, all);
}

export const percent = (p: Pick<Progress, "done" | "total">) => (p.total > 0 ? Math.round((100 * Math.min(p.done, p.total)) / p.total) : 0);

export function ago(ms: number): string {
  const min = Math.floor(Math.max(0, ms) / 60_000);
  if (min < 1) return "az önce";
  if (min < 60) return `${min} dk önce`;
  return `${Math.floor(min / 60)} sa önce`;
}

/** "%43 · 3/7 · Testleri koşuyor · 2 dk önce". */
export function progressLine(p: Progress, now: number): string {
  return [`%${percent(p)}`, `${p.done}/${p.total}`, p.active, ago(now - p.updatedAt)].filter(Boolean).join(" · ");
}
