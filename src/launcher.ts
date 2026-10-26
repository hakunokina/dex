// Office → pane bridge. The pane bound to `agentId` starts its Claude session and flags `handled`.
import { callHost, splitId } from "./api/hosts";
import { startOnce } from "./startOnce";

export function startAgent(agentId: string, options: { cols?: number; rows?: number; fresh?: boolean } = {}) {
  const [host, id] = splitId(agentId);
  return startOnce(agentId, () => callHost(host, "agent_start", { id, options }));
}

/** #2464: a leader's session in the patron's pick — its own method, an older dexd lacks it (`hostSupports`). */
export function startLeader(agentId: string, projectId: string, options: { cols?: number; rows?: number; fresh?: boolean } = {}) {
  const [host, id] = splitId(agentId);
  return startOnce(agentId, () => callHost(host, "leader_start", { id, projectId: splitId(projectId)[1], options }));
}

export interface StartRequest {
  agentId: string;
  handled: boolean;
}

export const START_EVENT = "dex:start";

/** Returns false when no pane is bound to the agent (nothing will start). */
export function requestStart(agentId: string): boolean {
  const detail: StartRequest = { agentId, handled: false };
  window.dispatchEvent(new CustomEvent(START_EVENT, { detail }));
  return detail.handled;
}

// Dock leader bar → the column pane bound to `agentId` takes focus and flashes (DEX-10b).
export const FOCUS_EVENT = "dex:focus";

export function requestFocus(agentId: string) {
  window.dispatchEvent(new CustomEvent(FOCUS_EVENT, { detail: agentId }));
}
