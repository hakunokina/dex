import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { recordSessionId } from "./session";
import { forgetSession } from "./layout/store";
import { kept, listenKept } from "./hot";
import { toast } from "./toast/store";
import { callHost, hostOf, onHostUp, qid } from "./api/hosts";
import { capturePty } from "./pty";
import { MOBILE } from "./api/platform";
import { soundEvent } from "./sound";

const hot = import.meta.hot;

export type AgentStatus = "offline" | "idle" | "working" | "waiting";

export interface Agent {
  id: string;
  name: string;
  shirt: string;
  hair: string;
  skin: string;
  teamId: string;
  kind: "leader" | "worker";
  title: string;
  model: string;
  /** DEX-13: `anthropic` (default) | `glm` | `muse`. */
  provider: string;
  /** DEX-26: the CLI the agent runs; only `claude` since #1710. */
  engine: string;
  /** DEX-17: zincirle geçilen geçici hedef; asıl ayarlar `engine/provider/model` olarak kalır. */
  activeEngine?: string | null;
  activeProvider?: string | null;
  activeModel?: string | null;
  chainReturnReady?: boolean;
  roleName?: string | null;
}

const lists = kept(hot, "lists", () => ({ all: [] as Agent[], active: [] as Agent[] }));
/** Members of the team on screen (live binding; filled from SQLite by src/team/store.ts). */
export let AGENTS: Agent[] = lists.active;
/** Every team's members. Agent ids are globally unique (they double as PTY ids). */
export let ALL_AGENTS: Agent[] = lists.all;

export function setAgents(all: Agent[], activeTeamId: string) {
  lists.all = ALL_AGENTS = all;
  lists.active = AGENTS = all.filter((a) => a.teamId === activeTeamId);
}

export const findAgent = (id: string | null) => ALL_AGENTS.find((a) => a.id === id);

// Status comes from Claude Code hooks (src-tauri/src/hooks.rs); the PTY only says whether the process lives.
const running = kept(hot, "running", () => new Set<string>());
const hookStatus = kept(hot, "hookStatus", () => new Map<string, AgentStatus>());
const statusListeners = kept(hot, "statusListeners", () => new Set<() => void>());
export const subscribeAgentStatus = (notify: () => void) => {
  statusListeners.add(notify);
  return () => void statusListeners.delete(notify);
};
const statusChanged = () => statusListeners.forEach(notify => notify());
// DEX-19: when the agent last turned idle (a leader ping waits for the status to settle).
const idleSince = kept(hot, "idleSince", () => new Map<string, number>());

export function markRunning(id: string, on: boolean) {
  if (on) running.add(id);
  else running.delete(id);
  hookStatus.delete(id);
  prompts.delete(id); // a new (or ended) session starts with an empty prompt
  idleSince.set(id, Date.now());
  if (on) setAgentError(id, null);
  statusChanged();
}

// DEX-13: why the agent's last turn/start failed, as one plain-Turkish sentence (providers.rs builds it
// from the StopFailure hook; the raw provider body never reaches the UI). Cleared by a new spawn or turn.
const agentError = kept(hot, "agentError", () => new Map<string, string>());
const chainNotice = kept(hot, "chainNotice", () => new Map<string, string>());
export const AGENT_ERROR_EVENT = "dex:agent-error";

export const getAgentError = (id: string) => chainNotice.get(id) ?? agentError.get(id) ?? null;

export function setAgentError(id: string, text: string | null) {
  if ((agentError.get(id) ?? null) === text) return;
  if (text) agentError.set(id, text);
  else agentError.delete(id);
  window.dispatchEvent(new CustomEvent(AGENT_ERROR_EVENT, { detail: id }));
}

function setChainNotice(id: string, text: string | null) {
  if (text) chainNotice.set(id, text);
  else chainNotice.delete(id);
  window.dispatchEvent(new CustomEvent(AGENT_ERROR_EVENT, { detail: id }));
}

export function dismissAgentAlert(id: string) {
  agentError.delete(id);
  setChainNotice(id, null);
}

/** Provider reset instant → local wall clock; the date only when it isn't today. */
function fmtReset(iso: string): string | null {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  const time = d.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString()
    ? time
    : `${d.toLocaleDateString("tr-TR", { day: "numeric", month: "long" })} ${time}`;
}

interface AgentErrorEvent {
  agentId: string;
  text: string;
  resetAt: string | null;
}

listenKept<AgentErrorEvent>(hot, "dex-agent-error", ({ payload: e }, host) => {
  const reset = e.resetAt ? fmtReset(e.resetAt) : null;
  setAgentError(qid(host, e.agentId), reset ? `${e.text} Sıfırlanma: ${reset}.` : e.text);
});

