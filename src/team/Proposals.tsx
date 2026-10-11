// DEX-22: approval cards for leaders' `propose_member` calls. Only the patron adds a member; the
// leader hears the answer in its next hook context (crates/dex-core/src/proposals.rs).
import { useState, useSyncExternalStore } from "react";
import { callHost, LOCAL, onEvent, onHostUp, qid, qOpt, remoteHosts, subscribeHosts } from "../api/hosts";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { soundEvent } from "../sound";
import "./proposals.css";

export interface Proposal {
  id: number;
  delegationId?: number | null;
  /** S4: the host the leader runs on (ids below are qualified). */
  host: string;
  key: string;
  teamId: string;
  teamName: string;
  leader: string;
  leaderName: string;
  roleSlug: string;
  roleName: string;
  name: string | null;
  model: string;
  provider: string;
  reason: string;
  status: "pending" | "approved" | "rejected";
  agentId: string | null;
  createdAt: number;
}

async function listProposals(host: string): Promise<Proposal[]> {
  const rows = await callHost<Proposal[]>(host, "proposal_list");
  return rows.map((p) => ({ ...p, host, key: `${host}:${p.id}`, teamId: qid(host, p.teamId), leader: qid(host, p.leader), agentId: qOpt(host, p.agentId) }));
}
const decide = (p: Proposal, approve: boolean, name: string | null) =>
  callHost<Proposal>(p.host, "proposal_decide", { id: p.id, approve, name });

// DEX-28: a new card while Dex is not in focus → desktop notification, once per proposal. Cards
// already there at startup are on screen when the patron looks.
const told = new Set<string>();
let primed = false;
async function notifyNew(rows: Proposal[]) {
  const fresh = rows.filter((p) => p.status === "pending" && !told.has(p.key));
  for (const p of fresh) told.add(p.key);
  const first = !primed;
  primed = true;
  if (first || !fresh.length) return;
  soundEvent("member_proposed");
  if (document.hasFocus()) return;
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (!ok) return;
  for (const p of fresh) {
    sendNotification({ title: `${p.leaderName} ${p.delegationId ? "iş modeli onayı istiyor" : "yeni üye öneriyor"}`, body: `${p.roleName} · ${p.teamName}\n${p.reason.slice(0, 160)}` });
  }
}

// #243: one list for the corner cards and the leader's Core balloon (module store, fetched once).
const byHost = new Map<string, Proposal[]>();
let rows: Proposal[] = [];
const listeners = new Set<() => void>();
let started = false;
function publish(next: Proposal[]) {
  rows = next;
  for (const l of listeners) l();
}
function start() {
  if (started) return;
  started = true;
  const reload = (host: string) =>
    void listProposals(host)
      .then((r) => {
        byHost.set(host, r);
        const all = [...byHost.values()].flat();
        publish(all);
        void notifyNew(all);
      })
      .catch(() => {});
  onHostUp(reload);
  onEvent("proposals-changed", (_, host) => reload(host));
  // A removed host's cards go with it.
  subscribeHosts(() => {
    const keep = new Set([LOCAL, ...remoteHosts().map((h) => h.id)]);
    const gone = [...byHost.keys()].filter((h) => !keep.has(h));
    for (const h of gone) byHost.delete(h);
    if (gone.length) publish([...byHost.values()].flat());
  });
}
const subscribe = (l: () => void) => {
  start();
  listeners.add(l);
  return () => void listeners.delete(l);
};
export const useProposals = () => useSyncExternalStore(subscribe, () => rows);
/** #252: the same list outside React (the office's big Core); starts the store like a subscriber. */
export function getProposals() {
  start();
  return rows;
}

/** #247: `hide` drops a leader's cards while its Core balloon carries them. */
export function MemberProposals({ hide }: { hide?: (leader: string) => boolean }) {
  const rows = useProposals().filter((p) => !hide?.(p.leader));
  if (!rows.length) return null;
  return (
    <aside className="proposals" aria-label="Patron onayları">
      {rows.map((p) => <Card key={p.key} p={p} />)}
    </aside>
  );
}

/** #243: also the Core balloon's content; `onError` lets Core show `!` (the card keeps its own line). */
export function Card({ p, onError }: { p: Proposal; onError?: (e: string) => void }) {
  const [name, setName] = useState(p.name ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (approve: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await decide(p, approve, name.trim() || null);
    } catch (e) {
      setError(String(e));
      setBusy(false);
      onError?.(String(e));
    }
  };
  return (
    <div className="proposal" role="alertdialog" aria-label={`${p.leaderName} ${p.delegationId ? "iş modeli onayı istiyor" : "üye öneriyor"}`}>
      <h4>
        {p.delegationId ? `İş #${p.delegationId} model onayı` : "Üye önerisi"} <span className="proposal-team">{p.teamName}</span>
      </h4>
      <p className="proposal-who">
        <b>{p.leaderName}</b> {p.delegationId ? <><b>{p.name}</b> ({p.roleName}) için model istiyor</> : <>takıma bir <b>{p.roleName}</b> istiyor</>}
        <span className="proposal-model">{p.delegationId || p.provider === "anthropic" ? p.model : `${p.provider}/${p.model}`}</span>
      </p>
      <p className="proposal-reason">{p.reason}</p>
      {!p.delegationId && <label>
        Ad
        <input value={name} placeholder="boş → otomatik" onChange={(e) => setName(e.target.value)} aria-label="Yeni üyenin adı" />
      </label>}
      {error && <p className="team-err">{error}</p>}
      <div className="proposal-actions">
        <button disabled={busy} data-key="r" aria-keyshortcuts="R" onClick={() => void answer(false)}>Reddet</button>
        <button className="primary" disabled={busy} data-key="e" aria-keyshortcuts="E" onClick={() => void answer(true)}>{p.delegationId ? "Onayla" : "Ekle"}</button>
      </div>
    </div>
  );
}
