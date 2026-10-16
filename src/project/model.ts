// #838 H: the project layer's pure rules — no DOM, no host calls (unit-tested in model.test.ts).
// Ids here are the client's: qualified with the host (src/api/hosts.ts `qid`), like team/agent ids.

export type ProjectErrorCode =
  | "project_required" | "project_not_found" | "project_unavailable" | "project_busy"
  | "project_root_conflict" | "project_mismatch" | "project_branch_mismatch" | "project_category_unset";

/** crates/dex-core/src/projects.rs `Project`; `id`/`teamId` qualified, `host` added here. */
export interface Project {
  id: string;
  host: string;
  teamId: string;
  name: string;
  rootPath: string | null;
  rootKey: string | null;
  /** null: not a Git repository (or a folderless legacy project). */
  repoKey: string | null;
  defaultBranch: string | null;
  worktrees: boolean;
  archivedAt: number | null;
  createdAt: number;
  updatedAt: number;
  /** Worktrees still waiting for cleanup. */
  worktreeCleanup: number;
}

/** `session_projects`: what a session is bound to (never the queue). Ids qualified. */
export interface SessionProject {
  projectId: string | null;
  delegationId: number | null;
}

/** The slice of a delegation these rules read (src/delegations.ts `Delegation`). */
export interface Job {
  id: number;
  toAgent: string;
  status: string;
  projectId?: string | null;
  teamId?: string | null;
}

export const PROJECT_ERROR_TEXT: Record<ProjectErrorCode, string> = {
  project_required: "Bu takımda birden çok proje var — önce bir proje seç.",
  project_not_found: "Proje bulunamadı (silinmiş ya da başka takımın).",
  project_unavailable: "Proje kullanılamıyor: arşivde ya da klasörü yok.",
  project_busy: "Projede süren iş, plan, inceleme, temizlenmemiş worktree ya da açık oturum var — önce bitmeli.",
  project_root_conflict: "Bu klasör/repo zaten başka bir projeye bağlı.",
  project_mismatch: "Proje adı ya da klasör/repo bağı geçersiz; ad benzersiz, repo kökü en üst klasör olmalı.",
  project_branch_mismatch: "Dal bu klasörün checkout'uyla uyuşmuyor.",
  project_category_unset: "Kategori kökü kullanılamıyor — önce bir kategori kökü seç ya da önerilen kökü kullan.",
};

const errText = (err: unknown) => (err instanceof Error ? err.message : String(err));

/** The stable code a project call failed with (RPC and IPC carry the same string), else null. */
export function projectErrorCode(err: unknown): ProjectErrorCode | null {
  const m = errText(err);
  return Object.prototype.hasOwnProperty.call(PROJECT_ERROR_TEXT, m) ? (m as ProjectErrorCode) : null;
}

/** What the patron reads: the code's sentence, or the host's own text for anything else. */
export function projectErrorText(err: unknown): string {
  const code = projectErrorCode(err);
  return code ? PROJECT_ERROR_TEXT[code] : errText(err);
}

/**
 * An older host lacks the method: only the method-list evidence (`supports`) or dexd's own
 * `unknown_method` text count — any other failure is an error, never a silent fallback.
 */
export const isUnknownMethod = (err: unknown, method: string) => errText(err) === `bilinmeyen metot: ${method}`;

/** `project_verify`'s last answer: ok, or the code that makes it unusable right now. */
export type ProjectCheck = "ok" | ProjectErrorCode;

export type ProjectState = "git" | "folder" | "none" | "unavailable" | "branch" | "archived";

export const STATE_TEXT: Record<ProjectState, string> = {
  git: "git",
  folder: "klasör · worktree/merge yok",
  none: "klasörsüz",
  unavailable: "kullanılamıyor",
  branch: "dal uyuşmuyor",
  archived: "arşivde",
};

/** The list itself never touches the disk; a verify result (if any) says whether it is usable. */
export function projectState(p: Project, check?: ProjectCheck | null): ProjectState {
  if (p.archivedAt !== null) return "archived";
  if (check === "project_branch_mismatch") return "branch";
  if (check && check !== "ok") return "unavailable";
  if (!p.rootPath) return "none";
  return p.repoKey ? "git" : "folder";
}