export function getStatus(id: string): AgentStatus {
  if (!running.has(id)) return "offline";
  return hookStatus.get(id) ?? "idle";
}

// Interrupts fire no hook (no Stop, no dedicated event), so the turn would stay "working".
// Watch the user's keys instead of Claude's output: when interrupted before any reply Claude
// prints nothing (it puts the prompt back), and the message it does print wraps with pane width.
// xterm sends a lone Esc as exactly "\x1b" (arrows etc. arrive as one "\x1b[…" string). A wrong
// guess (e.g. Esc dismissing a dialog mid-turn) is corrected by the next hook event.
export function noteInput(id: string, data: string) {
  const s = getStatus(id);
  const interrupt = (data === "\x1b" || data === "\x03") && (s === "working" || s === "waiting");
  if (interrupt) {
    hookStatus.set(id, "idle");
    idleSince.set(id, Date.now());
    statusChanged();
  }
  // DEX-19: what the patron is doing in this pane, so a leader ping never lands in a half-typed prompt.
  // Focus / mouse reports xterm sends on its own are not keys, nor its answers to Claude's terminal
  // queries (DA1/DA2, mode reports, cursor position, OSC colors, DCS/XTVERSION): those arrive right
  // after a session starts and would hold a never-touched pane "typing" forever (DEX-24).
  // Several replies can come in one chunk: only a chunk made of nothing but these is skipped.
  if (!isKeyInput(data)) return null;
  // An interrupt puts the prompt back into Claude's input box: text we never saw typed.
  const prompt = interrupt ? { chars: 0, unsure: true } : promptAfter(prompts.get(id) ?? EMPTY, data.replace(NOT_KEYS, ""));
  prompts.set(id, prompt);
  const state = { at: Date.now(), draft: prompt.chars > 0 || prompt.unsure };
  input.set(id, state);
  return state;
}

/** #18: the draft follows what is left in the prompt, so deleting it all ends the draft too.
 * Typed/pasted text adds, Backspace takes one off; Enter / Ctrl+C / Ctrl+U empty it. A key whose effect
 * on the text we cannot tell (arrows and history, Tab, Delete, Ctrl+W…) keeps it a draft until one of
 * those — a false draft only delays a job, a false empty would paste into the patron's text. */
function promptAfter(prev: Prompt, keys: string): Prompt {
  if (keys === "\r" || keys === "\x03" || keys === "\x15") return EMPTY;
  if (keys === "\x1b") return prev; // Esc alone leaves the text (double Esc is a menu)
  const text = keys.replace(PASTE_MARKS, "");
  if (/^[\x7f\b]+$/.test(text)) return { ...prev, chars: Math.max(0, prev.chars - text.length) };
  if (/^[^\x00-\x1f\x7f]+$/.test(text)) return { ...prev, chars: prev.chars + [...text].length };
  return { ...prev, unsure: true };
}

/** #58: the phone types into its own input line, not the PTY — unsent text there is a draft too. */
export function noteDraftLine(id: string, hasText: boolean) {
  const prompt = prompts.get(id) ?? EMPTY;
  const state = { at: Date.now(), draft: hasText || prompt.chars > 0 || prompt.unsure };
  input.set(id, state);
  return state;
}

/** #18: Claude put a prompt back we never saw typed (cancelled first reply): a draft until cleared. */
export function markPromptUnsure(id: string) {
  prompts.set(id, { chars: 0, unsure: true });
  input.set(id, { at: Date.now(), draft: true });
}

/** A patron key (or paste), not a report xterm sends by itself. */
export const isKeyInput = (data: string) => data.replace(NOT_KEYS, "") !== "";

