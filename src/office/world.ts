import type { Agent, AgentStatus } from "../agents";
import type { Delegation } from "../delegations";
import type { Question } from "../questions";
import { GLYPHS } from "./glyphs";
import { drawCoreXL, drawCoreFragments, drawCoreFeet, type CorePalette } from "./companion/coreEngine";
import { LOOKS, type Step } from "./companion/coreState";
import { CoreXL, coreBody, XL_PIN, type Body, type Pointer } from "./companion/coreXL";
import {
  BOARD_PAD_PX, areaInset, boardRect, fitUnit, HOLE_AT_PX, HOLE_R_PX, LABEL_PX, LABELS_FROM_PX, layout, ownershipKey, latestOwners, pickMode,
  routes, type Mode, type Point,
} from "./placement";
import { PCB_TILE_PX, workerSlices } from "./sprites";
import { pcbImage, workerImage } from "./assets";
import { chipNameStyle, coreLabel, OFFLINE_NAME_ALPHA, teamSignature } from "./label";
import { DelegationTouches, TOUCH_MS, reachAt, tentacleContour, fragmentPaths, plantedFeet, type FragmentBox } from "./symbiote";
import { drawCoreName, ellipsis, roundRect } from "./coreName";

/** The world before Office measures its panel (#270: then the panel in world units). */
export const W = 960;
export const H = 540;

export interface PcbTheme {
  outside: string; board: string; boardEdge: string; grid: string; trace: string; traceHot: string;
  chip: string; chipOff: string; chipEdge: string; pin: string; via: string; text: string; muted: string;
  faint: string; red: string; redHi: string; redGlow: string; green: string; amber: string;
  /** Modern §7: the team accent — traces, the Core / leader ring. Red stays for status and errors. */
  team: string; teamHi: string; teamGlow: string;
  provider: string; font: string; mono: string; radius: number;
}

export interface WorldData {
  status: (id: string) => AgentStatus;
  delegations: () => Delegation[];
  active: (delegation: Delegation) => boolean;
  result: (agentId: string) => Delegation | null;
  question: (agentId: string) => Question | null;
  stalled: (id: string) => number | null;
  /** FIX-4: the first reply is late (FIX-3 watchdog). */
  slow: (id: string) => boolean;
  start: (id: string) => boolean;
  /** #209: today's token + $ for the chip ("" = none / old dexd). */
  cost: (id: string) => string;
  /** #252 (Core XL, same inputs as the strip): this leader's member proposals. */
  proposals: (leaderId: string) => readonly { key: string; status: string }[];
  /** Escalated questions of this leader (patron decisions). */
  escalated: (leaderId: string) => number;
  /** Worker questions waiting on this leader. */
  workerAsks: (leaderId: string) => number;
  /** The leader's running tools (`agent.step`). */
  steps: (id: string) => readonly Step[];
  /** Agent/provider failure text (the pane alert). */
  agentError: (id: string) => string | null;
}

/** #252: where Office puts the balloon / hover card. `body` = the big Core's square (leaders only). */
export interface Anchor { agent: Agent; x: number; y: number; top: number; body: Body | null }

interface Chip {
  agent: Agent;
  x: number; y: number; tx: number; ty: number; w: number; h: number; boot: number;
  /** #270: a leader's layout mode (its traces follow it). */
  mode?: Mode;
}

interface Trace { leaderId: string; workerId: string; points: Point[] }

const MOVE_SPEED = 8;
const BOOT_SECONDS = 0.7;

export class World {
  chips: Chip[] = [];
  time = 0;
  w = W;
  h = H;
  /** #277: CSS px per world unit (Office reads it for the view); 1 until Office measures the panel. */
  unit = 1;
  private cssW = 0;
  private cssH = 0;
  private traces: Trace[] = [];
  private ownership = "";
  private readonly delegationTouches = new DelegationTouches();
  private touches: { leader: string; worker: string; at: number }[] = [];
  /** #270: columns / stack, with hysteresis across resizes. */
  private mode: Mode = pickMode(W / H, null);
  /** #252: the big Core of each leader (state, pupils, blink), kept across frames. */
  private cores = new Map<string, CoreXL>();
  /** #252: the cursor in world units (Office feeds it); null = never seen. */
  private pointer: Pointer | null = null;
  hoverId: string | null = null;
  /** prefers-reduced-motion (Office keeps it current). */
  reduced = false;
  /** #411: the team's name (Office keeps it current) — the leaders' "Ana Core" label and the board's signature. */
  teamName = "";

  constructor(agents: Agent[], private data: WorldData) {
    this.sync(agents);
  }

  /** Re-layout keeps chip objects and animation clocks; only targets change. */
  sync(agents: Agent[]) {
    const previous = new Map(this.chips.map((chip) => [chip.agent.id, chip]));
    const delegations = this.data.delegations();
    this.fitScale(agents, delegations);
    const targets = layout(agents, delegations, this.w, this.h, this.mode, this.unit);
    this.chips = agents.map((agent) => {
      const target = targets.get(agent.id)!;
      const old = previous.get(agent.id);
      if (old) {
        Object.assign(old, { agent, tx: target.x, ty: target.y, w: target.w, h: target.h, mode: target.mode });
        return old;
      }
      return { agent, x: target.x, y: target.y, tx: target.x, ty: target.y, w: target.w, h: target.h, mode: target.mode, boot: 0 };
    });
    this.ownership = ownershipKey(agents, delegations);
    this.rebuildTraces(delegations);
  }

