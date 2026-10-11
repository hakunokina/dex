// Worker → leader questions (DEX-24): rows from crates/dex-core/src/questions.rs. Delivery is the
// backend's (Stop block / UserPromptSubmit / SessionStart hand each row out once); this module only
// opens a turn for answers: idle worker → one pasted line, closed worker → resumed with it.
// Open questions are delivered by core without depending on this client's timer.
import { useSyncExternalStore } from "react";
import { callHost, LOCAL, onHostUp, qid, remoteHosts } from "./api/hosts";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { getStatus } from "./agents";
import { pastePing, pingable } from "./delegations";
import { kept, listenKept } from "./hot";
import { soundEvent } from "./sound";

const hot = import.meta.hot;

export interface Question {
  /** The host's row id; unique only within `host` (S4). */
  id: number;
  host: string;
  /** `<host>:<id>`. */
  key: string;
  delegationId: number;
  delegationStatus: string;
  worker: string;
  workerName: string;
  leader: string;
  leaderName: string;
  teamId: string;
  teamName: string;
  question: string;
  options: string[];
  context: string | null;
  status: "open" | "escalated" | "answered";
  note: string | null;
  answer: string | null;
  answeredBy: "leader" | "patron" | null;
  leaderDeliveredAt: number | null;
  answerDeliveredAt: number | null;
  createdAt: number;
  updatedAt: number;
}

const box = kept(hot, "rows", () => ({ rows: [] as Question[] }));
const byHost = kept(hot, "byHost", () => new Map<string, Question[]>());
let rows = box.rows;
const listeners = kept(hot, "listeners", () => new Set<() => void>());
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
const snapshot = () => rows;
export const getQuestions = snapshot;
export const useQuestions = () => useSyncExternalStore(subscribe, snapshot);

const fromHost = (host: string, q: Question): Question => ({
  ...q,
  host,
  key: `${host}:${q.id}`,
  worker: qid(host, q.worker),
  leader: qid(host, q.leader),
  teamId: qid(host, q.teamId),
});

async function reload(host: string) {
  const list = await callHost<Question[]>(host, "question_list");
  byHost.set(host, list.map((q) => fromHost(host, q)));
  const hosts = new Set([LOCAL, ...remoteHosts().map((h) => h.id)]);
  for (const h of [...byHost.keys()]) if (!hosts.has(h)) byHost.delete(h);
  box.rows = rows = [...byHost.values()].flat();
  for (const l of listeners) l();
}

export const answerQuestion = (q: Question, text: string) => callHost<Question>(q.host, "question_answer", { id: q.id, text });

/** Unanswered and its job still going on. */
const pending = (q: Question) => q.status !== "answered" && (q.delegationStatus === "running" || q.delegationStatus === "queued");

/** The worker's newest pending question (dock `?`, hover line). */
export function workerQuestion(list: Question[], worker: string): Question | null {
  let last: Question | null = null;
  for (const q of list) if (q.worker === worker && pending(q) && (!last || q.id > last.id)) last = q;
  return last;
}

/** Questions waiting on this leader (escalated ones are the patron's now). */
export const leaderPending = (list: Question[], leader: string) =>
  list.filter((q) => q.leader === leader && q.status === "open" && pending(q)).length;

// ---- turns for idle / closed receivers ----

/** A closed worker is resumed with this prompt (TerminalPane answers it). */
export const RESUME_EVENT = "dex:resume-with";
export interface ResumeWith {
  agentId: string;
  prompt: string;
  handled: boolean;
}

const RETRY_MS = 30000;
const pinged = kept(hot, "pinged", () => new Map<string, number>()); // answer key → at

function due(key: string) {
  const at = pinged.get(key);
  return !at || Date.now() - at >= RETRY_MS;
}

function ping() {
  const byAgent = new Map<string, { keys: string[]; text: string[] }>();
  const add = (agent: string, key: string, text: string) => {
    const e = byAgent.get(agent) ?? { keys: [], text: [] };
    e.keys.push(key);
    e.text.push(text);
    byAgent.set(agent, e);
  };
  for (const q of rows) {
    if (q.status === "answered" && q.answerDeliveredAt === null && due(`a${q.key}`)) add(q.worker, `a${q.key}`, `#${q.id}`);
  }
  for (const [agent, { keys, text }] of byAgent) {
    const note = `[Dex] Sorunun cevabı geldi: ${text.join(", ")}. Ayrıntı bu mesajın ekinde.`;
    if (getStatus(agent) === "offline") {
      // Closed worker: its job waits for this answer, so it is resumed (a closed leader is not: SessionStart).
      const req: ResumeWith = { agentId: agent, prompt: note, handled: false };
      window.dispatchEvent(new CustomEvent(RESUME_EVENT, { detail: req }));
      if (!req.handled) continue;
    } else if (!pingable(agent)) continue;
    else pastePing(agent, note);
    for (const k of keys) pinged.set(k, Date.now());
  }
}
kept(hot, "pingTimer", () => setInterval(() => ping(), 1000));

// Escalated → desktop notification once (the card is in the UI).
const told = kept(hot, "told", () => new Set<string>());
async function notifyEscalated() {
  const fresh = rows.filter((q) => q.status === "escalated" && !told.has(q.key));
  if (!fresh.length) return;
  for (const q of fresh) told.add(q.key);
  soundEvent("question_escalated");
  if (document.hasFocus()) return; // DEX-28: the patron is looking at Dex, the card is enough
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (!ok) return;
  for (const q of fresh) {
    sendNotification({ title: `${q.leaderName} bir soruyu sana çıkardı`, body: `${q.workerName}: ${q.question.slice(0, 160)}` });
  }
}

listenKept(hot, "questions-changed", (_, host) => {
  void reload(host)
    .then(() => {
      ping();
      return notifyEscalated();
    })
    .catch(() => {});
});
// Delegation status feeds `pending` (a finished job's question stops marking the dock).
listenKept(hot, "delegations-changed", (_, host) => void reload(host).catch(() => {}));

kept(hot, "rowsUp", () =>
  onHostUp((host) =>
    void reload(host)
      .then(() => {
        for (const q of rows) if (q.host === host && q.status === "escalated") told.add(q.key); // already on screen as a card
      })
      .catch(() => {}),
  ),
);
