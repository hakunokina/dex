// FIX-5: dockview registry + panel commands without the React views. This module is light
// (type-only dockview imports, stores, pty) so the topbar, palette and board can call
// `openAgent`/`openPanel`/… without pulling `dockview-react` into the first-open chunk —
// the `Workspace` component itself loads lazily.
import type {
  AddPanelOptions, DockviewApi, IDockviewGroupPanel, IDockviewPanel,
} from "dockview-react";
import { kept } from "../hot";
import { requestFocus } from "../launcher";
import { killPty } from "../pty";
import { addShell, assistantOffered, catchupOffered, getFavorite, getSnapshot, removeShell, setAssistantOffered, setCatchupOffered, setDock, setFavorite, setShellGroup, shellGroupOf, shellPtyId } from "./store";
import { getActiveTeamId, membersOf } from "../team/store";
import { isAssignable } from "../team/rolePolicy";
import { newShellCwd, resolveShellCwd } from "../project/store";
import { projectErrorText } from "../project/model";
import { toast } from "../toast/store";
import { remapDock, type FavoriteTarget } from "./favorite";
import { migrateNotesDock, NOTES_ID, NOTES_TITLE } from "./notesDock";
import { focusSetting, selectSettingsTab, type SettingsTab } from "../settings/tab";
import { movePlaysFx, workerGroups, type WorkerRows } from "./workerGroups";
import { playCircuit } from "../office/circuitFx";
import { ReadyActions } from "./readyActions";
import { opensAssistant } from "./assistantPlacement";
import { removesEmptyGroup, resolveShellGroup } from "./shellGroup";

const hot = import.meta.hot;
const apis = kept(hot, "apis", () => new Map<string, DockviewApi>());
const pendingOpens = kept(hot, "pendingOpens", () => new ReadyActions());
/** Teams in a bulk rebuild (load / reset / reconcile): their panel removals aren't the patron closing a shell. */
const bulk = new Set<string>();
/** Bumped by every bulk rebuild, so a layout listener can tell the patron's edits from Dex's. */
const rebuilds = new Map<string, number>();
const workerRows = new WeakMap<DockviewApi, WorkerRows>();

function rememberWorkerRows(teamId: string, api: DockviewApi, pair: WorkerRows) {
  workerRows.set(api, pair);
  // Panel params survive Dockview JSON; bind the role to its group so a patron drag doesn't carry it elsewhere.
  for (const pane of api.panels) {
    if (kindOf(teamId, pane.id) !== "worker") continue;
    const workerRow = pane.group.id === pair.done ? "done" : pair.working.has(pane.group.id) ? "working" : undefined;
    if (pane.params?.workerRow !== workerRow || pane.params?.workerRowGroup !== (workerRow ? pane.group.id : undefined)) {
      pane.api.updateParameters({ workerRow, workerRowGroup: workerRow ? pane.group.id : undefined });
    }
  }
}

function workerRowsOf(teamId: string, api: DockviewApi) {
  const saved = workerRows.get(api);
  // Shell closes/reconcile bump rebuilds too; only a replaced layout invalidates surviving group roles.
  if (saved) {
    for (const id of saved.working) if (!api.getGroup(id)) saved.working.delete(id);
    if (saved.working.size || saved.done && api.getGroup(saved.done)) return saved;
  }
  const office = api.getPanel("office");
  const groups = api.groups.filter(g => g.api.location.type === "grid" && g.panels.some(p => kindOf(teamId, p.id) === "worker"));
  const done = groups.find(g => g.panels.some(p => p.params?.workerRow === "done" && p.params?.workerRowGroup === g.id))?.id;
  const working = new Set(groups.filter(g => g.panels.some(p => p.params?.workerRow === "working" && p.params?.workerRowGroup === g.id)).map(g => g.id));
  const pair = done || working.size ? { done, working } : (office ? workerGroups(office.group.element.getBoundingClientRect(), groups.map(g => {
    const box = g.element.getBoundingClientRect();
    return { id: g.id, left: box.left, top: box.top, width: box.width, height: box.height };
  })) : { working: new Set<string>() });
  rememberWorkerRows(teamId, api, pair);
  return pair;
}

