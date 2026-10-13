// S4: every call names its host — a bare host id for lists/creates, a qualified team/agent id
// (src/api/hosts.ts) otherwise. Records come back with qualified ids and their `host`.
import { callHost, LOCAL, onEvent, qid, qOpt, splitId } from "../api/hosts";
import type { Unlisten } from "../api/transport";
import { chainView } from "./chainView";

export type TeamKind = "main" | "service";
export type TeamProfile = "dev" | "edu";
export type TestLevel = "full" | "light";
export type AgentKind = "leader" | "worker";
/** Görev #120: Claude'un sürümlü kimlikleri; eski takma adlar (opus/sonnet/haiku) DB'de geçerli kalır. */
export type Model = "claude-fable-5-1" | "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-4-5-20251001";

export const MODELS: Model[] = ["claude-fable-5-1", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5-20251001"];

export interface TeamRec {
  /** Qualified (`<host>:<id>` off the local core). */
  id: string;
  /** S4: `local` or a dexd host id — where the team's sessions, memory and code live. */
  host: string;
  name: string;
  kind: TeamKind;
  profile: TeamProfile;
  /** Project folder (main teams only; unique across teams). */
  cwd: string | null;
  createdAt: number;
  /** Where the team's Claude sessions start. */
  workdir: string | null;
  memoryDir: string | null;
  /** Görev #144: workers' jobs in their own git worktree. Absent from an older dexd. */
  worktrees?: boolean;
  /** Absent from older hosts; defaults to full. Independent of Lens/QA membership. */
  testLevel?: TestLevel;
  /** Merged worktrees/orphan folders awaiting cleanup; absent from older cores. */
  worktreeCleanup?: string[];
  /** Modern §7: accent name (src/team/accent.ts); absent → derived from the id. */
  accent?: string | null;
  /** #838: the selected project (qualified). Absent from older hosts. */
  defaultProjectId?: string | null;
  /** #838: the category folder new projects are suggested under (null: app data). */
  workspaceRoot?: string | null;
  suggestedWorkspaceRoot?: string | null;
}

export interface AgentRec {
  id: string;
  host: string;
  teamId: string;
  name: string;
  kind: AgentKind;
  title: string;
  /** Free-form since DEX-13: the provider's catalog decides what is valid (`Model` = anthropic's). */
  model: string;
  shirt: string;
  hair: string;
  skin: string;
  sort: number;
  provider: string;
  /** DEX-26: `claude` | `codex`. */
  engine: string;
  /** DEX-30: bulk change waiting for the running session to end. */
  pending: PendingModel | null;
  roleSlug: string | null;
  /** Catalog name of `roleSlug`; null when unset. */
  roleName: string | null;
  /** DEX-17: zincir kapalı mı + zincirle geçilen geçici sağlayıcı/model (yoksa asıl değer geçerli). */
  chainOff: boolean;
  activeProvider: string | null;
  activeModel: string | null;
  activeEngine: string | null;
  chainReturnReady: boolean;
}

/** DEX-13: provider catalog + token state (src-tauri/src/providers.rs). */
export interface ProviderModel {
  id: string;
  label: string;
  /** Görev #120: still valid, only shown to a member that already uses it. */
  legacy?: boolean;
}
export interface ProviderInfo {
  id: string;
  label: string;
  baseUrl: string | null;
  models: ProviderModel[];
  hasToken: boolean;
  tokenMask: string | null;
}
export interface ProviderImportRow {
  provider: string;
  baseUrl: string | null;
  /** Masked — the raw token never crosses the IPC boundary. */
  token: string | null;
  note: string;
}
// Providers are the host's: a remote team's tokens go to (and stay on) its server.
export const listProviders = (host: string) => callHost<ProviderInfo[]>(host, "provider_list");
export const importProviders = (host: string) => callHost<ProviderImportRow[]>(host, "provider_import_routes");
export const setProviderToken = (host: string, id: string, token: string) => callHost<void>(host, "provider_set_token", { id, token });

/** DEX-25: the local LiteLLM server — its model list is live, never a static catalog. */
export interface LocalModel {
  id: string;
  /** `litellm_params.model`, e.g. `hosted_vllm/qwen3-coder-next`. */
  backend: string | null;
  /** Only rows Dex registered (LiteLLM DB) can be removed; config-file rows are protected. */
  removable: boolean;
}
export interface LocalStatus {
  baseUrl: string;
  models: LocalModel[];
  /** LiteLLM has a DB → `/model/new` works. */
  registry: boolean;
}
export const setLocalBaseUrl = (host: string, url: string) => callHost<void>(host, "provider_local_set_base_url", { url });
export const localStatus = (host: string) => callHost<LocalStatus>(host, "provider_local_status");
export const localAdd = (host: string, name: string) => callHost<LocalStatus>(host, "provider_local_add", { name });
export const localDelete = (host: string, name: string) => callHost<LocalStatus>(host, "provider_local_delete", { name });

/** DEX-17: motor zinciri — kota bitince otomatik sağlayıcı geçişi + resume. */
export interface ChainExhausted {
  provider: string;
  resetAt: number;
  reason: string;
}
export interface ChainView {
  order: string[];
  disabled: string[];
  exhausted: ChainExhausted[];
}
export const chainGet = (host: string) => callHost<Partial<ChainView>>(host, "chain_get").then(chainView);
export const chainSetOrder = (host: string, order: string[]) => callHost<Partial<ChainView>>(host, "chain_set_order", { order }).then(chainView);
export const chainSetDisabled = (host: string, ids: string[]) => callHost<Partial<ChainView>>(host, "chain_set_disabled", { ids }).then(chainView);
export function chainSetAgent(id: string, off: boolean) {
  const [host, raw] = splitId(id);
  return callHost<AgentRec>(host, "chain_set_agent", { id: raw, off }).then((r) => agent(host, r));
}
export function chainBack(id: string) {
  const [host, raw] = splitId(id);
  return callHost<void>(host, "chain_back", { id: raw });
}

/** Role catalog entry (crates/dex-core/seed/roles.json); the system prompt stays in Rust. */
export interface Role {
  slug: string;
  name: string;
  category: string;
  description: string;
}

/** Developer teams default to 1 `lead` + 4 workers with these roles. */
export const DEFAULT_WORKER_ROLES = ["frontend", "backend", "qa", "explorer"];

export interface NewTeam {
  name: string;
  kind: TeamKind;
  profile?: TeamProfile;
  cwd?: string | null;
  /** Developer profile: exactly 4; omitted → DEFAULT_WORKER_ROLES. Edu omits this. */
  workerRoles?: string[];
  members?: TeamMemberTemplate[];
}

export interface TeamMemberTemplate {
  name: string;
  kind: AgentKind;
  title?: string;
  model: string;
  provider?: string | null;
  engine?: "claude" | "codex" | null;
  roleSlug?: string | null;
  chainOff?: boolean;
  effort?: null;
}
export interface TeamCloneTemplate { kind: TeamKind; profile: TeamProfile; members: TeamMemberTemplate[] }
export interface TeamFolderPreview {
  folderName: string;
  workspaceRoot: string;
  mainRoot: string;
  exists: { workspace: boolean; main: boolean };
  owner?: string | null;
}
export const teamFolderPreview = (host: string, name: string) => callHost<TeamFolderPreview>(host, "team_folder_preview", { name });
export function teamCloneTemplate(id: string) {
  const [host, teamId] = splitId(id);
  return callHost<TeamCloneTemplate>(host, "team_clone_template", { teamId });
}

export interface NewAgent {
  teamId: string;
  name: string;
  kind: AgentKind;
  title?: string;
  model: Model | string;
  /** DEX-13: absent → `anthropic`. */
  provider?: string;
  /** Empty title + role → title is the role's name. */
  roleSlug?: string | null;
  /** DEX-26: absent → `claude`. */
  engine?: string;
}

export type TeamPatch = Partial<Pick<TeamRec, "name" | "cwd" | "worktrees" | "workspaceRoot" | "testLevel">>;
export type AgentPatch = Partial<Pick<AgentRec, "name" | "kind" | "title" | "model" | "provider" | "roleSlug" | "engine">>;

const team = (host: string, t: TeamRec): TeamRec => ({ ...t, host, id: qid(host, t.id), defaultProjectId: qOpt(host, t.defaultProjectId) });
const agent = (host: string, a: AgentRec): AgentRec => ({ ...a, host, id: qid(host, a.id), teamId: qid(host, a.teamId) });

export const listTeams = (host: string) => callHost<TeamRec[]>(host, "team_list").then((r) => r.map((t) => team(host, t)));
export const createTeam = (host: string, t: NewTeam) => callHost<TeamRec>(host, "team_create", { team: t }).then((r) => team(host, r));
export function updateTeam(id: string, patch: TeamPatch) {
  const [host, raw] = splitId(id);
  return callHost<TeamRec>(host, "team_update", { id: raw, patch }).then((r) => team(host, r));
}
export function deleteTeam(id: string, stopAgents = false) {
  const [host, raw] = splitId(id);
  return callHost<void>(host, "team_delete", { id: raw, stopAgents });
}
/** Görev #43: what deleting the team would stop. */
export interface TeamDeleteInfo { running: string[]; openDelegations: number; activePlans: number }
/** null: the host's dexd predates `team_delete_info` (its `team_delete` ignores `stopAgents`). */
export function teamDeleteInfo(id: string) {
  const [host, raw] = splitId(id);
  return callHost<TeamDeleteInfo>(host, "team_delete_info", { id: raw }).catch((err) => {
    if (isUnknownMethod(err, "team_delete_info")) return null;
    throw err;
  });
}
/**
 * A dexd answers a method it lacks with `unknown_method`; only the error text crosses the
 * transports (crates/dexd/src/methods.rs, unchanged since the first dexd).
 */
const isUnknownMethod = (err: unknown, method: string) =>
  (err instanceof Error ? err.message : String(err)) === `bilinmeyen metot: ${method}`;

/** #1099: client viewing belongs to the connection, not to an agent session. */
export function teamViewed(id: string, viewed: boolean) {
  const [host, teamId] = splitId(id);
  return callHost<void>(host, "team_viewed", { teamId, viewed }).catch((err) => {
    if (isUnknownMethod(err, "team_viewed")) return;
    throw err;
  });
}

/** DEX-26: engines an agent can run; codex models come live from the codex login. */
export const ENGINES = [{ id: "claude", label: "Claude Code" }, { id: "codex", label: "Codex" }];
export const listCodexModels = (host: string) => callHost<ProviderModel[]>(host, "codex_models");

/** The catalog ships with the build; the local core's copy stands for every host. */
export const listRoles = () => callHost<Role[]>(LOCAL, "role_list");

export const listAgents = (host: string) => callHost<AgentRec[]>(host, "agent_list").then((r) => r.map((a) => agent(host, a)));
export function createAgent(a: NewAgent) {
  const [host, teamId] = splitId(a.teamId);
  return callHost<AgentRec>(host, "agent_create", { agent: { ...a, teamId } }).then((r) => agent(host, r));
}
export function updateAgent(id: string, patch: AgentPatch) {
  const [host, raw] = splitId(id);
  return callHost<AgentRec>(host, "agent_update", { id: raw, patch }).then((r) => agent(host, r));
}
export function deleteAgent(id: string) {
  const [host, raw] = splitId(id);
  return callHost<void>(host, "agent_delete", { id: raw });
}

/** DEX-30: one engine/provider/model for all of a team's workers. `provider` is Claude-only. */
export interface PendingModel {
  engine: string;
  provider?: string | null;
  model: string;
}
/** Agent ids changed now vs. waiting for their running session to end. */
export interface WorkersOutcome {
  applied: string[];
  pending: string[];
}
export async function setWorkersModel(teamId: string, target: PendingModel): Promise<WorkersOutcome> {
  const [host, raw] = splitId(teamId);
  const out = await callHost<WorkersOutcome>(host, "team_set_workers_model", { teamId: raw, target });
  return { applied: out.applied.map((id) => qid(host, id)), pending: out.pending.map((id) => qid(host, id)) };
}
export function applyPending(id: string) {
  const [host, raw] = splitId(id);
  return callHost<boolean>(host, "agent_apply_pending", { id: raw });
}

export const onTeamsChanged = (cb: () => void): Unlisten => onEvent("teams-changed", cb);
