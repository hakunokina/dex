// DEX-36: a team's screen is one dockview — leaders, workers, shells, the office and the task board
// are all panels the patron can split, tab, float and drag anywhere. Agent/shell panels
// only borrow slot elements (./host.tsx), so none of that ever remounts an xterm or touches a PTY.
//
// FIX-5: the registry + open commands live in ./panels.ts (no views), so the first-open chunk never
// carries dockview. The heavy panel views below load lazily, when their panel first renders.
import { createContext, lazy, Suspense, useContext, useEffect, useState, useSyncExternalStore, type SyntheticEvent } from "react";
import {
  DockviewReact,
  type DockviewApi, type DockviewReadyEvent, type DockviewTheme,
  type IDockviewHeaderActionsProps, type IDockviewPanelHeaderProps, type IDockviewPanelProps,
  type GetTabContextMenuItemsParams, type resolveMessages,
} from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import { Boxes, Brain, CircuitBoard, Cpu, Lock, NotebookPen, Play, Plus, Settings, Square, SquareKanban, SquareTerminal, StickyNote, Users, X } from "lucide-react";
import { hostOf } from "../api/hosts";
import { requestStart } from "../launcher";
import { killPty } from "../pty";
import { activeCount, markSeen, useDelegations } from "../delegations";
import { getTeam, useTeams } from "../team/store";
import { token } from "../theme";
import { STATUS_LABEL } from "../TerminalPane";
import { isShellRunning, restartShell, subscribeShellRunning } from "../ShellPane";
import { leaderPending, useQuestions } from "../questions";
import { AgentMarks, useAgentStatus } from "./Rack";
import { Heartbeat } from "./AgentHeartbeat";
import { SlotPanel } from "./host";
import { PanelBoundary } from "./PanelBoundary";
import { OfficeBar } from "../office/OfficeBar";
import { getSnapshot, resumableSession, setDock, shellPtyId, subscribe } from "./store";
import {
  agentPanelId, bindApi, boundApi, closeShell, growIntoCorner, isBulk, isNotes, load, NOTES, openPanel, openShell, reconcile,
  resetLayout, shellPanelId, syncAssistantPanel, unbindApi, type NotesPage,
} from "./panels";
import "./workspace.css";

const Office = lazy(() => import("../office/Office").then(m => ({ default: m.Office })));
const BoardView = lazy(() => import("../board/Board").then(m => ({ default: m.BoardView })));
const SettingsView = lazy(() => import("../settings/Settings").then(m => ({ default: m.SettingsView })));
const TeamPanel = lazy(() => import("../team/TeamBar").then(m => ({ default: m.TeamPanel })));
const NotesEditor = lazy(() => import("./Notes").then(m => ({ default: m.NotesEditor })));
const Procs = lazy(() => import("../procs/Procs").then(m => ({ default: m.Procs })));
const MemoryView = lazy(() => import("../memory/MemoryMap").then(m => ({ default: m.MemoryMap })));

/** A panel view still loading — the dock tab is already there, the body catches up. */
function PanelFallback() {
  return <div className="panel-fill" />;
}
function lazyPanel(el: React.ReactNode) {
  return <PanelBoundary><Suspense fallback={<PanelFallback />}>{el}</Suspense></PanelBoundary>;
}

// ---- Panels ----

interface Ctx {
  teamId: string;
  active: boolean;
  focusedId: string | null;
  onFocus: (id: string) => void;
  onOfficeSelect: (agentId: string) => void;
}
const WorkspaceCtx = createContext<Ctx>(null!);

function OfficePanel() {
  const { teamId, active, focusedId, onOfficeSelect, onFocus } = useContext(WorkspaceCtx);
  // The canvas animates only for the team on screen (DEX-16: its World keeps running elsewhere).
  return (
    <div className="panel-fill office-panel">
      <OfficeBar teamId={teamId} active={active} onFocus={onFocus} />
      <div className="office-stage">{active && lazyPanel(<Office key={teamId} teamId={teamId} focusedId={focusedId} onSelect={onOfficeSelect} />)}</div>
    </div>
  );
}