/** Only already open workers in the managed groups follow their job; other placements belong to the patron. */
export function moveWorkerPanel(agentId: string, finished: boolean) {
  for (const [teamId, api] of apis) {
    const pane = api.getPanel(agentPanelId(agentId));
    if (!pane || kindOf(teamId, pane.id) !== "worker") continue;
    const pair = workerRowsOf(teamId, api);
    const managed = finished ? pair.working.has(pane.group.id) : pane.group.id === pair.done;
    if (!managed || pane.group.api.location.type !== "grid") continue;
    const working = workingGroupsOf(api, pair);
    const targetId = finished ? pair.done : working[working.length - 1]?.id;
    let target = targetId ? api.panels.find(p => p.group.id === targetId)?.group : undefined;
    const from = pane.group.element.getBoundingClientRect();
    quietly(teamId, () => {
      const referenceGroup = finished
        ? working.sort((a, b) => a.element.getBoundingClientRect().left - b.element.getBoundingClientRect().left)[0] ?? pane.group
        : pane.group;
      target ??= api.addGroup({ referenceGroup, direction: finished ? "left" : "right" });
      pane.api.moveTo({ group: target, position: "center", skipSetActive: true });
      if (finished) pair.done = pane.group.id;
      else pair.working.add(pane.group.id);
      rememberWorkerRows(teamId, api, pair);
    });
    const plays = () => movePlaysFx(teamId, getActiveTeamId(), window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    if (!from.width || !from.height || !plays()) continue;
    requestAnimationFrame(() => {
      if (api.getPanel(pane.id) !== pane || pane.group !== target || !plays()) return;
      const to = (pane.api.isVisible ? pane.group.element : pane.view.tab.element).getBoundingClientRect();
      if (to.width && to.height) playCircuit(from, to, false);
    });
  }
}

/** Surviving grid groups in Dex's worker-opening order. */
function workingGroupsOf(api: DockviewApi, pair: WorkerRows) {
  return [...pair.working].flatMap(id => {
    const group = api.groups.find(g => g.id === id);
    return group?.api.location.type === "grid" ? [group] : [];
  });
}

/** `Workspace` registers its dockview api here; callers outside the dock use the `open*` helpers. */
export function bindApi(teamId: string, api: DockviewApi) {
  apis.set(teamId, api);
  pendingOpens.flush(teamId);
}
export function unbindApi(teamId: string, api: DockviewApi) {
  if (apis.get(teamId) === api) apis.delete(teamId);
}
export const isBulk = (teamId: string) => bulk.has(teamId);
export const boundApi = (teamId: string) => apis.get(teamId);

export const agentPanelId = (agentId: string) => `agent:${agentId}`;
export const shellPanelId = (shellId: string) => `shell:${shellId}`;

type Kind = "leader" | "assistant" | "worker" | "shell" | "office" | "board" | "settings" | "team" | "notes" | "catchup" | "memory" | "procs";

function kindOf(teamId: string, panelId: string): Kind | null {
  if (panelId === "office" || panelId === "board" || panelId === "settings" || panelId === "team" || panelId === "memory" || panelId === "procs" || panelId === "catchup") return panelId;
  if (panelId.startsWith("shell:")) return "shell";
  if (isNotes(panelId)) return "notes";
  if (panelId.startsWith("agent:")) {
    const a = membersOf(teamId).find(m => agentPanelId(m.id) === panelId);
    return a ? a.roleSlug === "assistant" ? "assistant" : a.kind : null;
  }
  return null;
}

type Spec = Omit<AddPanelOptions, "position" | "floating">;

function agentSpec(teamId: string, agentId: string): Spec | null {
  const a = membersOf(teamId).find(m => m.id === agentId);
  return a ? { id: agentPanelId(a.id), component: "agent", tabComponent: "agent", title: a.name, params: { agentId: a.id } } : null;
}
const shellSpec = (shellId: string): Spec => ({ id: shellPanelId(shellId), component: "shell", tabComponent: "shell", title: "Terminal", params: { shellId } });
export const FIXED: Record<"office" | "board" | "settings" | "team" | "memory" | "procs", Spec> = {
  office: { id: "office", component: "office", tabComponent: "fixed", title: "Kart" },
  board: { id: "board", component: "board", tabComponent: "fixed", title: "Görevler" },
  settings: { id: "settings", component: "settings", tabComponent: "fixed", title: "Ayarlar" },
  team: { id: "team", component: "team", tabComponent: "fixed", title: "Takım" },
  procs: { id: "procs", component: "procs", tabComponent: "fixed", title: "Süreçler" },
  memory: { id: "memory", component: "memory", tabComponent: "fixed", title: "Hafıza" },
};

// ---- Notes: a free panel (#46; DEX-41's locked group is gone). #2086: the shared and the team page
// are one "Notlar" panel; saved docks with the old two pages migrate in `notesDock`. ----

export const NOTES: Spec = { id: NOTES_ID, component: "notes", tabComponent: "notes", title: NOTES_TITLE };
const isNotes = (panelId: string) => panelId === NOTES_ID;
export { isNotes, quietly };

// ---- #2235: "Sen yokken" — the catch-up summary, a tab of Core's group (Core | Notlar | Sen yokken | Echo). ----

export const CATCHUP: Spec = { id: "catchup", component: "catchup", tabComponent: "catchup", title: "Sen yokken" };
export const isCatchup = (panelId: string) => panelId === CATCHUP.id;

/** The notes' share of the workspace in the default screen, and when reopened. */
const NOTES_W = 0.38;
const NOTES_H = 0.4;

/** Id of the grid group at the bottom-right corner: last child of every branch, all the way down. */
function cornerGroup(api: DockviewApi): string | undefined {
  type Node = { type: "branch"; data: Node[] } | { type: "leaf"; data: { id: string } };
  let n = api.toJSON().grid.root as Node | undefined;
  while (n?.type === "branch") n = n.data[n.data.length - 1];
  return n?.data.id;
}

// ---- #49: the floating screens (Görevler, Ayarlar, Takım) open in the middle of the Dex window ----

const FLOAT_SIZE = { board: [760, 520], settings: [880, 620], team: [920, 680], memory: [1020, 680] } as const;
type FloatKind = keyof typeof FLOAT_SIZE;
const isFloatKind = (id: string): id is FloatKind => id in FLOAT_SIZE;
/** Gap kept between a float and the dock's edge when the window is too small for it. */
const FLOAT_MARGIN = 16;

type Box = { top: number; left: number; width: number; height: number };
type Float = { group: IDockviewPanel["group"]; overlay: { element: HTMLElement; setBounds(b: Partial<Box>): void } };
/** DockviewApi keeps its component private; its `floatingGroups` (public on the component) is the only way to the overlay bounds. */
const floatsOf = (api: DockviewApi) => (api as unknown as { component: { floatingGroups: readonly Float[] } }).component.floatingGroups;
const floatOf = (api: DockviewApi, p: IDockviewPanel) => floatsOf(api).find(f => f.group === p.group);

/**
 * Default size, shrunk to fit; centred on the window (not the workspace under the top bar), as
 * near as the dock lets it — a float can't leave the dock's box.
 */
function centerFloat(api: DockviewApi, p: IDockviewPanel) {
  const f = floatOf(api, p);
  const box = f?.overlay.element.parentElement?.getBoundingClientRect();
  if (!f || !box?.width || !box.height || !isFloatKind(p.id)) return;
  const [w, h] = FLOAT_SIZE[p.id];
  // Pencere merkezine oturtulabilirlik de bir sınırdır: dock kutusu pencerenin
  // içinde kaymışsa (üstte topbar), float kutuya değil pencereye göre kırpılır.
  // Yoksa clamp üst bandı tutar ve büyük float (Ayarlar 880×620) merkezden sapar.
  const width = Math.max(0, Math.min(w, box.width - 2 * FLOAT_MARGIN, window.innerWidth - 2 * box.left - 2 * FLOAT_MARGIN));
  const height = Math.max(0, Math.min(h, box.height - 2 * FLOAT_MARGIN, window.innerHeight - 2 * box.top - 2 * FLOAT_MARGIN));
  const clamp = (v: number, max: number) => Math.round(Math.max(0, Math.min(v, max)));
  f.overlay.setBounds({
    width, height,
    left: clamp((window.innerWidth - width) / 2 - box.left, box.width - width),
    top: clamp((window.innerHeight - height) / 2 - box.top, box.height - height),
  });
}

/** Worker/shell cells narrower than this stack as tabs instead of splitting further. */
const MIN_CELL_W = 420;

/** Where a newly opened panel goes: next to its own kind first, else its default neighbour. */
function add(teamId: string, api: DockviewApi, spec: Spec): IDockviewPanel {
  const kind = kindOf(teamId, spec.id);
  if (kind === "assistant") {
    // #1840: the last tab of Core's group (Core | Notlar | Echo), not a column of its own.
    const anchor = api.panels.find(p => kindOf(teamId, p.id) === "leader") ?? api.panels.find(p => isNotes(p.id));
    return api.addPanel({ ...spec, params: { ...spec.params, assistant: true },
      position: anchor ? { referenceGroup: anchor.group } : { direction: "right" } });
  }
  if (kind === "catchup") {
    // #2235: a tab of Core's group, next to the notes (Echo, added later, stays last).
    const anchor = api.panels.find(p => isNotes(p.id)) ?? api.panels.find(p => kindOf(teamId, p.id) === "leader");
    return api.addPanel({ ...spec, position: anchor ? { referenceGroup: anchor.group } : { direction: "right" } });
  }
  if (kind === "worker") {
    const pair = workerRowsOf(teamId, api);
    const groups = workingGroupsOf(api, pair);
    const working = groups[groups.length - 1];
    const done = pair.done ? api.getGroup(pair.done) : undefined;
    if (done) {
      return api.addPanel({ ...spec, position: working ? { referenceGroup: working.id } : { referenceGroup: done.id, direction: "right" } });
    }
  }
  const peers = api.panels.filter(p => {
    const k = kindOf(teamId, p.id);
    return kind === "leader" ? k === "leader" : (kind === "worker" || kind === "shell") && (k === "worker" || k === "shell");
  });
  const last = peers[peers.length - 1];
  const office = api.getPanel("office");
  if (kind === "board" || kind === "settings" || kind === "team" || kind === "memory") {
    const [width, height] = FLOAT_SIZE[kind];
    const p = api.addPanel({ ...spec, floating: { width, height } });
    centerFloat(api, p);
    return p;
  }
  if (kind === "notes") {
    // #46: back in the bottom-right corner.
    const corner = cornerGroup(api);
    const p = api.addPanel({ ...spec, ...(corner ? { position: { referenceGroup: corner, direction: "below" as const } } : {}) });
    if (api.height) p.group.api.setSize({ height: Math.round(api.height * NOTES_H) });
    return p;
  }
  if (kind === "procs" || kind === "shell") {
    // #65/#2266: one terminal group — a new shell (or Süreçler) tabs onto it, empty or not; none → a new row at the bottom left.
    const home = shellGroup(teamId, api);
    if (home) return api.addPanel({ ...spec, position: { referenceGroup: home.id } });
  }
  if (kind === "shell") {
    const ref = shellRowAnchor(teamId, api);
    if (ref) return api.addPanel({ ...spec, position: { referencePanel: ref, direction: "below" } });
  }
  if (last) {
    const split = kind !== "leader" && last.group.api.width >= 2 * MIN_CELL_W;
    return api.addPanel({ ...spec, position: split ? { referencePanel: last, direction: "right" } : { referenceGroup: last.group } });
  }
  // #50: leaders take the right column, workers/shells go under the Kart in the left one.
  if (kind === "leader" && office) return api.addPanel({ ...spec, position: { direction: "right" } });
  if ((kind === "worker" || kind === "shell") && office) {
    return api.addPanel({ ...spec, position: { referencePanel: office, direction: "below" } });
  }
  const leader = api.panels.find(p => kindOf(teamId, p.id) === "leader");
  if ((kind === "worker" || kind === "shell" || kind === "office") && leader) {
    return api.addPanel({ ...spec, position: { referenceGroup: leader.group, direction: "left" } });
  }
  return api.addPanel({ ...spec, ...(office ? { position: { referencePanel: office, direction: "below" } } : {}) });
}

/** Where a terminal row starts when there is none: under the lowest worker, else under the Kart. */
function shellRowAnchor(teamId: string, api: DockviewApi): IDockviewPanel | undefined {
  const box = (p: IDockviewPanel) => p.group.element.getBoundingClientRect();
  const workers = api.panels.filter(p => kindOf(teamId, p.id) === "worker")
    .sort((a, b) => box(b).bottom - box(a).bottom || box(a).left - box(b).left);
  return workers[0] ?? api.getPanel("office");
}

// ---- #2266: the fixed terminal group — its pane (and "Yeni terminal") stays when every tab is closed ----

/** The fixed group as it stands (remembered id first, then the terminal's group, then an empty one); null when none. */
export function shellGroup(teamId: string, api: DockviewApi): IDockviewGroupPanel | null {
  const grid = api.groups.filter(g => g.api.location.type === "grid");
  const id = resolveShellGroup(grid.map(g => ({ id: g.id, panels: g.panels.map(p => p.id) })), shellGroupOf(teamId));
  return (id && api.getGroup(id)) || null;
}

/**
 * Resolve and remember the fixed group; a layout without one (saved before #2266, or the patron
 * dragged its last tab away) gets an empty one at the terminal row's default spot and size, once.
 */
export function ensureShellGroup(teamId: string, api: DockviewApi): IDockviewGroupPanel | null {
  if (!api.totalPanels) return null;
  let g: IDockviewGroupPanel | null = shellGroup(teamId, api);
  if (!g) {
    quietly(teamId, () => {
      const ref = shellRowAnchor(teamId, api);
      g = api.addGroup(ref ? { referencePanel: ref, direction: "below" } : { direction: "below" });
      if (api.height) g.api.setSize({ height: Math.round(api.height * SHELLS_H) });
    });
  }
  setShellGroup(teamId, g?.id);
  return g;
}

/** Close a tab: a group emptied this way goes — except the fixed terminal group, whose pane stays. */
export function closePanel(api: DockviewApi, panel: IDockviewPanel) {
  const teamId = [...apis].find(([, a]) => a === api)?.[0];
  const removeEmptyGroup = teamId === undefined || removesEmptyGroup(shellGroup(teamId, api)?.id ?? null, panel.group.id);
  // DockviewApi.removePanel drops the options; the component (public on the api object) takes them.
  (api as unknown as { component: { removePanel(p: IDockviewPanel, o: { removeEmptyGroup: boolean }): void } }).component.removePanel(panel, { removeEmptyGroup });
}

function open(teamId: string, spec: Spec | null, onOpened?: () => void): boolean {
  if (!spec) return false;
  pendingOpens.run(teamId, apis.has(teamId), () => {
    const api = apis.get(teamId)!;
    let p = api.getPanel(spec.id);
    if (!p) {
      const pair = kindOf(teamId, spec.id) === "worker" ? workerRowsOf(teamId, api) : undefined;
      p = add(teamId, api, spec);
      if (pair) {
        pair.working.add(p.group.id);
        rememberWorkerRows(teamId, api, pair);
      }
    }
    p.api.setActive();
    onOpened?.();
  });
  return true;
}

export const openAgent = (teamId: string, agentId: string) => open(teamId, agentSpec(teamId, agentId));
export const openPanel = (teamId: string, which: keyof typeof FIXED) => open(teamId, FIXED[which]);
/** Ayarlar on a given page (Bağlantılar for a host that lost its token, Kaynak sınırları for the limits). */
export function openSettings(teamId: string, tab: SettingsTab, setting?: string) {
  selectSettingsTab(tab);
  // Mounted view focuses at once; an opening view consumes it on mount.
  if (setting) focusSetting(setting);
  return openPanel(teamId, "settings");
}

const afterPaint = () => new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r())));

