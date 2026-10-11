// FIX-4: called on the backend's `agent-session` (a turn is on disk); DEX-1 stores the id with the
// pane record and persists it, so ▶ can `claude --resume <id>` after a restart.
import { setSessionForAgent } from "./layout/store";

export function recordSessionId(agentId: string, sessionId: string, engine: string | null): void {
  setSessionForAgent(agentId, sessionId, engine);
}
