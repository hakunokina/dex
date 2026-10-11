export interface CoreStatus {
  version: number;
  capturedAtMs: number;
  maintenance?: {
    day: string;
    startedAtMs: number;
    finished: boolean;
    deleted: number;
    bytes: number;
    skipped: Record<string, number>;
  } | null;
  agents: {
    agentId: string;
    teamId: string;
    status: "offline" | "idle" | "working" | "waiting";
    lastHookSeq: number | null;
    lastHookAtMs: number | null;
    lastProcessedHookSeq: number | null;
    pty: { alive: boolean; pid: number | null; generation: number | null; startedAtMs: number | null };
    delegations: { id: number; status: "queued" | "running" }[];
    questions: { id: number; status: "open" | "escalated" }[];
    idleClose: { armed: boolean; generation: number | null; quietPeriodMs: number; silentForMs: number };
    chain: { disabled: boolean; temporary: boolean; returnPending: boolean; transitioning: boolean };
  }[];
}
