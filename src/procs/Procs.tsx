// #1076: docked process panel; refresh only for the visible tab in the active team.
import { useEffect, useId, useRef, useState } from "react";
import { ChevronRight, Cpu, X } from "lucide-react";
import { hostName, hostSupports, rawId, useHosts } from "../api/hosts";
import { useTeams } from "../team/store";
import { killAgentProcess, listAgentProcesses } from "./api";
import { agentRows, cpuText, keepOrder, KIND_LABEL, killQuestion, memText, REFRESH_MS, totals, type AgentRow } from "./view";
import { askConfirm } from "../confirm";
import "./procs.css";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function Procs({ host, active, onClose }: { host: string; active: boolean; onClose: () => void }) {
  const panel = useRef<HTMLElement>(null);
  // Keyboard: the panel takes focus on open and gives it back on close — unless focus went elsewhere
  // already (another panel or the board took focus). `back` is read at first render: StrictMode's re-run of the
  // effect would otherwise read the panel's own close button.
  const [back] = useState(() => document.activeElement as HTMLElement | null);
  useEffect(() => {
    if (!active) return;
    const el = panel.current;
    el?.querySelector<HTMLElement>(".procs-close")?.focus();
    return () => {
      const now = document.activeElement;
      if (!now || now === document.body || !now.isConnected || el?.contains(now)) back?.focus?.();
    };
  }, [active, back]);
  return (
    <aside
      ref={panel}
      className="procs"
      aria-label="Süreçler"
    >
      <ProcsView key={host} host={host} active={active} onClose={onClose} />
    </aside>
  );
}