  /**
   * #270: the panel changed size (CSS px). Chips snap to the new layout — a resize drag redraws
   * every frame, so following it is already smooth and a lagging chip would tear off its trace.
   */
  resize(cssW: number, cssH: number) {
    if (Math.abs(cssW - this.cssW) < 0.01 && Math.abs(cssH - this.cssH) < 0.01) return;
    this.cssW = cssW;
    this.cssH = cssH;
    this.mode = pickMode(cssW / cssH, this.mode);
    const delegations = this.data.delegations();
    const agents = this.chips.map((chip) => chip.agent);
    this.fitScale(agents, delegations);
    const targets = layout(agents, delegations, this.w, this.h, this.mode, this.unit);
    for (const chip of this.chips) {
      const t = targets.get(chip.agent.id)!;
      Object.assign(chip, { x: t.x, y: t.y, tx: t.x, ty: t.y, w: t.w, h: t.h, mode: t.mode });
    }
    this.rebuildTraces(delegations);
  }

  /** #277: the unit that fills the panel with this team at full chip size; the world is the panel in it. */
  private fitScale(agents: Agent[], delegations: Delegation[]) {
    if (this.cssW <= 0 || this.cssH <= 0) return;
    this.unit = fitUnit(this.cssW, this.cssH, agents, delegations, this.mode);
    this.w = this.cssW / this.unit;
    this.h = this.cssH / this.unit;
  }

  update(dt: number) {
    const delegations = this.data.delegations();
    this.refreshLayout(delegations);
    this.time += dt;
    let moved = false;
    for (const chip of this.chips) {
      const k = 1 - Math.exp(-MOVE_SPEED * dt);
      if (Math.abs(chip.tx - chip.x) > 0.05 || Math.abs(chip.ty - chip.y) > 0.05) {
        chip.x += (chip.tx - chip.x) * k;
        chip.y += (chip.ty - chip.y) * k;
        moved = true;
      } else {
        chip.x = chip.tx;
        chip.y = chip.ty;
      }
      chip.boot = Math.max(0, chip.boot - dt);
    }
    if (moved) this.rebuildTraces(delegations);
    for (const contact of this.delegationTouches.observe(delegations)) this.touch(contact.worker, contact.leader);
    this.touches = this.touches.filter(t => this.time * 1000 - t.at < TOUCH_MS);
    this.stepCores(dt, delegations);
  }

  /** #252: the cursor moved over the canvas (world units). */
  pointAt(x: number, y: number) {
    this.pointer = { x, y, inside: true, inWindow: true, at: this.time * 1000 };
  }

  /** #252: the cursor left the canvas (`window`: left the window too — the eyes start home at once). */
  pointerOut(window: boolean) {
    if (!this.pointer) return;
    this.pointer = { ...this.pointer, inside: false, inWindow: this.pointer.inWindow && !window, at: this.time * 1000 };
  }

  /** #252: a decision sent from the office balloon failed: that leader's Core shows `!`. */
  coreFail(leaderId: string) {
    this.cores.get(leaderId)?.tracker.fail(Date.now());
  }

  /** The big Core's square of a leader chip at its current position (null: not a leader / too small). */
  private bodyOf(chip: Chip): Body | null {
    return chip.agent.kind === "leader" ? coreBody(chip) : null;
  }

  private stepCores(dt: number, delegations: Delegation[]) {
    const now = Date.now();
    const anim = this.time * 1000;
    const live = new Set<string>();
    for (const chip of this.chips) {
      const body = this.bodyOf(chip);
      if (!body) continue;
      const id = chip.agent.id;
      live.add(id);
      let core = this.cores.get(id);
      if (!core) this.cores.set(id, (core = new CoreXL()));
      core.step({
        snap: {
          status: this.data.status(id),
          jobs: delegations.filter((d) => d.fromAgent === id),
          proposals: this.data.proposals(id),
          escalated: this.data.escalated(id),
          workerAsks: this.data.workerAsks(id),
          steps: this.data.steps(id),
          agentError: this.data.agentError(id),
        },
        now, anim, dt, body,
        pointer: this.pointer,
        workerAt: (worker) => {
          const target = this.chips.find((item) => item.agent.id === worker);
          return target ? { x: target.x, y: target.y } : null;
        },
        hovered: this.hoverId === id,
        reduced: this.reduced,
      });
    }
    for (const id of this.cores.keys()) if (!live.has(id)) this.cores.delete(id);
  }

  /** Offline chip click: sweep its pins, then ask the existing pane bridge to start it. */
  activate(agentId: string): boolean {
    const chip = this.chips.find((item) => item.agent.id === agentId);
    if (chip?.agent.kind === "worker") this.touch(agentId);
    if (!chip || this.data.status(agentId) !== "offline") return false;
    chip.boot = BOOT_SECONDS;
    window.setTimeout(() => this.data.start(agentId), 260);
    return true;
  }

  hit(x: number, y: number): Agent | null {
    const chip = [...this.chips].reverse().find((item) =>
      x >= item.x - item.w / 2 && x <= item.x + item.w / 2 && y >= item.y - item.h / 2 && y <= item.y + item.h / 2,
    );
    return chip?.agent ?? null;
  }

  isAnimated() {
    const delegations = this.data.delegations();
    this.refreshLayout(delegations);
    // #252 spec §5.3: the big Core always moves a little (breath, pupils) unless motion is reduced.
    if (!this.reduced && this.chips.some((chip) => this.bodyOf(chip))) return true;
    return (!this.reduced && this.touches.length > 0) || this.chips.some((chip) => {
      const status = this.data.status(chip.agent.id);
      return chip.boot > 0 || status === "working" || status === "waiting" ||
        Math.abs(chip.tx - chip.x) > 0.05 || Math.abs(chip.ty - chip.y) > 0.05 ||
        this.data.result(chip.agent.id) !== null || this.data.question(chip.agent.id) !== null;
    }) || delegations.some(this.data.active);
  }

