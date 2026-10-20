// Delegations (DEX-4): rows from dex-core. Starting, queueing and delivery are backend-owned.
import { useSyncExternalStore } from "react";
import { callHost, LOCAL, onHostUp, qid, qOpt, remoteHosts, splitId } from "./api/hosts";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { findAgent, getStatus, idleFor, inputState } from "./agents";
import { writePty } from "./pty";
import { POPOUT } from "./api/platform";
import { kept, listenKept } from "./hot";
import { toast } from "./toast/store";
import { moveWorkerPanel } from "./layout/panels";
import { workerMoves } from "./layout/workerGroups";
import { jobProjectName } from "./project/store";
import { soundEvent } from "./sound";
import { createDelegationSoundTracker } from "./sound/model";
import { isActive, unreadResult as findUnreadResult } from "./delegationsModel";
export { activeCount, isActive } from "./delegationsModel";

const hot = import.meta.hot;
// Retire the previous module's result ping after a hot update as well.
if (hot) {
  if (hot.data.pingTimer !== undefined) clearInterval(hot.data.pingTimer);
  delete hot.data.pingTimer;
  hot.data.rowsUp?.();
  delete hot.data.rowsUp;
}

export type DelegationStatus = "queued" | "running" | "done" | "done_no_report" | "failed";

export interface Delegation {
  /** The host's row id (shown as `#id`); unique only within `host`. */
  id: number;
  /** S4: where the row lives; agent/team ids below are qualified. */
  host: string;
  /** `<host>:<id>` — the client-wide key. */
  key: string;
  fromAgent: string;
  toAgent: string;
  /** Durable role pool; optional for older hosts. */
  targetRole?: string | null;
  queuePosition?: number | null;
  teamId: string;
  serviceTeamId: string | null;
  /** #838: the job's project (qualified), fixed at delegation; absent from older hosts. */
  projectId?: string | null;
  prompt: string;
  status: DelegationStatus;
  model?: string | null;
  modelApproval?: "pending" | "approved" | "rejected" | null;
  actualModel?: string | null;
  actualProvider?: string | null;
  actualEngine?: string | null;
  summary: string | null;
  resultPath: string | null;
  taskId: number | null;
  createdAt: number;
  updatedAt: number;
  /** DEX-19: the leader got the result (Stop block / prompt context / SessionStart context). */
  deliveredAt?: number | null;
  /** DEX-19: the leader finished the turn the result arrived in. */
  readAt?: number | null;
  /** DEX-19: the patron opened the worker's pane (clears the dock mark; delivery to the leader is unaffected). */
  seenAt?: number | null;
}

const taken = (d: Delegation) => !!d.readAt || !!d.seenAt || seen.has(d.key);

const box = kept(hot, "rows", () => ({ rows: [] as Delegation[] }));
const byHost = kept(hot, "byHost", () => new Map<string, Delegation[]>());
const delegationSounds = kept(hot, "delegationSounds", () => {
  const tracker = createDelegationSoundTracker();
  for (const [host, list] of byHost) tracker.seed(host, list);
  return tracker;
});
let rows = box.rows;
const listeners = kept(hot, "listeners", () => new Set<() => void>());
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
const snapshot = () => rows;
/** Current rows outside React (the office hover card reads them every frame). */
export const getDelegations = snapshot;
export const useDelegations = () => useSyncExternalStore(subscribe, snapshot);

function fromHost(host: string, d: Delegation): Delegation {
  return {
    ...d,
    host,
    key: `${host}:${d.id}`,
    fromAgent: qid(host, d.fromAgent),
    toAgent: d.toAgent ? qid(host, d.toAgent) : "",
    teamId: qid(host, d.teamId),
    serviceTeamId: qOpt(host, d.serviceTeamId),
    projectId: qOpt(host, d.projectId),
  };
}

async function reload(host: string) {
  const list = await callHost<Delegation[]>(host, "delegation_list");
  const previous = byHost.get(host);
  const next = list.map((d) => fromHost(host, d));
  delegationSounds.seed(host, next); // initial rows are history, not new work
  byHost.set(host, next);
  if (previous) for (const move of workerMoves(previous, next)) moveWorkerPanel(move.agentId, move.finished);
  const hosts = new Set([LOCAL, ...remoteHosts().map((h) => h.id)]);
  for (const h of [...byHost.keys()]) if (!hosts.has(h)) byHost.delete(h); // removed host
  box.rows = rows = [...byHost.values()].flat();
  for (const l of listeners) l();
}

const finished = (d: Delegation) => !isActive(d);

/** The worker's newest delegation, when it is finished and nobody has taken the result in yet. */
export function unreadResult(list: Delegation[], agentId: string): Delegation | null {
  return findUnreadResult(list, agentId, taken);
}

/**
 * Newest finished delegation of the worker, for the hover line ("Bitti — lidere bildirildi / okundu").
 * #838: `project` names the job's project where the team has several ("#12 · Site Bitti — …").
 */
