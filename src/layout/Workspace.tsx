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
  type IDockviewHeaderActionsProps, type IDockviewPanelHeaderProps, type IDockviewPanelProps, type IWatermarkPanelProps,
  type GetTabContextMenuItemsParams, type resolveMessages,
} from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import { Boxes, Brain, CircuitBoard, Cpu, History, Lock, NotebookPen, PictureInPicture2, Play, Settings, Square, SquareKanban, SquareTerminal, Users, X } from "lucide-react";
import { hostOf } from "../api/hosts";
import { requestStart } from "../launcher";
import { killPty } from "../pty";
import { activeCount, markSeen, useDelegations } from "../delegations";
import { useProgress, workerProgress } from "../progress";
import { ProgressChip } from "../ProgressView";
import { useTeams } from "../team/store";
import { token } from "../theme";
import { STATUS_LABEL } from "../TerminalPane";
import { isShellRunning, restartShell, subscribeShellRunning } from "../ShellPane";
import { leaderPending, useQuestions } from "../questions";
import { AgentMarks, useAgentStatus } from "./Rack";
import { Heartbeat } from "./AgentHeartbeat";
import { SlotPanel } from "./host";
import { PanelBoundary } from "./PanelBoundary";
import { useLeaderPrompt, useNoteBody } from "./noteMarks";
import { noteFilled } from "./notePrompts";
import { OfficeBar } from "../office/OfficeBar";
import { getSnapshot, isPoppedOut, resumableSession, setDock, shellGroupOf, shellPtyId, subscribe } from "./store";
import { detachLeader, focusPopout, reattachLeader } from "./popoutWindows";
import {
  agentPanelId, bindApi, boundApi, closePanel, closeShell, ensureShellGroup, FIXED, growIntoCorner, isBulk, isCatchup, isNotes, load, openPanel,
  openShell, reconcile, resetLayout, shellPanelId, unbindApi,
} from "./panels";
import { isShellPanel } from "./shellGroup";
import { EmptyState } from "../EmptyState";
import { ensureCatchup, useCatchup } from "../catchup/store";
import { waitingCount } from "../catchup/model";
import "../catchup/catchup.css";
import "./workspace.css";

const Office = lazy(() => import("../office/Office").then(m => ({ default: m.Office })));
const BoardView = lazy(() => import("../board/Board").then(m => ({ default: m.BoardView })));
const SettingsView = lazy(() => import("../settings/Settings").then(m => ({ default: m.SettingsView })));
const TeamPanel = lazy(() => import("../team/TeamBar").then(m => ({ default: m.TeamPanel })));
const NotesPage = lazy(() => import("./NotesPage").then(m => ({ default: m.NotesPage })));
const CatchupPage = lazy(() => import("../catchup/CatchupPage").then(m => ({ default: m.CatchupPage })));
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
  const { teamId, active, focusedId, onOfficeSelect } = useContext(WorkspaceCtx);
  // The canvas animates only for the team on screen (DEX-16: its World keeps running elsewhere).
  return (
    <div className="panel-fill office-panel">
      <OfficeBar teamId={teamId} active={active} />
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
  // #2258: a leader in its own window — the slot is parked, this panel holds its place and the way back.
  const out = useSyncExternalStore(subscribe, () => isPoppedOut(params.agentId));
  if (out) return <PoppedOutPanel agentId={params.agentId} />;
  return <SlotPanel slot={agentPanelId(params.agentId)} api={api} />;
}

function PoppedOutPanel({ agentId }: { agentId: string }) {
  return (
    <div className="panel-fill pane-popped" role="status">
      <PictureInPicture2 className="pane-popped-icon" aria-hidden />
      <span>Ayrı pencerede açık</span>
      <div className="pane-popped-actions">
        <button onClick={() => void focusPopout(agentId)} title="Ayrı pencereyi öne getir">Pencereye git</button>
        <button onClick={() => void reattachLeader(agentId)} title="Pencereyi kapat, paneli buraya geri al">Geri al</button>
      </div>
    </div>
  );
}

function ShellPanel({ api, params }: IDockviewPanelProps<{ shellId: string }>) {
  return <SlotPanel slot={shellPanelId(params.shellId)} api={api} />;
}