  needsAnimation() { return this.isAnimated(); }

  /** A single contact, also used for offline worker clicks. Repeated clicks restart it. */
  private touch(worker: string, leader?: string) {
    if (this.reduced) return;
    const trace = this.traces.find(t => t.workerId === worker && (!leader || t.leaderId === leader));
    if (!trace) return;
    this.touches = this.touches.filter(t => t.worker !== worker);
    this.touches.push({ leader: trace.leaderId, worker, at: this.time * 1000 });
  }

  draw(ctx: CanvasRenderingContext2D, focusedId: string | null, palette: PcbTheme, core: CorePalette): Anchor[] {
    const delegations = this.data.delegations();
    this.refreshLayout(delegations);
    this.rebuildTraces(delegations);
    drawBoard(ctx, palette, this.w, this.h, this.unit, teamSignature(this.teamName));
    this.drawTraces(ctx, palette);
    this.drawFragments(ctx, palette, core);
    return this.chips.map((chip) => {
      const body = this.bodyOf(chip);
      if (body) this.drawCore(ctx, chip, body, palette, core);
      else this.drawChip(ctx, chip, chip.agent.id === focusedId, palette);
      return { agent: chip.agent, x: chip.x, y: chip.y, top: chip.h / 2, body };
    });
  }

  /** All fragments precede all chips and text; protected plates also constrain their paths. */
  private drawFragments(ctx: CanvasRenderingContext2D, p: PcbTheme, pal: CorePalette) {
    const inset = areaInset(this.unit);
    const board = { x: inset, y: inset, w: this.w - 2 * inset, h: this.h - 2 * inset };
    for (const chip of this.chips) {
      const body = this.bodyOf(chip), core = this.cores.get(chip.agent.id);
      if (!body || !core) continue;
      const blocks: FragmentBox[] = this.chips.filter(c => c !== chip).map(c => ({ x: c.x - c.w / 2 - 6, y: c.y - c.h / 2 - 12, w: c.w + 12, h: c.h + 18 }));
      // The provider badge is not a block: upper traces leave through it, and pieces drawn first slip under it.
      blocks.push({ x: body.x - (chip.w + 40) / 2, y: body.y + body.s / 2 + XL_PIN, w: chip.w + 40, h: 40 });
      const paths = fragmentPaths(body, this.traces.filter(t => t.leaderId === chip.agent.id).map(t => t.points), blocks, board);
      drawCoreFragments(ctx, body, core.fragments.sample(paths, body, this.reduced), core.fragments.phase, pal, p.chipEdge);
      drawCoreFeet(ctx, body, plantedFeet(paths, body, this.time * 1000, this.reduced || core.state === "sleep"), pal, p.chipEdge);
    }
  }

  /** Delegation ownership changes move existing chips; boot and packet clocks stay intact. */
  private refreshLayout(delegations: Delegation[]) {
    const agents = this.chips.map((chip) => chip.agent);
    const ownership = ownershipKey(agents, delegations);
    if (ownership === this.ownership) return;
    this.fitScale(agents, delegations);
    const targets = layout(agents, delegations, this.w, this.h, this.mode, this.unit);
    for (const chip of this.chips) {
      const target = targets.get(chip.agent.id)!;
      const moved = chip.tx !== target.x || chip.ty !== target.y;
      Object.assign(chip, { tx: target.x, ty: target.y, w: target.w, h: target.h, mode: target.mode });
      // Ownership-only relayout snaps affected chips so their target-routed trace
      // never detaches from the moving body or crosses an intermediate cell.
      if (moved) Object.assign(chip, { x: target.x, y: target.y });
    }
    this.ownership = ownership;
    // The new owner's contact is observed in this update, before the next draw.
    this.rebuildTraces(delegations);
  }

  private rebuildTraces(delegations: Delegation[]) {
    const leaders = this.chips.filter((chip) => chip.agent.kind === "leader");
    if (!leaders.length) { this.traces = []; return; }
    const owners = latestOwners(delegations);
    const groups = new Map(leaders.map((leader) => [leader.agent.id, [] as Chip[]]));
    this.chips.filter((chip) => chip.agent.kind === "worker").forEach((worker, index) => {
      const leader = leaders.find((item) => item.agent.id === owners.get(worker.agent.id)) ?? leaders[index % leaders.length];
      groups.get(leader.agent.id)!.push(worker);
    });
    this.traces = [];
    for (const leader of leaders) {
      const box = (chip: Chip) => ({ id: chip.agent.id, x: chip.tx, y: chip.ty, w: chip.w, h: chip.h });
      const paths = routes({ ...box(leader), mode: leader.mode }, (groups.get(leader.agent.id) ?? []).map(box));
      for (const [workerId, points] of paths) this.traces.push({ leaderId: leader.agent.id, workerId, points });
    }
  }

