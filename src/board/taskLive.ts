// Pano kartının canlı rozeti + sade önizleme (saf; React yok).
// Rozet kuralı tek fonksiyonda: çekirdek yeni alan verince yalnız taskLive değişir.
import type { Delegation } from "../delegations";
import type { AgentStatus } from "../agents";
import type { Task } from "./api";

/** Sütundan bağımsız canlı durum: devretmenin gerçek durumundan türetilir. */
export type LiveKind = "working" | "queued" | "patron" | "merge";

export interface LiveState {
  kind: LiveKind | null;
  /** Göreve bağlı en yeni devretmenin iş numarası (kartta `#id`); yoksa null. */
  delegationId: number | null;
}

export const LIVE_LABEL: Record<LiveKind, string> = {
  working: "Çalışıyor",
  queued: "Sırada",
  patron: "Patrondan bekliyor",
  merge: "Birleştirme bekliyor",
};

/** "Sürüyor" sütunu bu sırayla dizilir: çalışanlar üstte. */
export const doingRank = (kind: LiveKind | null): number =>
  kind === "working" ? 0 : kind === "queued" ? 1 : kind === "patron" ? 2 : kind === "merge" ? 3 : 4;

type LiveDelegation = Pick<Delegation, "id" | "host" | "taskId" | "status" | "toAgent" | "modelApproval">;

const live = (status: Delegation["status"]) => status === "queued" || status === "running";
const finished = (status: Delegation["status"]) => status === "done" || status === "done_no_report";

export function taskLive(
  task: Pick<Task, "id" | "status">,
  host: string,
  delegations: readonly LiveDelegation[],
  agentStatus: (id: string) => AgentStatus,
): LiveState {
  let newest: LiveDelegation | null = null;
  for (const d of delegations)
    if (d.host === host && d.taskId === task.id && (!newest || d.id > newest.id)) newest = d;
  if (!newest) return { kind: null, delegationId: null };
  if (live(newest.status)) {
    const st = newest.toAgent ? agentStatus(newest.toAgent) : null;
    if (newest.modelApproval === "pending" || st === "waiting")
      return { kind: "patron", delegationId: newest.id };
    if (newest.status === "running" && st === "working") return { kind: "working", delegationId: newest.id };
    return { kind: "queued", delegationId: newest.id };
  }
  // Devretme bitti ama görev hâlâ açık: dalı birleşmedi.
  if (finished(newest.status) && task.status !== "done") return { kind: "merge", delegationId: newest.id };
  return { kind: null, delegationId: newest.id };
}

const HEADING = /^#{1,6}\s/;
const LIST_MARK = /^([-*+]|\d+[.)])\s+/;

/** Kart önizlemesi: markdown ham görünmez; biçim başlıkları atılır, ilk anlamlı cümle(ler). */
export function plainPreview(md: string): string {
  const lines: string[] = [];
  for (const raw of md.split("\n")) {
    let line = raw.trim();
    if (!line || HEADING.test(line)) continue;
    line = line
      .replace(LIST_MARK, "")
      .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/[*_`~]+/g, "")
      .replace(/\s{2,}/g, " ")
      .trim();
    if (line) lines.push(line);
  }
  const text = lines.join(" ");
  if (!text) return "";
  // En çok iki cümle, en çok 220 karakter.
  const sentences = text.match(/[^.!?…]+[.!?…]+["”']?|\S.*$/g) ?? [text];
  let out = "";
  for (const s of sentences.slice(0, 2)) {
    const next = out ? `${out} ${s.trim()}` : s.trim();
    if (next.length > 220) break;
    out = next;
  }
  if (!out) out = text.slice(0, 220).trim();
  return text.length > out.length ? `${out}…` : out;
}
