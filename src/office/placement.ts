// #270: the office board's size, layout and trace routes — pure, so node:test covers it (world.ts draws).
// The board fills the Kart panel: Office measures the canvas, `fitUnit` picks how many CSS px one world
// unit is, and the world is the panel in those units. Wide boards keep the left/right columns; square
// and tall ones stack the workers in rows above and below Core.
// #277: the unit is the largest at which no chip or Core is squeezed below its full size — chips, Core
// and fonts grow with the panel (both sides count, no upper bound) until the content fills it. The edge
// is a thin strip of fixed CSS px around the chips.
// #1958: Echo (the assistant) is no worker column member: its own slot right under its Core's labels.
import type { Agent } from "../agents";
import type { Delegation } from "../delegations";
import { isAssignable } from "../team/rolePolicy.ts";
import { coreBody, corePort, XL_LABEL, XL_MIN, XL_PIN, XL_TARGET_H, type Body } from "./companion/coreXL.ts";

export type Point = { x: number; y: number };
export type Mode = "columns" | "stack";
export type Target = { x: number; y: number; w: number; h: number; mode?: Mode };

// ---- edge (#277: CSS px, so it stays thin at any scale) ----

/** Panel edge → the strip around the chip area (the team signature lives in it). */
export const BOARD_PAD_PX = 4;
export const EDGE_PX = 14;
/** World units a chip's provider badge reaches past its box: each cell's padding (edge and neighbours). */
export const PIN_CLEAR = 6;

/** Chip area inset in world units at `u` CSS px per unit. */
export const areaInset = (u: number) => (BOARD_PAD_PX + EDGE_PX) / u;

/** #1958: the assistant floats beside its Core — never in a worker column, never on a trace. */
export const isEcho = (agent: Agent) =>
  agent.kind !== "leader" && !isAssignable({ roleSlug: (agent as { roleSlug?: string | null }).roleSlug ?? null });

// ---- scale ----

/** Smallest text on the board (world units: the provider badge). */
export const SMALLEST_FONT = 8;
/** Readable minimum on screen (CSS px). */
export const MIN_FONT_PX = 9;
export const MIN_U = MIN_FONT_PX / SMALLEST_FONT;
/** A columns cell wider than this centres its content (very wide panels: no stretched traces). */
export const COLUMNS_MAX_W = 860;

/**
 * CSS px per world unit for a panel and its team: the largest unit at which the layout keeps every chip
 * and Core at full size, never below the readable minimum `MIN_U`.
 */
export function fitUnit(cssW: number, cssH: number, agents: Agent[], delegations: Delegation[], mode: Mode) {
  if (cssW <= 0 || cssH <= 0 || !agents.length) return MIN_U;
  const fits = (u: number) => fullSize(agents, layout(agents, delegations, cssW / u, cssH / u, mode, u));
  let lo = MIN_U;
  if (!fits(lo)) return lo;
  let hi = lo * 2;
  while (fits(hi) && hi < 1e4) { lo = hi; hi *= 2; }
  for (let i = 0; i < 24; i++) {
    const mid = (lo + hi) / 2;
    if (fits(mid)) lo = mid; else hi = mid;
  }
  return lo;
}

/** Every chip at its full size: leaders (targets with a `mode`) as Core / leader chip, workers as chips. */
function fullSize(agents: Agent[], targets: Map<string, Target>) {
  const stack = [...targets.values()].some((t) => t.mode === "stack");
  const workerH = stack ? Math.min(WORKER_H, ROW_MAX_H * CHIP_SHARE) : WORKER_H;
  const e = 0.01;
  return agents.every((agent) => {
    const t = targets.get(agent.id);
    if (!t) return true;
    if (t.mode) return t.w >= LEADER_W - e && t.h >= (agent.kind === "leader" ? XL_TARGET_H : LEADER_H) - e;
    if (isEcho(agent)) return t.w >= LEADER_W - e && t.h >= ECHO_H - e;
    return t.w >= WORKER_W - e && t.h >= workerH - e;
  });
}

