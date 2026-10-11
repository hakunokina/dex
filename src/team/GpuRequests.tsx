// #290: patron approval cards for leaders' `gpu_request` calls. Only the patron opens the GPU to a
// worker, per type and budget; dexd turns the answer into a grant and tells the leader
// (crates/dex-core/src/gpu.rs). Same card look and corner stack as the member proposals.
import { useState, useSyncExternalStore } from "react";
import { callHost, LOCAL, onEvent, onHostUp, qid, remoteHosts, subscribeHosts } from "../api/hosts";
import { budgetMax, parseBudget, waitingToastText, type GpuLimits, type GpuRequest, type GpuWaiting } from "../gpu/view";
import { findAgent } from "../agents";
import { toast } from "../toast/store";
import { useTeams } from "./store";
import "./proposals.css";

type Row = GpuRequest & { host: string; key: string; maxBudget: number };

const byHost = new Map<string, Row[]>();
const maxByHost = new Map<string, number>();
const reloads = new Map<string, object>();
let rows: Row[] = [];
const listeners = new Set<() => void>();
let started = false;
const told = new Set<string>();
function publish() {
  rows = [...byHost.values()].flat();
  for (const l of listeners) l();
}
function start() {
  if (started) return;
  started = true;
  // gpu_request_list holds only pending rows; a decision drops the card on the next reload.
  const reload = (host: string) => {
    const version = {};
    reloads.set(host, version);
    void callHost<GpuRequest[]>(host, "gpu_request_list")
      .then(async (r) => {
        if (reloads.get(host) !== version) return;
        const maxBudget = maxByHost.get(host) ?? budgetMax();
        byHost.set(host, r.map((q) => ({ ...q, host, key: `${host}:${q.id}`, maxBudget })));
        publish();
        const limits = r.length ? await callHost<GpuLimits>(host, "gpu_limits_get").catch(() => null) : null;
        if (reloads.get(host) !== version || !limits) return;
        const nextMax = budgetMax(limits.daily_image_limit);
        maxByHost.set(host, nextMax);
        byHost.set(host, (byHost.get(host) ?? []).map((q) => ({ ...q, maxBudget: nextMax })));
        publish();
      })
      .catch(() => {}); // pre-#289 dexd: no GPU, no cards
  };
  onHostUp(reload);
  onEvent("gpu-requests-changed", (_, host) => reload(host));
  onEvent<GpuLimits>("gpu-limits-changed", ({ payload }, host) => {
    const maxBudget = budgetMax(payload.daily_image_limit);
    maxByHost.set(host, maxBudget);
    byHost.set(host, (byHost.get(host) ?? []).map((r) => ({ ...r, maxBudget })));
    publish();
  });
  // #311: a worker's job waits for the patron's LLM → one toast per job (each poll re-sends the event).
  onEvent<GpuWaiting>("gpu-waiting", ({ payload: w }, host) => {
    const key = `gpu-waiting:${host}:${w.jobId}`;
    if (told.has(key)) return;
    told.add(key);
    const agent = qid(host, w.agent);
    toast({
      key,
      tone: "info",
      ...waitingToastText(findAgent(agent)?.name ?? w.agentName, w.etaS),
      actions: [{ label: "Aç", run: (ctx) => ctx.focusAgent(agent) }],
    });
  });
  subscribeHosts(() => {
    const keep = new Set([LOCAL, ...remoteHosts().map((h) => h.id)]);
    const gone = [...new Set([...byHost.keys(), ...reloads.keys()])].filter((h) => !keep.has(h));
    for (const h of gone) { byHost.delete(h); maxByHost.delete(h); reloads.delete(h); }
    if (gone.length) publish();
  });
}
const subscribe = (l: () => void) => {
  start();
  listeners.add(l);
  return () => void listeners.delete(l);
};
const useGpuRequests = () => useSyncExternalStore(subscribe, () => rows);

export function GpuRequests() {
  const rows = useGpuRequests().filter((r) => r.status === "pending");
  if (!rows.length) return null;
  return (
    <aside className="proposals" aria-label="GPU izin istekleri">
      {rows.map((r) => <Card key={r.key} r={r} />)}
    </aside>
  );
}

function Card({ r }: { r: Row }) {
  const [budget, setBudget] = useState(String(r.budget));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // #305: which team asks (gpu_request_list carries only the raw team id).
  const teamName = useTeams().teams.find((t) => t.id === qid(r.host, r.teamId))?.name ?? r.teamId;
  const answer = async (approve: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await callHost(r.host, "gpu_request_decide", approve ? { id: r.id, approve, budget: parseBudget(budget, r.budget, r.maxBudget) } : { id: r.id, approve });
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  return (
    <div className="proposal" role="alertdialog" aria-label={`${r.leaderName} GPU izni istiyor`}>
      <h4>
        GPU izni #{r.id} <span className="proposal-team">{teamName}</span>
      </h4>
      <p className="proposal-who">
        <b>{r.leaderName}</b>, <b>{r.workerName}</b> için istiyor
        {r.types.map((t) => <span key={t} className="proposal-model">{t}</span>)}
      </p>
      <p className="proposal-reason">{r.reason}</p>
      <label>
        Bütçe (görsel, en çok {r.maxBudget})
        <input type="number" min={1} max={r.maxBudget} value={budget} onChange={(e) => setBudget(e.target.value)} aria-label="GPU bütçesi" />
      </label>
      {error && <p className="team-err" role="alert">{error}</p>}
      <div className="proposal-actions">
        <button disabled={busy} onClick={() => void answer(false)}>Reddet</button>
        <button className="primary" disabled={busy} onClick={() => void answer(true)}>Onayla</button>
      </div>
    </div>
  );
}