/** Open (or bring forward) the agent's panel, then put the keyboard in its terminal. */
export async function revealAgent(teamId: string, agentId: string) {
  const spec = agentSpec(teamId, agentId);
  if (!spec) return;
  await new Promise<void>(resolve => open(teamId, spec, resolve));
  await afterPaint(); // the team stage must lose `inert` before focus lands
  requestFocus(agentId);
}

/** DEX-29: the patron's shell in the team workdir (#838: its selected project's), as a new panel. Returns its id. */
export async function openShell(teamId: string): Promise<string | null> {
  if (!apis.has(teamId)) return null;
  try {
    const cwd = await resolveShellCwd(teamId);
    if (!apis.has(teamId)) return null;
    const id = addShell(teamId, cwd);
    open(teamId, shellSpec(id));
    return id;
  } catch (error) {
    toast({ key: `shell-project:${teamId}`, tone: "err", title: "Terminal açılamadı", body: projectErrorText(error) });
    return null;
  }
}

export const revealShell = (teamId: string, shellId: string) => open(teamId, shellSpec(shellId));

/** #46: reopen the closed notes panel (or bring it forward). */
export const openNotes = (teamId: string) => open(teamId, NOTES);

/** #2235: reopen the closed "Sen yokken" tab (or bring it forward). */
export const openCatchup = (teamId: string) => open(teamId, CATCHUP);