export const activeProjects = (list: readonly Project[]) => list.filter((p) => p.archivedAt === null);

/**
 * Project marks (picker, chips, card row) only where there is a choice: a team with one project
 * keeps its old look.
 */
export const showsProjects = (list: readonly Project[]) => activeProjects(list).length > 1;

/** "Yeni klasör aç" needs a saved category root; a pending suggestion must be accepted first ("Kullan"). */
export const folderCategoryReady = (categoryRoot: string | null, suggestedRoot: string | null) => !!categoryRoot && !suggestedRoot;

/** Top-bar picker: arrow/Home/End move over `n` rows (projects + the "+" row), clamped at the ends. */
export function pickStep(i: number, key: string, n: number): number {
  if (key === "Home") return 0;
  if (key === "End") return n - 1;
  if (key === "ArrowDown") return Math.min(i + 1, n - 1);
  if (key === "ArrowUp") return Math.max(i - 1, 0);
  return i;
}

export const projectName = (list: readonly Project[], id: string | null | undefined) =>
  (id && list.find((p) => p.id === id)?.name) || null;

/**
 * The project a worker header names: its session's binding, else the job it is running. The next
 * queued job never stands in for it (#838 §5).
 */
export function workerProjectId(bind: SessionProject | undefined, jobs: readonly Job[], agentId: string): string | null {
  if (bind) return bind.projectId;
  const running = jobs.find((j) => j.toAgent === agentId && j.status === "running");
  return running?.projectId ?? null;
}

/** Service workers use the requesting team's project list, from their bound job. */
export function workerProjectTeam(bind: SessionProject | undefined, jobs: readonly Job[], agentId: string, ownTeam: string): string {
  const job = jobs.find(j => j.toAgent === agentId && (bind ? j.id === bind.delegationId : j.status === "running"));
  return job?.teamId ?? ownTeam;
}

/**
 * #910: a live session bound to P1 does not take the P2 job at the head of its queue (FIFO: the
 * worker's newer jobs wait behind it). That job, or null.
 */
export function blockedJob(bind: SessionProject | undefined, jobs: readonly Job[], agentId: string, live: boolean): Job | null {
  if (!live || !bind?.projectId) return null;
  let head: Job | null = null;
  for (const j of jobs) if (j.toAgent === agentId && j.status === "queued" && (!head || j.id < head.id)) head = j;
  return head?.projectId && head.projectId !== bind.projectId ? head : null;
}

/** A new patron shell opens in the selected project's folder; an old host's team folder otherwise. */
export function shellCwd(list: readonly Project[], selected: string | null | undefined, workdir: string | null): string | null {
  const p = list.find((x) => x.id === selected && x.archivedAt === null);
  return p?.rootPath ?? workdir;
}

/** The last path segment — a new project's suggested name. */
export function folderName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";
}

export type ProjectTarget = { root: string; createFolder?: never } | { createFolder: true; root?: never };
export function projectCreateParams(project: { name: string; worktrees?: boolean }, target: ProjectTarget, branch?: string | null) {
  return { project, ...target, branch: branch || null };
}

export const leaderProjectMismatch = (bind: SessionProject | undefined, selected: string | null, live: boolean) =>
  live && !!bind && bind.projectId !== selected;

/** An archived or missing project must not retain its graph/read context. */
export function memoryProjectId(list: readonly Project[], id: string | null): string | null {
  return list.some(p => p.id === id && p.archivedAt === null) ? id : null;
}

/** Graph and document reads must use exactly the same selected scope. */
export function memoryRequest(kind: "graph" | "read", teamId: string, projectId: string | null, legacy: boolean, id?: string) {
  return {
    method: `${legacy ? "memory" : projectId ? "project_memory" : "team_memory"}_${kind}`,
    params: { teamId, ...(!legacy && projectId ? { projectId } : {}), ...(kind === "read" ? { id } : {}) },
  };
}
