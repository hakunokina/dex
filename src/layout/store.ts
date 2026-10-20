// Pane layout state + persistence, one layout per team (DEX-7; docking since DEX-36).
// On disk: { version: 2, active: teamId, teams: { [teamId]: { dock, shells, seed? } }, sessions: { [agentId]: { id, engine } } }.
// `dock` is dockview's own JSON (src/layout/Workspace.tsx owns it); the pre-DEX-36 shape
// (left/right columns + worker dock) is converted on load: its shells survive, its column
// leaders become the default layout's left/right leaders (`seed`).
// S4: the layout is this window's, never a host's — always the local core (layout.json).
import { callLocal, hostOf, qid, splitId } from "../api/hosts";
import type { SerializedDockview } from "dockview-react";
import { kept } from "../hot";
import type { FavoriteRec } from "./favorite";

/** A patron shell (DEX-29): persisted by id + cwd, never by session. */
export interface ShellRec {
  id: string;
  cwd: string | null;
}

/** PTY id of a shell — never collides with an agent id. A remote team's shell (`<host>:<uuid>`) runs on that host. */
export function shellPtyId(id: string) {
  const [host, raw] = splitId(id);
  return qid(host, `sh-${raw}`);
}

export interface LayoutState {
  /** dockview JSON; null = build the default layout at the next mount. */
  dock: SerializedDockview | null;
  shells: ShellRec[];
  /** Migrated pre-DEX-36 layout: leaders that sat in the left/right columns. */
  seed?: { left: string[]; right: string[] };
  /** The assistant whose pane Dex already opened once; closed after that, it stays closed (rack reopens it). */
  assistant?: string;
  /** #2235: Dex already added the "Sen yokken" tab once; closed by the patron, it stays closed (palette reopens it). */
  catchup?: boolean;
}

export interface Layouts {
  active: string | null;
  teams: Record<string, LayoutState>;
  /** Patron düzeni: bir takımdan kaydedilir, yeni ve mevcut takımlara uygulanır. */
  favorite: FavoriteRec | null;
}

const SAVE_DEBOUNCE_MS = 400;

// Kept across vite hot updates (DEX-18): a reset layout would drop every panel.
const hot = import.meta.hot;
const box = kept(hot, "state", () => ({ state: { active: null, teams: {}, favorite: null } as Layouts }));
let state = box.state;
const listeners = kept(hot, "listeners", () => new Set<() => void>());

// Last known session per agent, with the engine that wrote it. Persisted as `sessions`; every agent
// panel reads its resume id from here. engine null = pre-DEX-34 entry, bound to the agent's engine on the first reload.
export interface SessionRec {
  id: string;
  engine: string | null;
}
const lastSession = kept(hot, "lastSession", () => new Map<string, SessionRec>());

/** An agent row's engine; empty means the default (claude), as in TeamBar. */
export const engineName = (engine: string | null | undefined) => engine || "claude";

/** DEX-34: the id ▶/delegation may resume — only a session of the agent's current engine. */
export function resumableSession(agentId: string, engine: string | undefined): string | null {
  const r = lastSession.get(agentId);
  return r && r.engine && r.engine === engineName(engine) ? r.id : null;
}

// A hot update from the pre-DEX-36 module hands over its in-memory shape: convert it like the file.
for (const [id, t] of Object.entries(state.teams)) {
  if (!Array.isArray((t as Partial<LayoutState>).shells)) state.teams[id] = parseLegacyTeam(t) ?? { dock: null, shells: [] };
}

function set(updater: (s: Layouts) => Layouts) {
  box.state = state = updater(state);
  for (const l of listeners) l();
  scheduleSave();
}

function setTeam(teamId: string, updater: (t: LayoutState) => LayoutState) {
  set(s => (s.teams[teamId] ? { ...s, teams: { ...s.teams, [teamId]: updater(s.teams[teamId]) } } : s));
}

export function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export const getSnapshot = (): Layouts => state;