function quietly(teamId: string, f: () => void) {
  bulk.add(teamId);
  rebuilds.set(teamId, (rebuilds.get(teamId) ?? 0) + 1);
  try {
    f();
  } finally {
    bulk.delete(teamId);
  }
}

export function closeShell(teamId: string, shellId: string) {
  const api = apis.get(teamId);
  const p = api?.getPanel(shellPanelId(shellId));
  if (api && p) quietly(teamId, () => closePanel(api, p));
  killPty(shellPtyId(shellId)).catch(() => {});
  removeShell(teamId, shellId);
}

/** DEX-7 side of a reset: the team's default screen, rebuilt around what's running now. */
export function resetLayout(teamId: string) {
  const api = apis.get(teamId);
  if (!api) return;
  quietly(teamId, () => {
    api.clear();
    workerRows.delete(api);
    buildDefault(teamId, api);
  });
  ensureShellGroup(teamId, api);
  if (getFavorite()) lockOffice(api);
}

// ---- Patron düzeni (favori): kaydet, uygula, sabitle ----

/** Favori varken Kart'ın grubu kilitlidir: sekme sürüklenemez, gruba sekme bırakılamaz. */
export function lockOffice(api: DockviewApi) {
  const g = api.getPanel("office")?.group;
  if (g && !g.locked) g.locked = true;
}
export const hasFavorite = () => getFavorite() !== null;

