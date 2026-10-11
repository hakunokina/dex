import type { Delegation } from "../delegations";
import type { Task } from "./api";

export function taskRoleName(
  task: Pick<Task, "id" | "agentId">,
  host: string,
  delegations: readonly Pick<Delegation, "taskId" | "host" | "targetRole">[],
  roles: readonly { slug: string; name: string }[],
): string | null {
  if (task.agentId) return null;
  const role = delegations.find(d => d.taskId === task.id && d.host === host && d.targetRole)?.targetRole;
  return role ? roles.find(r => r.slug === role)?.name ?? role : null;
}
