import type { Agent, AgentStatus } from "../agents";
import type { Delegation } from "../delegations";
import type { Question } from "../questions";
import { GLYPHS } from "./glyphs";
import { drawCoreXL, drawCoreFragments, drawCoreFeet, XL_BODY, type CorePalette } from "./companion/coreEngine";
import { LOOKS, type Step } from "./companion/coreState";
import { CoreXL, coreBody, XL_PIN, type Body, type Pointer } from "./companion/coreXL";
import { areaInset, fitUnit, isEcho, layout, ownershipKey, latestOwners, pickMode, routes, type Mode, type Point } from "./placement";
import { ECHO_FOOT, WORKER_FOOT } from "./assets";
import { chipNameStyle, coreLabel, OFFLINE_NAME_ALPHA, teamSignature } from "./label";
import { DelegationTouches, TOUCH_MS, reachAt, tentacleContour, fragmentPaths, type FragmentBox } from "./symbiote";
import { drawCoreName, ellipsis, measureCoreName, roundRect } from "./coreName";
import {
  LASER, LOOK, clockAt, coreEyeK, dropAt, echoBeside, echoDrift, echoLight, facingOf, labelRect, laserAngle, lasing, lifeOf, linear, markAlpha, markAt,
  pathLength, pointAt, routeToBody, sacLook, scale, smoothRoute, spotOf, strandTone, textBlock, workerLight, type Light, type Rect, type Rgb, type Spot,
  type Tone,
} from "./symbioteCard";
import type { BodyDraw, Flow, Laser, LitScene, PointLight, Shadow } from "./litLayer";
import type { OfficeTheme } from "./theme";

/** The world before Office measures its panel (#270: then the panel in world units). */
export const W = 960;
export const H = 540;

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

/**
 * A delegation strand (#1958: the soft curve of the octilinear route, ending on the body's Core-facing
 * edge). `points` = the feet / droplets' path; `route` = from the Core's centre (tentacle, relief, drops).
 */
interface Trace { leaderId: string; workerId: string; points: Point[]; route: Point[]; length: number }

/** One body on the membrane this frame: a worker's sac, Echo, or a leader too small for Core XL. */
interface View {
  chip: Chip; spot: Spot; echo: boolean; light: Light; status: AgentStatus; hot: boolean;
  /** #2020: 0 dormant .. 1 live (working, an active job, a question, an unread result). */
  life: number;
}

/** #2020: Echo beside its Core's name: body spot, its Core-style label rows and the box it takes (hit, calm). */
type EchoPlace = ReturnType<typeof echoBeside> & { box: Rect };