/** Patron düzeni: olduğu gibi saklanır (ajan eşleme uygulanırken yapılır). */
export function setFavorite(fav: FavoriteRec | null) {
  set(s => ({ ...s, favorite: fav }));
}
export const getFavorite = (): FavoriteRec | null => state.favorite;

/** Workspace reports every dockview change here (debounced to disk like the rest). */
export function setDock(teamId: string, dock: SerializedDockview | null) {
  setTeam(teamId, t => ({ ...t, dock, seed: dock ? undefined : t.seed }));
}

export const assistantOffered = (teamId: string) => state.teams[teamId]?.assistant;
export function setAssistantOffered(teamId: string, agentId: string) {
  if (state.teams[teamId] && state.teams[teamId].assistant !== agentId) setTeam(teamId, t => ({ ...t, assistant: agentId }));
}

export const catchupOffered = (teamId: string) => state.teams[teamId]?.catchup === true;
export function setCatchupOffered(teamId: string) {
  if (state.teams[teamId] && !state.teams[teamId].catchup) setTeam(teamId, t => ({ ...t, catchup: true }));
}

/** DEX-29: new shell; returns its id (the caller opens its panel). */
export function addShell(teamId: string, cwd: string | null): string {
  const id = qid(hostOf(teamId), crypto.randomUUID());
  setTeam(teamId, t => ({ ...t, shells: [...t.shells, { id, cwd }] }));
  return id;
}

export function removeShell(teamId: string, id: string) {
  setTeam(teamId, t => ({ ...t, shells: t.shells.filter(s => s.id !== id) }));
}

export function setSessionForAgent(agentId: string, sessionId: string, engine: string | null) {
  lastSession.set(agentId, { id: sessionId, engine: engine === null ? null : engineName(engine) });
  set(s => ({ ...s })); // listeners re-read resume ids; the save carries `sessions`
}

// ---- Team lifecycle (driven by src/team/store.ts) ----

export function setActiveTeam(teamId: string) {
  if (state.active !== teamId) set(s => ({ ...s, active: teamId }));
}

/** A team without a saved layout gets the default one when its workspace mounts. */
export function ensureTeam(teamId: string) {
  if (!state.teams[teamId]) set(s => ({ ...s, teams: { ...s.teams, [teamId]: { dock: null, shells: [] } } }));
}

export function dropTeam(teamId: string) {
  if (!state.teams[teamId]) return;
  set(s => {
    const teams = { ...s.teams };
    delete teams[teamId];
    return { ...s, teams };
  });
}

/** A deleted agent's resume id goes with it (its panel is dropped by the workspace). */
export function clearAgent(agentId: string) {
  if (lastSession.delete(agentId)) set(s => ({ ...s }));
}

/** DEX-30: the agent switched engine — a session id means nothing to another engine. */
export function forgetSession(agentId: string) {
  if (lastSession.delete(agentId)) set(s => ({ ...s }));
}

/**
 * DEX-34: reconcile saved sessions with the DB's engines — catches a switch made while the app was
 * closed (or by any path besides this UI). A mismatch is forgotten; a pre-DEX-34 entry (no engine)
 * is taken as the agent's current engine, so an upgrade keeps resume.
 */
export function syncSessionEngines(engineOf: Map<string, string>) {
  for (const [agentId, rec] of [...lastSession]) {
    if (!engineOf.has(agentId)) continue;
    const engine = engineName(engineOf.get(agentId));
    if (rec.engine === null) setSessionForAgent(agentId, rec.id, engine);
    else if (rec.engine !== engine) forgetSession(agentId);
  }
}

// ---- Persistence ----

let saveTimer: number | undefined;

function scheduleSave() {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    const sessions = Object.fromEntries(lastSession);
    callLocal("layout_save", { layout: { version: 2, active: state.active, teams: state.teams, sessions, favorite: state.favorite } }).catch(() => {});
  }, SAVE_DEBOUNCE_MS);
}