  private drawTraces(ctx: CanvasRenderingContext2D, p: PcbTheme) {
    const delegations = this.data.delegations();
    ctx.lineWidth = 1.5;
    ctx.lineCap = "square";
    ctx.lineJoin = "bevel";
    for (const trace of this.traces) {
      const active = delegations.find((d) => d.fromAgent === trace.leaderId && d.toAgent === trace.workerId && this.data.active(d));
      makePath(ctx, trace.points);
      ctx.strokeStyle = active ? p.traceHot : p.trace;
      ctx.stroke();
      trace.points.slice(1, -1).forEach((point) => drawVia(ctx, point.x, point.y, p));

      const question = this.data.question(trace.workerId);
      const result = this.data.result(trace.workerId);
      const contact = this.touches.find(t => t.worker === trace.workerId && t.leader === trace.leaderId);
      if (contact && !this.reduced) {
        const age = this.time * 1000 - contact.at;
        const leader = this.chips.find(c => c.agent.id === trace.leaderId);
        const body = leader && this.bodyOf(leader);
        const route = body ? [{ x: body.x, y: body.y }, ...trace.points] : trace.points;
        const contour = tentacleContour(route, reachAt(age), age);
        if (contour.length) {
          ctx.save();
          makePath(ctx, contour); ctx.closePath();
          ctx.fillStyle = "#030405"; ctx.fill();
          ctx.strokeStyle = p.red; ctx.lineWidth = 0.8; ctx.stroke();
          ctx.restore();
        }
      }
      if (question) drawPacket(ctx, [...trace.points].reverse(), (this.time * 0.24 + question.id * 0.11) % 1, p.amber, 3.2, p.mono, "?");
      else if (result) drawPacket(ctx, [...trace.points].reverse(), (this.time * 0.28 + result.id * 0.07) % 1,
        result.status === "done" ? p.green : p.amber, 3.4, p.mono);
    }
  }

  /** #252: a leader as the big Core (spec §2–§5): body, then name + role (+ cost pill) under it. */
  private drawCore(ctx: CanvasRenderingContext2D, chip: Chip, body: Body, p: PcbTheme, pal: CorePalette) {
    const core = this.cores.get(chip.agent.id);
    const look = LOOKS[core?.state ?? "idle"];
    const anim = this.time * 1000;
    const prev = core?.tracker.prev;
    ctx.save();
    if (chip.boot > 0) {
      ctx.shadowColor = p.teamGlow;
      ctx.shadowBlur = 22 + (Math.sin(this.time * 2.5) + 1) * 8;
    }
    const frame = {
      t: anim,
      age: core ? anim - core.tracker.since : Infinity,
      from: prev ? pal.tokens[LOOKS[prev].token] ?? null : null,
      pupil: core?.pupil ?? { x: 0, y: 0 },
      lid: core?.lidNow ?? 0,
      hover: core?.hover ?? 0,
      reduced: this.reduced,
      provider: providerOf(chip.agent, p),
      boot: chip.boot > 0 ? 1 - chip.boot / BOOT_SECONDS : 0,
      boardBg: p.board,
      edge: p.chipEdge,
      mono: p.mono,
      weight: core?.weight,
    };
    drawCoreXL(ctx, body.x, body.y, body.s, look, frame, pal);
    ctx.restore();

    // name + role under the body (spec §5.4); the cost pill beside the role
    const bottom = body.y + body.s / 2 + XL_PIN;
    const nameY = bottom + 10;
    const roleY = nameY + 15;
    const maxW = Math.max(40, chip.w + 40);
    ctx.save();
    // #451: offline only dims — the team-coloured name keeps ≥4.5:1.
    if (this.data.status(chip.agent.id) === "offline" && chip.boot <= 0) ctx.globalAlpha *= OFFLINE_NAME_ALPHA;
    // #411: "Ana Core" in the team colour (--team-hi: ≥4.5:1 on the board for every accent); #430: over the plate.
    drawCoreName(ctx, {
      x: chip.x, nameY, roleY, maxW,
      name: this.nameOf(chip),
      role: (chip.agent.roleName || chip.agent.title || "").toLocaleUpperCase("tr-TR"),
      cost: this.data.cost(chip.agent.id),
      plate: pcbImage() ? p.board : null,
      nameColor: p.teamHi, roleColor: p.muted, font: p.font, mono: p.mono,
    }, (text, minX, maxX, cy) => drawTokenPill(ctx, text, minX, maxX, cy, p));
    ctx.restore();

    const box = { ...chip, x: body.x, y: body.y, w: body.s, h: body.s };
    const stalled = this.data.stalled(chip.agent.id);
    if (stalled !== null) drawClock(ctx, box, p.amber);
    if (this.data.slow(chip.agent.id)) drawGlass(ctx, box, p.amber, stalled !== null);
  }

  /** #411: a leader shows as "Ana Core"; workers keep their names. */
  private nameOf(chip: Chip) {
    return chip.agent.kind === "leader" ? coreLabel(this.teamName, chip.agent.name) : chip.agent.name;
  }

