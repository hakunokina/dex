/** Transcript accounting from dex-core; both local and remote hosts expose these RPCs. */
export interface UsageTokens {
  /** Uncached input; cached input is counted only in cacheRead. */
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

export interface UsageTotal {
  tokens: UsageTokens;
  /** null if any contributing provider/model has no configured price. */
  usd: number | null;
}

export interface UsageSummary {
  costLabel: "API eşdeğer maliyeti";
  total: UsageTotal;
  /** Host-local agent IDs; qualify them when rendering across hosts. */
  agents: Record<string, UsageTotal>;
  delegations: Record<string, UsageTotal>;
  /** A missing/unreadable transcript leaves persisted totals available but incomplete. */
  warnings: string[];
}

/** USD per million tokens, exact provider + transcript model matching. */
export interface UsagePrice {
  provider: string;
  model: string;
  rates: { input: number; output: number; cacheRead: number; cacheWrite: number } | null;
}

export interface UsageSummaryParams { teamId: string; since?: number }
export interface UsageDelegationParams { id: number }
export interface UsagePricesParams { prices: UsagePrice[] }

/** Latest request occupancy, never cumulative session usage. */
export interface AgentContext {
  tokens: number;
  window: number | null;
  model: string;
  sessionId: string | null;
  /** Epoch milliseconds of the measured transcript event; null before first usage. */
  updatedAt: number | null;
}
export type TeamContext = Record<string, AgentContext>;
export interface AgentContextEvent { agentId: string; teamId: string; context: AgentContext }
