// FIX-5: dockview registry + panel commands without the React views. This module is light
// (type-only dockview imports, stores, pty) so the topbar, palette and board can call
// `openAgent`/`openPanel`/… without pulling `dockview-react` into the first-open chunk —
// the `Workspace` component itself loads lazily.
import type {
  AddPanelOptions, DockviewApi, IDockviewPanel,
} from "dockview-react";
import { kept } from "../hot";
import { requestFocus } from "../launcher";
import { killPty } from "../pty";
import { addShell, assistantOffered, getFavorite, getSnapshot, removeShell, setAssistantOffered, setDock, setFavorite, shellPtyId } from "./store";
import { membersOf } from "../team/store";
import { isAssignable } from "../team/rolePolicy";
import { newShellCwd, resolveShellCwd } from "../project/store";
import { projectErrorText } from "../project/model";
import { toast } from "../toast/store";
import { remapDock, type FavoriteTarget } from "./favorite";
import { selectSettingsTab, type SettingsTab } from "../settings/tab";
import { workerGroups, type WorkerRows } from "./workerGroups";
import { playCircuit } from "../office/circuitFx";
import { ReadyActions } from "./readyActions";
import { assistantDirection, opensAssistant } from "./assistantPlacement";

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
    if (!from.width || !from.height || window.matchMedia("(prefers-reduced-motion: reduce)").matches) continue;
    requestAnimationFrame(() => {
      if (api.getPanel(pane.id) !== pane || pane.group !== target) return;
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

type Kind = "leader" | "assistant" | "worker" | "shell" | "office" | "board" | "settings" | "team" | "notes" | "memory" | "procs";

function kindOf(teamId: string, panelId: string): Kind | null {
  if (panelId === "office" || panelId === "board" || panelId === "settings" || panelId === "team" || panelId === "memory" || panelId === "procs") return panelId;
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

// ---- Notes: two pages, free panels (#46; DEX-41's locked group is gone) ----

export type NotesPage = "shared" | "team";
export const NOTES: Record<NotesPage, Spec> = {
  shared: { id: "notes:shared", component: "notes", tabComponent: "notes", title: "Ortak not", params: { page: "shared" } },
  team: { id: "notes:team", component: "notes", tabComponent: "notes", title: "Takım notu", params: { page: "team" } },
};
const isNotes = (panelId: string) => panelId.startsWith("notes:");
export { isNotes, quietly };

/** The notes' share of the workspace in the default screen, and when reopened with neither page open. */
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

const FLOAT_SIZE = { board: [760, 520], settings: [620, 380], team: [920, 680], memory: [1020, 680] } as const;
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
  const width = Math.max(0, Math.min(w, box.width - 2 * FLOAT_MARGIN));
  const height = Math.max(0, Math.min(h, box.height - 2 * FLOAT_MARGIN));
  const clamp = (v: number, max: number) => Math.round(Math.max(0, Math.min(v, max)));
  f.overlay.setBounds({
    width, height,
    left: clamp((window.innerWidth - width) / 2 - box.left, box.width - width),
    top: clamp((window.innerHeight - height) / 2 - box.top, box.height - height),
  });
}

/** Worker/shell cells narrower than this stack as tabs instead of splitting further. */
const MIN_CELL_W = 420;

function assistantOrigin(api: DockviewApi, leader: IDockviewPanel | undefined) {
  return leader && api.width > 0 ? { assistantLeader: leader.id, assistantLeadShare: leader.group.api.width / api.width } : {};
}

/** Where a newly opened panel goes: next to its own kind first, else its default neighbour. */
function add(teamId: string, api: DockviewApi, spec: Spec): IDockviewPanel {
  const kind = kindOf(teamId, spec.id);
  if (kind === "assistant") {
    const leader = api.panels.find(p => kindOf(teamId, p.id) === "leader");
    const direction = assistantDirection(window.innerWidth);
    return api.addPanel({ ...spec, params: { ...spec.params, assistantDirection: direction, ...assistantOrigin(api, leader) },
      position: { ...(leader ? { referencePanel: leader } : {}), direction } });
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
    // #46: onto the other page wherever the patron put it, else back in the bottom-right corner.
    const other = api.panels.find(p => isNotes(p.id));
    if (other) return api.addPanel({ ...spec, position: { referenceGroup: other.group } });
    const corner = cornerGroup(api);
    const p = api.addPanel({ ...spec, ...(corner ? { position: { referenceGroup: corner, direction: "below" as const } } : {}) });
    if (api.height) p.group.api.setSize({ height: Math.round(api.height * NOTES_H) });
    return p;
  }
  if (kind === "procs") {
    const shell = api.panels.find(p => kindOf(teamId, p.id) === "shell");
    if (shell) return api.addPanel({ ...spec, position: { referenceGroup: shell.group } });
  }
  if (kind === "shell") {
    // #65: one terminal group — a new shell tabs onto it; none open → a new row at the bottom left.
    const shells = api.panels.filter(p => p.id.startsWith("shell:"));
    const shell = shells[shells.length - 1];
    if (shell) return api.addPanel({ ...spec, position: { referenceGroup: shell.group } });
    const box = (p: IDockviewPanel) => p.group.element.getBoundingClientRect();
    const workers = peers.filter(p => kindOf(teamId, p.id) === "worker")
      .sort((a, b) => box(b).bottom - box(a).bottom || box(a).left - box(b).left);
    const ref = workers[0] ?? office;
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
/** Ayarlar on a given tab (Sunucular for a host that lost its token, Ajanlar for the limits). */
export function openSettings(teamId: string, tab: SettingsTab) {
  selectSettingsTab(tab);
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

/** #46: reopen a closed notes page (both: the shared one comes forward). */
export function openNotes(teamId: string) {
  open(teamId, NOTES.team);
  return open(teamId, NOTES.shared);
}

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
  if (api && p) quietly(teamId, () => api.removePanel(p));
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
  const { dock } = remapDock(fav.dock, fav, favoriteTarget(teamId, fav.shells.length));
  workerRows.delete(api);
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
  for (const n of Object.values(NOTES)) api.getPanel(n.id)?.api.setTitle(n.title ?? "");
  if (api.totalPanels === 0) buildDefault(teamId, api);
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
  const { dock } = remapDock(fav.dock, fav, favoriteTarget(teamId, fav.shells.length));
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
 *   office ........... | leader + notes (tabs)
 *   workers grid ..... |
 *   shells ........... |
 * A second leader splits under the first, more leaders tab onto the second; without a leader the
 * notes take the right column. Workers and shells line up under the Kart, as many side by side as
 * fit at MIN_CELL_W.
 */
function buildDefault(teamId: string, api: DockviewApi) {
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
    ? api.addPanel({ ...NOTES.shared, inactive: true, position: { referenceGroup: lead.group } })
    : put(NOTES.shared, office, "right");
  api.addPanel({ ...NOTES.team, inactive: true, position: { referenceGroup: notes.group } });

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
 * Membership and viewport changes reuse the same agent slot and the existing session. Dex opens the
 * pane once per assistant (or on `fresh` default layouts); after the patron closes it, only the rack does.
 */
export function syncAssistantPanel(teamId: string, api: DockviewApi, fresh = false) {
  const assistant = membersOf(teamId).find(a => a.roleSlug === "assistant");
  if (!assistant || !api.totalPanels) return;
  const spec = agentSpec(teamId, assistant.id)!;
  const pane = api.getPanel(spec.id);
  const opens = fresh || opensAssistant(assistant.id, assistantOffered(teamId));
  setAssistantOffered(teamId, assistant.id);
  if (!pane && !opens) return;
  const direction = assistantDirection(window.innerWidth);
  if (pane?.params?.assistantDirection === direction) return;
  // Floating placement belongs to the patron.
  if (pane && pane.api.location.type !== "grid") return;
  quietly(teamId, () => {
    if (!pane) add(teamId, api, { ...spec, inactive: true });
    else {
      const leader = api.panels.find(p => kindOf(teamId, p.id) === "leader");
      if (!leader) return;
      const origin = pane.params?.assistantLeader ? {} : assistantOrigin(api, leader);
      pane.api.moveTo({ group: leader.group, position: direction === "below" ? "bottom" : "right", skipSetActive: true });
      pane.api.updateParameters({ assistantDirection: direction, ...origin });
    }
  });
}

/** Drop panels whose agent left the team or whose shell is gone. */
export function reconcile(teamId: string, api: DockviewApi) {
  const agents = new Set(membersOf(teamId).map(a => agentPanelId(a.id)));
  const shells = new Set((getSnapshot().teams[teamId]?.shells ?? []).map(s => shellPanelId(s.id)));
  const stale = api.panels.filter(p => (p.id.startsWith("agent:") && (!agents.has(p.id) || p.params?.assistantDirection && kindOf(teamId, p.id) !== "assistant")) || (p.id.startsWith("shell:") && !shells.has(p.id)));
  if (stale.length) quietly(teamId, () => {
    const origins = stale.map(p => ({ leader: p.params?.assistantLeader, share: p.params?.assistantLeadShare }));
    stale.forEach(p => api.removePanel(p));
    for (const { leader, share } of origins) {
      if (typeof leader === "string" && typeof share === "number" && share > 0 && share < 1) {
        api.getPanel(leader)?.group.api.setSize({ width: api.width * share });
      }
    }
  });
  syncAssistantPanel(teamId, api);
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
        api.fromJSON(saved);
      } catch (e) {
        console.warn("dockview düzeni okunamadı, varsayılana dönülüyor:", e);
        api.clear();
      }
    }
    reconcile(teamId, api);
    // #42: the rack moved to the top bar; a saved layout's rack panel goes, its neighbours take the room.
    const rack = api.getPanel("rack");
    if (rack) api.removePanel(rack);
    if (api.totalPanels === 0) buildDefault(teamId, api);
    // Saved pre-DEX-39 layouts carry their serialized title; migrate it without resetting layout.
    api.getPanel("office")?.api.setTitle("Kart");
    // FIX-3: notes tabs were "Ortak"/"Takım" (the latter clashed with the Takım panel).
    for (const n of Object.values(NOTES)) api.getPanel(n.id)?.api.setTitle(n.title ?? "");
    // #46: DEX-41 saved its notes group locked (no drops); the pages are free panels now.
    for (const n of Object.values(NOTES)) {
      const g = api.getPanel(n.id)?.group;
      if (g) g.locked = false;
    }
    if (getFavorite()) lockOffice(api);
  });
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