  private drawChip(ctx: CanvasRenderingContext2D, chip: Chip, focused: boolean, p: PcbTheme) {
    const status = this.data.status(chip.agent.id);
    const result = this.data.result(chip.agent.id);
    const question = this.data.question(chip.agent.id);
    const stalled = this.data.stalled(chip.agent.id);
    const offline = status === "offline" && chip.boot <= 0;
    const leader = chip.agent.kind === "leader";
    const look = chipLook(status, result, question, p);
    const x = chip.x - chip.w / 2;
    const y = chip.y - chip.h / 2;
    const detailed = chip.h >= 28 && chip.w >= 52;
    const labelled = chip.h >= 16 && chip.w >= 30;
    const art = !leader && detailed ? workerArt(chip) : null;

    ctx.save();
    if (status === "working" || chip.boot > 0) {
      const pulse = 0.16 + (Math.sin(this.time * 2.5) + 1) * 0.09;
      ctx.shadowColor = p.redGlow;
      ctx.shadowBlur = leader ? 22 + pulse * 22 : 14 + pulse * 18;
    }
    ctx.globalAlpha = offline ? 0.55 : 1;
    const radius = leader ? p.radius * 0.75 : p.radius * 0.5;
    if (art) {
      // #401: worker.png (3-slice: body = chip box, pins outside), then a solid plate under the live labels.
      for (const { src, dst } of art.slices) ctx.drawImage(art.img, src.x, src.y, src.w, src.h, dst.x, dst.y, dst.w, dst.h);
      ctx.shadowBlur = 0;
      roundRect(ctx, x + 3, y + 3, chip.w - 6, chip.h - 6, radius * 0.7);
      ctx.save(); ctx.globalAlpha *= PLATE_ALPHA; ctx.fillStyle = offline ? p.chipOff : p.chip; ctx.fill(); ctx.restore();
    }
    roundRect(ctx, x, y, chip.w, chip.h, radius);
    if (!art) {
      ctx.fillStyle = offline ? p.chipOff : p.chip;
      ctx.fill();
    }
    ctx.shadowBlur = 0;
    // #229: status ring; at rest the leader keeps a dim team-coloured ring (Modern §7), the error ring is dashed.
    ctx.save();
    if (!look.ring && !focused && leader && !offline) ctx.globalAlpha *= 0.55;
    ctx.strokeStyle = look.ring ?? (focused || (leader && !offline) ? p.team : p.chipEdge);
    ctx.lineWidth = focused || look.ring ? 1.8 : 1;
    if (look.error) ctx.setLineDash([4, 3]);
    // #401: at rest the artwork's own rim is the edge; status and focus rings stay.
    if (!art || focused || look.ring) ctx.stroke();
    ctx.restore();

    // The artwork has its pins; the offline click's red sweep still runs over them.
    if (!art || chip.boot > 0) drawPins(ctx, chip, p, status, chip.boot);
    if (detailed) drawProviderBadge(ctx, chip, p);
    const glyph = glyphOf(chip.agent);
    const glyphColor = offline ? p.muted : leader ? p.teamHi : p.text;
    // #451: the name's colour follows the kind only; an offline Core is dimmed, not greyed.
    const nameStyle = chipNameStyle(leader, offline);
    const led = ledSpot(chip);
    if (detailed) {
      // #229: row1 = glyph + name, then the role line, row2 = LED (bottom-left) + token pill (bottom-right).
      const pad = led.x - led.r - x;
      const row2 = chip.h >= 40;
      const row1Y = y + Math.max(12, chip.h * 0.22);
      const roleY = row1Y + (leader ? 17 : 13);
      const nameRight = x + chip.w - pad - (look.mark ? 10 : 0);
      let nameX = x + pad;
      const size = Math.min(leader ? 18 : 14, chip.h * 0.32);
      if (glyph && size >= 8) {
        drawGlyph(ctx, glyph, nameX, row1Y - size / 2, size, glyphColor);
        nameX += size + 5;
      }
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.fillStyle = p[nameStyle.color];
      ctx.font = `${leader ? 600 : 550} ${leader ? 14 : 12}px ${p.font}`;
      ctx.save();
      if (nameStyle.alpha !== null) ctx.globalAlpha = nameStyle.alpha;
      ctx.fillText(ellipsis(ctx, this.nameOf(chip), nameRight - nameX), nameX, row1Y);
      ctx.restore();
      const role = chip.agent.roleName || chip.agent.title;
      if (role) {
        // Without row2 the LED sits on the role line's height: the role starts right of it.
        const roleX = row2 ? x + pad : led.x + led.r + 4;
        ctx.save();
        ctx.globalAlpha *= 0.85;
        ctx.font = `600 9px ${p.mono}`;
        ctx.fillText(ellipsis(ctx, role.toLocaleUpperCase("tr-TR"), x + chip.w - pad - roleX), roleX, roleY);
        ctx.restore();
      }
      // Patron (#232): no cost → no pill at all.
      const cost = row2 ? this.data.cost(chip.agent.id) : "";
      if (cost) drawTokenPill(ctx, cost, led.x + led.r + 6, x + chip.w - pad, led.y, p);
    } else if (labelled) {
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillStyle = p[nameStyle.color];
      ctx.font = `${leader ? 600 : 550} ${Math.max(7, Math.min(10, chip.h * 0.5))}px ${p.font}`;
      ctx.save();
      if (nameStyle.alpha !== null) ctx.globalAlpha = nameStyle.alpha;
      ctx.fillText(ellipsis(ctx, this.nameOf(chip), chip.w - 12), chip.x, chip.y);
      ctx.restore();
    } else if (glyph) {
      // #229: tiny chips keep only the glyph (and the LED).
      const size = Math.min(chip.h - 2, chip.w * 0.5, 14);
      if (size >= 6) drawGlyph(ctx, glyph, chip.x - size / 2, chip.y - size / 2, size, glyphColor);
    }

    drawLed(ctx, led, look, status, p, this.time, chip.boot);
    if (look.mark) drawMark(ctx, chip, look.mark, look.markColor, this.time, p.mono, look.blink);
    if (stalled !== null) drawClock(ctx, chip, p.amber);
    if (this.data.slow(chip.agent.id)) drawGlass(ctx, chip, p.amber, stalled !== null);
    ctx.restore();
  }
}