/** #42: the rack lives in the top bar; this only lets a saved layout with its old panel load (panels.load drops it). */
const LegacyRackPanel = () => null;

function AgentPanel({ api, params }: IDockviewPanelProps<{ agentId: string }>) {
  const { active } = useContext(WorkspaceCtx);
  const delegations = useDelegations();
  const [visible, setVisible] = useState(api.isVisible);
  useEffect(() => {
    const d = api.onDidVisibilityChange(e => setVisible(e.isVisible));
    return () => d.dispose();
  }, [api]);
  // DEX-19: a worker's unread result counts as seen once its panel is on screen.
  useEffect(() => {
    if (active && visible) markSeen(params.agentId);
  }, [active, visible, delegations, params.agentId]);
  return <SlotPanel slot={agentPanelId(params.agentId)} api={api} />;
}

function ShellPanel({ api, params }: IDockviewPanelProps<{ shellId: string }>) {
  return <SlotPanel slot={shellPanelId(params.shellId)} api={api} />;
}

function NotesPanel({ params }: IDockviewPanelProps<{ page: NotesPage }>) {
  const { teamId } = useContext(WorkspaceCtx);
  const team = params.page === "team";
  return lazyPanel(<NotesEditor scope={team ? teamId : "shared"}
    placeholder={team ? `${getTeam(teamId)?.name ?? "Bu takım"} notları — yalnız sen görürsün` : "Ortak notlar — tüm takımlarda aynı, yalnız sen görürsün"} />);
}

/** S4: limits and first-reply thresholds are the team's host's (a remote team → its server). */
function SettingsPanel() {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<SettingsView host={hostOf(teamId)} />);
}

function TeamSettingsPanel({ api }: IDockviewPanelProps) {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<TeamPanel teamId={teamId} onClose={() => api.close()} onReset={() => resetLayout(teamId)} />);
}

function BoardPanel() {
  return <div className="board-docked">{lazyPanel(<BoardView />)}</div>;
}

function MemoryPanel() {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<MemoryView teamId={teamId} />);
}

function ProcsPanel({ api }: IDockviewPanelProps) {
  const { teamId, active } = useContext(WorkspaceCtx);
  const [visible, setVisible] = useState(api.isVisible);
  useEffect(() => {
    setVisible(api.isVisible);
    const d = api.onDidVisibilityChange(e => setVisible(e.isVisible));
    return () => d.dispose();
  }, [api]);
  return lazyPanel(<Procs host={hostOf(teamId)} active={active && visible} onClose={() => api.close()} />);
}

const COMPONENTS = {
  office: OfficePanel,
  rack: LegacyRackPanel,
  board: BoardPanel,
  settings: SettingsPanel,
  team: TeamSettingsPanel,
  memory: MemoryPanel,
  procs: ProcsPanel,
  agent: AgentPanel,
  shell: ShellPanel,
  notes: NotesPanel,
};