function NotesPanel() {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<NotesPage teamId={teamId} />);
}

/** #2235: `visible` = this tab on screen in the team on screen — the auto-ack's gate. */
function CatchupPanel({ api }: IDockviewPanelProps) {
  const { teamId, active } = useContext(WorkspaceCtx);
  const [visible, setVisible] = useState(api.isVisible);
  useEffect(() => {
    setVisible(api.isVisible);
    const d = api.onDidVisibilityChange(e => setVisible(e.isVisible));
    return () => d.dispose();
  }, [api]);
  return lazyPanel(<CatchupPage teamId={teamId} visible={active && visible} />);
}

/** S4: limits and first-reply thresholds are the team's host's (a remote team → its server). */
function SettingsPanel() {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<SettingsView host={hostOf(teamId)} teamId={teamId} />);
}

function TeamSettingsPanel({ api, containerApi }: IDockviewPanelProps) {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<TeamPanel teamId={teamId} onClose={() => closeTab(containerApi, api.id)} onReset={() => resetLayout(teamId)} />);
}

function BoardPanel() {
  return <div className="board-docked">{lazyPanel(<BoardView />)}</div>;
}

function MemoryPanel() {
  const { teamId } = useContext(WorkspaceCtx);
  return lazyPanel(<MemoryView teamId={teamId} />);
}

function ProcsPanel({ api, containerApi }: IDockviewPanelProps) {
  const { teamId, active } = useContext(WorkspaceCtx);
  const [visible, setVisible] = useState(api.isVisible);
  useEffect(() => {
    setVisible(api.isVisible);
    const d = api.onDidVisibilityChange(e => setVisible(e.isVisible));
    return () => d.dispose();
  }, [api]);
  return lazyPanel(<Procs host={hostOf(teamId)} active={active && visible} onClose={() => closeTab(containerApi, api.id)} />);
}

/** #2266: the fixed terminal group with every tab closed — its pane stays; the other groups never sit empty. */
function ShellWatermark({ group }: IWatermarkPanelProps) {
  const { teamId, onFocus } = useContext(WorkspaceCtx);
  const fixed = useSyncExternalStore(subscribe, () => shellGroupOf(teamId));
  if (!group || group.id !== fixed) return null;
  return (
    <div className="shell-empty">
      <EmptyState mood="team" title="Terminal yok"
        actions={<button className="primary" onClick={() => { void openShell(teamId).then(id => { if (id) onFocus(id); }); }}>Terminal aç</button>}>
        Bu alan terminallerin yeri; hepsi kapansa da burada bekler.
      </EmptyState>
    </div>
  );
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
  catchup: CatchupPanel,
};

/** Agent tab: status dot + name + the rack's marks, so a tabbed-away worker still shows its state. */
function AgentTab({ api, containerApi, params }: IDockviewPanelHeaderProps<{ agentId: string }>) {
  const { teamId } = useContext(WorkspaceCtx);
  const agentId = params?.agentId ?? api.id.slice("agent:".length);
  const status = useAgentStatus(agentId);
  const out = useSyncExternalStore(subscribe, () => isPoppedOut(agentId));
  const agent = useTeams().agents.find(a => a.id === agentId);
  const name = agent?.name ?? api.title;
  const session = useSyncExternalStore(subscribe, () => agent ? resumableSession(agent.id, agent.engine) : null);
  const delegations = useDelegations();
  const questions = useQuestions();
  const delegated = agent?.kind === "leader" ? activeCount(delegations, agent.id, "from") : 0;
  const pending = agent?.kind === "leader" ? leaderPending(questions, agent.id) : 0;
  useProgress();
  const progress = agent?.kind === "worker" ? workerProgress(agentId, delegations) : null;
  const control = (e: SyntheticEvent) => e.stopPropagation();
  return (
    <div className="dex-tab" title={`${name} · ${agent?.kind === "leader" && status === "waiting" ? "terminalde izin/girdi bekliyor" : STATUS_LABEL[status]}`}>
      <Heartbeat id={agentId} status={status} />
      <span className="dex-tab-name">{name}</span>
      <AgentMarks id={agentId} />
      {progress && <ProgressChip p={progress} />}
      {delegated > 0 && <span className="pane-deleg" title="Aktif devretmeler">⇢ {delegated}</span>}
      {pending > 0 && <span className="pane-ask" title="Worker soruları cevap bekliyor">? {pending}</span>}
      {/* #2258: the leader's pane in its own window (PiP-like); the assistant stays a tab. */}
      {agent?.kind === "leader" && agent.roleSlug !== "assistant" && (
        <button className={`dex-tab-action${out ? " on" : ""}`} aria-pressed={out}
          title={out ? "Ayrı pencereye git" : "Pencereye ayır"} aria-label={`${name}: ${out ? "ayrı pencereye git" : "pencereye ayır"}`}
          onPointerDown={control} onClick={(e) => { control(e); void (out ? focusPopout(agentId) : detachLeader(teamId, agentId)); }}>
          <PictureInPicture2 />
        </button>
      )}
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
          closeTab(containerApi, api.id);
        }}
      >
        <X />
      </button>
    </div>
  );
}

