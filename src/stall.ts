// Stall watchdog (DEX-28): an agent that stays "working" with no hook event for N minutes gets an
// amber clock (dock bar + hover card), its leader a note when it is on a delegated job, and the patron
// a desktop notification. One timer per agent, re-armed by every hook event: no polling, no
// transcript reads. It only tells; nothing is ever stopped.
//
// A running Bash sends PreToolUse and nothing until PostToolUse, so the backend passes its declared
// timeout (`toolGraceMs`, crates/dex-core/src/hooks.rs) and the threshold grows by that much while it runs.
import { useSyncExternalStore } from "react";
import { callHost, qid, splitId } from "./api/hosts";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { findAgent, getStatus } from "./agents";
import { pastePing, pingable } from "./delegations";
import { POPOUT } from "./api/platform";
import { kept, listenKept } from "./hot";
import { soundEvent } from "./sound";

const hot = import.meta.hot;

const KEY = "dex.stallMinutes";
// #2055: 20 dk — a worker on a long job is told about once, not nudged (the backend sends one note per silence).
const DEFAULT_MINUTES = 20;
/** Palette choices; 0 = off. */
export const STALL_CHOICES = [0, 5, 10, 20, 30];

export function stallMinutes(): number {
  try {
    const raw = localStorage.getItem(KEY);
    const v = Number(raw);
    return raw === null || !Number.isFinite(v) || v < 0 ? DEFAULT_MINUTES : v;
  } catch {
    return DEFAULT_MINUTES;
  }
}

interface Watch {
  /** Last hook event (ms epoch). */
  last: number;
  /** Extra silence the running tool may take. */
  grace: number;
  timer: number;
}

const watches = kept(hot, "watches", () => new Map<string, Watch>());
/** Agent → minutes of silence when it was marked. */
const stalled = kept(hot, "stalled", () => new Map<string, number>());
const box = kept(hot, "version", () => ({ v: 0 }));
const listeners = kept(hot, "listeners", () => new Set<() => void>());

function changed() {
  box.v++;
  for (const l of listeners) l();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
/** Re-renders on every mark change (read the marks with `stalledFor`). */
export const useStalled = () => useSyncExternalStore(subscribe, () => box.v);

/** Minutes of silence while still working, or null. A status change without a hook (Esc, exit) drops it. */
export const stalledFor = (id: string) => (getStatus(id) === "working" ? stalled.get(id) ?? null : null);

export const stallText = (min: number) => `${min} dk'dır hiçbir olay yok — takılmış olabilir`;

function schedule(id: string, w: Watch) {
  clearTimeout(w.timer);
  const m = stallMinutes();
  if (!m) return void watches.delete(id);
  w.timer = window.setTimeout(() => fire(id), Math.max(0, m * 60_000 + w.grace - (Date.now() - w.last)));
  watches.set(id, w);
}

function disarm(id: string) {
  const w = watches.get(id);
  if (w) clearTimeout(w.timer);
  watches.delete(id);
}

function fire(id: string) {
  const w = watches.get(id);
  watches.delete(id);
  if (!w || getStatus(id) !== "working") return; // interrupted (Esc) or closed since: no hook said so
  const min = Math.round((Date.now() - w.last) / 60_000);
  stalled.set(id, min);
  changed();
  if (POPOUT) return; // #2258: the mark shows here too; the note and the notification are the main window's
  soundEvent("agent_stalled");
  void tell(id, min);
}

async function tell(id: string, min: number) {
  const name = findAgent(id)?.name ?? id;
  // Leader: a note for its next Stop / prompt (backend); an idle leader gets the turn opened now.
  const [host, raw] = splitId(id);
  const leadRaw = await callHost<string | null>(host, "delegation_stalled", { agentId: raw, minutes: min }).catch(() => null);
  const lead = leadRaw && qid(host, leadRaw);
  if (lead && pingable(lead)) pastePing(lead, `[Dex] ${name} ${min} dk'dır sessiz, takılmış olabilir. Ayrıntı bu mesajın ekinde.`);
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (ok) sendNotification({ title: `${name} takılmış olabilir`, body: `${stallText(min)}. Dex hiçbir şeyi durdurmadı.` });
}

export function setStallMinutes(m: number) {
  try {
    localStorage.setItem(KEY, String(m));
  } catch {
    /* private storage: the default stays */
  }
  for (const [id, w] of watches) schedule(id, w);
  if (!m && stalled.size) {
    stalled.clear();
    changed();
  }
}

interface HookEvent {
  agentId: string;
  status: string | null;
  toolGraceMs?: number | null;
}

listenKept<HookEvent>(hot, "dex-hook", ({ payload }, host) => {
  const e = { ...payload, agentId: qid(host, payload.agentId) };
  const was = stalled.delete(e.agentId);
  // Events that set no status (e.g. a plain Notification) re-arm only a watch that is already running.
  if (e.status === "working" || (e.status === null && watches.has(e.agentId))) {
    schedule(e.agentId, { last: Date.now(), grace: e.toolGraceMs ?? 0, timer: watches.get(e.agentId)?.timer ?? 0 });
  } else if (e.status) disarm(e.agentId);
  if (was) changed();
});