/** Aktif takımın o anki dock'unu favori olarak saklar (aynı layout.json'a). */
export function saveFavoriteLayout(teamId: string): boolean {
  const api = apis.get(teamId);
  if (!api) return false;
  const members = membersOf(teamId);
  setFavorite({
    dock: api.toJSON(),
    leaders: members.filter(a => a.kind === "leader").map(a => a.id),
    workers: members.filter(a => a.kind === "worker" && isAssignable(a)).map(a => a.id),
    shells: (getSnapshot().teams[teamId]?.shells ?? []).map(s => s.id),
    from: teamId,
  });
  lockOffice(api);
  return true;
}

function favoriteTarget(teamId: string, shellCount: number): FavoriteTarget {
  const members = membersOf(teamId);
  const have = getSnapshot().teams[teamId]?.shells ?? [];
  const shells = have.map(s => s.id);
  // Kaynaktaki shell yuvası kadar kabuk: eksikse üret, fazlası durur (panelsiz kalır).
  for (let i = shells.length; i < shellCount; i++) shells.push(addShell(teamId, newShellCwd(teamId)));
  const pick = (kind: "leader" | "worker") => members.filter(a => a.kind === kind && isAssignable(a)).map(a => ({ id: a.id, name: a.name }));
  return { leaders: pick("leader"), workers: pick("worker"), shells };
}