/** Agent tab: status dot + name + the rack's marks, so a tabbed-away worker still shows its state. */
function AgentTab({ api, params }: IDockviewPanelHeaderProps<{ agentId: string }>) {
  const agentId = params?.agentId ?? api.id.slice("agent:".length);
  const status = useAgentStatus(agentId);
  const agent = useTeams().agents.find(a => a.id === agentId);
  const name = agent?.name ?? api.title;
  const session = useSyncExternalStore(subscribe, () => agent ? resumableSession(agent.id, agent.engine) : null);
  const delegations = useDelegations();
  const questions = useQuestions();
  const delegated = agent?.kind === "leader" ? activeCount(delegations, agent.id, "from") : 0;
  const pending = agent?.kind === "leader" ? leaderPending(questions, agent.id) : 0;
  const control = (e: SyntheticEvent) => e.stopPropagation();
  return (
    <div className="dex-tab" title={`${name} · ${agent?.kind === "leader" && status === "waiting" ? "terminalde izin/girdi bekliyor" : STATUS_LABEL[status]}`}>
      <Heartbeat id={agentId} status={status} />
      <span className="dex-tab-name">{name}</span>
      <AgentMarks id={agentId} />
      {delegated > 0 && <span className="pane-deleg" title="Aktif devretmeler">⇢ {delegated}</span>}
      {pending > 0 && <span className="pane-ask" title="Worker soruları cevap bekliyor">? {pending}</span>}
      {status === "offline" ? (
        <button className="dex-tab-action" title={session ? "Oturumu sürdür" : "Claude'u başlat"}
          onPointerDown={control} onClick={(e) => { control(e); requestStart(agentId); }}><Play /></button>
      ) : (
        <button className="dex-tab-action" title="Durdur" onPointerDown={control}
          onClick={(e) => { control(e); void killPty(agentId); }}><Square /></button>
      )}
      <button
        className="dex-tab-close"
        title="Paneli kapat (oturum sürer)"
        aria-label={`${name} sekmesini kapat`}
        onPointerDown={e => e.stopPropagation()}
        onClick={e => {
          e.stopPropagation();
          api.close();
        }}
      >
        <X />
      </button>
    </div>
  );
}

function ShellTab({ api, params }: IDockviewPanelHeaderProps<{ shellId: string }>) {
  const shellId = params?.shellId ?? api.id.slice("shell:".length);
  const ptyId = shellPtyId(shellId);
  const running = useSyncExternalStore(subscribeShellRunning, () => isShellRunning(ptyId));
  return (
    <div className="dex-tab" title="Terminal">
      <SquareTerminal className="dex-tab-icon" />
      <span className="dex-tab-name">Terminal</span>
      {!running && <button className="dex-tab-action" title="Kabuğu yeniden başlat" onPointerDown={e => e.stopPropagation()}
        onClick={e => { e.stopPropagation(); restartShell(ptyId); }}><Play /></button>}
      <TabClose api={api} title="Terminali kapat" />
    </div>
  );
}

const FIXED_ICONS = { office: CircuitBoard, board: SquareKanban, settings: Settings, team: Users, memory: Brain, procs: Cpu } as const;
function FixedTab({ api }: IDockviewPanelHeaderProps) {
  const Icon = FIXED_ICONS[api.id as keyof typeof FIXED_ICONS] ?? Boxes;
  // Favori varken Kart sabittir: × yerine kilit; sekme sürüklenmez (pointerdown dockview'a ulaşmaz, tıklama yine etkinleştirir).
  const hasFav = useSyncExternalStore(subscribe, () => getSnapshot().favorite !== null);
  const locked = api.id === "office" && hasFav;
  return (
    <div className="dex-tab" title={locked ? `${api.title} · favori düzende sabit` : api.title}
      onPointerDown={locked ? e => e.stopPropagation() : undefined}
      onClick={locked ? () => api.setActive() : undefined}>
      <Icon className="dex-tab-icon" />
      <span className="dex-tab-name">{api.title}</span>
      {locked
        ? <span className="dex-tab-lock" title="Favori düzende sabit — kapatılamaz, taşınamaz"><Lock /></span>
        : <TabClose api={api} title={`${api.title} panelini kapat`} />}
    </div>
  );
}

function TabClose({ api, title }: { api: IDockviewPanelHeaderProps["api"]; title: string }) {
  return <button className="dex-tab-close" title={title} aria-label={title} onPointerDown={e => e.stopPropagation()}
    onClick={e => { e.stopPropagation(); api.close(); }}><X /></button>;
}

