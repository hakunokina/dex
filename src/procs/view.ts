// #916: the process panel's data as dexd reports it (crates/dex-core/src/proc.rs, RPCs
// agent_processes / kill_agent_process) and the patron's view of it. Pure: unit-tested (view.test.ts).

export type ProcKind = "browser-tool" | "browser" | "node" | "cargo" | "godot" | "shell" | "other";

export interface AgentProcess {
  pid: number;
  ppid: number;
  name: string;
  /** Masked command line, ≤200 chars. */
  cmd: string;
  /** Share of the whole machine, %. */
  cpuPct: number;
  /** Working set, MB. */
  memMb: number;
  kind: ProcKind;
  /** The agent's own CLI: never killable from here. */
  root: boolean;
}

/** One agent's tree; `error` = it could not be read this time (the other agents still come). */
export interface AgentProcs {
  processes: AgentProcess[];
  error?: string;
}

/** `agent_processes` without `agent`: every agent with a live session on the host (raw ids). */
export type ProcessMap = Record<string, AgentProcs>;

export interface AgentRow {
  /** Host-raw agent id. */
  agent: string;
  cpuPct: number;
  memMb: number;
  /** Root first, then the heaviest child. */
  procs: AgentProcess[];
  /** Browser roots only; renderer/GPU children do not increase this count. */
  browsers: number;
  browserTools: number;
  /** #925: this agent's tree could not be read. */
  error?: string;
}

/** The request itself takes ~300 ms (two CPU samples); the next starts this long after it lands. */
export const REFRESH_MS = 3000;

export const KIND_LABEL: Record<ProcKind, string> = {
  "browser-tool": "Tarayıcı aracı",
  browser: "Tarayıcı",
  node: "Node",
  cargo: "Cargo",
  godot: "Godot",
  shell: "Kabuk",
  other: "Diğer",
};

const isBrowser = (p: AgentProcess) => !p.root && p.kind === "browser";
const isBrowserTool = (p: AgentProcess) => !p.root && p.kind === "browser-tool";
const browserCount = (procs: AgentProcess[]) => {
  const pids = new Set(procs.filter(isBrowser).map((p) => p.pid));
  return procs.filter((p) => isBrowser(p) && !pids.has(p.ppid)).length;
};
const byLoad = (a: { cpuPct: number; memMb: number }, b: { cpuPct: number; memMb: number }) =>
  b.cpuPct - a.cpuPct || b.memMb - a.memMb;
const round1 = (n: number) => Math.round(n * 10) / 10;

/** One row per agent with a process or an error (a shell pane's empty list drops out), heaviest CPU first, then RAM. */
export function agentRows(map: ProcessMap): AgentRow[] {
  return Object.entries(map)
    .filter(([, { processes, error }]) => processes.length > 0 || error)
    .map(([agent, { processes, error }]) => {
      const procs = processes.map((p) => ({ ...p, kind: Object.prototype.hasOwnProperty.call(KIND_LABEL, p.kind) ? p.kind : "other" as const }));
      return {
        agent,
        cpuPct: round1(procs.reduce((s, p) => s + p.cpuPct, 0)),
        memMb: round1(procs.reduce((s, p) => s + p.memMb, 0)),
        procs: [...procs.filter((p) => p.root), ...procs.filter((p) => !p.root).sort(byLoad)],
        browsers: browserCount(procs),
        browserTools: procs.filter(isBrowserTool).length,
        ...(error ? { error } : {}),
      };
    })
    .sort((a, b) => byLoad(a, b) || a.agent.localeCompare(b.agent));
}

/** The previous order of agents and of each agent's processes kept across refreshes, so a row never
 *  moves under the patron's pointer; newcomers go last in their own (load) order. Fresh sort = no `prev`. */
export function keepOrder(prev: AgentRow[] | null, next: AgentRow[]): AgentRow[] {
  if (!prev) return next;
  const by = <T>(old: readonly T[], items: T[], key: (x: T) => string | number) => {
    const rank = new Map(old.map((x, i) => [key(x), i]));
    const at = (x: T) => rank.get(key(x)) ?? Infinity;
    return [...items].sort((a, b) => at(a) - at(b)); // stable: newcomers keep their order
  };
  const old = new Map(prev.map((r) => [r.agent, r]));
  return by(prev, next, (r) => r.agent).map((r) => {
    const o = old.get(r.agent);
    return o ? { ...r, procs: by(o.procs, r.procs, (p) => p.pid) } : r;
  });
}

export interface Totals {
  cpuPct: number;
  memMb: number;
  browsers: number;
  browserMemMb: number;
  browserTools: number;
  browserToolMemMb: number;
}

export function totals(rows: AgentRow[]): Totals {
  const browsers = rows.flatMap((r) => r.procs.filter(isBrowser));
  const tools = rows.flatMap((r) => r.procs.filter(isBrowserTool));
  return {
    cpuPct: round1(rows.reduce((s, r) => s + r.cpuPct, 0)),
    memMb: round1(rows.reduce((s, r) => s + r.memMb, 0)),
    browsers: rows.reduce((s, r) => s + r.browsers, 0),
    browserMemMb: round1(browsers.reduce((s, p) => s + p.memMb, 0)),
    browserTools: tools.length,
    browserToolMemMb: round1(tools.reduce((s, p) => s + p.memMb, 0)),
  };
}

export const cpuText = (pct: number) => `%${pct.toFixed(1)}`;
export const memText = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`);

/** Kill confirmation: what is being closed, and that the agent itself keeps running. */
export function killQuestion(agentName: string, p: AgentProcess): string {
  return `${agentName} ajanının alt süreci kapatılsın mı?\n\n${KIND_LABEL[p.kind]} · ${p.name} (pid ${p.pid}) · ${memText(p.memMb)}\n${p.cmd}\n\nAjan çalışmaya devam eder; bu süreci kullanan iş yarıda kalabilir.`;
}
