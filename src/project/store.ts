// #838 H: each team's projects and each session's binding, fetched from the team's host and kept
// fresh by its events (`projects-changed`, `teams-changed`, `delegations-changed`, reconnect).
// A team is loaded when something first asks for it; sync getters serve the office hover card.
import { useSyncExternalStore } from "react";
import { callHost, hostInfo, hostOf, onHostUp, qid } from "../api/hosts";
import { kept, latest, listenKept } from "../hot";
import { getStatus } from "../agents";
import { getTeam, getTeamsSnapshot, refreshTeams } from "../team/store";
import { toast } from "../toast/store";
import * as api from "./api";
import {
  activeProjects, projectErrorCode, projectErrorText, projectName, shellCwd, showsProjects, workerProjectId, workerProjectTeam,
  type Job, type Project, type ProjectTarget, type ProjectCheck, type SessionProject,
} from "./model";

export interface TeamProjects {
  /** `unsupported`: the host predates projects — the old single-folder UI stays. */
  status: "loading" | "ready" | "unsupported" | "error";
  /** Archived ones included (see `activeProjects`). */
  list: Project[];
  error: string | null;
}

interface State {
  teams: Record<string, TeamProjects>;
  /** Qualified agent id → its session's binding. */
  sessions: Record<string, SessionProject>;
  /** `project_verify`'s last answer per project. */
  checks: Record<string, ProjectCheck>;
}

const hot = import.meta.hot;
const box = kept(hot, "state", () => ({ state: { teams: {}, sessions: {}, checks: {} } as State }));
const listeners = kept(hot, "listeners", () => new Set<() => void>());
const sessionHosts = kept(hot, "sessionHosts", () => new Map<string, Record<string, SessionProject>>());
const loads = kept(hot, "loads", () => new Map<string, number>());
const sessionLoads = kept(hot, "sessionLoads", () => new Map<string, number>());

function set(next: Partial<State>) {
  box.state = { ...box.state, ...next };
  for (const l of listeners) l();
}
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};

const LOADING: TeamProjects = { status: "loading", list: [], error: null };
const NONE: TeamProjects = { status: "ready", list: [], error: null };

async function load(teamId: string) {
  const request = (loads.get(teamId) ?? 0) + 1;
  loads.set(teamId, request);
  // Service teams own no repository (#838 §1): no project UI.
  if (getTeam(teamId)?.kind === "service") return setTeam(teamId, NONE);
  try {
    const list = await api.listProjects(teamId);
    if (loads.get(teamId) !== request) return;
    setTeam(teamId, { status: "ready", list, error: null });
  } catch (err) {
    if (loads.get(teamId) !== request) return;
    if (err instanceof api.ProjectsUnsupported) setTeam(teamId, { status: "unsupported", list: [], error: null });
    else setTeam(teamId, { status: "error", list: box.state.teams[teamId]?.list ?? [], error: projectErrorText(err) });
  }
}
const setTeam = (teamId: string, t: TeamProjects) => set({ teams: { ...box.state.teams, [teamId]: t } });

/** Starts the first load of `teamId` (idempotent); later loads come from events. */
function ensure(teamId: string) {
  if (!teamId || box.state.teams[teamId]) return;
  box.state.teams[teamId] = LOADING; // no emit: the load's answer does
  queueMicrotask(() => void load(teamId));
}

export const reloadProjects = (teamId: string) => load(teamId);

const reloadSessions = latest(hot, "sessions", async (host: string) => {
  const request = (sessionLoads.get(host) ?? 0) + 1;
  sessionLoads.set(host, request);
  try {
    const sessions = await api.sessionProjects(host);
    if (sessionLoads.get(host) !== request) return;
    sessionHosts.set(host, sessions);
  } catch (error) {
    if (sessionLoads.get(host) !== request) return;
    toast({ key: `session-projects:${host}`, tone: "err", title: "Oturum projeleri okunamadı", body: projectErrorText(error) });
    return; // the headers keep the last binding; the next event retries
  }
  set({ sessions: Object.assign({}, ...sessionHosts.values()) });
});
export const refreshSessions = (agentId: string) => void reloadSessions(hostOf(agentId));

const loadedOn = (host: string) => Object.keys(box.state.teams).filter((t) => hostOf(t) === host);
listenKept<{ teamId?: string } | null>(hot, "projects-changed", (e, host) => {
  const team = e.payload?.teamId;
  for (const t of team ? [qid(host, team)] : loadedOn(host)) if (box.state.teams[t]) void load(t);
  void reloadSessions(host);
});
// set_default / update also move the team's legacy folder (teams.cwd): the team store reloads too.
listenKept(hot, "teams-changed", (_e, host) => loadedOn(host).forEach((t) => void load(t)));
listenKept(hot, "delegations-changed", (_e, host) => void reloadSessions(host));
kept(hot, "projectsUp", () => onHostUp((host) => {
  loadedOn(host).forEach((t) => void load(t));
  void reloadSessions(host);
}));

// ---- reads ----

export function getTeamProjects(teamId: string): TeamProjects {
  ensure(teamId);
  return box.state.teams[teamId] ?? LOADING;
}
export const useTeamProjects = (teamId: string) => useSyncExternalStore(subscribe, () => getTeamProjects(teamId));
export const useSessions = () => useSyncExternalStore(subscribe, () => box.state.sessions);
export const getSessions = () => box.state.sessions;
export const useChecks = () => useSyncExternalStore(subscribe, () => box.state.checks);