/** #46: a free panel like the others; closed, the palette's "Notları aç" brings it back. */
function NotesTab({ api }: IDockviewPanelHeaderProps) {
  const Icon = api.id === NOTES.shared.id ? NotebookPen : StickyNote;
  return (
    <div className="dex-tab" title={`Notlar · ${api.title}`}>
      <Icon className="dex-tab-icon" />
      <span className="dex-tab-name">{api.title}</span>
      <TabClose api={api} title={`${api.title} sekmesini kapat`} />
    </div>
  );
}

const TABS = { agent: AgentTab, shell: ShellTab, fixed: FixedTab, notes: NotesTab };

function FallbackTab(props: IDockviewPanelHeaderProps) {
  if (props.api.id.startsWith("agent:")) return <AgentTab {...props as IDockviewPanelHeaderProps<{ agentId: string }>} />;
  if (props.api.id.startsWith("shell:")) return <ShellTab {...props as IDockviewPanelHeaderProps<{ shellId: string }>} />;
  if (isNotes(props.api.id)) return <NotesTab {...props} />;
  return <FixedTab {...props} />;
}

/** #46: a group holding a shell gets "Yeni terminal" on its header (#65: the Kart's toolbar always has one). */
function GroupActions({ panels }: IDockviewHeaderActionsProps) {
  const { teamId, onFocus } = useContext(WorkspaceCtx);
  if (!panels.some(p => p.id.startsWith("shell:"))) return null;
  return (
    <div className="dex-group-actions">
      <button className="dex-group-action" title="Yeni terminal" aria-label="Yeni terminal"
        onPointerDown={e => e.stopPropagation()}
        onClick={() => {
          void openShell(teamId).then(id => { if (id) onFocus(id); });
        }}>
        <Plus />
      </button>
    </div>
  );
}

// ---- FIX-1: dockview's own strings in Turkish (its built-in menu items are English-only) ----

function tabMenu({ panel, group, api: dock }: GetTabContextMenuItemsParams) {
  const { api } = panel;
  const max = api.isMaximized();
  // Favoride Kart sabittir: kapatılamaz, yüzdürülemez; "diğerleri" de onu kapatamaz.
  const pinned = panel.id === "office" && getSnapshot().favorite !== null;
  return [
    { label: "Yüzdür", action: () => dock.addFloatingGroup(panel), disabled: pinned || api.location.type === "floating" },
    { label: max ? "Geri yükle" : "Büyüt", action: () => max ? api.exitMaximized() : api.maximize(), disabled: !max && api.location.type !== "grid" },
    "separator" as const,
    { label: "Kapat", action: () => api.close(), disabled: !!pinned },
    { label: "Diğerlerini kapat", action: () => group.panels.filter(p => p !== panel && !(p.id === "office" && getSnapshot().favorite !== null)).forEach(p => p.api.close()), disabled: group.panels.length < 2 },
  ];
}

type Messages = Required<NonNullable<Parameters<typeof resolveMessages>[0]>>;
const EDGE: Record<string, string> = { top: "üstüne", bottom: "altına", left: "soluna", right: "sağına", center: "içine" };
const MESSAGES: Messages = {
  panelOpened: t => `${t} açıldı`,
  panelClosed: t => `${t} kapandı`,
  groupMaximized: t => `${t} büyütüldü`,
  groupRestored: t => `${t} geri yüklendi`,
  groupFloated: t => `${t} yüzdürüldü`,
  groupDocked: t => `${t} yerleştirildi`,
  groupPoppedOut: t => `${t} yeni pencerede açıldı`,
  closeTab: t => `${t} sekmesini kapat`,
  closeTabPlain: () => "Sekmeyi kapat",
  movePickTarget: (s, t, i, n) => `${s} taşınıyor. Hedef ${t}, ${i}/${n}. Yer seçmek için Enter, iptal için Escape.`,
  movePickEdge: (p, t) => `${t} ${EDGE[p] ?? p}. Değiştirmek için oklar, onay için Enter, geri için Escape.`,
  moveCommitted: (s, t, p) => `${s}, ${t} ${EDGE[p] ?? p} taşındı.`,
  moveCancelled: () => "Taşıma iptal edildi.",
  moveNotAllowed: () => "Bu taşımaya izin yok.",
  moveFloated: s => `${s} yüzdürüldü.`,
};