function showFavorite(teamId: string, api: DockviewApi): boolean {
  const fav = getFavorite();
  if (!fav) return false;
  const { dock } = remapDock(migrateNotesDock(fav.dock), fav, favoriteTarget(teamId, fav.shells.length));
  workerRows.delete(api);
  setShellGroup(teamId, undefined); // the favourite's group ids are another layout's
  quietly(teamId, () => {
    try {
      api.fromJSON(dock);
    } catch (e) {
      console.warn("favori düzen okunamadı, varsayılana dönülüyor:", e);
      api.clear();
    }
  });
  reconcile(teamId, api);
  const rack = api.getPanel("rack");
  if (rack) api.removePanel(rack);
  api.getPanel("office")?.api.setTitle("Kart");
  api.getPanel(NOTES.id)?.api.setTitle(NOTES.title ?? "");
  api.getPanel(CATCHUP.id)?.api.setTitle(CATCHUP.title ?? "");
  if (api.totalPanels === 0) buildDefault(teamId, api);
  ensureShellGroup(teamId, api);
  lockOffice(api);
  return true;
}

/**
 * Favoriyi takıma uygular: açık dock varsa anında yeniden kurar, yoksa saklanan düzeni
 * yazar (workspace mount'ta `load` onu kurar). Yeni takımlar da bu yoldan favoriyle açılır.
 */
export function applyFavoriteLayout(teamId: string): boolean {
  const fav = getFavorite();
  if (!fav) return false;
  const api = apis.get(teamId);
  if (api) return showFavorite(teamId, api);
  const { dock } = remapDock(migrateNotesDock(fav.dock), fav, favoriteTarget(teamId, fav.shells.length));
  setShellGroup(teamId, undefined);
  setDock(teamId, dock);
  return true;
}

/** Favoriyi her takıma uygular (açık olmayanlarınki mount'ta kurulur). Uygulanan takım sayısını döner. */
export function applyFavoriteToAll(teamIds: string[]): number {
  if (!getFavorite()) return 0;
  let n = 0;
  for (const id of teamIds) if (applyFavoriteLayout(id)) n++;
  return n;
}

/** DEX-29: grid columns for `n` cells in a w×h box — terminals read best around 2:1 (w:h). */
function gridCols(n: number, w: number, h: number): number {
  if (n <= 1 || w <= 0 || h <= 0) return 1;
  let best = 1;
  let bestScore = -1;
  for (let c = 1; c <= n; c++) {
    const score = Math.min(w / c, (h / Math.ceil(n / c)) * 2);
    if (score > bestScore) {
      bestScore = score;
      best = c;
    }
  }
  return best;
}

/** #45: the leader column's width (#50: now on the right). */
const LEAD_W = 0.4;
/** #50: the Kart's share of the left column when workers or shells sit under it; the shells' row under workers. */
const OFFICE_H = 0.35;
const SHELLS_H = 0.3;

/**
 * #50 default screen — the root splits into columns, so the leader keeps the full height:
 *   office ........... | leader + notes + assistant (tabs)
 *   workers grid ..... |
 *   shells ........... |
 * A second leader splits under the first, more leaders tab onto the second; without a leader the
 * notes take the right column. Workers and shells line up under the Kart, as many side by side as
 * fit at MIN_CELL_W.
 */