function drawBoard(ctx: CanvasRenderingContext2D, p: PcbTheme, w: number, h: number, u: number, signature: string) {
  const board = boardRect(w, h, u);
  ctx.fillStyle = p.outside;
  ctx.fillRect(0, 0, w, h);
  roundRect(ctx, board.x, board.y, board.w, board.h, p.radius);
  ctx.fillStyle = p.board; ctx.fill(); ctx.strokeStyle = p.boardEdge; ctx.lineWidth = 1; ctx.stroke();
  ctx.save(); roundRect(ctx, board.x, board.y, board.w, board.h, p.radius); ctx.clip();
  const texture = pcbPattern(ctx, u, board.x, board.y);
  if (texture) {
    // #401: the tiled PCB replaces the grid; dimmed so labels and traces keep their contrast.
    ctx.globalAlpha = PCB_ALPHA; ctx.fillStyle = texture; ctx.fillRect(board.x, board.y, board.w, board.h);
  } else {
    ctx.strokeStyle = p.grid; ctx.lineWidth = 0.6;
    for (let x = board.x + 24; x < board.x + board.w; x += 24) { ctx.beginPath(); ctx.moveTo(x, board.y); ctx.lineTo(x, board.y + board.h); ctx.stroke(); }
    for (let y = board.y + 24; y < board.y + board.h; y += 24) { ctx.beginPath(); ctx.moveTo(board.x, y); ctx.lineTo(board.x + board.w, y); ctx.stroke(); }
  }
  ctx.restore();
  // #277: holes and labels in the thin edge strip (fixed CSS px), tucked into the corners.
  const at = (BOARD_PAD_PX + HOLE_AT_PX) / u;
  for (const [x, y] of [[at, at], [w - at, at], [at, h - at], [w - at, h - at]]) {
    ctx.beginPath(); ctx.arc(x, y, HOLE_R_PX / u, 0, Math.PI * 2); ctx.fillStyle = p.outside; ctx.fill();
    ctx.beginPath(); ctx.arc(x, y, (HOLE_R_PX - 2) / u, 0, Math.PI * 2); ctx.strokeStyle = p.boardEdge; ctx.lineWidth = 1 / u; ctx.stroke();
  }
  if (w * u < LABELS_FROM_PX) return;
  const textX = at + (HOLE_R_PX + 6) / u;
  const baseY = h - (BOARD_PAD_PX + 5) / u;
  ctx.fillStyle = p.faint; ctx.font = `600 ${LABEL_PX / u}px ${p.mono}`; ctx.textBaseline = "alphabetic";
  ctx.textAlign = "left"; ctx.fillText("DEX  /  AGENT FABRIC", textX, baseY);
  ctx.textAlign = "right"; ctx.fillText("REV.39", w - textX, baseY);
  if (!signature) return;
  // #411: the team's name signed in the bottom-right, just left of REV.39 — never over the left label.
  const gap = 14 / u;
  const leftEnd = textX + ctx.measureText("DEX  /  AGENT FABRIC").width + gap;
  const right = w - textX - ctx.measureText("REV.39").width - gap;
  ctx.font = `italic 500 ${(LABEL_PX + 2) / u}px ${p.font}`;
  const text = ellipsis(ctx, signature, right - leftEnd);
  if (!text || right - leftEnd < 40 / u) return;
  ctx.save();
  ctx.globalAlpha = 0.75; ctx.fillStyle = p.teamHi;
  ctx.fillText(text, right, baseY);
  ctx.restore();
}

/** #401: texture over the board fill, and the plate under a worker's labels (contrast as on the plain chip). */
const PCB_ALPHA = 0.55;
const PLATE_ALPHA = 0.88;

const patterns = new WeakMap<CanvasRenderingContext2D, { img: HTMLImageElement; pattern: CanvasPattern }>();

/** The PCB tile repeating from the board's corner, 256 CSS px per tile (null: not decoded yet). */
function pcbPattern(ctx: CanvasRenderingContext2D, u: number, x: number, y: number): CanvasPattern | null {
  const img = pcbImage();
  if (!img) return null;
  let cached = patterns.get(ctx);
  if (!cached || cached.img !== img) {
    const pattern = ctx.createPattern(img, "repeat");
    if (!pattern) return null;
    patterns.set(ctx, (cached = { img, pattern }));
  }
  const k = PCB_TILE_PX / u / img.width;
  cached.pattern.setTransform(new DOMMatrix([k, 0, 0, k, x, y]));
  return cached.pattern;
}

function workerArt(chip: Chip) {
  const img = workerImage();
  const slices = img && workerSlices({ x: chip.x - chip.w / 2, y: chip.y - chip.h / 2, w: chip.w, h: chip.h });
  return img && slices ? { img, slices } : null;
}

function drawPins(ctx: CanvasRenderingContext2D, chip: Chip, p: PcbTheme, status: AgentStatus, boot: number) {
  const detailed = chip.h >= 24 && chip.w >= 40;
  const count = detailed ? (chip.agent.kind === "leader" ? 10 : 7) : 0;
  const sweep = boot > 0 ? Math.floor((1 - boot / BOOT_SECONDS) * count * 2) : -1;
  for (let i = 0; i < count; i++) {
    const px = chip.x - chip.w / 2 + (i + 1) * chip.w / (count + 1);
    ctx.fillStyle = sweep >= i ? p.red : status === "offline" ? p.faint : p.pin;
    ctx.fillRect(px - 1.5, chip.y - chip.h / 2 - 5, 3, 5);
    ctx.fillStyle = sweep >= count + count - i - 1 ? p.red : status === "offline" ? p.faint : p.pin;
    ctx.fillRect(px - 1.5, chip.y + chip.h / 2, 3, 5);
  }
  const vertical = detailed ? Math.max(3, Math.floor(count / 2)) : chip.h >= 9 ? 2 : 0;
  const pinH = detailed ? 3 : Math.max(1, Math.min(2, chip.h / 4));
  for (let i = 0; i < vertical; i++) {
    const py = chip.y - chip.h / 2 + (i + 1) * chip.h / (vertical + 1);
    ctx.fillStyle = status === "offline" ? p.faint : p.pin;
    ctx.fillRect(chip.x - chip.w / 2 - 5, py - pinH / 2, 5, pinH); ctx.fillRect(chip.x + chip.w / 2, py - pinH / 2, 5, pinH);
  }
}