// ---- Workspace ----

interface Props {
  teamId: string;
  active: boolean;
  focusedId: string | null;
  onFocus: (id: string) => void;
  onOfficeSelect: (agentId: string) => void;
}

export function Workspace(props: Props) {
  const { teamId } = props;
  const [api, setApi] = useState<DockviewApi | null>(null);
  const [theme] = useState(dexTheme);
  const agents = useTeams().agents;
  const shells = useSyncExternalStore(subscribe, () => getSnapshot().teams[teamId]?.shells);

  const onReady = (e: DockviewReadyEvent) => {
    load(teamId, e.api);
    bindApi(teamId, e.api);
    setDock(teamId, e.api.toJSON());
    setApi(e.api);
  };


  useEffect(() => {
    if (!api) return;
    const subs = [
      api.onDidLayoutChange(() => setDock(teamId, api.toJSON())),
      growIntoCorner(teamId, api),
      // Closing a shell's tab (✕, middle click, "close others"…) ends the shell. A move between groups
      // may remove-then-add, so only a panel still gone a tick later counts.
      api.onDidRemovePanel(p => {
        if (!p.id.startsWith("shell:") || isBulk(teamId)) return;
        setTimeout(() => {
          if (boundApi(teamId) === api && !api.getPanel(p.id)) closeShell(teamId, p.id.slice("shell:".length));
        });
      }),
      // Favoride Kart kapatılamaz: başka bir yoldan (orta tık gibi) gittiyse geri gelir.
      api.onDidRemovePanel(p => {
        if (p.id !== "office" || isBulk(teamId) || getSnapshot().favorite === null) return;
        setTimeout(() => {
          if (boundApi(teamId) === api && !api.getPanel("office")) openPanel(teamId, "office");
        });
      }),
    ];
    return () => {
      subs.forEach(s => s.dispose());
      unbindApi(teamId, api);
    };
  }, [api, teamId]);

  // Members come and go (team panel), shells close: their panels follow.
  useEffect(() => {
    if (api) reconcile(teamId, api);
  }, [api, teamId, agents, shells]);

  useEffect(() => {
    if (!api || !agents.some(a => a.teamId === teamId && a.roleSlug === "assistant")) return;
    let frame = 0;
    const resize = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => syncAssistantPanel(teamId, api));
    };
    window.addEventListener("resize", resize);
    return () => { window.removeEventListener("resize", resize); cancelAnimationFrame(frame); };
  }, [api, teamId, agents]);

  return (
    <WorkspaceCtx.Provider value={props}>
      <DockviewReact
        className="workspace"
        theme={theme}
        components={COMPONENTS}
        tabComponents={TABS}
        defaultTabComponent={FallbackTab}
        rightHeaderActionsComponent={GroupActions}
        onReady={onReady}
        // HTML5 DnD is unreliable in WebView2 while Tauri's file-drop is on (see board/Board.tsx).
        dndStrategy="pointer"
        // Hidden tabs stay in the DOM; slot content survives either way, this keeps the office's canvas too.
        defaultRenderer="always"
        floatingGroupBounds="boundedWithinViewport"
        getTabContextMenuItems={tabMenu}
        messages={MESSAGES}
      />
    </WorkspaceCtx.Provider>
  );
}

/** Colours/radii/fonts come from src/App.css tokens (workspace.css); only the numeric gap is read here. */
const dexTheme = (): DockviewTheme => ({
  name: "dex",
  className: "dockview-theme-abyss-spaced dockview-theme-dex",
  colorScheme: "dark",
  gap: parseFloat(token("--dock-gap")) || 6,
  dndOverlayMounting: "absolute",
  dndPanelOverlay: "group",
});