function buildDefault(teamId: string, api: DockviewApi) {
  setShellGroup(teamId, undefined); // a fresh grid: the new shell row becomes the fixed group
  const members = membersOf(teamId);
  const leaders = members.filter(a => a.kind === "leader").map(a => a.id);
  const t = getSnapshot().teams[teamId];
  const pick = (ids: string[] | undefined, not?: string) => ids?.find(id => id !== not && leaders.includes(id));
  const first = pick(t?.seed?.left) ?? leaders[0];
  const second = pick(t?.seed?.right, first) ?? leaders.find(id => id !== first);
  const W = api.width || 1600;
  const H = api.height || 900;

  const put = (spec: Spec, ref: IDockviewPanel, direction: "right" | "below") =>
    api.addPanel({ ...spec, inactive: true, position: { referencePanel: ref, direction } });
  const office = api.addPanel(FIXED.office);
  // Takım and Ayarlar tab next to the Kart (open() just brings them forward); Kart stays active.
  for (const id of ["team", "settings"] as const) api.addPanel({ ...FIXED[id], inactive: true, position: { referenceGroup: office.group } });
  const lead = first ? api.addPanel({ ...agentSpec(teamId, first)!, position: { referencePanel: office, direction: "right" } }) : null;
  const two = second ? put(agentSpec(teamId, second)!, lead!, "below") : null;
  for (const id of leaders.filter(id => id !== first && id !== second)) {
    api.addPanel({ ...agentSpec(teamId, id)!, inactive: true, position: { referenceGroup: two!.group } });
  }
  const notes = lead
    ? api.addPanel({ ...NOTES, inactive: true, position: { referenceGroup: lead.group } })
    : put(NOTES, office, "right");
  // #2235: "Sen yokken" tabs onto the notes' group; the assistant (synced below) appends after it.
  api.addPanel({ ...CATCHUP, inactive: true, position: { referenceGroup: notes.group } });
  setCatchupOffered(teamId);

  const workers = members.filter(a => a.kind === "worker" && isAssignable(a)).map(a => agentSpec(teamId, a.id)!);
  // #65: a team without a shell gets one — its group carries "Yeni terminal", as on the patron's first team.
  const shellIds = t?.shells.length ? t.shells.map(s => s.id) : [addShell(teamId, newShellCwd(teamId))];
  const shells = shellIds.map(shellSpec);
  const leftW = W * (1 - (lead ? LEAD_W : NOTES_W));
  const below = workers.length || shells.length ? H * (1 - OFFICE_H) : 0;
  const shellsH = workers.length && shells.length ? H * SHELLS_H : below;

  // Each row's first cell goes under the column (workers first, so shells land under the whole grid),
  // then the row splits to the right; extra shells tab onto the last one.
  const cols = gridCols(workers.length, leftW, below - (shells.length ? shellsH : 0));
  const heads: IDockviewPanel[] = workers.length ? [put(workers[0], office, "below")] : [];
  const shellCols = Math.max(1, Math.min(shells.length, Math.floor(leftW / MIN_CELL_W)));
  const shellCells: IDockviewPanel[] = shells.length ? [put(shells[0], heads[0] ?? office, "below")] : [];
  shells.slice(1).forEach((s, i) => {
    const last = shellCells[shellCells.length - 1];
    if (i + 1 >= shellCols) api.addPanel({ ...s, inactive: true, position: { referenceGroup: last.group } });
    else shellCells.push(put(s, last, "right"));
  });
  // Top row of workers first, then every column grows down — rows line up across columns.
  workers.slice(1, cols).forEach(c => heads.push(put(c, heads[heads.length - 1], "right")));
  const tails = [...heads];
  workers.slice(cols).forEach((c, i) => {
    const col = i % cols;
    tails[col] = put(c, tails[col], "below");
  });

  notes.group.api.setSize({ width: W * (lead ? LEAD_W : NOTES_W) });
  if (two) lead!.group.api.setSize({ height: H / 2 });
  if (below) office.group.api.setSize({ height: H * OFFICE_H });
  if (workers.length && shellCells.length) shellCells[0].group.api.setSize({ height: shellsH });
  heads.forEach(p => p.group.api.setSize({ width: leftW / heads.length }));
  shellCells.forEach(p => p.group.api.setSize({ width: leftW / shellCells.length }));
  // A rebuilt default layout includes the assistant again, even one the patron had closed.
  syncAssistantPanel(teamId, api, true);
  office.api.setActive();
}

/**
 * Membership changes reuse the same agent slot and the existing session. Dex opens the pane once per
 * assistant (or on `fresh` default layouts); after the patron closes it, only the rack does.
 */
function syncAssistantPanel(teamId: string, api: DockviewApi, fresh = false) {
  const assistant = membersOf(teamId).find(a => a.roleSlug === "assistant");
  if (!assistant || !api.totalPanels) return;
  const spec = agentSpec(teamId, assistant.id)!;
  const pane = api.getPanel(spec.id);
  const opens = fresh || opensAssistant(assistant.id, assistantOffered(teamId));
  setAssistantOffered(teamId, assistant.id);
  if (!pane && !opens) return;
  // An open pane stays where it is — the patron's drags included.
  if (pane && !pane.params?.assistantDirection) return;
  quietly(teamId, () => {
    if (!pane) {
      add(teamId, api, { ...spec, inactive: true });
      return;
    }
    // #1840: layouts saved before the tab put the assistant in a column of its own next to Core; it
    // joins Core's tabs once and Core gets its width back. Floating placement belongs to the patron.
    const leader = api.panels.find(p => kindOf(teamId, p.id) === "leader");
    if (leader && pane.api.location.type === "grid" && pane.group !== leader.group) {
      const alone = pane.group.panels.length === 1;
      pane.api.moveTo({ group: leader.group, position: "center", skipSetActive: true });
      const share = pane.params?.assistantLeadShare;
      if (alone && typeof share === "number" && share > 0 && share < 1) leader.group.api.setSize({ width: api.width * share });
    }
    pane.api.updateParameters({ assistant: true, assistantDirection: undefined, assistantLeader: undefined, assistantLeadShare: undefined });
  });
}

/**
 * #2235: a layout saved before the catch-up tab gets it once, tabbed onto Core's group without
 * stealing the active tab. Closed by the patron after that, it stays closed (the palette reopens it).
 */
function syncCatchupPanel(teamId: string, api: DockviewApi) {
  if (!api.totalPanels) return;
  if (api.getPanel(CATCHUP.id)) {
    setCatchupOffered(teamId);
    return;
  }
  if (catchupOffered(teamId)) return;
  setCatchupOffered(teamId);
  quietly(teamId, () => add(teamId, api, { ...CATCHUP, inactive: true }));
}

/** Drop panels whose agent left the team or whose shell is gone. */
export function reconcile(teamId: string, api: DockviewApi) {
  const agents = new Set(membersOf(teamId).map(a => agentPanelId(a.id)));
  const shells = new Set((getSnapshot().teams[teamId]?.shells ?? []).map(s => shellPanelId(s.id)));
  const stale = api.panels.filter(p => (p.id.startsWith("agent:") && (!agents.has(p.id) || (p.params?.assistant || p.params?.assistantDirection) && kindOf(teamId, p.id) !== "assistant")) || (p.id.startsWith("shell:") && !shells.has(p.id)));
  if (stale.length) quietly(teamId, () => stale.forEach(p => closePanel(api, p)));
  syncAssistantPanel(teamId, api);
  syncCatchupPanel(teamId, api);
}