/** Load layout.json once at startup; a bad team entry is skipped (it gets its default layout). */
export async function hydrate() {
  let file: unknown;
  try {
    file = await callLocal("layout_load");
  } catch {
    return;
  }
  const raw = file as { active?: unknown; teams?: Record<string, unknown>; sessions?: Record<string, unknown>; favorite?: unknown } | null;
  for (const [agentId, rec] of Object.entries(raw?.sessions ?? {})) {
    const parsed = parseSession(rec);
    if (parsed) lastSession.set(agentId, parsed);
  }
  const teams: Record<string, LayoutState> = {};
  for (const [id, t] of Object.entries(raw?.teams ?? {})) {
    const parsed = parseTeam(t) ?? parseLegacyTeam(t);
    if (parsed) teams[id] = parsed;
  }
  box.state = state = { active: typeof raw?.active === "string" ? raw.active : null, teams, favorite: parseFavorite(raw?.favorite) };
  for (const l of listeners) l();
}

/** Patron düzeni: dock + kaynak takımdaki yuva sırası; bozuksa yok sayılır. */
function parseFavorite(raw: unknown): FavoriteRec | null {
  const { dock, leaders, workers, shells, from } = (raw ?? {}) as Record<string, unknown>;
  if (typeof dock !== "object" || dock === null || !("grid" in dock)) return null;
  const ids = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
  return { dock: dock as SerializedDockview, leaders: ids(leaders), workers: ids(workers), shells: ids(shells), from: typeof from === "string" ? from : "" };
}

function parseTeam(team: unknown): LayoutState | null {
  const { dock, shells, assistant, catchup } = (team ?? {}) as { dock?: unknown; shells?: unknown; assistant?: unknown; catchup?: unknown };
  if (!Array.isArray(shells)) return null;
  // dockview validates its own JSON at fromJSON; a rejected one falls back to the default layout there.
  const d = typeof dock === "object" && dock !== null && "grid" in dock ? (dock as SerializedDockview) : null;
  return { dock: d, shells: parseShells(shells), ...(typeof assistant === "string" ? { assistant } : {}), ...(catchup === true ? { catchup } : {}) };
}

/**
 * Pre-DEX-36: { left: PaneRec[], right: PaneRec[], dockShells, … }. Column shells and dock shells
 * both become shells; column agents seed the default layout's sides; resume ids move to `sessions`.
 */
function parseLegacyTeam(team: unknown): LayoutState | null {
  const { left, right, dockShells } = (team ?? {}) as { left?: unknown; right?: unknown; dockShells?: unknown };
  if (!Array.isArray(left) || !Array.isArray(right)) return null;
  const shells = parseShells(dockShells);
  const side = (panes: unknown[]) =>
    panes.flatMap(p => {
      const { id, agentId, sessionId, sessionEngine, kind, cwd } = (p ?? {}) as Record<string, unknown>;
      if (kind === "shell") {
        if (typeof id === "string" && id) shells.push({ id, cwd: typeof cwd === "string" ? cwd : null });
        return [];
      }
      if (typeof agentId !== "string" || !agentId) return [];
      if (typeof sessionId === "string" && sessionId && !lastSession.has(agentId)) {
        lastSession.set(agentId, { id: sessionId, engine: typeof sessionEngine === "string" && sessionEngine ? sessionEngine : null });
      }
      return [agentId];
    });
  return { dock: null, shells, seed: { left: side(left), right: side(right) } };
}

/** DEX-34: `{ id, engine }`; a bare string is the pre-DEX-34 form (engine unknown). */
function parseSession(raw: unknown): SessionRec | null {
  if (typeof raw === "string") return raw ? { id: raw, engine: null } : null;
  const { id, engine } = (raw ?? {}) as { id?: unknown; engine?: unknown };
  return typeof id === "string" && id ? { id, engine: typeof engine === "string" && engine ? engine : null } : null;
}

/** DEX-29: a bad entry is dropped, not the team's whole layout. */
function parseShells(raw: unknown): ShellRec[] {
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((s) => {
    const { id, cwd } = (s ?? {}) as { id?: unknown; cwd?: unknown };
    return typeof id === "string" && id ? [{ id, cwd: typeof cwd === "string" ? cwd : null }] : [];
  });
}