/** Columns from this aspect (w / h) up, stack below `STACK_BELOW`; in between the board keeps its mode. */
export const COLUMNS_FROM = 1.3;
export const STACK_BELOW = 1.15;

export function pickMode(aspect: number, prev: Mode | null): Mode {
  if (aspect >= COLUMNS_FROM) return "columns";
  if (aspect < STACK_BELOW) return "stack";
  return prev ?? (aspect >= (COLUMNS_FROM + STACK_BELOW) / 2 ? "columns" : "stack");
}

// ---- ownership ----

export function latestOwners(delegations: Delegation[]) {
  const owners = new Map<string, string>();
  for (let i = delegations.length - 1; i >= 0; i--) {
    const delegation = delegations[i];
    if (!owners.has(delegation.toAgent)) owners.set(delegation.toAgent, delegation.fromAgent);
  }
  return owners;
}

/** The leaders (the first agent when there is none), the column / row workers and the Echo slot's agents. */
export function crew(agents: Agent[]) {
  const leaders = agents.filter((agent) => agent.kind === "leader");
  const effectiveLeaders = leaders.length ? leaders : agents.slice(0, 1);
  const rest = agents.filter((agent) => !effectiveLeaders.some((leader) => leader.id === agent.id));
  return { effectiveLeaders, workers: rest.filter((agent) => !isEcho(agent)), echoes: rest.filter(isEcho) };
}

export function ownershipKey(agents: Agent[], delegations: Delegation[]) {
  const { effectiveLeaders, workers } = crew(agents);
  const owners = latestOwners(delegations);
  return `${effectiveLeaders.map((leader) => leader.id).join(",")}|${workers.map((worker, index) => {
    const owner = effectiveLeaders.find((leader) => leader.id === owners.get(worker.id)) ?? effectiveLeaders[index % effectiveLeaders.length];
    return `${worker.id}:${owner?.id ?? ""}`;
  }).join(",")}`;
}

// ---- layout ----

const WORKER_W = 98;
const WORKER_H = 48;
/** A leader's full width, and its height as a plain chip (no Core XL). */
const LEADER_W = 132;
const LEADER_H = 76;
/** #277: a worker's height in its slot / row, and the big Core's in its cell (the rest: pins, balloon room). */
const CHIP_SHARE = 0.72;
const CORE_SHARE = 0.92;
/** Stack: the narrowest worker a row packs before it wraps, and the gap between row chips. */
const ROW_MIN_W = 84;
const ROW_GAP = 14;
/** Stack: a row's height cap and the least room between Core and the rows (trace bends). */
const ROW_MAX_H = 64;
const ROUTE_MIN = 22;
/** Columns: the bend room kept before workers widen to full size. */
const ROUTE_FLOOR = 36;
/** A small board keeps Core XL (body ≥ 48) before it gives the rows room. */
const CORE_MIN_H = 2 * XL_PIN + XL_LABEL + XL_MIN + 8;
/**
 * #1958: Echo's slot under its Core's labels (as wide as the Core column): its height at full size, the
 * least it shrinks to, and the gap from the Core target (Echo's provider badge sits above its slot).
 */
export const ECHO_H = 44;
/** A short Kart: Echo gives way to a two-row label first; below that only once Core is at its minimum. */
const ECHO_SOFT_H = 28;
const ECHO_MIN_H = 12;
export const ECHO_GAP = 16;

