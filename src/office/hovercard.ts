import { getAgentError, getStatus, type Agent, type AgentStatus } from "../agents";
import { getDelegations, lastResultLine, unreadResult } from "../delegations";
import { getQuestions, workerQuestion } from "../questions";
import { stallText, stalledFor } from "../stall";
import { slowReply } from "../firstReply";
import { agentToday } from "../usage/costStore";
import { breakdownLine, costText, warningText } from "../usage/costFormat";
import { agentProjectName, jobProjectName } from "../project/store";

const STATUS_TEXT: Record<AgentStatus, string> = {
  offline: "kapalı",
  idle: "boşta",
  working: "çalışıyor",
  waiting: "bekliyor",
};

const STATUS_COLOR: Record<AgentStatus, string> = {
  offline: "#5a5f66",
  idle: "#9aa0a8",
  working: "#e5383b",
  waiting: "#e5383b",
};

const KIND_TEXT: Record<string, string> = { leader: "Lider", worker: "Worker" };

/** DEX-13: non-anthropic agents show which provider serves their model. */
const PROVIDER_TEXT: Record<string, string> = { glm: "GLM", muse: "Muse", local: "Yerel LLM" };

function fmtDur(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}d ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}sa ${String(m % 60).padStart(2, "0")}d`;
}

/**
 * Chip hover card as a DOM overlay (nothing is drawn onto the canvas).
 * `update` is called every frame from the render loop: the card follows the
 * chip and flips to the other side when it would cross the canvas edge.
 * pointer-events: none keeps activate/focus clicks on the canvas.
 */
export class HoverCard {
  private el: HTMLDivElement;
  private dot: HTMLElement;
  private name: HTMLElement;
  private kind: HTMLElement;
  private rows = new Map<string, { row: HTMLElement; value: HTMLElement }>();
  private since = new Map<string, number>(); // agentId → when we last saw it non-offline
  private lastKey = "";
  private w = 0;
  private h = 0;
  private visible = false;

  constructor(parent: HTMLElement) {
    this.el = document.createElement("div");
    this.el.className = "office-card";
    const head = document.createElement("div");
    head.className = "oc-head";
    this.dot = document.createElement("i");
    this.dot.className = "oc-dot";
    this.name = document.createElement("b");
    this.kind = document.createElement("span");
    this.kind.className = "oc-kind";
    head.append(this.dot, this.name, this.kind);
    const grid = document.createElement("div");
    grid.className = "oc-grid";
    for (const key of ["Rol", "Model", "Proje", "Durum", "Süre", "Bugün", "Kırılım", "Devretme", "Soru", "Uyarı"]) {
      const row = document.createElement("div");
      row.className = key === "Uyarı" ? "oc-row warn" : "oc-row";
      const k = document.createElement("span");
      k.className = "oc-k";
      k.textContent = key;
      const v = document.createElement("span");
      v.className = "oc-v";
      row.append(k, v);
      grid.append(row);
      this.rows.set(key, { row, value: v });
    }
    this.el.append(head, grid);
    this.el.style.display = "none";
    parent.appendChild(this.el);
  }

  /** Screen-space anchor = character mid-top; boxW/boxH = overlay host size in CSS px. */
  update(agent: Agent | null, x: number, y: number, boxW: number, boxH: number) {
    if (!agent) {
      this.hide();
      return;
    }
    const status = getStatus(agent.id);
    if (status === "offline") this.since.delete(agent.id);
    else if (!this.since.has(agent.id)) this.since.set(agent.id, Date.now());
    const since = this.since.get(agent.id);

    const dur = since ? fmtDur(Date.now() - since) : "";
    // Rebuild text only when something actually changed; position updates every frame.
    // DEX-28: a stall mark joins the warning row (the provider error, if any, stays first).
    const stall = stalledFor(agent.id);
    const slow = slowReply(agent.id); // FIX-3
    const back = agent.chainReturnReady ? `${providerText(agent.provider)} yeniden kullanılabilir — geri dön` : null;
    const error = [getAgentError(agent.id), back, slow && `⏳ ${slow}`, stall === null ? null : `⏱ ${stallText(stall)}`].filter(Boolean).join("\n") || null;
    const result = agent.kind === "worker" ? lastResultLine(getDelegations(), agent.id, jobProjectName) : null; // DEX-19
    const project = agentProjectName(agent, getDelegations()); // #838: only where the team has several
    // #232: the chip's error state (unread failed result, dashed ring) also turns the card head red.
    const failed = unreadResult(getDelegations(), agent.id)?.status === "failed";
    const q = agent.kind === "worker" ? workerQuestion(getQuestions(), agent.id) : null; // DEX-24
    const ask = q ? `${q.status === "escalated" ? "Patrona çıktı" : "Lidere sordu"}: ${q.question}` : null;
    // #209: today's API-equivalent cost; hidden with no tokens (or an old dexd).
    const today = agentToday(agent.id);
    const spent = costText(today?.total);
    const parts = spent ? `${breakdownLine(today!.total.tokens)}${warningText(today!.warnings) ? " · eksik olabilir" : ""}` : "";
    const key = `${spent}|${parts}|${agent.id}|${status}|${agent.kind}|${agent.roleName}|${agent.title}|${agent.engine}|${agent.provider}|${agent.model}|${agent.activeEngine}|${agent.activeProvider}|${agent.activeModel}|${dur}|${error}|${result}|${ask}|${failed}|${project}`;
    if (key !== this.lastKey) {
      this.lastKey = key;
      this.dot.style.background = STATUS_COLOR[status];
      this.dot.classList.toggle("blink", status === "waiting");
      this.el.classList.toggle("error", failed);
      this.name.textContent = agent.name;
      this.kind.textContent = KIND_TEXT[agent.kind] ?? "";
      this.kind.style.display = this.kind.textContent ? "" : "none";
      this.rows.get("Rol")!.value.textContent = agent.roleName || agent.title || "—";
      // DEX-26: motor · model; a codex agent's provider is not used.
      const engine = agent.activeEngine ?? agent.engine;
      const provider = agent.activeProvider ?? agent.provider;
      const model = agent.activeModel ?? agent.model;
      const prov = engine === "codex" ? "Codex" : PROVIDER_TEXT[provider];
      this.rows.get("Model")!.value.textContent = prov ? `${prov} · ${model}` : model || "—";
      const proj = this.rows.get("Proje")!;
      proj.row.style.display = project ? "" : "none";
      proj.value.textContent = project ?? "";
      this.rows.get("Durum")!.value.textContent = STATUS_TEXT[status];
      const session = this.rows.get("Süre")!;
      session.row.style.display = since ? "" : "none";
      session.value.textContent = dur;
      const bugun = this.rows.get("Bugün")!;
      bugun.row.style.display = spent ? "" : "none";
      bugun.value.textContent = spent && `${spent} · API eşdeğeri`;
      const kirilim = this.rows.get("Kırılım")!;
      kirilim.row.style.display = spent ? "" : "none";
      kirilim.value.textContent = parts;
      const deleg = this.rows.get("Devretme")!;
      deleg.row.style.display = result ? "" : "none";
      deleg.value.textContent = result ?? "";
      const soru = this.rows.get("Soru")!;
      soru.row.style.display = ask ? "" : "none";
      soru.value.textContent = ask ?? "";
      const warn = this.rows.get("Uyarı")!;
      warn.row.style.display = error ? "" : "none";
      warn.value.textContent = error ?? "";
      this.w = 0; // content changed → re-measure
    }

    this.el.style.display = "";
    this.visible = true;
    if (!this.w) {
      this.w = this.el.offsetWidth;
      this.h = this.el.offsetHeight;
    }
    const GAP = 12; // character → card
    const PAD = 6; // canvas edge breathing room
    let left = x + GAP; // prefer the right side
    if (left + this.w > boxW - PAD) left = x - GAP - this.w; // flip at the right edge
    left = Math.min(Math.max(left, PAD), Math.max(PAD, boxW - this.w - PAD));
    let top = y - this.h - GAP; // above the character
    if (top < PAD) top = Math.min(y + GAP, Math.max(PAD, boxH - this.h - PAD)); // below if clipped
    this.el.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
  }

  hide() {
    if (this.visible) {
      this.el.style.display = "none";
      this.visible = false;
    }
  }

  destroy() {
    this.el.remove();
  }
}

function providerText(id: string) {
  return PROVIDER_TEXT[id] ?? (id === "anthropic" ? "Anthropic" : id);
}