/** #2020: a strand this frame: its life, colour and the returning drop (`along`). */
interface StrandState { trace: Trace; job: boolean; life: number; tone: Tone; along: number | null }

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
  private traceKey = "";
  /** #1958: which way each body looks (columns: towards its Core). */
  private facing = new Map<string, 1 | -1 | 0>();
  /** #2020: Echo beside the first Core's name (none: the Core is too small, Echo keeps its slot). */
  private echoes = new Map<string, EchoPlace>();
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
  /** #1958: the last draw's lit scene (CSS px) for Office's light layer. */
  scene: LitScene = { time: 0, reduced: false, core: null, lights: [], shadows: [], flows: [], bodies: [], lasers: [], strands: { key: "", routes: [], cores: [], calm: [] }, flowColor: [0, 0, 0] };

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
    for (const chip of this.chips) {
      const k = 1 - Math.exp(-MOVE_SPEED * dt);
      if (Math.abs(chip.tx - chip.x) > 0.05 || Math.abs(chip.ty - chip.y) > 0.05) {
        chip.x += (chip.tx - chip.x) * k;
        chip.y += (chip.ty - chip.y) * k;
      } else {
        chip.x = chip.tx;
        chip.y = chip.ty;
      }
      chip.boot = Math.max(0, chip.boot - dt);
    }
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
          if (!target) return null;
          // #1958: the gaze goes to the body, not the label beside it.
          const spot = this.spotOf(target);
          return { x: spot.bx, y: spot.by };
        },
        hovered: this.hoverId === id,
        reduced: this.reduced,
      });
    }
    for (const id of this.cores.keys()) if (!live.has(id)) this.cores.delete(id);
  }

  /** Offline chip click: sweep its light up, then ask the existing pane bridge to start it. */
  activate(agentId: string): boolean {
    const chip = this.chips.find((item) => item.agent.id === agentId);
    if (chip?.agent.kind === "worker") this.touch(agentId);
    if (!chip || this.data.status(agentId) !== "offline") return false;
    chip.boot = BOOT_SECONDS;
    window.setTimeout(() => this.data.start(agentId), 260);
    return true;
  }

  hit(x: number, y: number): Agent | null {
    for (const chip of this.chips) {
      const box = this.echoes.get(chip.agent.id)?.box;
      if (box && x >= box.x && x <= box.x + box.w && y >= box.y && y <= box.y + box.h) return chip.agent;
    }
    const chip = [...this.chips].reverse().find((item) =>
      x >= item.x - item.w / 2 && x <= item.x + item.w / 2 && y >= item.y - item.h / 2 && y <= item.y + item.h / 2,
    );
    return chip?.agent ?? null;
  }

  isAnimated() {
    this.refreshLayout(this.data.delegations());
    // #1958: the membrane's pulse, the bodies' light and Core's breath always move unless motion is reduced.
    if (!this.reduced) return true;
    return this.chips.some((chip) => chip.boot > 0 || Math.abs(chip.tx - chip.x) > 0.05 || Math.abs(chip.ty - chip.y) > 0.05);
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

  /**
   * The ink over the lit layer: contact tentacles, Core (droplets, feet, body, name), the bodies' labels
   * and marks, the drops' `?`, the team's signature. `fallback`: no light layer (no WebGL2, lost context,
   * maps loading) — the membrane, strands and bodies are drawn here first, flat.
   */
  draw(ctx: CanvasRenderingContext2D, focusedId: string | null, palette: OfficeTheme, core: CorePalette, fallback = false): Anchor[] {
    const delegations = this.data.delegations();
    this.refreshLayout(delegations);
    this.rebuildTraces(delegations);
    this.placeEchoes(ctx, palette);
    const time = this.reduced ? 0 : this.time;
    const views = this.chips.filter((chip) => !this.bodyOf(chip)).map((chip) => this.viewOf(chip, delegations, time, chip.agent.id === focusedId));
    const strands = this.strandStates(views, delegations, time);
    if (fallback) this.drawFallback(ctx, palette, views, strands, time);
    this.drawContacts(ctx, palette);
    this.drawFragments(ctx, palette, core);
    const anchors = this.chips.map((chip) => {
      const body = this.bodyOf(chip);
      if (body) this.drawCore(ctx, chip, body, palette, core);
      const echo = this.echoes.get(chip.agent.id)?.spot;
      if (echo) return { agent: chip.agent, x: echo.bx, y: echo.by, top: echo.D / 2 + 8, body };
      return { agent: chip.agent, x: chip.x, y: chip.y, top: chip.h / 2, body };
    });
    for (const view of views) this.drawLabel(ctx, view, palette, time);
    this.drawDropMarks(ctx, palette, time);
    this.drawSignature(ctx, palette);
    this.scene = this.litScene(views, strands, palette, time);
    return anchors;
  }

  /** The body's spot in the chip's slot at its current position (Echo: beside its Core's name). */
  private spotOf(chip: Chip) {
    return this.echoes.get(chip.agent.id)?.spot ?? spotOf(chip, this.facing.get(chip.agent.id) ?? 0, isEcho(chip.agent));
  }

  /**
   * #2020 (patron): Echo is labelled like Core — its body just left of the first Core's name block, its name
   * and role (+ cost) centred under it. The layout keeps Echo's slot under Core (routes unchanged).
   */
  private placeEchoes(ctx: CanvasRenderingContext2D, p: OfficeTheme) {
    this.echoes.clear();
    const leader = this.chips.find((chip) => chip.agent.kind === "leader");
    const body = leader && this.bodyOf(leader);
    if (!leader || !body) return;
    const nameY = body.y + body.s / 2 + XL_PIN + 10;
    ctx.save();
    const block = measureCoreName(ctx, { ...this.coreText(leader), maxW: Math.max(40, leader.w + 40), font: p.font, mono: p.mono });
    for (const chip of this.chips.filter((item) => isEcho(item.agent))) {
      const place = echoBeside({ x: leader.x, nameY, roleY: nameY + 15 }, block.width, spotOf(chip, 0, true).D);
      const label = measureCoreName(ctx, { ...this.echoText(chip), maxW: place.maxW, font: p.font, mono: p.mono });
      const { bx, by, D } = place.spot;
      const half = Math.max(D, label.width) / 2;
      this.echoes.set(chip.agent.id, { ...place, box: { x: bx - half, y: by - D / 2 - 8, w: 2 * half, h: place.roleY + 8 - (by - D / 2 - 8) } });
    }
    ctx.restore();
  }

  /** Core's label texts (as drawCore draws them). */
  private coreText(chip: Chip) {
    return { name: this.nameOf(chip), role: (chip.agent.roleName || chip.agent.title || "").toLocaleUpperCase("tr-TR"), cost: this.data.cost(chip.agent.id) };
  }

  private echoText(chip: Chip) {
    return { name: chip.agent.name, role: (chip.agent.roleName || chip.agent.title || "").toLocaleUpperCase("tr-TR"), cost: this.data.cost(chip.agent.id) };
  }

  private viewOf(chip: Chip, delegations: Delegation[], time: number, focused: boolean): View {
    const id = chip.agent.id;
    const status = this.data.status(id);
    const echo = isEcho(chip.agent);
    const job = !echo && delegations.some((d) => d.toAgent === id && this.data.active(d));
    // #2020: an idle worker with a running job is still at work: its sac beats like a working one.
    let light = workerLight(job && status === "idle" ? "working" : status, echo ? null : this.data.result(id), this.data.question(id), time, this.reduced);
    // An offline click wakes the body: its light comes up in the team's red.
    if (chip.boot > 0) light = { ...light, tone: "red", k: 1 - chip.boot / BOOT_SECONDS, pulse: 1, offline: false, mark: null, error: false };
    return { chip, spot: this.spotOf(chip), echo, light, status, hot: focused || this.hoverId === id, life: echo ? 0 : lifeOf(light, job) };
  }

  /** #2020: each strand's life and drop; live strands last (their flow ids win where strands overlap at Core). */
  private strandStates(views: View[], delegations: Delegation[], time: number): StrandState[] {
    const byId = new Map(views.map((view) => [view.chip.agent.id, view]));
    const states = this.traces.map((trace): StrandState => {
      const job = delegations.some((d) => d.fromAgent === trace.leaderId && d.toAgent === trace.workerId && this.data.active(d));
      const view = byId.get(trace.workerId);
      const question = this.data.question(trace.workerId);
      const result = this.data.result(trace.workerId);
      const along = dropAt(question, result, time, this.reduced);
      // The drop's colour wins (it travels on the strand); else the worker's tone, plain work in the theme red (#2036).
      const tone = strandTone(view?.light.tone ?? null, question, result, along !== null);
      return { trace, job, life: Math.max(view?.life ?? 0, job ? 1 : 0), tone, along };
    });
    return [...states.filter((s) => s.life <= 0), ...states.filter((s) => s.life > 0)];
  }

  /** All fragments precede all chips and text; protected plates also constrain their paths. */
  private drawFragments(ctx: CanvasRenderingContext2D, p: OfficeTheme, pal: CorePalette) {
    const inset = areaInset(this.unit);
    const board = { x: inset, y: inset, w: this.w - 2 * inset, h: this.h - 2 * inset };
    for (const chip of this.chips) {
      const body = this.bodyOf(chip), core = this.cores.get(chip.agent.id);
      if (!body || !core) continue;
      const blocks: FragmentBox[] = this.chips.filter(c => c !== chip).map(c => {
        const echo = this.echoes.get(c.agent.id)?.box;
        return echo ? { x: echo.x - 6, y: echo.y - 6, w: echo.w + 12, h: echo.h + 12 } : { x: c.x - c.w / 2 - 6, y: c.y - c.h / 2 - 12, w: c.w + 12, h: c.h + 18 };
      });
      // The provider badge is not a block: upper traces leave through it, and pieces drawn first slip under it.
      blocks.push({ x: body.x - (chip.w + 40) / 2, y: body.y + body.s / 2 + XL_PIN, w: chip.w + 40, h: 40 });
      const paths = fragmentPaths(body, this.traces.filter(t => t.leaderId === chip.agent.id).map(t => t.points), blocks, board);
      drawCoreFragments(ctx, body, core.fragments.sample(paths, body, this.reduced), core.fragments.phase, pal, p.edge);
      drawCoreFeet(ctx, body, core.sampleFeet(paths, body, this.time * 1000, this.reduced || core.state === "sleep", ctx.canvas), pal, p.edge);
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

  /** Routes follow the layout targets: rebuilt (smoothed) only when a target or an owner changes. */
  private rebuildTraces(delegations: Delegation[]) {
    const key = `${this.ownership}|${this.chips.map((c) => `${c.tx},${c.ty},${c.w},${c.h},${c.mode ?? ""}`).join(";")}`;
    if (key === this.traceKey) return;
    this.traceKey = key;
    this.traces = [];
    this.facing.clear();
    const leaders = this.chips.filter((chip) => chip.agent.kind === "leader");
    if (!leaders.length) return;
    const owners = latestOwners(delegations);
    const groups = new Map(leaders.map((leader) => [leader.agent.id, [] as Chip[]]));
    // #1958: Echo has no strand and is not in the round-robin (placement `crew`).
    this.chips.filter((chip) => chip.agent.kind === "worker" && !isEcho(chip.agent)).forEach((worker, index) => {
      const leader = leaders.find((item) => item.agent.id === owners.get(worker.agent.id)) ?? leaders[index % leaders.length];
      groups.get(leader.agent.id)!.push(worker);
    });
    const target = (chip: Chip) => ({ id: chip.agent.id, x: chip.tx, y: chip.ty, w: chip.w, h: chip.h });
    let seed = 0;
    for (const leader of leaders) {
      const core = { ...target(leader), mode: leader.mode };
      const body = coreBody(core);
      const workers = groups.get(leader.agent.id) ?? [];
      const paths = routes(core, workers.map(target));
      for (const worker of workers) {
        const raw = paths.get(worker.agent.id);
        const facing = facingOf(target(worker), core, leader.mode, false);
        this.facing.set(worker.agent.id, facing);
        if (!raw) continue;
        const points = smoothRoute(routeToBody(raw, spotOf(target(worker), facing, false)), seed++);
        const route = body ? [{ x: body.x, y: body.y }, ...points] : points;
        this.traces.push({ leaderId: leader.agent.id, workerId: worker.agent.id, points, route, length: pathLength(route) });
      }
    }
  }

  /** Contact tentacles along the strands (#1597: the product's own contour). */
  private drawContacts(ctx: CanvasRenderingContext2D, p: OfficeTheme) {
    if (this.reduced) return;
    for (const trace of this.traces) {
      const contact = this.touches.find(t => t.worker === trace.workerId && t.leader === trace.leaderId);
      if (!contact) continue;
      const age = this.time * 1000 - contact.at;
      const contour = tentacleContour(trace.route, reachAt(age), age);
      if (!contour.length) continue;
      ctx.save();
      makePath(ctx, contour); ctx.closePath();
      ctx.fillStyle = "#030405"; ctx.fill();
      ctx.strokeStyle = p.red; ctx.lineWidth = 0.8; ctx.stroke();
      ctx.restore();
    }
  }

  /** A question drop carries its `?` (the lit layer draws the drop itself). */
  private drawDropMarks(ctx: CanvasRenderingContext2D, p: OfficeTheme, time: number) {
    for (const trace of this.traces) {
      const question = this.data.question(trace.workerId);
      const along = question ? dropAt(question, null, time, this.reduced) : null;
      if (along === null) continue;
      const at = pointAt(trace.route, along);
      ctx.fillStyle = p.amber; ctx.font = `700 11px ${p.mono}`; ctx.textAlign = "center"; ctx.textBaseline = "alphabetic";
      ctx.fillText("?", at.x, at.y - 8);
    }
  }

  /** #411: the team's name signed in the bottom-right corner (CSS px, as before). */
  private drawSignature(ctx: CanvasRenderingContext2D, p: OfficeTheme) {
    const signature = teamSignature(this.teamName);
    const u = this.unit;
    if (!signature || this.w * u < 320) return;
    ctx.save();
    ctx.font = `italic 500 ${11 / u}px ${p.font}`; ctx.textAlign = "right"; ctx.textBaseline = "alphabetic";
    ctx.globalAlpha = 0.75; ctx.fillStyle = p.teamHi;
    ctx.fillText(ellipsis(ctx, signature, this.w * 0.4), this.w - 16 / u, this.h - 12 / u);
    ctx.restore();
  }

  /** #252: a leader as the big Core (spec §2–§5): body, then name + role (+ cost pill) under it. */
  private drawCore(ctx: CanvasRenderingContext2D, chip: Chip, body: Body, p: OfficeTheme, pal: CorePalette) {
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
      edge: p.edge,
      mono: p.mono,
      weight: core?.weight,
      // #2036 (patron): the team's colour lives in Core's eye (the strands stay red).
      eye: p.teamHi,
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
    // #411: "Ana Core" in the team colour; #1958: the calm zone under it keeps the membrane quiet.
    drawCoreName(ctx, {
      x: chip.x, nameY, roleY, maxW,
      ...this.coreText(chip),
      plate: null,
      nameColor: p.teamHi, roleColor: p.muted, font: p.font, mono: p.mono,
    }, (text, minX, maxX, cy) => drawTokenPill(ctx, text, minX, maxX, cy, p));
    ctx.restore();

    const stalled = this.data.stalled(chip.agent.id);
    const at = { x: body.x + body.s / 2 + 10, y: body.y + body.s / 2 - 6 };
    if (stalled !== null) drawClock(ctx, at.x, at.y, 5, p.amber);
    if (this.data.slow(chip.agent.id)) drawGlass(ctx, at.x - (stalled !== null ? 14 : 0), at.y, 11, p.amber);
  }

  /** #411: a leader shows as "Ana Core"; workers keep their names. */
  private nameOf(chip: Chip) {
    return chip.agent.kind === "leader" ? coreLabel(this.teamName, chip.agent.name) : chip.agent.name;
  }

  /**
   * #1958 §7: a body's label beside it (provider badge, glyph + name, role, cost pill), the status mark
   * on the body's outer top corner, the stall clock / slow glass on the inner one, the focus ring.
   */
  private drawLabel(ctx: CanvasRenderingContext2D, view: View, p: OfficeTheme, time: number) {
    const { chip, spot, light } = view;
    const leader = chip.agent.kind === "leader";
    const block = textBlock(spot, chip.h, !!this.data.cost(chip.agent.id));
    const right = spot.align === "right";
    const maxW = spot.textR - spot.textL;
    const echo = this.echoes.get(chip.agent.id);
    if (echo) this.drawEchoName(ctx, chip, echo, light, p);
    else if (maxW >= 12 && view.life > 0 && !light.offline) {
      // #2020: a live worker's label sits in a faint glow of its colour (the text keeps its dark halo).
      const r = labelRect(spot, block);
      const glow = ctx.createRadialGradient(0, 0, 0, 0, 0, 32);
      glow.addColorStop(0, p[light.tone]); glow.addColorStop(1, "rgba(0,0,0,0)");
      ctx.save();
      ctx.translate(r.x + r.w / 2, r.y + r.h / 2 + 3);
      ctx.scale((r.w * 0.62) / 32, (r.h * 0.8) / 32);
      ctx.globalAlpha = 0.2 * view.life * (0.7 + 0.3 * light.pulse);
      ctx.fillStyle = glow;
      ctx.beginPath(); ctx.arc(0, 0, 32, 0, Math.PI * 2); ctx.fill();
      ctx.restore();
    }
    if (!echo && maxW >= 12) {
      ctx.save();
      ctx.textBaseline = "middle";
      if (light.offline) ctx.globalAlpha = 0.62;
      // provider badge above the name, on the body's side of the block
      const provider = providerOf(chip.agent, p);
      ctx.font = `700 8px ${p.mono}`;
      const pt = ellipsis(ctx, provider.label, maxW - 8);
      const pw = ctx.measureText(pt).width + 8;
      const px = right ? spot.textR - pw : spot.textL;
      const py = block.top - 12;
      roundRect(ctx, px, py, pw, 10, 3);
      ctx.fillStyle = provider.fill ?? p.surface; ctx.fill();
      ctx.strokeStyle = provider.fill ? p.edge : provider.color; ctx.lineWidth = 1; ctx.stroke();
      ctx.fillStyle = provider.color; ctx.textAlign = "left";
      ctx.fillText(pt, px + 4, py + 5.5);
      ctx.shadowColor = LABEL_SHADOW; ctx.shadowBlur = 3;
      // glyph + name (#451: the name's colour follows the kind; an offline leader is dimmed, not greyed)
      const style = chipNameStyle(leader, light.offline);
      const nameColor = light.offline && !leader ? p.muted : p[style.color];
      const glyph = glyphOf(chip.agent);
      const size = 13;
      const glyphW = glyph ? size + 5 : 0;
      ctx.font = `550 12px ${p.font}`;
      const name = ellipsis(ctx, this.nameOf(chip), maxW - glyphW);
      const glyphColor = view.echo ? LOOK.echo.color : light.offline ? p.muted : leader ? p.teamHi : p.text;
      if (glyph) drawGlyph(ctx, glyph, right ? spot.textR - size : spot.textL, block.nameY - size / 2, size, glyphColor);
      ctx.save();
      if (style.alpha !== null) ctx.globalAlpha = style.alpha;
      ctx.fillStyle = nameColor; ctx.textAlign = right ? "right" : "left";
      ctx.fillText(name, right ? spot.textR - glyphW : spot.textL + glyphW, block.nameY);
      ctx.restore();
      const role = chip.agent.roleName || chip.agent.title;
      if (block.rows >= 2 && role) {
        ctx.font = `600 9px ${p.mono}`;
        ctx.fillStyle = light.offline ? p.muted : p.text2;
        ctx.textAlign = right ? "right" : "left";
        ctx.fillText(ellipsis(ctx, role.toLocaleUpperCase("tr-TR"), maxW), right ? spot.textR : spot.textL, block.roleY);
      }
      // Patron (#232): no cost → no pill at all.
      if (block.rows >= 3) {
        ctx.shadowBlur = 0;
        const cost = this.data.cost(chip.agent.id);
        ctx.font = `600 10px ${p.mono}`;
        const cw = Math.min(maxW, ctx.measureText(cost).width + 10);
        if (right) drawTokenPill(ctx, cost, spot.textR - cw, spot.textR, block.costY, p);
        else drawTokenPill(ctx, cost, spot.textL, spot.textL + cw, block.costY, p);
      }
      ctx.restore();
    }
    if (light.mark) {
      const color = p[light.tone];
      const at = markAt(spot);
      ctx.save();
      ctx.globalAlpha = markAlpha(light.mark, time, this.reduced);
      ctx.beginPath(); ctx.arc(at.x, at.y, 6.5, 0, Math.PI * 2);
      ctx.fillStyle = p.surface; ctx.fill();
      ctx.lineWidth = 1.4; ctx.strokeStyle = color;
      if (light.error) ctx.setLineDash([3, 2]);
      ctx.stroke();
      ctx.fillStyle = color; ctx.font = `700 9px ${p.mono}`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.fillText(light.mark, at.x, at.y + 0.5);
      ctx.restore();
    }
    const stalled = this.data.stalled(chip.agent.id);
    const slow = this.data.slow(chip.agent.id);
    if (stalled !== null || slow) {
      const at = clockAt(spot);
      const r = Math.max(2, Math.min(5, spot.D / 8));
      if (stalled !== null) drawClock(ctx, at.x, at.y, r, p.amber);
      if (slow) drawGlass(ctx, at.x - (stalled !== null ? (spot.facing === 1 ? -1 : 1) * (2 * r + 4) : 0), at.y, 2 * r + 1, p.amber);
    }
    if (view.hot) {
      ctx.save();
      ctx.setLineDash([3, 3]); ctx.lineWidth = 1.2; ctx.strokeStyle = p.teamHi;
      ctx.beginPath(); ctx.ellipse(spot.bx, spot.by, spot.D * 0.62, spot.fh * 0.62 + 2, 0, 0, Math.PI * 2); ctx.stroke();
      ctx.restore();
    }
  }

  /** #2020: Echo's provider badge on its body's top edge, its name and role (+ cost) under it (Core's look). */
  private drawEchoName(ctx: CanvasRenderingContext2D, chip: Chip, place: EchoPlace, light: Light, p: OfficeTheme) {
    const { bx, by, D } = place.spot;
    ctx.save();
    if (light.offline) ctx.globalAlpha *= OFFLINE_NAME_ALPHA;
    const provider = providerOf(chip.agent, p);
    ctx.font = `700 8px ${p.mono}`;
    const pt = ellipsis(ctx, provider.label, place.maxW - 8);
    const pw = ctx.measureText(pt).width + 8;
    roundRect(ctx, bx - pw / 2, by - D / 2 - 8, pw, 10, 3);
    ctx.fillStyle = provider.fill ?? p.surface; ctx.fill();
    ctx.strokeStyle = provider.fill ? p.edge : provider.color; ctx.lineWidth = 1; ctx.stroke();
    ctx.fillStyle = provider.color; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.fillText(pt, bx, by - D / 2 - 2.5);
    ctx.shadowColor = LABEL_SHADOW; ctx.shadowBlur = 3;
    drawCoreName(ctx, {
      x: bx, nameY: place.nameY, roleY: place.roleY, maxW: place.maxW, ...this.echoText(chip), plate: null,
      nameColor: light.offline ? p.muted : LOOK.echo.color, roleColor: p.muted, font: p.font, mono: p.mono,
    }, (text, minX, maxX, cy) => drawTokenPill(ctx, text, minX, maxX, cy, p));
    ctx.restore();
  }

  /** §4–§9 in CSS px: lights, shadows, strand flows and body quads for the lit layer. */
  private litScene(views: View[], strandStates: StrandState[], p: OfficeTheme, time: number): LitScene {
    const u = this.unit;
    const c = colorsOf(p);
    const lights: PointLight[] = [];
    const shadows: Shadow[] = [];
    const bodies: BodyDraw[] = [];
    const flows: Flow[] = [];
    const lasers: Laser[] = [];
    let origin: { x: number; y: number } | null = null;
    const cores: Body[] = [];
    const calm: Rect[] = [];
    for (const chip of this.chips) {
      const body = this.bodyOf(chip);
      if (!body) continue;
      const core = this.cores.get(chip.agent.id);
      const state = core?.state ?? "idle";
      origin ??= { x: body.x * u, y: body.y * u };
      lights.push({
        x: body.x * u, y: body.y * u, z: body.s * 0.55 * u, r: LOOK.core.radius * (body.s / XL_BODY) * u,
        color: scale(c.eye, coreEyeK(state, LOOKS[state].glow, core?.lidNow ?? 0)), spec: 0.6,
      });
      // The relief and calm zones follow the layout targets (rebuilt only when it changes).
      const at = coreBody({ x: chip.tx, y: chip.ty, w: chip.w, h: chip.h })!;
      cores.push(at);
      const maxW = Math.max(40, chip.w + 40);
      calm.push({ x: at.x - maxW / 2, y: at.y + at.s / 2 + XL_PIN, w: maxW, h: 34 });
    }
    for (const { chip, spot, echo, light, status, hot, life } of views) {
      const { bx, by, D, fh } = spot;
      const grow = hot ? 1.05 : 1;
      if (echo) {
        const foot = ECHO_FOOT;
        const { intensity, pulse } = echoLight(status, time, this.reduced);
        const drift = echoDrift(time, this.reduced);
        const size = (D / foot.w) * grow;
        bodies.push({
          kind: "echo", x: (bx - foot.cx * size) * u, y: (by - foot.cy * size + drift.dy) * u, size: size * u, rot: drift.rot,
          state: c.echo, intensity, translucency: 1, alpha: LOOK.echo.alpha, desat: light.offline ? 0.75 : 0, dim: 0, pulse, height: D * 0.6 * u,
        });
        shadows.push({ x: bx * u, y: by * u, rx: D * 0.5 * u, ry: fh * 0.5 * u, lift: D * 0.55 * u, k: 0.55 });
        lights.push({
          x: bx * u, y: by * u, z: D * 0.7 * u, r: LOOK.echo.radius * (D / 40) * u,
          color: scale(c.echo, LOOK.echo.light * (intensity / LOOK.echo.glow) * (0.6 + 0.4 * pulse)), spec: 0.4,
        });
      } else {
        const foot = WORKER_FOOT;
        const sac = sacLook(light, life);
        const size = (D / foot.w) * grow * sac.size;
        bodies.push({
          kind: "worker", x: (bx - foot.cx * size) * u, y: (by - foot.cy * size) * u, size: size * u, rot: 0,
          state: c[light.tone], intensity: sac.intensity, translucency: LOOK.worker.translucency,
          alpha: sac.alpha, desat: sac.desat, dim: sac.dim, pulse: light.pulse, height: D * LOOK.worker.heightK * u,
        });
        shadows.push({ x: bx * u, y: by * u, rx: D * 0.5 * u, ry: fh * 0.5 * u, lift: D * LOOK.worker.heightK * 0.35 * u, k: LOOK.worker.shadow });
        if (light.k > 0.3 && !light.offline) {
          lights.push({
            x: bx * u, y: by * u, z: D * 0.45 * u, r: D * (0.9 + (LOOK.worker.halo - 0.9) * life) * u,
            color: scale(c[light.tone], 0.9 * light.k * (0.35 + 0.65 * light.pulse)), spec: 0.3, halo: 0.35 * sac.halo,
          });
        }
        if (lasing(light, life)) {
          lasers.push({ x: bx * u, y: by * u, r: D * LASER.orbit * u, angle: laserAngle(time, this.reduced, this.chips.indexOf(chip)) });
        }
      }
      // calm zone under the label, at the layout target (Echo beside Core: under its body)
      const place = this.echoes.get(chip.agent.id);
      const target = spotOf({ x: chip.tx, y: chip.ty, w: chip.w, h: chip.h }, spot.facing, echo);
      if (place) calm.push({ x: place.box.x, y: place.nameY - 10, w: place.box.w, h: place.roleY - place.nameY + 18 });
      else calm.push(labelRect(target, textBlock(target, chip.h, !!this.data.cost(chip.agent.id))));
    }
    // Strands: active work flows Core → worker; unread results / questions travel back as a lit drop. #2020:
    // only live strands keep the red (in their tone); the rest dry out.
    for (const { trace, job, life, tone, along } of strandStates) {
      if (along !== null) {
        const at = pointAt(trace.route, along);
        lights.push({ x: at.x * u, y: at.y * u, z: 8 * u, r: 26 * u, color: scale(c[tone], 1.1), spec: 0.5 });
      }
      flows.push({ flow: job && !this.reduced ? 1 : 0, packet: along ?? -1, length: trace.length * u, packetK: 3, life, color: c[tone] });
    }
    // The flow ids follow this order: a strand turning live re-draws the id layer.
    const order = strandStates.map((s) => `${this.traces.indexOf(s.trace)}${s.life > 0 ? "+" : ""}`).join(",");
    const strands = { key: `${this.traceKey}|${calm.map((r) => r.h).join(",")}|${order}`, routes: strandStates.map((s) => s.trace.route), cores, calm };
    return { time, reduced: this.reduced, core: origin, lights, shadows, flows, bodies, lasers, strands, flowColor: scale(c.red, 1.1) };
  }

  /** The flat stand-in for the lit layer: membrane, Core glow, strands, drops and radial bodies. */
  private drawFallback(ctx: CanvasRenderingContext2D, p: OfficeTheme, views: View[], strands: StrandState[], time: number) {
    ctx.fillStyle = LOOK.fallback.ground;
    ctx.fillRect(0, 0, this.w, this.h);
    for (const chip of this.chips) {
      const body = this.bodyOf(chip);
      if (!body) continue;
      const glow = ctx.createRadialGradient(body.x, body.y, body.s * 0.2, body.x, body.y, body.s * 2.2);
      glow.addColorStop(0, LOOK.fallback.glow); glow.addColorStop(1, "rgba(0,0,0,0)");
      ctx.fillStyle = glow;
      ctx.fillRect(body.x - body.s * 2.2, body.y - body.s * 2.2, body.s * 4.4, body.s * 4.4);
    }
    ctx.save();
    ctx.lineCap = "round"; ctx.lineJoin = "round";
    for (const { trace, life, tone, along } of strands) {
      makePath(ctx, trace.route);
      // #2020: a dormant strand is a thin dry thread; a live one a full vein lit in its colour.
      ctx.strokeStyle = life > 0 ? LOOK.fallback.strand : LOOK.fallback.dry;
      ctx.lineWidth = LOOK.strand.width * (life > 0 ? 0.7 : 0.35); ctx.stroke();
      if (life > 0) {
        ctx.save();
        ctx.strokeStyle = p[tone]; ctx.globalAlpha = 0.75 * life; ctx.lineWidth = 2.2;
        ctx.shadowColor = p[tone]; ctx.shadowBlur = 6;
        ctx.stroke();
        ctx.restore();
      }
      if (along === null) continue;
      const at = pointAt(trace.route, along);
      ctx.beginPath(); ctx.arc(at.x, at.y, 3.4, 0, Math.PI * 2);
      ctx.fillStyle = p[tone];
      ctx.fill();
    }
    ctx.restore();
    for (const view of views) {
      const { spot, echo, light, status, hot, life } = view;
      const sac = echo ? null : sacLook(light, life);
      const k = (hot ? 1.05 : 1) * (sac ? sac.size : 1);
      const rx = (spot.D / 2) * k, ry = (spot.fh / 2) * k;
      const x = spot.bx, y = spot.by + (echo ? echoDrift(time, this.reduced).dy : 0);
      if (sac && sac.halo > 0) {
        // #2020: a live sac's light on the membrane around it.
        const r = spot.D * LOOK.worker.halo;
        const halo = ctx.createRadialGradient(x, y, 0, x, y, r);
        halo.addColorStop(0, p[light.tone]); halo.addColorStop(1, "rgba(0,0,0,0)");
        ctx.save();
        ctx.globalAlpha = 0.45 * sac.halo;
        ctx.fillStyle = halo; ctx.fillRect(x - r, y - r, 2 * r, 2 * r);
        ctx.restore();
      }
      // The core's light reaches further out the stronger (and the brighter in its rhythm) it is.
      const strength = echo ? echoLight(status, time, this.reduced).intensity / 1.25 : light.offline ? 0 : Math.min(1, light.k) * (0.55 + 0.45 * light.pulse);
      const fill = ctx.createRadialGradient(x, y, 0, x, y, Math.max(rx, ry));
      fill.addColorStop(0, echo ? LOOK.echo.color : light.offline ? p.muted : p[light.tone]);
      fill.addColorStop(0.2 + 0.45 * strength, LOOK.fallback.strand);
      fill.addColorStop(1, LOOK.fallback.ground);
      ctx.save();
      ctx.globalAlpha = echo ? LOOK.echo.alpha : sac!.alpha;
      ctx.beginPath(); ctx.ellipse(x, y, rx, ry, 0, 0, Math.PI * 2);
      ctx.fillStyle = fill; ctx.fill();
      ctx.lineWidth = 1; ctx.strokeStyle = p.edge; ctx.stroke();
      ctx.restore();
      if (!echo && lasing(light, life)) drawLaser(ctx, spot, laserAngle(time, this.reduced, this.chips.indexOf(view.chip)), p.red);
    }
  }
}

/** #2036: the laser's flat stand-in — its tail as fading arcs on the orbit, the head brightest. */
function drawLaser(ctx: CanvasRenderingContext2D, spot: Spot, angle: number, color: string) {
  const rx = spot.D * LASER.orbit, ry = spot.fh * LASER.orbit;
  const steps = 6;
  ctx.save();
  ctx.lineCap = "round"; ctx.lineWidth = LASER.width * 1.6;
  ctx.strokeStyle = color; ctx.shadowColor = color; ctx.shadowBlur = 6;
  for (let i = 0; i < steps; i++) {
    const a1 = angle - (i / steps) * LASER.tail, a0 = angle - ((i + 1) / steps) * LASER.tail;
    ctx.globalAlpha = (1 - i / steps) ** 1.6;
    ctx.beginPath(); ctx.ellipse(spot.bx, spot.by, rx, ry, 0, a0, a1); ctx.stroke();
  }
  ctx.restore();
}

/** Under the labels: a soft dark halo (HANDOFF §10, with the calm zone it keeps AA). */
const LABEL_SHADOW = "rgba(0,0,0,.85)";

type Colors = Record<Tone | "eye" | "echo", Rgb>;
const colorCache = new WeakMap<OfficeTheme, Colors>();

/** The theme's colours in linear RGB, once per theme read. */
function colorsOf(p: OfficeTheme): Colors {
  let c = colorCache.get(p);
  if (!c) {
    c = {
      green: linear(p.green), redHi: linear(p.redHi), amber: linear(p.amber), red: linear(p.red), muted: linear(p.muted),
      eye: linear(p.teamHi), echo: linear(LOOK.echo.color),
    };
    colorCache.set(p, c);
  }
  return c;
}

/** Provider badge colours (patron, 2026-10-14): Anthropic brand orange, Muse blue, GLM black, local green. */
const PROVIDER_LOOK: Record<string, { color: string; fill?: string }> = {
  anthropic: { color: "#D97757" },
  "anthropic-2": { color: "#D97757" },
  muse: { color: "#3B82F6" },
  glm: { color: "#F5F5F5", fill: "#000000" },
  local: { color: "#22C55E" },
};

function providerOf(agent: Agent, p: OfficeTheme): { label: string; color: string; fill?: string } {
  const provider = agent.activeProvider ?? agent.provider ?? "anthropic";
  const label = provider === "local" ? "LOCAL" : provider === "anthropic-2" ? "ANTHROPIC 2" : provider.toUpperCase();
  return { label, ...(PROVIDER_LOOK[provider] ?? { color: p.amber }) };
}

/** #229 spec: the only theme-less constants (readable on the dark membrane). */
const PILL_FILL = "rgba(255,255,255,.06)";
const PILL_EDGE = "rgba(255,255,255,.09)";

/** #229: token pill right-aligned in [minX, maxX], ellipsized — never onto the role line. */
function drawTokenPill(ctx: CanvasRenderingContext2D, text: string, minX: number, maxX: number, cy: number, p: OfficeTheme) {
  ctx.font = `600 10px ${p.mono}`;
  const value = ellipsis(ctx, text, maxX - minX - 10);
  const w = ctx.measureText(value).width + 10;
  roundRect(ctx, maxX - w, cy - 6.5, w, 13, 4);
  ctx.fillStyle = PILL_FILL; ctx.fill();
  ctx.strokeStyle = PILL_EDGE; ctx.lineWidth = 1; ctx.stroke();
  ctx.fillStyle = p.text; ctx.textAlign = "right"; ctx.textBaseline = "middle";
  ctx.fillText(value, maxX - 5, cy + 0.5);
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

function drawClock(ctx: CanvasRenderingContext2D, x: number, y: number, radius: number, color: string) {
  ctx.save();
  ctx.strokeStyle = color; ctx.lineWidth = Math.min(1.4, radius / 2); ctx.beginPath(); ctx.arc(x, y, radius, 0, Math.PI * 2);
  ctx.moveTo(x, y - radius * 0.6); ctx.lineTo(x, y); ctx.lineTo(x + radius * 0.5, y + radius * 0.3); ctx.stroke();
  ctx.restore();
}

/** FIX-4: the dock's amber hourglass (Rack `SlowGlass`), `size` wide, centred on (cx, cy). */
function drawGlass(ctx: CanvasRenderingContext2D, cx: number, cy: number, size: number, color: string) {
  const k = size / 12;
  ctx.save();
  ctx.translate(cx - size / 2, cy - size / 2);
  ctx.scale(k, k);
  ctx.strokeStyle = color; ctx.lineWidth = 1.2; ctx.lineCap = "round";
  ctx.stroke(new Path2D("M3 1.5h6M3 10.5h6M3.5 1.5c0 3 5 3 5 4.5s-5 1.5-5 4.5M8.5 1.5c0 3-5 3-5 4.5s5 1.5 5 4.5"));
  ctx.restore();
}

function makePath(ctx: CanvasRenderingContext2D, points: Point[]) {
  ctx.beginPath(); ctx.moveTo(points[0].x, points[0].y); for (let i = 1; i < points.length; i++) ctx.lineTo(points[i].x, points[i].y);
}