export function layout(agents: Agent[], delegations: Delegation[], w: number, h: number, mode: Mode, u = MIN_U) {
  const { effectiveLeaders, workers, echoes } = crew(agents);
  const owners = latestOwners(delegations);
  const groups = new Map(effectiveLeaders.map((leader) => [leader.id, [] as Agent[]]));
  workers.forEach((worker, index) => {
    const leader = effectiveLeaders.find((item) => item.id === owners.get(worker.id)) ?? effectiveLeaders[index % effectiveLeaders.length];
    groups.get(leader.id)!.push(worker);
  });
  const targets = new Map<string, Target>();
  if (!effectiveLeaders.length) return targets;

  const inset = Math.min(areaInset(u), w / 4, h / 4);
  const area = { x: inset, y: inset, w: Math.max(1, w - 2 * inset), h: Math.max(1, h - 2 * inset) };
  const columns = effectiveLeaders.length <= 4
    ? Math.ceil(Math.sqrt(effectiveLeaders.length))
    : effectiveLeaders.length <= 12 ? 3 : Math.ceil(Math.sqrt(effectiveLeaders.length));
  const placements: Array<{
    leader: Agent; left: Agent[]; right: Agent[]; echo: Agent[]; row: number; column: number; span: number;
  }> = [];
  let row = 0;
  let column = 0;
  for (const leader of effectiveLeaders) {
    const group = groups.get(leader.id) ?? [];
    const span = group.length >= 8 ? columns : group.length >= 4 ? Math.min(2, columns) : 1;
    if (column + span > columns) { row++; column = 0; }
    const cut = sideCut(group);
    // Stack: `left` = the rows above Core, `right` = below (same role-boundary cut).
    // Echo stays with the first Core (one assistant per team).
    placements.push({ leader, left: group.slice(0, cut), right: group.slice(cut), echo: placements.length ? [] : echoes, row, column, span });
    column += span;
    if (column === columns) { row++; column = 0; }
  }
  const rows = Math.max(1, ...placements.map((placement) => placement.row + 1));
  const minRowH = Math.min(48, area.h / rows);
  const rowNeeds = Array.from({ length: rows }, () => minRowH);
  for (const placement of placements) {
    const echo = placement.echo.length ? ECHO_GAP + ECHO_H : 0;
    const need = mode === "columns"
      ? Math.max(24 + Math.max(placement.left.length, placement.right.length) * 34, echo && 80 + echo)
      : 80 + echo + (placement.left.length + placement.right.length) * 30;
    rowNeeds[placement.row] = Math.max(rowNeeds[placement.row], need);
  }
  const extraBudget = Math.max(0, area.h - minRowH * rows);
  const weights = rowNeeds.map((need) => Math.max(0, need - minRowH));
  const weightTotal = weights.reduce((sum, weight) => sum + weight, 0);
  const rowHeights = rowNeeds.map((_, index) => weightTotal
    ? minRowH + extraBudget * weights[index] / weightTotal
    : area.h / rows);
  const rowTops = rowHeights.map((_, index) => area.y + rowHeights.slice(0, index).reduce((sum, height) => sum + height, 0));
  const unitW = area.w / columns;

  for (const placement of placements) {
    const cell = {
      x: area.x + placement.column * unitW,
      y: rowTops[placement.row],
      w: unitW * placement.span,
      h: rowHeights[placement.row],
    };
    if (mode === "stack") placeStack(targets, cell, placement.leader, placement.left, placement.right, placement.echo);
    else placeColumns(targets, cell, placement.leader, placement.left, placement.right, placement.echo);
  }
  return targets;
}

type Cell = { x: number; y: number; w: number; h: number };

/**
 * #1958: Echo's slot height in `room` (the leader's share of the cell, Core wanting `want`): Echo gives
 * way first (to a two-row label), then Core (to `coreMin`), then Echo again.
 */
function echoHeight(echo: Agent[], room: number, want: number, coreMin: number) {
  if (!echo.length) return 0;
  const left = room - ECHO_GAP;
  if (left - want >= ECHO_SOFT_H) return Math.min(ECHO_H, left - want);
  return Math.max(ECHO_MIN_H, Math.min(ECHO_SOFT_H, left - coreMin));
}

/** The gap over Echo: its full size unless Core at `leaderH` leaves less (a tiny Kart keeps Core XL). */
const echoGap = (echo: Agent[], room: number, leaderH: number, echoH: number) =>
  echo.length ? Math.max(2, Math.min(ECHO_GAP, room - leaderH - echoH)) : 0;

/** Echo's slot(s) `gap` under the leader target `core`, sharing the Core column's width. */
function placeEcho(targets: Map<string, Target>, echo: Agent[], core: Target, h: number, gap: number) {
  const w = core.w / Math.max(1, echo.length);
  const y = core.y + core.h / 2 + gap + h / 2;
  echo.forEach((agent, i) => targets.set(agent.id, { x: core.x - core.w / 2 + w * (i + 0.5), y, w, h }));
}