function ShellTab({ api, containerApi, params }: IDockviewPanelHeaderProps<{ shellId: string }>) {
  const shellId = params?.shellId ?? api.id.slice("shell:".length);
  const ptyId = shellPtyId(shellId);
  const running = useSyncExternalStore(subscribeShellRunning, () => isShellRunning(ptyId));
  return (
    <div className="dex-tab" title="Terminal">
      <SquareTerminal className="dex-tab-icon" />
      <span className="dex-tab-name">Terminal</span>
      {!running && <button className="dex-tab-action" title="Kabuğu yeniden başlat" onPointerDown={e => e.stopPropagation()}
        onClick={e => { e.stopPropagation(); restartShell(ptyId); }}><Play /></button>}
      <TabClose api={api} containerApi={containerApi} title="Terminali kapat" />
    </div>
  );
}

const FIXED_ICONS = { office: CircuitBoard, board: SquareKanban, settings: Settings, team: Users, memory: Brain, procs: Cpu } as const;
function FixedTab({ api, containerApi }: IDockviewPanelHeaderProps) {
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
        : <TabClose api={api} containerApi={containerApi} title={`${api.title} panelini kapat`} />}
    </div>
  );
}

type TabApis = Pick<IDockviewPanelHeaderProps, "api" | "containerApi">;
function TabClose({ api, containerApi, title }: TabApis & { title: string }) {
  return <button className="dex-tab-close" title={title} aria-label={title} onPointerDown={e => e.stopPropagation()}
    onClick={e => { e.stopPropagation(); closeTab(containerApi, api.id); }}><X /></button>;
}
/** Every close in Dex's own UI goes through `closePanel`: the fixed terminal group keeps its pane. */
function closeTab(containerApi: DockviewApi, panelId: string) {
  const p = containerApi.getPanel(panelId);
  if (p) closePanel(containerApi, p);
}

/** #46: a free panel like the others; closed, the palette's "Notları aç" brings it back.
 *  #2040/#2086: the name is red while either note or one of the team's appends holds something. */
function NotesTab({ api, containerApi }: IDockviewPanelHeaderProps) {
  const { teamId } = useContext(WorkspaceCtx);
  const shared = useNoteBody("shared");
  const body = useNoteBody(teamId);
  const lp = useLeaderPrompt(teamId);
  const filled = noteFilled(shared) || noteFilled(body, lp);
  return (
    <div className="dex-tab" title={`${api.title}${filled ? " · dolu" : " · boş"}`}>
      <NotebookPen className="dex-tab-icon" />
      <span className={`dex-tab-name notes-tab-name${filled ? " filled" : ""}`}>{api.title}</span>
      <TabClose api={api} containerApi={containerApi} title={`${api.title} sekmesini kapat`} />
    </div>
  );
}

/** #2235: the badge is the waiting count alone; the snapshot is fetched once for the team on
 *  screen (the store refreshes it on host events — no polling, each call scans the memory graph). */