export function load(teamId: string, api: DockviewApi) {
  workerRows.delete(api);
  const saved = getSnapshot().teams[teamId]?.dock;
  // Yeni takım (kayıtlı düzeni yok) favoriyle açılır.
  if (!saved && getFavorite()) {
    showFavorite(teamId, api);
    centerFloats(api);
    return;
  }
  quietly(teamId, () => {
    if (saved) {
      try {
        api.fromJSON(migrateNotesDock(saved));
      } catch (e) {
        console.warn("dockview düzeni okunamadı, varsayılana dönülüyor:", e);
        api.clear();
        setShellGroup(teamId, undefined);
      }
    }
    reconcile(teamId, api);
    // #42: the rack moved to the top bar; a saved layout's rack panel goes, its neighbours take the room.
    const rack = api.getPanel("rack");
    if (rack) api.removePanel(rack);
    if (api.totalPanels === 0) buildDefault(teamId, api);
    // Saved pre-DEX-39 layouts carry their serialized title; migrate it without resetting layout.
    api.getPanel("office")?.api.setTitle("Kart");
    // FIX-3/#2086: a saved title wins over the spec's; the notes tab is "Notlar".
    const notes = api.getPanel(NOTES.id);
    notes?.api.setTitle(NOTES.title ?? "");
    api.getPanel(CATCHUP.id)?.api.setTitle(CATCHUP.title ?? "");
    // #46: DEX-41 saved its notes group locked (no drops); the notes are a free panel now.
    if (notes) notes.group.locked = false;
    if (getFavorite()) lockOffice(api);
  });
  ensureShellGroup(teamId, api);
  centerFloats(api);
}

function centerFloats(api: DockviewApi) {
  // #49: a saved float's place was that session's; a reopened Dex starts these in the middle again.
  // Next frame: at `onReady` the dock may not be laid out yet.
  requestAnimationFrame(() => {
    for (const id of Object.keys(FLOAT_SIZE)) {
      const p = api.getPanel(id);
      if (p?.group.api.location.type === "floating") centerFloat(api, p);
    }
  });
}

// ---- #50: emptying the cell under the top row's right end hands its room to that group ----

type GridNode = { type: "branch"; data: GridNode[] } | { type: "leaf"; data: { id: string } };
type Corner = { corner: string; above: string };

/**
 * A grid of two rows (root split top/bottom), each ending in a single group on the right: the
 * bottom-right group and the one above it. A full-width bottom row is what keeps the group above
 * from growing down when its neighbour below goes — dockview only widens the bottom row's rest.
 */
function cornerOf(api: DockviewApi): Corner | null {
  const grid = api.toJSON().grid as { root: GridNode; orientation: string };
  if (grid.orientation !== "VERTICAL" || grid.root.type !== "branch" || grid.root.data.length !== 2) return null;
  const [top, bottom] = grid.root.data;
  const above = top.type === "branch" ? top.data[top.data.length - 1] : undefined;
  const corner = bottom.type === "branch" ? bottom.data[bottom.data.length - 1] : undefined;
  return above?.type === "leaf" && corner?.type === "leaf" ? { corner: corner.data.id, above: above.data.id } : null;
}

/**
 * The patron emptied the bottom-right group (closed or dragged out its last panel): the group above
 * it moves to the right edge as a full-height column, the rest of both rows stays on the left.
 * Patron edits only — a bulk rebuild just re-reads the grid.
 */
export function growIntoCorner(teamId: string, api: DockviewApi) {
  let prev = cornerOf(api);
  let seen = rebuilds.get(teamId);
  return api.onDidLayoutChange(() => {
    const was = seen === rebuilds.get(teamId) ? prev : null;
    prev = cornerOf(api);
    seen = rebuilds.get(teamId);
    if (!was || api.getGroup(was.corner)) return;
    const above = api.getGroup(was.above);
    const grid = api.toJSON().grid as { root: GridNode; orientation: string };
    const top = grid.root.type === "branch" && grid.orientation === "VERTICAL" ? grid.root.data[0] : undefined;
    const end = top?.type === "branch" ? top.data[top.data.length - 1] : undefined;
    // Still the top row's right end (a drag elsewhere may have reshaped the grid).
    if (!above || end?.type !== "leaf" || end.data.id !== above.id) return;
    const width = above.api.width;
    const active = above.activePanel;
    quietly(teamId, () => {
      above.api.moveTo({ position: "right", skipSetActive: true });
      active?.api.setActive();
      active?.group.api.setSize({ width });
    });
    prev = cornerOf(api);
    seen = rebuilds.get(teamId);
  });
}