/** The team shows project marks (picker, chips, hover row): it has more than one open project. */
export const teamShowsProjects = (teamId: string) => {
  const t = getTeamProjects(teamId);
  return t.status === "ready" && showsProjects(t.list);
};

/** The team's selected project (TeamRec.defaultProjectId), if still open. */
export function selectedProject(teamId: string): Project | null {
  const id = getTeam(teamId)?.defaultProjectId;
  return activeProjects(getTeamProjects(teamId).list).find((p) => p.id === id) ?? null;
}

// ---- writes (each answer also arrives as `projects-changed`; reloading here keeps the panel in step) ----

async function after<T>(teamId: string, f: Promise<T>): Promise<T> {
  const out = await f;
  await load(teamId);
  return out;
}
export const createProject = (teamId: string, p: api.NewProject, target: string | ProjectTarget, branch?: string | null) =>
  after(teamId, api.createProject(teamId, p, target, branch));
/** A new folder named `name` under the team's category root, bound as a project (team panel + top-bar picker). */
export const createFolderProject = async (teamId: string, host: string, categoryRoot: string, name: string, branch: string | null = null) => {
  // project_list alone predates createFolder; require the newer folder API's explicit advertisement.
  const target = hostInfo(host)?.methods?.includes("team_folder_preview")
    ? { createFolder: true as const }
    : await callHost<string>(host, "fs.mkdir", { parent: categoryRoot, name });
  return createProject(teamId, { name }, target, branch);
};
export const updateProject = (p: Project, change: Parameters<typeof api.updateProject>[1]) => after(p.teamId, api.updateProject(p, change));
export const archiveProject = (p: Project) => after(p.teamId, api.archiveProject(p));
export const reopenProject = (p: Project) => after(p.teamId, api.reopenProject(p));
export const setDefaultProject = async (teamId: string, id: string | null) => {
  await after(teamId, api.setDefaultProject(teamId, id));
  // Await the team refresh as well: controlled pickers and the next terminal see the new choice.
  await refreshTeams();
};

/** Folder/Git/branch now; the answer is kept as the project's state (`unavailable`, `branch`). */
export async function verifyProject(p: Project): Promise<ProjectCheck> {
  let check: ProjectCheck;
  try {
    await api.verifyProject(p);
    check = "ok";
  } catch (err) {
    const code = projectErrorCode(err);
    if (!code) throw err;
    check = code;
  }
  set({ checks: { ...box.state.checks, [p.id]: check } });
  return check;
}

/**
 * What an agent's header / hover card names, where the team has a choice: a leader works in the
 * session's project while live (unknown bindings stay explicit), a worker in its session's or job's.
 */
export function agentProjectName(agent: { id: string; teamId: string; kind: string }, jobs: readonly Job[]): string | null {
  const bind = box.state.sessions[agent.id];
  const teamId = agent.kind === "worker" ? workerProjectTeam(bind, jobs, agent.id, agent.teamId) : agent.teamId;
  if (!teamShowsProjects(teamId)) return null;
  if (agent.kind === "leader") {
    if (getStatus(agent.id) !== "offline" && !bind) return "proje bağı bilinmiyor";
    const id = getStatus(agent.id) !== "offline" ? bind?.projectId : selectedProject(agent.teamId)?.id;
    return projectName(getTeamProjects(teamId).list, id) ?? (id ? "proje kullanılamıyor" : "takım (proje seçili değil)");
  }
  const id = workerProjectId(bind, jobs, agent.id);
  return projectName(getTeamProjects(teamId).list, id) ?? (id ? "proje kullanılamıyor" : null);
}

/** The selection is for new shells and the leader's next session; a running leader keeps its own. */
export function leaderNote(teamId: string, name: string) {
  const { agents } = getTeamsSnapshot();
  const leaders = agents.filter((a) => a.teamId === teamId && a.kind === "leader" && getStatus(a.id) !== "offline");
  if (!leaders.length) return;
  toast({
    key: `project-select:${teamId}`,
    tone: "info",
    title: `Aktif proje: ${name}`,
    body: `${leaders.map((a) => a.name).join(", ")} süren oturumunda kalır; yeni proje bağlamı için lider panelinde "Temiz oturum".`,
  });
}

/** #838: a new patron shell's folder — the selected project's, else the team's (older host). */
export function newShellCwd(teamId: string): string | null {
  const team = getTeam(teamId);
  const selected = team?.defaultProjectId;
  return shellCwd(getTeamProjects(teamId).list, selected, team?.workdir ?? null);
}

export async function resolveShellCwd(teamId: string): Promise<string | null> {
  await load(teamId);
  const projects = getTeamProjects(teamId);
  if (projects.status === "unsupported") return getTeam(teamId)?.workdir ?? null;
  if (projects.status !== "ready") throw new Error(projects.error ?? "Projeler okunamadı");
  return (await api.projectContext(teamId, getTeam(teamId)?.defaultProjectId)).cwd;
}

/** #838: a job's project name for its delivery / board line, where its team has several projects. */
export const jobProjectName = (job: { teamId: string | null; projectId?: string | null }): string | null =>
  job.teamId && teamShowsProjects(job.teamId) ? projectName(getTeamProjects(job.teamId).list, job.projectId) : null;