function CatchupTab({ api, containerApi }: IDockviewPanelHeaderProps) {
  const { teamId, active } = useContext(WorkspaceCtx);
  const entry = useCatchup(teamId);
  useEffect(() => { if (active) ensureCatchup(teamId); }, [active, teamId]);
  const waiting = entry?.data ? waitingCount(entry.data) : 0;
  return (
    <div className="dex-tab" title={waiting ? `${api.title} · ${waiting} seni bekliyor` : api.title}>
      <History className="dex-tab-icon" />
      <span className="dex-tab-name">{api.title}</span>
      {waiting > 0 && <span className="catchup-badge" role="status" aria-label={`${waiting} seni bekliyor`}>{waiting}</span>}
      <TabClose api={api} containerApi={containerApi} title={`${api.title} sekmesini kapat`} />
    </div>
  );
}

const TABS = { agent: AgentTab, shell: ShellTab, fixed: FixedTab, notes: NotesTab, catchup: CatchupTab };

function FallbackTab(props: IDockviewPanelHeaderProps) {
  if (props.api.id.startsWith("agent:")) return <AgentTab {...props as IDockviewPanelHeaderProps<{ agentId: string }>} />;
  if (props.api.id.startsWith("shell:")) return <ShellTab {...props as IDockviewPanelHeaderProps<{ shellId: string }>} />;
  if (isNotes(props.api.id)) return <NotesTab {...props} />;
  if (isCatchup(props.api.id)) return <CatchupTab {...props} />;
  return <FixedTab {...props} />;
}

/** #46: a group holding a shell (or Süreçler) gets "Yeni terminal" and the Süreçler toggle on its header.
 *  #2266: the fixed terminal group keeps them whatever its tabs are, and while it is empty. */
function GroupActions({ panels, containerApi, group }: IDockviewHeaderActionsProps) {
  const { teamId, onFocus } = useContext(WorkspaceCtx);
  const fixed = useSyncExternalStore(subscribe, () => shellGroupOf(teamId));
  const [procsOpen, setProcsOpen] = useState(() => !!containerApi.getPanel(FIXED.procs.id));
  useEffect(() => {
    const sync = () => setProcsOpen(!!containerApi.getPanel(FIXED.procs.id));
    sync();
    const subs = [containerApi.onDidAddPanel(sync), containerApi.onDidRemovePanel(sync)];
    return () => subs.forEach(d => d.dispose());
  }, [containerApi]);
  if (group.id !== fixed && !panels.some(p => isShellPanel(p.id))) return null;
  return (
    <div className="dex-group-actions">
      <button className="dex-group-action" title="Yeni terminal" aria-label="Yeni terminal"
        onPointerDown={e => e.stopPropagation()}
        onClick={() => {
          void openShell(teamId).then(id => { if (id) onFocus(id); });
        }}>
        <SquareTerminal />
      </button>
      {/* #916: which agent runs what, CPU/RAM; idle Playwright/browser children can be closed there. */}
      <button className={`dex-group-action${procsOpen ? " on" : ""}`} aria-pressed={procsOpen}
        title="Süreçler (ajanların CPU/RAM'i)" aria-label="Süreçler"
        onPointerDown={e => e.stopPropagation()}
        onClick={() => {
          const p = containerApi.getPanel(FIXED.procs.id);
          if (p) closePanel(containerApi, p); else void openPanel(teamId, "procs");
        }}>
        <Cpu />
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
    { label: "Kapat", action: () => closePanel(dock, panel), disabled: !!pinned },
    { label: "Diğerlerini kapat", action: () => group.panels.filter(p => p !== panel && !(p.id === "office" && getSnapshot().favorite !== null)).forEach(p => closePanel(dock, p)), disabled: group.panels.length < 2 },
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
      // #2266: dockview drops a group its last tab was dragged out of; the fixed terminal group comes back.
      api.onDidLayoutChange(() => { if (boundApi(teamId) === api) ensureShellGroup(teamId, api); }),
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

  return (
    <WorkspaceCtx.Provider value={props}>
      <DockviewReact
        className="workspace"
        theme={theme}
        components={COMPONENTS}
        tabComponents={TABS}
        defaultTabComponent={FallbackTab}
        rightHeaderActionsComponent={GroupActions}
        watermarkComponent={ShellWatermark}
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
