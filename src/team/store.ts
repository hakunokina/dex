// Teams + members from SQLite (src-tauri/src/teams.rs), plus which team is on screen.
// The active team's members become `AGENTS` (src/agents.ts); the active id is persisted in layout.json.
// S4: one list over every host. A host that is not connected keeps its last known teams (memory, or
// the localStorage copy after a restart) so its tabs and layout stay, shown as "bağlantı yok".
// #151: the local core too (`dexd --local`, down until "Çekirdeği başlat").
import { useSyncExternalStore } from "react";
import { hasLocalCore } from "../api/platform";
import { hostStatus, initHosts, isHostUp, LOCAL, onHostUp, remoteHosts, subscribeHosts } from "../api/hosts";
import { dismissAgentAlert, getStatus, setAgents } from "../agents";
import { kept, latest } from "../hot";
import { requestStart } from "../launcher";
import * as layout from "../layout/store";
import { killPty } from "../pty";
import * as api from "./api";
import { sortAgents } from "./order";
import { roleSync } from "./roles";
import type { AgentRec, Role, TeamRec } from "./api";

export interface TeamsState {
  ready: boolean;
  teams: TeamRec[];
  agents: AgentRec[];
  /** Role catalog (static per build; refetched when the local core reconnects — #1720). */
  roles: Role[];
  activeId: string;
}

// DEX-18: kept across hot updates; a fresh `ready: false` here unmounted every pane (and killed its PTY).
const hot = import.meta.hot;
const box = kept(hot, "state", () => ({ state: { ready: false, teams: [], agents: [], roles: [], activeId: "" } as TeamsState }));
let state = box.state;
const listeners = kept(hot, "listeners", () => new Set<() => void>());

function emit(next: TeamsState) {
  box.state = state = next;
  setAgents(state.agents, state.activeId);
  for (const l of listeners) l();
}

export const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
export const getTeamsSnapshot = () => state;
export const useTeams = () => useSyncExternalStore(subscribe, getTeamsSnapshot);

export const getActiveTeamId = () => state.activeId;
export const getTeam = (id: string) => state.teams.find(t => t.id === id);
export const membersOf = (teamId: string) => state.agents.filter(a => a.teamId === teamId);
export const isRunning = (agentId: string) => getStatus(agentId) !== "offline";

interface HostPart {
  teams: TeamRec[];
  agents: AgentRec[];
}
const CACHE_KEY = "dex.hostCache";

function readCache(): Record<string, HostPart> {
  try {
    return JSON.parse(localStorage.getItem(CACHE_KEY) ?? "{}") ?? {};
  } catch {
    return {};
  }
}

async function hostPart(host: string, cache: Record<string, HostPart>): Promise<HostPart> {
  const known = (): HostPart => {
    const teams = state.teams.filter(t => t.host === host);
    return teams.length ? { teams, agents: state.agents.filter(a => a.host === host) } : cache[host] ?? { teams: [], agents: [] };
  };
  if (host !== LOCAL && !isHostUp(host)) return known();
  try {
    const [teams, agents] = await Promise.all([api.listTeams(host), api.listAgents(host)]);
    return { teams, agents };
  } catch (e) {
    if (host === LOCAL && hostStatus(LOCAL) !== "down") throw e;
    return known(); // dropped mid-call / the local core is not running: the next status change reloads
  }
}

async function reload(ready = true) {
  // S5-A: the Android build has no in-process core — its teams/commands don't exist there.
  const hosts = [...(hasLocalCore ? [LOCAL] : []), ...remoteHosts().map(h => h.id)];
  const cache = readCache();
  // Roles are read at emit time, not captured here: a newer list written meanwhile is never undone.
  const rolesTry =
    hasLocalCore && (!state.roles.length || roles.stale)
      ? roles.refresh().catch((e) => {
          if (!state.roles.length && hostStatus(LOCAL) !== "down") throw e;
        })
      : undefined;
  const [parts] = await Promise.all([Promise.all(hosts.map(h => hostPart(h, cache))), rolesTry]);
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(Object.fromEntries(hosts.map((h, i) => [h, parts[i]]))));
  } catch {
    /* no storage: a restart without the server shows no tabs for it */
  }
  const teams = parts.flatMap(p => p.teams);
  // Görev #81: one display order (leaders, role groups, natural names) for every list of members.
  const agents = sortAgents(parts.flatMap(p => p.agents), state.roles.map(r => r.slug));
  const known = new Set(teams.map(t => t.id));
  let activeId = state.activeId || layout.getSnapshot().active || "";
  if (!known.has(activeId)) activeId = teams[0]?.id ?? "";

  for (const t of teams) layout.ensureTeam(t.id);
  for (const id of Object.keys(layout.getSnapshot().teams)) if (!known.has(id)) layout.dropTeam(id);
  const agentIds = new Set(agents.map(a => a.id));
  for (const a of state.agents) if (!agentIds.has(a.id)) layout.clearAgent(a.id);
  // DEX-30/34: a session id can't be resumed across engines (their transcripts differ); every
  // reload checks each saved session's engine against the DB, so no engine-switch path can leave one stale.
  layout.syncSessionEngines(new Map(agents.map(a => [a.id, a.engine])));
  if (activeId) layout.setActiveTeam(activeId);

  emit({ ready, teams, agents, roles: state.roles, activeId });
}

