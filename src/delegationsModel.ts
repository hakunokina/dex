import type { Delegation } from "./delegations";

export function roleQueueSummary(list: readonly Delegation[], teamId: string, roles: readonly { slug: string; name: string }[] = []) {
  const queued = list.filter(d => d.teamId === teamId && !d.toAgent && d.status === "queued"
    && d.targetRole && d.queuePosition != null);
  const groups = new Map<string, Delegation[]>();
  for (const d of queued) {
    const jobs = groups.get(d.targetRole!) ?? [];
    jobs.push(d);
    groups.set(d.targetRole!, jobs);
  }
  const title = [...groups].map(([role, jobs]) => {
    jobs.sort((a, b) => a.queuePosition! - b.queuePosition! || a.id - b.id);
    const name = roles.find(r => r.slug === role)?.name ?? role;
    return `${name}: ${jobs.map(d => `#${d.id} (${d.queuePosition})`).join(", ")}`;
  }).join("\n");
  return { count: queued.length, title };
}

export const isActive = (d: Delegation) => d.status === "queued" || d.status === "running";

/** Active delegations sent by (`from`) or given to (`to`) the agent. */
export const activeCount = (list: Delegation[], agentId: string, side: "from" | "to") =>
  list.filter((d) => isActive(d) && (side === "from" ? d.fromAgent === agentId : !!d.toAgent && d.toAgent === agentId)).length;

/** The worker's newest delegation, when finished and its result has not been taken. */
export function unreadResult(list: Delegation[], agentId: string, taken: (d: Delegation) => boolean): Delegation | null {
  if (!agentId) return null;
  let last: Delegation | null = null;
  for (const d of list) if (d.toAgent === agentId && (!last || d.id > last.id)) last = d;
  return last && !isActive(last) && !taken(last) ? last : null;
}