/**
 * #229: readable provider label straddling the chip's top edge (was the corner triangle + active code).
 * The chain's active target wins over the configured one, as before.
 */
function drawProviderBadge(ctx: CanvasRenderingContext2D, chip: Chip, p: PcbTheme) {
  const { label, color } = providerOf(chip.agent, p);
  ctx.font = `700 8px ${p.mono}`;
  const text = ellipsis(ctx, label, chip.w - 28);
  const w = ctx.measureText(text).width + 8;
  const x = chip.x - chip.w / 2 + 8;
  const y = chip.y - chip.h / 2 - 5;
  roundRect(ctx, x, y, w, 10, 3);
  ctx.fillStyle = p.board; ctx.fill();
  ctx.strokeStyle = color; ctx.lineWidth = 1; ctx.stroke();
  ctx.fillStyle = color; ctx.textAlign = "left"; ctx.textBaseline = "middle";
  ctx.fillText(text, x + 4, y + 5.5);
}

function providerOf(agent: Agent, p: PcbTheme) {
  const engine = agent.activeEngine ?? agent.engine;
  const provider = agent.activeProvider ?? agent.provider;
  const [label, color] = engine === "codex" ? ["CODEX", p.provider]
    : !provider || provider === "anthropic" ? ["ANTHROPIC", p.muted]
    : [provider === "local" ? "LOCAL" : provider.toUpperCase(), p.amber];
  return { label, color };
}

/** #229 spec: the only theme-less constants (readable on both boards). */
const PILL_FILL = "rgba(255,255,255,.06)";
const PILL_EDGE = "rgba(255,255,255,.09)";

/** #229: token pill right-aligned on row2, ellipsized inside [minX, maxX] — never onto the role line. */
function drawTokenPill(ctx: CanvasRenderingContext2D, text: string, minX: number, maxX: number, cy: number, p: PcbTheme) {
  ctx.font = `600 10px ${p.mono}`;
  const value = ellipsis(ctx, text, maxX - minX - 10);
  const w = ctx.measureText(value).width + 10;
  roundRect(ctx, maxX - w, cy - 6.5, w, 13, 4);
  ctx.fillStyle = PILL_FILL; ctx.fill();
  ctx.strokeStyle = PILL_EDGE; ctx.lineWidth = 1; ctx.stroke();
  ctx.fillStyle = p.text; ctx.textAlign = "right"; ctx.textBaseline = "middle";
  ctx.fillText(value, maxX - 5, cy + 0.5);
}

interface Look { ring: string | null; led: string | null; mark: string | null; markColor: string; blink: boolean; error: boolean }

/** #229 status → ring + LED + mark. An unread result wins, then a question / waiting (one amber look), then working. */
function chipLook(status: AgentStatus, result: Delegation | null, question: Question | null, p: PcbTheme): Look {
  const none: Look = { ring: null, led: null, mark: null, markColor: p.text, blink: false, error: false };
  if (result?.status === "done") return { ...none, ring: p.green, led: p.green, mark: "✓", markColor: p.green };
  if (result?.status === "failed") return { ...none, ring: p.redHi, led: p.redHi, mark: "!", markColor: p.redHi, error: true };
  if (question || status === "waiting") return { ...none, ring: p.amber, led: p.amber, mark: "?", markColor: p.amber, blink: true };
  if (result) return { ...none, ring: p.amber, led: p.amber, mark: "!", markColor: p.amber }; // done_no_report
  if (status === "working") return { ...none, ring: p.red, led: p.red };
  return none;
}

type GlyphPaths = Array<{ path: Path2D; fill: boolean }>;
const glyphCache = new Map<string, GlyphPaths>();

/** Catalog slug → glyph; roles outside the catalog get none (patron #232: no fallback glyph). */
function glyphOf(agent: Agent): GlyphPaths | null {
  // Runtime members are AgentRec (src/team/api.ts), which carries the catalog slug.
  const slug = (agent as Agent & { roleSlug?: string | null }).roleSlug;
  if (!slug) return null;
  let paths = glyphCache.get(slug);
  if (!paths) {
    const parts = GLYPHS.get(slug);
    if (!parts) return null;
    paths = parts.map((part) => ({ path: new Path2D(part.d), fill: part.fill }));
    glyphCache.set(slug, paths);
  }
  return paths;
}

/** 24×24 stroke glyph scaled to `size` at (x, y); canvas has no currentColor, so it is painted with `color`. */
function drawGlyph(ctx: CanvasRenderingContext2D, glyph: GlyphPaths, x: number, y: number, size: number, color: string) {
  ctx.save();
  ctx.translate(x, y); ctx.scale(size / 24, size / 24);
  ctx.strokeStyle = color; ctx.fillStyle = color; ctx.lineWidth = 2; ctx.lineCap = "round"; ctx.lineJoin = "round";
  for (const part of glyph) { if (part.fill) ctx.fill(part.path); ctx.stroke(part.path); }
  ctx.restore();
}

type LedSpot = { x: number; y: number; r: number };

function ledSpot(chip: Chip): LedSpot {
  const r = Math.max(1, Math.min(3, chip.h / 5, chip.w / 8));
  const insetX = Math.max(r + 2, Math.min(11, chip.w / 4));
  const insetY = Math.max(r + 1, Math.min(10, chip.h / 3));
  return { x: chip.x - chip.w / 2 + insetX, y: chip.y + chip.h / 2 - insetY, r };
}