function placeColumns(targets: Map<string, Target>, full: Cell, leader: Agent, left: Agent[], right: Agent[], echo: Agent[] = []) {
  const fitW = Math.min(full.w, COLUMNS_MAX_W);
  const cell = { ...full, x: full.x + (full.w - fitW) / 2, w: fitW };
  const padX = PIN_CLEAR;
  const padY = PIN_CLEAR;
  const bothSides = left.length > 0 && right.length > 0;
  // #252: a leader is the big Core (pins + 120 body + labels, spec §5.1); its traces leave the body.
  const xl = leader.kind === "leader";
  const room = cell.h - padY * 2 - 6;
  const want = xl ? Math.min(XL_TARGET_H, Math.max(cell.h * CORE_SHARE, CORE_MIN_H), room) : Math.min(LEADER_H, cell.h * 0.4, room);
  const echoH = echoHeight(echo, room, want, xl ? CORE_MIN_H : 16);
  const leaderH = Math.max(16, Math.min(want, xl ? CORE_MIN_H : 16), Math.min(want, room - (echo.length ? ECHO_GAP + echoH : 0)));
  const gap = echoGap(echo, room, leaderH, echoH);
  const echoRoom = echo.length ? gap + echoH : 0;
  const body = xl ? coreBody({ x: 0, y: 0, w: LEADER_W, h: leaderH }) : null;
  // The Core + Echo block is centred: Core sits `echoRoom / 2` above the cell's middle.
  const span = body ? { h: body.s, y: body.y - echoRoom / 2 } : { h: leaderH, y: -echoRoom / 2 };
  const sideCount = Number(left.length > 0) + Number(right.length > 0);
  const desiredRouteGap = Math.max(sideRouteGap(left.length, cell.h, padY, span.h, span.y), sideRouteGap(right.length, cell.h, padY, span.h, span.y));
  // #270: a narrow cell keeps its workers ≥ the detailed chip's width before it widens the bends.
  const minLeaderW = xl ? XL_MIN + 2 * XL_PIN + 8 : 28;
  const maxRouteGap = sideCount
    ? Math.min(cell.w - padX * 2 - 36 - sideCount * 8, cell.w - padX * 2 - minLeaderW - sideCount * 52) / sideCount
    : 0;
  const desiredLeaderW = Math.min(LEADER_W, Math.max(cell.w * (bothSides ? 0.34 : 0.42), xl ? minLeaderW : 0));
  // #277: past ROUTE_FLOOR the bends give way to full-width workers (a tall cell's desired gap is huge).
  const fullWorkersGap = sideCount ? (cell.w - padX * 2 - desiredLeaderW) / sideCount - WORKER_W : 0;
  const routeGap = sideCount ? Math.max(4, Math.min(desiredRouteGap, maxRouteGap, Math.max(fullWorkersGap, ROUTE_FLOOR))) : 0;
  const maxLeaderW = cell.w - padX * 2 - sideCount * (routeGap + 8);
  const leaderW = Math.max(28, Math.min(desiredLeaderW, maxLeaderW));
  const availableWorkerW = sideCount
    ? (cell.w - padX * 2 - leaderW - sideCount * routeGap) / sideCount
    : WORKER_W;
  const workerW = Math.max(8, Math.min(WORKER_W, availableWorkerW));
  let leaderX = cell.x + cell.w / 2;
  if (left.length && !right.length) leaderX = cell.x + cell.w - padX - leaderW / 2;
  else if (right.length && !left.length) leaderX = cell.x + padX + leaderW / 2;
  const cy = cell.y + cell.h / 2 - echoRoom / 2;
  const core: Target = { x: leaderX, y: cy, w: leaderW, h: leaderH, mode: "columns" };
  targets.set(leader.id, core);
  placeEcho(targets, echo, core, echoH, gap);

  const placeSide = (list: Agent[], side: "left" | "right") => list.forEach((agent, index) => {
    const slotH = (cell.h - padY * 2) / list.length;
    const workerH = Math.max(2, Math.min(WORKER_H, slotH * CHIP_SHARE));
    const x = side === "left"
      ? cell.x + padX + workerW / 2
      : cell.x + cell.w - padX - workerW / 2;
    targets.set(agent.id, { x, y: cell.y + padY + slotH * (index + 0.5), w: workerW, h: workerH });
  });
  placeSide(left, "left");
  placeSide(right, "right");
}

