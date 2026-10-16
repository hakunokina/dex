import { callHost, onEvent, qOpt, rawId, splitId } from "../api/hosts";
import type { Unlisten } from "../api/transport";
import { getActiveTeamId, subscribe as onTeams } from "../team/store";

export type TaskStatus = "todo" | "doing" | "done";

export interface Task {
  id: number;
  title: string;
  description: string;
  status: TaskStatus;
  agentId: string | null;
  teamId: string | null;
  /** #838: the project the task belongs to (qualified); absent from older hosts. */
  projectId?: string | null;
  /** #2104: the card's newest job (its number), from the host's DB; absent from older hosts. */
  delegationId?: number | null;
  /** #2104: a closed card that is not done work: the leader's cancel or a same-titled replacement. */
  closedReason?: "cancelled" | "superseded" | null;
  createdAt: number;
  updatedAt: number;
}

export interface NewTask {
  title: string;
  description?: string;
  status?: TaskStatus;
  agentId?: string | null;
}

/** Absent key = unchanged; `agentId: null` clears the assignment. */
export type TaskPatch = Partial<Pick<Task, "title" | "description" | "status" | "agentId">>;

// The board shows the team on screen; new tasks land in it. S4: on that team's host — task ids are
// the host's, agent/team ids qualified here.
const onScreen = () => splitId(getActiveTeamId());
const fromHost = (host: string, t: Task): Task => ({
  ...t, agentId: qOpt(host, t.agentId), teamId: qOpt(host, t.teamId), projectId: qOpt(host, t.projectId),
});
const bareAgent = <T extends { agentId?: string | null }>(x: T): T => (x.agentId ? { ...x, agentId: rawId(x.agentId) } : x);

export async function listTasks() {
  const [host, team] = onScreen();
  return (await callHost<Task[]>(host, "task_list", { teamId: team || null })).map((t) => fromHost(host, t));
}
export async function createTask(task: NewTask) {
  const [host, team] = onScreen();
  return fromHost(host, await callHost<Task>(host, "task_create", { task: { ...bareAgent(task), teamId: team || null } }));
}
export async function updateTask(id: number, patch: TaskPatch) {
  const [host] = onScreen();
  return fromHost(host, await callHost<Task>(host, "task_update", { id, patch: bareAgent(patch) }));
}
export async function deleteTask(id: number) {
  const [host] = onScreen();
  return callHost<void>(host, "task_delete", { id });
}

/** Fires after any mutation, from the UI or (later) from agents via MCP, and when the team on screen changes. */
export async function onTasksChanged(cb: () => void): Promise<Unlisten> {
  let team = getActiveTeamId();
  const offTeams = onTeams(() => {
    if (getActiveTeamId() !== team) {
      team = getActiveTeamId();
      cb();
    }
  });
  const off = onEvent("tasks-changed", cb);
  return () => {
    off();
    offTeams();
  };
}