function drawLed(ctx: CanvasRenderingContext2D, spot: LedSpot, look: Look, status: AgentStatus, p: PcbTheme, time: number, boot: number) {
  let color = p.faint; let alpha = 0.35;
  if (look.led) { color = look.led; alpha = look.blink ? (Math.floor(time * 6) % 2 ? 1 : 0.12) : 1; }
  else if (status === "idle") { color = p.red; alpha = 0.45; }
  else if (boot > 0) { color = p.red; alpha = 1 - boot / BOOT_SECONDS; }
  ctx.save(); ctx.globalAlpha *= alpha; ctx.fillStyle = color; ctx.shadowColor = color; ctx.shadowBlur = alpha > 0.7 ? Math.min(8, spot.r * 3) : 0;
  ctx.beginPath(); ctx.arc(spot.x, spot.y, spot.r, 0, Math.PI * 2); ctx.fill(); ctx.restore();
}

function drawMark(ctx: CanvasRenderingContext2D, chip: Chip, value: string, color: string, time: number, mono: string, blink = false) {
  if (blink && Math.floor(time * 6) % 2 === 0) return;
  const compact = chip.h < 24 || chip.w < 40;
  const fontSize = compact ? Math.max(6, Math.min(9, chip.h * 0.7)) : 13;
  // #229: top-right inside the chip (was outside its right edge).
  ctx.fillStyle = color; ctx.font = `700 ${fontSize}px ${mono}`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
  ctx.fillText(value, compact ? chip.x + chip.w / 2 - fontSize / 2 : chip.x + chip.w / 2 - 9,
    compact ? chip.y : chip.y - chip.h / 2 + 10);
}

function drawClock(ctx: CanvasRenderingContext2D, chip: Chip, color: string) {
  const compact = chip.h < 24 || chip.w < 40;
  const radius = Math.max(2, Math.min(5, chip.h / 4));
  const x = compact ? chip.x + chip.w / 2 - radius - 2 : chip.x + chip.w / 2 + 10;
  const y = compact ? chip.y : chip.y + chip.h / 2 - 6;
  ctx.strokeStyle = color; ctx.lineWidth = Math.min(1.4, radius / 2); ctx.beginPath(); ctx.arc(x, y, radius, 0, Math.PI * 2);
  ctx.moveTo(x, y - radius * 0.6); ctx.lineTo(x, y); ctx.lineTo(x + radius * 0.5, y + radius * 0.3); ctx.stroke();
}

/** FIX-4: the dock's amber hourglass (Rack `SlowGlass`), in the clock's slot — left of it when both show. */
function drawGlass(ctx: CanvasRenderingContext2D, chip: Chip, color: string, besideClock: boolean) {
  const compact = chip.h < 24 || chip.w < 40;
  const size = 2 * Math.max(2, Math.min(5, chip.h / 4)) + 1;
  const cx = (compact ? chip.x + chip.w / 2 - size / 2 - 2 : chip.x + chip.w / 2 + 10) - (besideClock ? size + 3 : 0);
  const cy = compact ? chip.y : chip.y + chip.h / 2 - 6;
  const k = size / 12;
  ctx.save();
  ctx.translate(cx - size / 2, cy - size / 2);
  ctx.scale(k, k);
  ctx.strokeStyle = color; ctx.lineWidth = 1.2; ctx.lineCap = "round";
  ctx.stroke(new Path2D("M3 1.5h6M3 10.5h6M3.5 1.5c0 3 5 3 5 4.5s-5 1.5-5 4.5M8.5 1.5c0 3-5 3-5 4.5s5 1.5 5 4.5"));
  ctx.restore();
}

function drawVia(ctx: CanvasRenderingContext2D, x: number, y: number, p: PcbTheme) {
  ctx.beginPath(); ctx.arc(x, y, 2.2, 0, Math.PI * 2); ctx.fillStyle = p.board; ctx.fill(); ctx.strokeStyle = p.via; ctx.lineWidth = 1; ctx.stroke();
}

function makePath(ctx: CanvasRenderingContext2D, points: Point[]) {
  ctx.beginPath(); ctx.moveTo(points[0].x, points[0].y); for (let i = 1; i < points.length; i++) ctx.lineTo(points[i].x, points[i].y);
}

function drawPacket(ctx: CanvasRenderingContext2D, points: Point[], progress: number, color: string, radius: number, mono: string, label?: string) {
  const p = pointAt(points, progress); ctx.save(); ctx.fillStyle = color; ctx.shadowColor = color; ctx.shadowBlur = 10;
  ctx.beginPath(); ctx.arc(p.x, p.y, radius, 0, Math.PI * 2); ctx.fill(); ctx.shadowBlur = 0;
  if (label) { ctx.fillStyle = color; ctx.font = `700 11px ${mono}`; ctx.textAlign = "center"; ctx.fillText(label, p.x, p.y - 8); }
  ctx.restore();
}

function pointAt(points: Point[], t: number) {
  const lengths = points.slice(1).map((point, i) => Math.hypot(point.x - points[i].x, point.y - points[i].y));
  const total = lengths.reduce((sum, n) => sum + n, 0); let distance = t * total;
  for (let i = 0; i < lengths.length; i++) {
    if (distance <= lengths[i]) { const k = lengths[i] ? distance / lengths[i] : 0;
      return { x: points[i].x + (points[i + 1].x - points[i].x) * k, y: points[i].y + (points[i + 1].y - points[i].y) * k }; }
    distance -= lengths[i];
  }
  return points[points.length - 1];
}
