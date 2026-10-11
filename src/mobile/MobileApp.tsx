// S5-A: the phone shell. A client only — every team lives on a `dexd` host over WebSocket; there is
// no local core and no docks. First open with no host → the pairing screen; then: team picker on
// top, one of four views (Kart / Ajanlar / Pano / Notlar), an agent screen over everything, and the
// extra-keys bar under the terminal. Z Flip8: the inner screen is a tall portrait, the cover screen
// a narrow one (`mobile.css` tightens the same layout under ~360px); folding/unfolding is a resize,
// which the PCB canvas and the terminal both follow (ResizeObserver / visualViewport).
import { useEffect, useState } from "react";
import { LOCAL, useHosts } from "../api/hosts";
import { initTeams, selectTeam, useTeams } from "../team/store";
import { Office } from "../office/Office";
import { START_EVENT, type StartRequest } from "../launcher";
import { AddServer } from "./AddServer";
import { AgentsView } from "./AgentsView";
import { AgentScreen } from "./AgentScreen";
import { BoardView } from "../board/Board";
import { NotesEditor } from "../layout/Notes";
import { ChevronLeft, CircuitBoard, KanbanSquare, NotebookPen, Server, Users } from "lucide-react";
import "../App.css";
import "./mobile.css";

type Tab = "card" | "agents" | "board" | "notes";
const TABS: { id: Tab; label: string; icon: typeof CircuitBoard }[] = [
  { id: "card", label: "Kart", icon: CircuitBoard },
  { id: "agents", label: "Ajanlar", icon: Users },
  { id: "board", label: "Pano", icon: KanbanSquare },
  { id: "notes", label: "Notlar", icon: NotebookPen },
];

export default function MobileApp() {
  const { ready, teams, activeId } = useTeams();
  const hosts = useHosts();
  const [tab, setTab] = useState<Tab>("card");
  const [serverSheet, setServerSheet] = useState(false);
  /** The agent screen shown over everything (its id), if any. */
  const [agent, setAgent] = useState<string | null>(null);
  // Office slap → nobody may have a pane mounted on the phone yet: open the screen and start there.
  const [pendingStart, setPendingStart] = useState<string | null>(null);
  useEffect(() => {
    const onStart = (e: Event) => {
      const req = (e as CustomEvent<StartRequest>).detail;
      req.handled = true;
      setAgent(req.agentId);
      setPendingStart(req.agentId);
    };
    window.addEventListener(START_EVENT, onStart);
    return () => window.removeEventListener(START_EVENT, onStart);
  }, []);

  useEffect(() => {
    void initTeams();
  }, []);

  const remoteTeams = teams.filter((t) => t.host !== LOCAL);
  const team = remoteTeams.find((t) => t.id === activeId) ?? remoteTeams[0] ?? null;
  useEffect(() => {
    if (team && team.id !== activeId) selectTeam(team.id);
  }, [team, activeId]);

  // No paired server yet → the first-open pairing screen (also reached from the top bar).
  if (!serverSheet && hosts.length === 0) {
    return <AddServer onDone={() => {}} first />;
  }

  return (
    <div className="m-app">
      <header className="m-top">
        <span className="m-brand"><img src="/dex-icon.svg" alt="" />DEX</span>
        <TeamPicker
          teams={remoteTeams}
          activeId={team?.id ?? ""}
          onPick={(id) => {
            selectTeam(id);
            setAgent(null);
          }}
        />
        <button
          className="m-host-btn"
          onClick={() => setServerSheet(true)}
          aria-label="Sunucular"
        >
          <Server />
          <span className={`m-host-dot ${hosts.every((h) => h.status === "connected") ? "connected" : hosts[0]?.status ?? "unauthorized"}`} />
          <span className="m-host-label">Sunucular</span>
        </button>
      </header>

      <main className="m-body">
        {serverSheet ? (
          <>
            <button className="m-back" onClick={() => setServerSheet(false)}>
              <ChevronLeft /> Geri
            </button>
            <AddServer onDone={() => setServerSheet(false)} />
          </>
        ) : !ready ? (
          <p className="m-hint">Yükleniyor…</p>
        ) : !team ? (
          <p className="m-hint">
            Bu sunucuda takım yok. Takımları masaüstü Dex'ten ya da sunucudan oluşturun; telefon
            yalnız izler ve yönetir.
          </p>
        ) : tab === "card" ? (
          <div className="m-card" inert={agent !== null}>
            <Office
              teamId={team.id}
              focusedId={agent}
              active={agent === null}
              onSelect={(id) => {
                // Chip tap: awake → its screen; asleep → the slap starts it (pendingStart) and opens it.
                setAgent(id);
              }}
            />
          </div>
        ) : tab === "agents" ? (
          <AgentsView teamId={team.id} onOpen={setAgent} />
        ) : tab === "board" ? (
          <div className="m-board">
            <BoardView />
          </div>
        ) : (
          <NotesEditor scope={team.id} placeholder="Takım notları…" />
        )}
      </main>

      <nav className="m-tabs" role="tablist">
        {TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            role="tab"
            aria-label={label}
            aria-selected={tab === id}
            className={`m-tab${tab === id ? " on" : ""}`}
            onClick={() => {
              setTab(id);
              setServerSheet(false);
            }}
          >
            <Icon />
            <span>{label}</span>
          </button>
        ))}
      </nav>

      {team && agent && (
        <AgentScreen
          teamId={team.id}
          agentId={agent}
          autoStart={agent === pendingStart}
          onClose={() => setAgent(null)}
          onStarted={() => setPendingStart(null)}
        />
      )}
    </div>
  );
}

function TeamPicker({ teams, activeId, onPick }: { teams: { id: string; name: string }[]; activeId: string; onPick: (id: string) => void }) {
  const [open, setOpen] = useState(false);
  const active = teams.find((t) => t.id === activeId);
  if (teams.length === 0) return <span className="m-team-name">—</span>;
  return (
    <div className="m-teams">
      <button className="m-team-btn" onClick={() => setOpen(!open)} aria-expanded={open}>
        {active?.name ?? "Takım seç"} <ChevronLeft className={`m-chev${open ? " up" : ""}`} size={14} />
      </button>
      {open && (
        <ul className="m-team-list">
          {teams.map((t) => (
            <li key={t.id}>
              <button
                className={t.id === activeId ? "on" : ""}
                onClick={() => {
                  onPick(t.id);
                  setOpen(false);
                }}
              >
                {t.name}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