type Prompt = { chars: number; unsure: boolean };
const EMPTY: Prompt = { chars: 0, unsure: false };
const prompts = kept(hot, "prompts", () => new Map<string, Prompt>());
const input = kept(hot, "input", () => new Map<string, { at: number; draft: boolean }>());
const NOT_KEYS = /\x1b\[[IO]|\x1b\[<[\d;]+[mM]|\x1b\[M[\s\S]{3}|\x1b\[[?>=][\d;]*\$?[a-zA-Z]|\x1b\[\d+;\d+R|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1bP[^\x1b]*\x1b\\/g;
const PASTE_MARKS = /\x1b\[20[01]~/g;

/** The patron's last key in the agent's pane (ms epoch, 0 = never) and whether unsent text may be in its prompt. */
export const inputState = (id: string) => input.get(id) ?? { at: 0, draft: false };

export const idleFor = (id: string) => (getStatus(id) === "idle" ? Date.now() - (idleSince.get(id) ?? 0) : 0);

interface HookEvent {
  agentId: string;
  event: string;
  status: AgentStatus | null;
  sessionId: string | null;
}

// FIX-4: the backend says which session `--resume` may open (kept only once a turn is on disk;
// dropped when the CLI could not find it). SessionStart alone is no longer a resumable id.
listenKept<{ agentId: string; sessionId: string | null; engine: string }>(hot, "agent-session", ({ payload: e }, host) => {
  if (e.sessionId) recordSessionId(qid(host, e.agentId), e.sessionId, e.engine);
  else forgetSession(qid(host, e.agentId));
});

// FIX-4: the saved session was gone; the backend started a fresh one — one line in the pane.
listenKept<{ agentId: string; text: string }>(hot, "dex-resume-lost", ({ payload: e }, host) => setAgentError(qid(host, e.agentId), e.text));

// DEX-17: motor zinciri geçişi — pane'de tek satır + masaüstü bildirimi.
interface ChainEvent {
  agentId: string;
  from: string;
  to: string;
  resetAt: string | null;
  text: string;
}
listenKept<ChainEvent>(hot, "dex-chain", ({ payload: e }, host) => {
  const id = qid(host, e.agentId);
  const reset = e.resetAt ? fmtReset(e.resetAt) : null;
  setChainNotice(id, reset ? `${e.text} Sıfırlanma: ${reset}.` : e.text);
  notifyChain(id, e.text);
});
listenKept<{ agentId: string; text: string; resetAt?: string | null }>(hot, "dex-chain-wait", ({ payload: e }, host) => {
  soundEvent("chain_exhausted");
  const id = qid(host, e.agentId);
  // #1018: zincirde kotası olan motor yoksa en erken sıfırlanma.
  const reset = e.resetAt ? fmtReset(e.resetAt) : null;
  const text = reset ? `${e.text} En erken sıfırlanma: ${reset}.` : e.text;
  setChainNotice(id, text);
  notifyChain(id, text);
});

async function notifyChain(id: string, body: string) {
  if (document.hasFocus()) return;
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (ok) sendNotification({ title: `${findAgent(id)?.name ?? id}: motor zinciri`, body });
}

listenKept<HookEvent>(hot, "dex-hook", ({ payload }, host) => {
  const e = { ...payload, agentId: qid(host, payload.agentId) };
  if (!e.status || !running.has(e.agentId)) return;
  const prev = getStatus(e.agentId);
  hookStatus.set(e.agentId, e.status);
  if (e.status === "idle" && prev !== "idle") idleSince.set(e.agentId, Date.now());
  if (e.status === "working") setAgentError(e.agentId, null); // a new turn got through
  if (e.status === "waiting" && prev !== "waiting") notifyWaiting(e.agentId);
  statusChanged();
});

interface BackendAgentState {
  agentId: string;
  status: AgentStatus;
  sessionId: string | null;
  engine: string | null;
  generation?: number;
}

function applyBackendState(s: BackendAgentState) {
  if (s.status === "offline") running.delete(s.agentId);
  else {
    running.add(s.agentId);
    if (!MOBILE) void capturePty(s.agentId, s.generation).catch(error => setAgentError(s.agentId, String(error)));
  }
  hookStatus.set(s.agentId, s.status);
  if (s.status === "idle") idleSince.set(s.agentId, Date.now());
  statusChanged();
}

listenKept<BackendAgentState>(hot, "agent-status", ({ payload }, host) => applyBackendState({ ...payload, agentId: qid(host, payload.agentId) }));
// S4: per host, again after every reconnect — what ran on (or stopped on) the server meanwhile.
kept(hot, "statusUp", () =>
  onHostUp((host) => {
    void callHost<BackendAgentState[]>(host, "agent_status_list")
      .then((states) => {
        const listed = new Set(states.map((s) => qid(host, s.agentId)));
        for (const id of [...running]) if (hostOf(id) === host && !listed.has(id)) applyBackendState({ agentId: id, status: "offline", sessionId: null, engine: null });
        for (const s of states) applyBackendState({ ...s, agentId: qid(host, s.agentId) });
      })
      .catch(() => {});
  }),
);

async function notifyWaiting(id: string) {
  soundEvent("agent_waiting");
  // Modern §6: Dex in front → an in-app toast instead of the desktop notification.
  if (document.hasFocus()) {
    toast({
      key: `waiting:${id}`,
      tone: "info",
      title: `${findAgent(id)?.name ?? id} seni bekliyor`,
      body: "İzin ya da girdi bekliyor.",
      actions: [{ label: "Aç", run: (ctx) => ctx.focusAgent(id) }],
    });
    return;
  }
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  const name = findAgent(id)?.name ?? id;
  if (ok) sendNotification({ title: `${name} seni bekliyor`, body: "İzin ya da girdi bekliyor." });
}