export function lastResultLine(list: Delegation[], agentId: string, project?: (d: Delegation) => string | null): string | null {
  if (!agentId) return null;
  let last: Delegation | null = null;
  for (const d of list) if (d.toAgent === agentId && (!last || d.id > last.id)) last = d;
  if (!last || !finished(last)) return null;
  const what = last.status === "done" ? "Bitti" : last.status === "done_no_report" ? "Raporsuz bitti" : "Başarısız";
  const where = taken(last) ? "okundu" : last.deliveredAt ? "lidere bildirildi" : "lider bekleniyor";
  const name = project?.(last);
  return `#${last.id}${name ? ` · ${name}` : ""} ${what} — ${where}`;
}

// The patron opened the worker's pane: its finished results count as read (backend keeps it; local set
// answers at once and covers a backend without the command).
const seen = kept(hot, "seen", () => new Set<string>());
export function markSeen(agentId: string) {
  if (!agentId) return;
  const keys = rows.filter((d) => d.toAgent === agentId && finished(d) && !taken(d)).map((d) => d.key);
  if (!keys.length) return;
  for (const k of keys) seen.add(k);
  box.rows = rows = [...rows];
  for (const l of listeners) l();
  const [host, raw] = splitId(agentId);
  void callHost(host, "delegation_seen", { agentId: raw }).catch(() => {});
}

// A finished job reaches the leader without waiting for its next prompt (DEX-19):
// - working/waiting → its next Stop hook answers `decision: block` with the result (backend);
// - idle → the core pastes a note and confirms UserPromptSubmit before marking delivery;
// - offline → SessionStart context when it opens (backend) + a desktop notification now.
const told = kept(hot, "told", () => new Set<string>());
listenKept<Delegation | null>(hot, "delegations-changed", ({ payload }, host) => {
  void reload(host)
    .catch(() => {});
  const d = payload && fromHost(host, payload);
  if (d) {
    const event = delegationSounds.change(host, d);
    if (event) soundEvent(event);
  }
  if (!d || isActive(d) || told.has(d.key)) return;
  told.add(d.key);
  if (getStatus(d.fromAgent) === "offline") void notifyDone(d);
  toastDone(d);
});

/** Modern §6: every finished job also shows in the app — the patron sees it without watching the leader. */
function toastDone(d: Delegation) {
  const who = findAgent(d.toAgent)?.name ?? d.toAgent;
  const project = jobProjectName(d);
  const [mark, how] = d.status === "done" ? ["✓", "bitti"] : d.status === "done_no_report" ? ["!", "raporsuz bitti"] : ["✗", "başarısız"];
  toast({
    key: `delegation:${d.key}`,
    tone: d.status === "done" ? "ok" : d.status === "failed" ? "err" : "info",
    title: `${mark} ${who} #${d.id} ${how}${project ? ` · ${project}` : ""}`,
    body: d.summary?.slice(0, 140) || undefined,
    actions: [{ label: "Aç", run: (ctx) => ctx.focusAgent(d.toAgent) }],
  });
}

/** Patron's keys in the leader pane within this window → the ping waits. */
const QUIET_MS = 5000;
/** Status must stay idle this long (a Stop hook that blocks flips it for a moment). */
const SETTLE_MS = 1500;
/** Idle long enough and the patron is not typing in that pane (DEX-24 pings workers the same way). */
export function pingable(agentId: string) {
  if (idleFor(agentId) < SETTLE_MS) return false; // offline / working / waiting / just settled
  const k = inputState(agentId);
  return !k.draft && Date.now() - k.at >= QUIET_MS; // the patron is typing here: never clobber the prompt
}

/** One line as a new turn; the UserPromptSubmit hook attaches the payload. */
export function pastePing(agentId: string, note: string) {
  if (POPOUT) return; // #2258: the main window pings; a pop-out would paste the note a second time
  // Bracketed paste keeps the note one literal input; Enter goes separately so it is not part of the paste.
  void writePty(agentId, `\x1b[200~${note}\x1b[201~`)
    .then(() => new Promise((r) => setTimeout(r, 150)))
    .then(() => writePty(agentId, "\r"))
    .catch(() => {});
}

async function notifyDone(d: Delegation) {
  if (POPOUT) return;
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (!ok) return;
  const who = findAgent(d.toAgent)?.name ?? d.toAgent;
  const lead = findAgent(d.fromAgent)?.name ?? d.fromAgent;
  const how = d.status === "done" ? "bitirdi" : d.status === "done_no_report" ? "raporsuz bitirdi" : "başaramadı";
  sendNotification({ title: `${who} #${d.id} işini ${how}`, body: `${lead} kapalı; açıldığında sonucu görecek.${d.summary ? `\n${d.summary.slice(0, 140)}` : ""}` });
}

// Each host's rows at start and again after it reconnects (what finished meanwhile).
kept(hot, "rowsUp", () => onHostUp((host) => void reload(host).catch(() => {})));