/** Stack rows lower than this do not fit: the cell falls back to columns (tiny, short panels). */
const ROW_MIN_H = 22;
/** …and a crowded row's chips get no narrower than this. */
const ROW_TIGHT_W = 40;

/** #270: square / tall cell — workers in rows above (`top`) and below (`bottom`) a centred Core. */
function placeStack(targets: Map<string, Target>, cell: Cell, leader: Agent, top: Agent[], bottom: Agent[], echo: Agent[] = []) {
  const padX = PIN_CLEAR;
  const padY = PIN_CLEAR;
  const innerW = Math.max(1, cell.w - padX * 2);
  const cellH = Math.max(1, cell.h - padY * 2);
  const xl = leader.kind === "leader";
  // #1958: Echo's slot is taken off the top: Core and the rows share what is left.
  const echoH = echoHeight(echo, cellH - 6, xl ? XL_TARGET_H : LEADER_H, xl ? CORE_MIN_H : 16);
  const gap = echoGap(echo, cellH - 6, xl ? CORE_MIN_H : 16, echoH);
  const echoRoom = echo.length ? gap + echoH : 0;
  const innerH = Math.max(1, cellH - echoRoom);
  const leaderW = Math.max(28, Math.min(LEADER_W, innerW));
  // Rows as wide as the cell packs them; too low for the height → more, narrower chips per row.
  const fit = (perRow: number) => {
    const topRows = Math.ceil(top.length / perRow);
    const bottomRows = Math.ceil(bottom.length / perRow);
    const rowCount = topRows + bottomRows;
    // Core takes what the rows leave, ≤ its full height (and never more than ~half the cell with rows).
    const rowFloor = rowCount * 28 + (topRows ? ROUTE_MIN : 0) + (bottomRows ? ROUTE_MIN : 0);
    const leaderH = Math.max(16, Math.min(xl ? CORE_MIN_H : 16, innerH - 6), Math.min(
      xl ? XL_TARGET_H : LEADER_H,
      rowCount ? Math.max(innerH * 0.5, innerH - rowFloor) : innerH,
      innerH - 6,
    ));
    const gaps = Number(topRows > 0) + Number(bottomRows > 0);
    const free = Math.max(0, innerH - leaderH - gaps * ROUTE_MIN);
    const rowH = rowCount ? Math.min(ROW_MAX_H, free / rowCount) : 0;
    return { perRow, topRows, rowCount, leaderH, gaps, free, rowH };
  };
  const most = Math.max(1, top.length, bottom.length);
  let plan = fit(Math.min(most, Math.max(1, Math.floor((innerW + ROW_GAP) / (ROW_MIN_W + ROW_GAP)))));
  while (plan.rowCount && plan.rowH < ROW_MIN_H && plan.perRow < most && innerW / (plan.perRow + 1) - ROW_GAP >= ROW_TIGHT_W) {
    plan = fit(plan.perRow + 1);
  }
  const { perRow, topRows, rowCount, leaderH, gaps, free, rowH } = plan;
  if (rowCount && rowH < ROW_MIN_H) return placeColumns(targets, cell, leader, top, bottom, echo);
  // What the capped rows leave widens the bend room (#277: all of it — the rows reach the board edges).
  const route = gaps ? ROUTE_MIN + Math.max(0, free - rowH * rowCount) / gaps : 0;
  const blockH = rowH * rowCount + leaderH + echoRoom + route * gaps;
  let y = cell.y + (cell.h - blockH) / 2;
  const cx = cell.x + cell.w / 2;
  const workerH = Math.max(2, Math.min(WORKER_H, rowH * CHIP_SHARE));

  const placeRows = (list: Agent[]) => {
    for (let r = 0; r * perRow < list.length; r++) {
      const items = list.slice(r * perRow, (r + 1) * perRow);
      const slotW = innerW / items.length;
      const workerW = Math.max(8, Math.min(WORKER_W, slotW - ROW_GAP));
      items.forEach((agent, i) => {
        targets.set(agent.id, { x: cell.x + padX + slotW * (i + 0.5), y: y + rowH * (r + 0.5), w: workerW, h: workerH });
      });
    }
  };
  placeRows(top);
  y += rowH * topRows + (topRows ? route : 0);
  const core: Target = { x: cx, y: y + leaderH / 2, w: leaderW, h: leaderH, mode: "stack" };
  targets.set(leader.id, core);
  placeEcho(targets, echo, core, echoH, gap);
  y += leaderH + echoRoom + (rowCount > topRows ? route : 0);
  placeRows(bottom);
}

