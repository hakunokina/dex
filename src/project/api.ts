// #838 H: the project calls (#927 — Tauri command = dexd method, same names, same error codes).
// Called with the host's bare ids; what comes back is qualified here (src/api/hosts.ts).
import { callHost, hostSupports, qid, qOpt, splitId } from "../api/hosts";
import { isUnknownMethod, projectCreateParams, type ProjectTarget, type Project, type SessionProject } from "./model";

export interface NewProject {
  name: string;
  worktrees?: boolean;
}
export interface ProjectPatch {
  name?: string;
  worktrees?: boolean;
}

export interface ProjectContext {
  teamId: string;
  projectId: string | null;
  projectName: string | null;
  cwd: string;
  workspaceRoot: string;
  suggestedWorkspaceRoot?: string | null;
}

export function projectContext(teamId: string, projectId?: string | null): Promise<ProjectContext> {
  const [host, raw] = splitId(teamId);
  if (projectId && splitId(projectId)[0] !== host) return Promise.reject(new Error("project_mismatch"));
  return callHost<ProjectContext>(host, "project_context", { teamId: raw, projectId: projectId ? splitId(projectId)[1] : null });
}

/** fs.list canonicalizes an existing directory and rejects paths outside the host's Roots. */
export async function categoryFolder(host: string, path: string, teamId?: string): Promise<string> {
  const listing = await callHost<{ path: string | null }>(host, "fs.list", { path, ...(teamId ? { teamId: splitId(teamId)[1] } : {}) });
  if (!listing.path) throw new Error("Kategori kökü için mevcut bir klasör seç.");
  return listing.path;
}

/** The host predates the project layer: the team keeps its single-folder UI. */
export class ProjectsUnsupported extends Error {
  constructor(host: string) {
    super(`${host}: proje katmanı yok (eski sürüm)`);
    this.name = "ProjectsUnsupported";
  }
}

const project = (host: string, p: Project): Project => ({ ...p, host, id: qid(host, p.id), teamId: qid(host, p.teamId) });

/** Archived ones included: the team panel lists them for "Yeniden aç". */
export async function listProjects(teamId: string): Promise<Project[]> {
  const [host, raw] = splitId(teamId);
  if (!hostSupports(host, "project_list")) throw new ProjectsUnsupported(host);
  try {
    const list = await callHost<Project[]>(host, "project_list", { teamId: raw, includeArchived: true });
    return list.map((p) => project(host, p));
  } catch (err) {
    if (isUnknownMethod(err, "project_list")) throw new ProjectsUnsupported(host);
    throw err;
  }
}

export function createProject(teamId: string, p: NewProject, target: string | ProjectTarget, branch?: string | null) {
  const [host, raw] = splitId(teamId);
  return callHost<Project>(host, "project_create", { teamId: raw, ...projectCreateParams(p, typeof target === "string" ? { root: target } : target, branch) }).then((r) => project(host, r));
}

/** `root`/`branch` re-bind (the core's busy check); a name or worktrees change alone is free. */
export function updateProject(p: Project, change: { patch?: ProjectPatch; root?: string; branch?: string }) {
  const [host, raw] = splitId(p.id);
  return callHost<Project>(host, "project_update", { teamId: splitId(p.teamId)[1], id: raw, ...change }).then((r) => project(host, r));
}

/** Refused (`project_busy`) while a job, plan item, review, worktree or bound session is open. */
export const archiveProject = (p: Project) => call(p, "project_archive");
export const reopenProject = (p: Project) => call(p, "project_reopen");
/** Folder + Git identity + branch now (the list never touches the disk). */
export const verifyProject = (p: Project) => call(p, "project_verify");

function call(p: Project, method: string) {
  const [host, raw] = splitId(p.id);
  return callHost<Project>(host, method, { teamId: splitId(p.teamId)[1], id: raw }).then((r) => project(host, r));
}

/** The team's selected project (`teams.default_project_id`); never moves a job, plan or session. */
export function setDefaultProject(teamId: string, id: string | null) {
  const [host, raw] = splitId(teamId);
  if (id && splitId(id)[0] !== host) return Promise.reject(new Error("project_mismatch"));
  return callHost<null>(host, "project_set_default", { teamId: raw, id: id && splitId(id)[1] });
}

/** Agent (qualified) → its session's binding, over all the host's teams. Older host: none. */
export async function sessionProjects(host: string): Promise<Record<string, SessionProject>> {
  if (!hostSupports(host, "session_projects")) return {};
  try {
    const map = await callHost<Record<string, SessionProject>>(host, "session_projects", { teamId: null });
    return Object.fromEntries(Object.entries(map).map(([a, s]) => [qid(host, a), { ...s, projectId: qOpt(host, s.projectId) }]));
  } catch (err) {
    if (isUnknownMethod(err, "session_projects")) return {};
    throw err;
  }
}
