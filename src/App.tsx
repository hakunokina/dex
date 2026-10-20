import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { AGENTS, getStatus } from "./agents";
import { openAgent, openPanel, openSettings } from "./layout/panels";
import { PaneHost } from "./layout/host";
import { mountedTeams } from "./layout/mountedTeams";
import { listenPopouts, restorePopouts } from "./layout/popoutWindows";
import { Rack } from "./layout/Rack";
import { initTeams, useTeams } from "./team/store";
import { useTeamViewing } from "./team/useTeamViewing";
import { hostName, LOCAL, useHosts, useLocalStatus, type HostView } from "./api/hosts";
import { CoreDriftBanner, LocalCoreBanner, localDrift } from "./team/CoreBanner";
import { HostEmptyStates, TeamTabs } from "./team/TeamBar";
import { usePaletteOpen } from "./palette/store";
import { useBoardOpen } from "./board/store";
import { MemberProposals } from "./team/Proposals";
import { GpuRequests } from "./team/GpuRequests";
import { EscalatedQuestions } from "./team/Questions";
import { ModelNews } from "./team/ModelNews";
import { useCoveredLeaders } from "./office/companion/coreShown";
import { Copy, Minus, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { UsageStrip } from "./usage/UsageStrip";
import { Toasts } from "./toast/Toasts";
import { teamAccent } from "./team/accent";
import { initSounds } from "./sound";
import "./App.css";

// FIX-5: the dockview workspace, the board drawer and the palette load after first paint.
const Workspace = lazy(() => import("./layout/Workspace").then(m => ({ default: m.Workspace })));
const Board = lazy(() => import("./board/Board").then(m => ({ default: m.Board })));
const CommandPalette = lazy(() => import("./palette/CommandPalette").then(m => ({ default: m.CommandPalette })));

export default function App() {
  useTeamViewing();
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const { ready, teams, activeId } = useTeams();
  const [visited, setVisited] = useState<readonly string[]>([]);
  const mounted = mountedTeams(visited, teams.map(t => t.id), activeId);
  // Adjust this component's state before its children render, so a switch never unmounts a visited view.
  if (mounted !== visited) setVisited(mounted);
  const paletteOpen = usePaletteOpen();
  const boardOpen = useBoardOpen();
  const covered = useCoveredLeaders();
  const hosts = useHosts();
  const localStatus = useLocalStatus();
  const statusOf = (host: string): HostView["status"] => (host === LOCAL ? localStatus : hosts.find((h) => h.rec.id === host)?.status ?? "unauthorized");
  // #151: the local core's build vs this UI's, re-read on every (re)connect; hidden per commit.
  const drift = localStatus === "connected" ? localDrift() : null;
  const [hiddenDrift, setHiddenDrift] = useState<string | null>(null);
  const driftShown = drift && drift.commit !== hiddenDrift ? drift : null;

  // Office click: always focus; an awake agent's panel also opens / comes forward. A sleeper was just
  // slapped (Office starts it) and gets no panel.
  const selectInOffice = (teamId: string, agentId: string) => {
    setFocusedId(agentId);
    if (getStatus(agentId) !== "offline") openAgent(teamId, agentId);
  };

  useEffect(() => {
    const stopSounds = initSounds();
    // #2258: leader windows left open last time come back once the teams are known.
    const stopPopouts = listenPopouts();
    void initTeams().then(restorePopouts);
    return () => {
      stopSounds?.();
      stopPopouts();
    };
  }, []);

  // Lightweight hosts watch every team; workspaces are constructed on first visit and then retained.
  // Running hidden agents get pinned terminals too, so their output survives even an ended PTY.
  return (
    <div className="app">
      <TopBar focusedId={focusedId} onFocus={setFocusedId} />
      {/* #139: the usage strip floats over the stage (right or left edge, vertically centred); dockview keeps the full width.
          #2034: set to the top bar, it sits left of the window controls instead. */}
      <div className="app-body">
        <main className="stage">
          <HostEmptyStates />
          {ready &&
            teams.map((t) => {
              const active = t.id === activeId;
              // S4: host not connected → panels grey and inert; its agents keep running on the server.
              const status = statusOf(t.host);
              const down = status !== "connected";
              const warn = !down && t.host === LOCAL && driftShown;
              return (
                <div key={t.id} className={`team-stage ${active ? "" : "hidden"} ${down ? "host-down" : warn ? "host-warn" : ""}`} inert={!active} data-accent={teamAccent(t)}>
                  {down && t.host === LOCAL && <LocalCoreBanner status={status} />}
                  {warn && <CoreDriftBanner drift={warn.drift} commit={warn.commit} onDismiss={() => setHiddenDrift(warn.commit)} />}
                  {down && t.host !== LOCAL && (
                    <div className="host-banner" role="status">
                      {status === "blocked"
                        ? `${hostName(t.host)} adresi engellendi — şifresiz bağlantı yalnız yerel/Tailscale adreslerinde kullanılabilir. Sunucuyu wss:// ile yeniden ekleyin.`
                        : status === "unauthorized" || status === "no-token"
                        ? `${hostName(t.host)} sunucusu bu cihazı tanımıyor — Ayarlar → Bağlantılar'dan yeniden eşleyin.`
                        : `${hostName(t.host)} sunucusuna bağlantı yok — yeniden bağlanılıyor. Ajanlar sunucuda çalışmaya devam ediyor.`}
                      {(status === "unauthorized" || status === "no-token" || status === "blocked") && (
                        <button onClick={() => void openSettings(t.id, "connections")}>Sunucular…</button>
                      )}
                    </div>
                  )}
                  <PaneHost teamId={t.id} opened={mounted.includes(t.id)} focusedId={focusedId} onFocus={setFocusedId} />
                  {mounted.includes(t.id) && <Suspense fallback={null}>
                    <Workspace
                      teamId={t.id}
                      active={active}
                      focusedId={focusedId}
                      onFocus={setFocusedId}
                      onOfficeSelect={(id) => selectInOffice(t.id, id)}
                    />
                  </Suspense>}
                </div>
              );
            })}
          <UsageStrip slot="stage" />
        </main>
      </div>
      {boardOpen && (
        <Suspense fallback={null}>
          <Board />
        </Suspense>
      )}
      {paletteOpen && (
        <Suspense fallback={null}>
          <CommandPalette onFocusAgent={setFocusedId} />
        </Suspense>
      )}
      {/* DEX-24: patron cards share one bottom-right stack (each list positions itself when alone). */}
      <div className="patron-cards">
        {/* #247: a leader's open, on-screen Core strip carries its cards; the corner skips them. */}
        <EscalatedQuestions hide={(l) => covered.has(l)} />
        <MemberProposals hide={(l) => covered.has(l)} />
        <GpuRequests />
        <ModelNews />
        {/* Modern §6: toasts sit under the cards, newest at the bottom. */}
        <Toasts onFocusAgent={setFocusedId} />
      </div>
    </div>
  );
}

function TopBar({ focusedId, onFocus }: { focusedId: string | null; onFocus: (id: string) => void }) {
  const { activeId } = useTeams();
  const openTeamSettings = useCallback(() => void openPanel(activeId, "team"), [activeId]);
  return (
    <header className="topbar" data-tauri-drag-region>
      {/* #70/#73: team tabs | DEX logo, fixed at the window's centre | leader + worker chips, window controls. */}
      <TeamTabs onOpenTeamSettings={openTeamSettings} />
      <span className="brand" data-tauri-drag-region>
        <BrandMark />
      </span>
      <div className="topbar-right" data-tauri-drag-region>
        <div className="topbar-rack" data-tauri-drag-region>
          {activeId && <Rack key={activeId} teamId={activeId} focusedId={focusedId} onFocus={onFocus} />}
        </div>
        <UsageStrip slot="top" />
        <WindowControls />
      </div>
    </header>
  );
}

// #85: "DEX" drawn as stroke skeletons (1 unit = 1 px at --brand-fs 38px; cap height 28, the X's ends cut flat by the
// viewBox) with one red dot running along the strokes' centre lines D → E → X, like the Kart's trace packets.
const BRAND_DOT_PATH =
  "M17.5 14C17.5 6 14.5 2.25 9 2.25H2.25V25.75H9C14.5 25.75 17.5 22 17.5 14" + // D, from its right-most point
  "L33.6 14H48.5H33.6V2.25H50.6H33.6V25.75H50.6" + // hop, E: middle bar, top bar, bottom bar
  "L66.4 25.75L84.6 2.25L75.5 14L66.4 2.25L84.6 25.75"; // hop, X: one stroke, then the other through the crossing

// #87: the dot only runs while someone on the team on screen is working — the Kart counter's source and 500 ms poll.
function useTeamWorking() {
  const [working, setWorking] = useState(false);
  useEffect(() => {
    const tick = () => setWorking(AGENTS.some((a) => getStatus(a.id) === "working"));
    tick();
    const t = setInterval(tick, 500);
    return () => clearInterval(t);
  }, []);
  return working;
}

function BrandMark() {
  const working = useTeamWorking();
  const svg = useRef<SVGSVGElement>(null);
  // Idle: freeze the run where it is while it fades out and the crossing's dot fades in; work resumes from there.
  useEffect(() => {
    if (working) svg.current?.unpauseAnimations();
    else svg.current?.pauseAnimations();
  }, [working]);
  return (
    <svg ref={svg} className={`brand-mark${working ? " live" : ""}`} viewBox="0 0 90 28">
      <title>DEX</title>
      <path d="M9 2.25C14.5 2.25 17.5 6 17.5 14C17.5 22 14.5 25.75 9 25.75H2.25V2.25Z M50.6 2.25H33.6V25.75H50.6M33.6 14H48.5 M64.66 0L86.34 28M86.34 0L64.66 28" />
      <g className="brand-run">{/* the fade sits on the group: the circle's own opacity is SMIL's */}
        <circle className="brand-dot" r="1.6">
          <animateMotion dur="7s" repeatCount="indefinite" path={BRAND_DOT_PATH} />
          <animate attributeName="opacity" dur="7s" repeatCount="indefinite" values="0;1;1;0" keyTimes="0;0.03;0.97;1" />
        </circle>
      </g>
      <circle className="brand-dot still" cx="75.5" cy="14" r="1.6" />
    </svg>
  );
}

// The native title bar is off (tauri.conf `decorations: false`); the top bar is the drag region.
function WindowControls() {
  const win = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const sync = () => void win.isMaximized().then(setMaximized).catch(() => {});
    sync();
    const off = win.onResized(sync);
    return () => void off.then((f) => f());
  }, [win]);
  return (
    <div className="win-ctl">
      <button onClick={() => void win.minimize()} title="Simge durumuna küçült" aria-label="Küçült">
        <Minus />
      </button>
      <button onClick={() => void win.toggleMaximize()} title={maximized ? "Geri al" : "Ekranı kapla"} aria-label="Büyüt">
        {maximized ? <Copy /> : <Square />}
      </button>
      <button className="close" onClick={() => void win.close()} title="Kapat" aria-label="Kapat">
        <X />
      </button>
    </div>
  );
}