function ProcsView({ host, active, onClose }: { host: string; active: boolean; onClose: () => void }) {
  const instance = useId();
  const headId = (agent: string) => `procs-agent-${instance}-${agent}`;
  useHosts(); // re-render on the host's hello (supported methods)
  const { agents, teams } = useTeams();
  const supported = hostSupports(host, "agent_processes");
  // #922: sorted by load on the first answer only; refreshes keep that order (keepOrder).
  const [rows, setRows] = useState<AgentRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const [killing, setKilling] = useState<number | null>(null);
  const [killError, setKillError] = useState<string | null>(null);

  useEffect(() => {
    if (!active || !supported) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = async () => {
      try {
        const m = await listAgentProcesses(host);
        if (!alive) return;
        setRows((prev) => keepOrder(prev, agentRows(m)));
        setError(null);
      } catch (e) {
        if (alive) setError(message(e));
      }
      if (alive) timer = setTimeout(load, REFRESH_MS);
    };
    void load();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [host, active, supported, nonce]);

  const label = (raw: string) => {
    const a = agents.find((x) => x.host === host && rawId(x.id) === raw);
    return { name: a?.name ?? raw, team: a ? teams.find((t) => t.id === a.teamId)?.name ?? "" : "" };
  };
  const toggle = (agent: string) =>
    setExpanded((s) => {
      const next = new Set(s);
      if (!next.delete(agent)) next.add(agent);
      return next;
    });
  const kill = async (row: AgentRow, pid: number) => {
    const p = row.procs.find((x) => x.pid === pid);
    if (killing !== null || !p || !(await askConfirm(killQuestion(label(row.agent).name, p)))) return;
    setKilling(pid);
    setKillError(null);
    try {
      await killAgentProcess(host, row.agent, pid);
      // The row goes with the process: keep the keyboard in the panel, on its agent.
      document.getElementById(headId(row.agent))?.focus();
      setNonce((n) => n + 1);
    } catch (e) {
      setKillError(`Kapatılamadı: ${message(e)}`);
    } finally {
      setKilling(null);
    }
  };

  const sum = totals(rows ?? []);
  return (
    <>
      <header className="procs-head">
        <Cpu aria-hidden />
        <span className="procs-title">Süreçler</span>
        <span className="procs-host">{hostName(host)}</span>
        <button className="icon-btn procs-close" onClick={onClose} title="Paneli kapat" aria-label="Süreç panelini kapat">
          <X />
        </button>
      </header>
      {rows && rows.length > 0 && (
        <p className="procs-sum">
          Ajanlar toplam <b>{cpuText(sum.cpuPct)}</b> CPU · <b>{memText(sum.memMb)}</b> RAM
          {(sum.browsers > 0 || sum.browserTools > 0) && <>
            {" · "}<span className="procs-heavy">{sum.browsers} tarayıcı ({memText(sum.browserMemMb)})</span>
            {" · "}<span>{sum.browserTools} tarayıcı aracı ({memText(sum.browserToolMemMb)})</span>
          </>}
        </p>
      )}
      {killError && <p className="procs-err" role="alert">{killError}</p>}
      {rows && error && <p className="procs-err" role="status">Son yenileme başarısız: {error}</p>}
      <div className="procs-body">
        {!supported ? (
          <p className="procs-note">{hostName(host)} çekirdeği süreç listesini bilmiyor — Dex'i güncelleyin.</p>
        ) : !rows && error ? (
          <div className="procs-note" role="alert">
            <p>Süreçler alınamadı: {error}</p>
            <button onClick={() => setNonce((n) => n + 1)}>Tekrar dene</button>
          </div>
        ) : !rows ? (
          <div aria-busy="true" aria-label="Süreçler ölçülüyor">
            {[0, 1, 2].map((i) => <div key={i} className="procs-skel skel" />)}
          </div>
        ) : rows.length === 0 ? (
          <p className="procs-note">Bu sunucuda çalışan ajan oturumu yok.</p>
        ) : (
          <ul className="procs-agents">
            {rows.map((row) => {
              const { name, team } = label(row.agent);
              const open = expanded.has(row.agent);
              const listId = `procs-list-${instance}-${row.agent}`;
              return (
                <li key={row.agent} className="procs-agent">
                  <button id={headId(row.agent)} className="procs-agent-head" aria-expanded={open} aria-controls={listId} onClick={() => toggle(row.agent)}>
                    <ChevronRight className={`procs-chev${open ? " open" : ""}`} aria-hidden />
                    <span className="procs-agent-name">
                      {name}
                      {team && <span className="procs-team">{team}</span>}
                    </span>
                    {row.browsers > 0 && <span className="procs-badge kind-browser">{row.browsers} tarayıcı</span>}
                    {row.browserTools > 0 && <span className="procs-badge kind-browser-tool">{row.browserTools} tarayıcı aracı</span>}
                    <span className="procs-count">{row.procs.length} süreç</span>
                    <span className="procs-num">{cpuText(row.cpuPct)}</span>
                    <span className="procs-num">{memText(row.memMb)}</span>
                  </button>
                  {row.error && <p className="procs-err" role="alert">Süreçleri okunamadı: {row.error}</p>}
                  {open && (
                    <ul id={listId} className="procs-list" aria-label={`${name} süreçleri`}>
                      {row.procs.map((p) => (
                        <li key={p.pid} className="procs-proc">
                          <span className={`procs-badge kind-${p.kind}`}>{p.root ? "Ajan" : KIND_LABEL[p.kind]}</span>
                          <span className="procs-proc-main">
                            <span className="procs-proc-name">{p.name} <span className="procs-pid">pid {p.pid}</span></span>
                            <span className="procs-cmd" title={p.cmd}>{p.cmd}</span>
                          </span>
                          <span className="procs-num">{cpuText(p.cpuPct)}</span>
                          <span className="procs-num">{memText(p.memMb)}</span>
                          {p.root ? (
                            <span className="procs-root" title="Ajanın kendi CLI süreci buradan kapatılmaz">kök</span>
                          ) : (
                            <button className="procs-kill" aria-disabled={killing !== null} onClick={() => void kill(row, p.pid)}
                              aria-label={`${p.name} (pid ${p.pid}) sürecini sonlandır`}>
                              {killing === p.pid ? "…" : "Sonlandır"}
                            </button>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </>
  );
}