/**
 * Görev #81: workers arrive role-grouped (src/team/order.ts); the left column takes the first run and the
 * right the rest, cut at the role boundary nearest the middle so a role stays in one column — unless that
 * leaves the columns more than one chip off the even split.
 */
export function sideCut(group: Agent[]) {
  const half = Math.ceil(group.length / 2);
  let best = half;
  let bestOff = Infinity;
  for (let i = 0; i <= group.length; i++) {
    if (i > 0 && i < group.length && group[i - 1].roleName === group[i].roleName) continue;
    const off = Math.abs(2 * i - group.length);
    if (off <= bestOff) { best = i; bestOff = off; }
  }
  return Math.max(best, group.length - best) <= half + 1 ? best : half;
}

/** `spanH`/`spanY`: the side edge the traces leave (#252: the big Core's body, above the cell centre). */
function sideRouteGap(count: number, cellH: number, padY: number, spanH: number, spanY = 0) {
  if (!count) return 0;
  const slotH = (cellH - padY * 2) / count;
  const spread = Math.min(Math.max(0, spanH - 18), Math.max(0, (count - 1) * 9));
  let maxDelta = 0;
  for (let index = 0; index < count; index++) {
    const workerY = -cellH / 2 + padY + slotH * (index + 0.5);
    const startY = spanY - spread / 2 + index * (count > 1 ? spread / (count - 1) : 0);
    maxDelta = Math.max(maxDelta, Math.abs(workerY - startY));
  }
  return maxDelta + 4;
}

// ---- traces ----

export type Box = { id: string; x: number; y: number; w: number; h: number };

/** The labels under a big Core (name / role + cost): bottom traces keep out of this band. */
export function labelBand(leader: { x: number; w: number }) {
  return (leader.w + 40) / 2 + 6;
}

/**
 * Routes from a leader (its layout target, `mode` from the layout) to its workers, keyed by worker id.
 * Columns: from the body's side edges (as before). Stack: the rows above leave the body's top edge; the
 * rows below leave its side edges and go down around the labels.
 */