// #1720: the newest role fetch wins; writing re-sorts the members by the new role order. Not `kept`:
// its writer must be this module's `emit`.
const roles = roleSync(api.listRoles, (list: Role[]) =>
  emit({ ...state, roles: list, agents: sortAgents(state.agents, list.map(r => r.slug)) }),
);
const init = kept(hot, "init", () => ({ started: null as Promise<void> | null, localSeen: false }));
const reloadLatest = latest(hot, "reload", () => reload());
export const refreshTeams = () => reloadLatest();

/**
 * Layout first (so defaults never overwrite a saved file), then teams. Idempotent.
 * #2260 `beforeReady`: runs once with the teams loaded but before `ready` mounts any pane — the
 * main window settles which leaders live in their own window there, so no pane subscribes too early.
 */
export function initTeams(beforeReady?: () => Promise<void> | void): Promise<void> {
  init.started ??= (async () => {
    await Promise.all([layout.hydrate(), initHosts()]);
    await reload(false);
    await beforeReady?.();
    emit({ ...state, ready: true });
    api.onTeamsChanged(() => void reloadLatest().catch(() => {}));
    // Added / removed / (re)connected host: its teams come, go or refresh.
    subscribeHosts(() => void reloadLatest().catch(() => {}));
    onHostUp((h) => {
      if (h === LOCAL) {
        // The first call is this subscription's own (roles were just loaded); later ones are a
        // reconnect — maybe to a new build with new roles (#1720).
        if (!init.localSeen) return void (init.localSeen = true);
        void roles.refresh().catch(() => {}); // failed: `stale`, the next reload retries
      }
      void reloadLatest().catch(() => {});
    });
  })();
  return init.started;
}

export function selectTeam(id: string) {
  if (id === state.activeId || !getTeam(id)) return;
  layout.setActiveTeam(id);
  emit({ ...state, activeId: id });
}

export async function createTeam(host: string, t: api.NewTeam) {
  const team = await api.createTeam(host, t);
  await reload();
  selectTeam(team.id);
  return team;
}

/** Görev #43: `stopAgents` closes the running members' sessions first (the core refuses otherwise). */
export async function removeTeam(id: string, stopAgents = false) {
  if (!stopAgents && membersOf(id).some(a => isRunning(a.id))) throw new Error("takımda çalışan ajan var");
  await api.deleteTeam(id, stopAgents);
  await reload();
}

export async function removeAgent(id: string) {
  if (isRunning(id)) throw new Error("çalışan ajan silinemez");
  await api.deleteAgent(id);
  await reload();
}

export async function addAgent(a: api.NewAgent) {
  const agent = await api.createAgent(a);
  await reload();
  return agent;
}

export async function patchAgent(id: string, patch: api.AgentPatch) {
  await api.updateAgent(id, patch);
  await reload();
}

/** DEX-17: ajanın zincirini aç/kapat + "geri dön" (asıl sağlayıcı kullanılabilirse). */
export async function setAgentChain(id: string, off: boolean) {
  await api.chainSetAgent(id, off);
  await reload();
}
export async function chainBack(id: string) {
  await api.chainBack(id);
  dismissAgentAlert(id);
  await reload();
}

/** DEX-30: all of the team's workers (never leaders) → one engine/provider/model; running ones wait.
 * #2443: `ids` = only those workers. */
export async function setWorkersModel(teamId: string, target: api.PendingModel, ids?: string[]) {
  const out = await api.setWorkersModel(teamId, target, ids);
  await reload();
  return out;
}

/** DEX-30: the DB's current record, not the last render's — a pending change may have just landed. */
export async function freshAgent(id: string) {
  await reloadLatest();
  return state.agents.find(a => a.id === id);
}

/**
 * DEX-30: stop the workers whose bulk change is waiting, land it, start them again. A new engine
 * means a new session (its transcript can't be resumed); same engine resumes on the new model.
 * Returns the ids that had no pane to start in (they stay stopped, already switched).
 */
export async function restartPending(teamId: string, only?: string[]): Promise<string[]> {
  const ids = membersOf(teamId)
    .filter(a => a.kind === "worker" && a.pending && isRunning(a.id) && (!only || only.includes(a.id)))
    .map(a => a.id);
  const unstarted: string[] = [];
  for (const id of ids) {
    await killPty(id);
    // The pane's own exit bookkeeping runs async; its start is refused until it has.
    for (let t = 0; isRunning(id) && t < 50; t++) await new Promise(r => setTimeout(r, 100));
    await api.applyPending(id);
    await reload();
    if (!requestStart(id)) unstarted.push(id);
  }
  return unstarted;
}

export async function patchTeam(id: string, patch: api.TeamPatch) {
  await api.updateTeam(id, patch);
  await reload();
}
