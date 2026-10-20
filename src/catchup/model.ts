// #2235: "Sen yokken" — pure view logic over the catchup contract (src/api/catchup.ts):
// the three sections in leader-decided order, the per-section cap ("+N daha"), kind labels,
// the waiting badge count, and where a click goes (panel-level only, no selection APIs).
import type { Catchup, CatchupBucket, CatchupItem, CatchupKind } from "../api/catchup";

export type SectionKey = "waiting" | "changed" | "memories";
export interface Section { key: SectionKey; title: string; bucket: CatchupBucket }

/** One screenful at most: a folded section shows this many rows, the rest hides behind "+N daha". */
export const SECTION_LIMIT = 6;

export function sections(c: Catchup): Section[] {
  return [
    { key: "waiting", title: "Seni bekleyenler", bucket: c.waiting },
    { key: "changed", title: "Sen yokken değişenler", bucket: c.changed },
    { key: "memories", title: "Yeni", bucket: c.memories },
  ];
}

export const bucketCount = (b: CatchupBucket): number => b.items.length + b.more;

/** The tab badge: only what waits on the patron. */
export const waitingCount = (c: Catchup): number => bucketCount(c.waiting);

export const isEmpty = (c: Catchup): boolean =>
  bucketCount(c.waiting) === 0 && bucketCount(c.changed) === 0 && bucketCount(c.memories) === 0;

/** Rows to paint and how many stay hidden (unfolded overflow + the server's `more`). */
export function visibleItems(bucket: CatchupBucket, expanded: boolean): { shown: CatchupItem[]; hidden: number } {
  const shown = expanded ? bucket.items : bucket.items.slice(0, SECTION_LIMIT);
  return { shown, hidden: bucket.items.length - shown.length + bucket.more };
}

export const KIND_LABEL: Record<CatchupKind, string> = {
  question_escalated: "Soru sana çıktı",
  question_open: "Soru bekliyor",
  member_proposal: "Üye önerisi",
  model_approval: "Model onayı",
  job_change_approval: "İş değişikliği onayı",
  gpu_approval: "GPU onayı",
  merge_waiting: "Birleştirme bekliyor",
  review_fix: "Düzeltme bekliyor",
  delegation_done: "Bitti",
  delegation_done_no_report: "Raporsuz bitti",
  delegation_failed: "Başarısız",
  delegation_merged: "Birleşti",
  plan_done: "Plan bitti",
  plan_cancelled: "Plan iptal",
  memory: "Hafıza",
};

export const kindLabel = (kind: string): string => KIND_LABEL[kind as CatchupKind] ?? kind;

export type Target =
  | { type: "agent"; agentId: string }
  | { type: "board" }
  | { type: "memory" }
  | { type: "gpu" };

/** Agents the caller resolved for this item's `ref` (undefined: not found / not applicable). */
export interface Resolved { worker?: string; leader?: string }

/**
 * Where a row goes, panel-level (leader decision: no card/node selection APIs):
 * a delegation opens its worker (Görevler when the row is gone), a plan the board, a
 * question its worker, a proposal/approval its leader, a memory the memory map.
 */
export function targetFor(item: Pick<CatchupItem, "kind">, r: Resolved): Target {
  switch (item.kind) {
    case "memory": return { type: "memory" };
    case "plan_done": case "plan_cancelled": return { type: "board" };
    case "gpu_approval": return { type: "gpu" };
    case "question_escalated": case "question_open":
      return r.worker ? { type: "agent", agentId: r.worker } : r.leader ? { type: "agent", agentId: r.leader } : { type: "board" };
    case "member_proposal": case "job_change_approval":
      return r.leader ? { type: "agent", agentId: r.leader } : { type: "board" };
    default: // delegations + model_approval (its ref may be a delegation id)
      return r.worker ? { type: "agent", agentId: r.worker } : r.leader ? { type: "agent", agentId: r.leader } : { type: "board" };
  }
}

/** Host-filtered local lists a row's numeric `ref` can be resolved against. */
export interface RefSources {
  questions: { id: number; worker?: string; leader?: string }[];
  proposals: { id: number; leader?: string }[];
  delegations: { id: number; worker?: string; leader?: string }[];
}

/**
 * Resolve a row's agents from the list its `ref` actually belongs to (dex-core::catchup):
 * questions → questions, proposals → member_proposals, delegations/merge/review →
 * delegations. Kinds whose `ref` names another table (`gpu_approval` → gpu_requests,
 * `plan_*` → plans, `memory` → graph id) resolve to nothing — looking their number up in
 * delegations opens an unrelated same-id job's agents on a collision (#2240). The one
 * overlap is `model_approval`: usually a proposal id, but core's proposal-less branch
 * emits a delegation id, so proposals first, then delegations.
 */
export function resolveRef(item: Pick<CatchupItem, "kind" | "ref">, s: RefSources): Resolved {
  if (typeof item.ref !== "number") return {};
  const ref = item.ref;
  const deleg = (): Resolved => {
    const d = s.delegations.find(d => d.id === ref);
    return d ? { worker: d.worker, leader: d.leader } : {};
  };
  switch (item.kind) {
    case "question_escalated": case "question_open": {
      const q = s.questions.find(q => q.id === ref);
      return q ? { worker: q.worker, leader: q.leader } : {};
    }
    case "member_proposal": case "job_change_approval": {
      const p = s.proposals.find(p => p.id === ref);
      return p ? { leader: p.leader } : {};
    }
    case "model_approval": {
      const p = s.proposals.find(p => p.id === ref);
      return p ? { leader: p.leader } : deleg();
    }
    case "gpu_approval": case "plan_done": case "plan_cancelled": case "memory":
      return {};
    default: // delegation_* + merge_waiting + review_fix
      return deleg();
  }
}

/** Short Turkish relative time for a row (`progressModel.ago` stops at hours; the window is 7 days). */
export function whenAgo(at: number, now: number): string {
  const min = Math.floor(Math.max(0, now - at) / 60_000);
  if (min < 1) return "az önce";
  if (min < 60) return `${min} dk önce`;
  if (min < 24 * 60) return `${Math.floor(min / 60)} sa önce`;
  return `${Math.floor(min / (24 * 60))} gün önce`;
}

/**
 * The 10 s auto-ack waits only while the panel is on screen and the window focused.
 * Returns the `generatedAt` the wait belongs to (null: not waiting). The caller arms its
 * timer for exactly this value and re-arms when it changes, so a refresh landing mid-wait
 * starts its own wait instead of being acked after ~1 s on screen (#2240).
 */
export const ACK_MS = 10_000;
export const ackWait = (o: { visible: boolean; focused: boolean; acked: boolean; generatedAt?: number }): number | null =>
  o.visible && o.focused && !o.acked && o.generatedAt !== undefined ? o.generatedAt : null;