export function routes(leader: Box & { mode?: Mode }, workers: Box[]): Map<string, Point[]> {
  const out = new Map<string, Point[]>();
  const body = coreBody(leader);
  if (leader.mode === "stack") {
    const above = workers.filter((w) => w.y < leader.y).sort((a, b) => a.x - b.x);
    above.forEach((worker, index) => {
      const start = topPort(leader, body, index, above.length);
      out.set(worker.id, tracePointsV(start, { x: worker.x, y: worker.y + worker.h / 2 }));
    });
    const below = workers.filter((w) => w.y >= leader.y);
    for (const dir of [-1, 1] as const) {
      const list = below.filter((w) => (w.x < leader.x ? -1 : 1) === dir);
      const band = labelBand(leader);
      const inBand = list.filter((w) => Math.abs(w.x - leader.x) < band + 12).sort((a, b) => Math.abs(a.x - leader.x) - Math.abs(b.x - leader.x));
      // Down-going x per worker: outside the band its own x; inside, the innermost worker takes the
      // outermost lane (and the lowest channel), so the nested routes never cross.
      const lane = new Map(list.map((w) => [w.id, w.x]));
      inBand.forEach((w, j) => lane.set(w.id, leader.x + dir * (band + 6 * (inBand.length - 1 - j))));
      // Outer lanes take the upper ports.
      const ordered = [...list].sort((a, b) => Math.abs(lane.get(b.id)! - leader.x) - Math.abs(lane.get(a.id)! - leader.x));
      ordered.forEach((worker, index) => {
        const start = body ? corePort(body, dir, index, ordered.length) : sidePort(leader, dir, index, ordered.length);
        const end = { x: worker.x, y: worker.y - worker.h / 2 };
        const j = inBand.indexOf(worker);
        out.set(worker.id, j < 0 ? downL(start, end, dir) : [
          start,
          { x: lane.get(worker.id)!, y: start.y },
          { x: lane.get(worker.id)!, y: end.y - 8 - 6 * j },
          { x: end.x, y: end.y - 8 - 6 * j },
          end,
        ]);
      });
    }
    return out;
  }
  const sides: Record<"left" | "right", Box[]> = { left: [], right: [] };
  for (const worker of workers) sides[worker.x < leader.x ? "left" : "right"].push(worker);
  for (const side of ["left", "right"] as const) {
    const list = sides[side].sort((a, b) => a.y - b.y);
    const dir = side === "left" ? -1 : 1;
    list.forEach((worker, index) => {
      const start = body ? corePort(body, dir, index, list.length) : sidePort(leader, dir, index, list.length);
      const end = { x: worker.x - dir * worker.w / 2, y: worker.y };
      // Routes snap to the new layout while chips animate there, so a reassignment
      // cannot drag a trace across neighbouring cells.
      out.set(worker.id, tracePoints(start, end, dir));
    });
  }
  return out;
}

function sidePort(leader: Box, dir: -1 | 1, index: number, count: number): Point {
  const spread = Math.min(Math.max(0, leader.h - 18), Math.max(0, (count - 1) * 9));
  return { x: leader.x + dir * leader.w / 2, y: leader.y - spread / 2 + index * (count > 1 ? spread / (count - 1) : 0) };
}

/** #270: a port on the top edge (the big Core's body, else the chip), spread around its centre. */
function topPort(leader: Box, body: Body | null, index: number, count: number): Point {
  const s = body ? body.s : leader.w;
  const spread = Math.min(Math.max(0, s - 18), Math.max(0, (count - 1) * 12));
  const x = leader.x - spread / 2 + index * (count > 1 ? spread / (count - 1) : 0);
  return { x, y: body ? body.y - body.s / 2 - XL_PIN : leader.y - leader.h / 2 };
}

/** Octilinear route: the middle segment is 45deg; very dense fallbacks stay orthogonal. */
export function tracePoints(start: Point, end: Point, dir: -1 | 1): Point[] {
  const gap = Math.max(0, (end.x - start.x) * dir);
  const vertical = Math.abs(end.y - start.y);
  if (vertical <= gap) {
    const horizontal = (gap - vertical) / 2;
    return compactPoints([
      start,
      { x: start.x + dir * horizontal, y: start.y },
      { x: start.x + dir * (horizontal + vertical), y: end.y },
      end,
    ]);
  }
  const middleX = start.x + dir * gap / 2;
  return compactPoints([start, { x: middleX, y: start.y }, { x: middleX, y: end.y }, end]);
}

/** #270: `tracePoints` turned 90°: leaves vertically, 45° in the middle, enters vertically. */
export function tracePointsV(start: Point, end: Point): Point[] {
  const flip = (p: Point) => ({ x: p.y, y: p.x });
  return tracePoints(flip(start), flip(end), end.y < start.y ? -1 : 1).map(flip);
}

/** #270: out of the side, a 45° turn, down into the worker's top. */
function downL(start: Point, end: Point, dir: -1 | 1): Point[] {
  const dx = Math.max(0, (end.x - start.x) * dir);
  const d = Math.min(14, dx, Math.max(0, end.y - start.y));
  return compactPoints([start, { x: end.x - dir * d, y: start.y }, { x: end.x, y: start.y + d }, end]);
}

export function compactPoints(points: Point[]) {
  return points.filter((point, index) => index === 0 || point.x !== points[index - 1].x || point.y !== points[index - 1].y);
}
