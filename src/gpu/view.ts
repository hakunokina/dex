// #290: the GPU broker as dexd reports it (crates/dex-core/src/gpu.rs, RPCs gpu_*) and the patron's
// wording of it. Pure: unit-tested (view.test.ts).

export interface GpuConfig {
  primary: string;
  fallback: string;
  tokenSet: boolean;
}
export interface GpuModel {
  name: string;
  /** Served: its task (`t2i`, `llm`…); disk-only: the disk category (`image`, `tts`…). */
  type: string;
  engine: string | null;
  path: string | null;
  /** Agents may use it through a recipe (with the patron's grant). */
  agent: boolean;
  /** Served on the machine (the patron's when not `agent`). */
  serve: boolean;
  disk: boolean;
  recipes: string[];
}
export interface GpuStatus {
  owner: string | null;
  llmModel: string | null;
  queued: number;
  runningJob: string | null;
  dailyUsed: number;
  dailyLimit: number;
}
export interface GpuOverview {
  tokenSet: boolean;
  connected: boolean;
  authorized: boolean;
  via: "primary" | "fallback" | null;
  url: string | null;
  models: GpuModel[];
  status: GpuStatus | null;
  error: string | null;
  /** #305: what `error` is — auth = 401; service = broker answered but models/status failed (5xx, timeout). */
  errorKind?: "unreachable" | "noToken" | "auth" | "service" | null;
  checkedAt: number;
}
export interface GpuRequest {
  id: number;
  teamId: string;
  leader: string;
  leaderName: string;
  worker: string;
  workerName: string;
  types: string[];
  reason: string;
  budget: number;
  status: "pending" | "approved" | "rejected";
  grantId: number | null;
  createdAt: number;
  decidedAt: number | null;
}

export type GpuTone = "ok" | "warn" | "down";

/** One status line: bağlı / bağlanamadı / token yok ya da yanlış / broker hatası, with the address that answered.
 *  Only a 401 says "token yanlış"; a broker that answers /health but fails the model list shows its error. */
export function connectionLine(o: GpuOverview): { tone: GpuTone; text: string } {
  const where = o.url ? ` (${o.via === "fallback" ? "yedek" : "birincil"}: ${o.url})` : "";
  const why = o.error ? ` — ${o.error}` : "";
  if (!o.connected) return { tone: "down", text: `bağlanamadı${why}` };
  if (!o.tokenSet) return { tone: "warn", text: `token yok — broker yanıt veriyor${where}` };
  if (!o.authorized && o.errorKind === "auth") return { tone: "warn", text: `token yanlış${where}` };
  if (!o.authorized) return { tone: "down", text: `broker yanıt veriyor, modeller alınamadı${where}${why}` };
  if (o.error) return { tone: "warn", text: `bağlı${where} — durum alınamadı: ${o.error}` };
  return { tone: "ok", text: `bağlı${where}` };
}

/** The three disjoint groups the patron sees (same split as dexd's `gpu_status`). */
export function modelGroups(models: GpuModel[]) {
  return {
    agent: models.filter((m) => m.agent),
    served: models.filter((m) => m.serve && !m.agent),
    diskOnly: models.filter((m) => !m.serve && !m.agent),
  };
}

export interface GpuLimits {
  daily_image_limit?: number | null;
  max_pending_jobs?: number | null;
  max_count_per_request?: number | null;
}
export type GpuLimitsForm = Record<keyof GpuLimits, string>;
export function limitsPatch(form: GpuLimitsForm, saved: GpuLimits = {}): Partial<GpuLimits> {
  const limits: Partial<GpuLimits> = {};
  for (const key of Object.keys(form) as (keyof GpuLimits)[]) {
    if (!form[key].trim()) continue;
    const n = Number(form[key]);
    if (n === saved[key]) continue;
    if (!Number.isSafeInteger(n) || n < 1 || (key === "max_count_per_request" && n > 4)) {
      throw new Error("Limitler pozitif tam sayı olmalı; istek başına en çok 4.");
    }
    limits[key] = n;
  }
  if (!Object.keys(limits).length) throw new Error("En az bir limiti değiştir.");
  return limits;
}

/** Broker'dan okunamayan günlük limit için geri dönüş (görsel; dex-core gpu.rs MAX_BUDGET ile aynı). */
export const GPU_BUDGET_MAX = 240;
/** Metin birimli türler (dex-core gpu.rs TEXT_TYPES ile aynı küme) ve tek tavan. */
export const GPU_TEXT_TYPES = ["embedding", "rerank"];
export const GPU_TEXT_BUDGET_MAX = 50000;
/** İzin türlerine göre bütçe birimi: metin/belge ya da görsel adedi. */
export function budgetUnit(types: string[]): "metin" | "görsel" {
  return types.some((t) => GPU_TEXT_TYPES.includes(t)) ? "metin" : "görsel";
}
export function budgetMax(limit?: number | null, types: string[] = []): number {
  if (budgetUnit(types) === "metin") return GPU_TEXT_BUDGET_MAX;
  return limit != null && Number.isSafeInteger(limit) && limit > 0 ? limit : GPU_BUDGET_MAX;
}
export function parseBudget(raw: string, fallback: number, limit?: number | null, types: string[] = []): number {
  const n = Math.round(Number(raw));
  return Math.max(1, Math.min(budgetMax(limit, types), !raw.trim() || !Number.isFinite(n) ? fallback : n));
}

/** Only the fields the patron changed; an untouched token field keeps the stored one. */
export function configPatch(saved: GpuConfig, form: { primary: string; fallback: string; token: string }) {
  const patch: { primary?: string; fallback?: string; token?: string } = {};
  if (form.primary.trim() !== saved.primary) patch.primary = form.primary.trim();
  if (form.fallback.trim() !== saved.fallback) patch.fallback = form.fallback.trim();
  if (form.token.trim()) patch.token = form.token.trim();
  return patch;
}

/** #311: the usage strip's GPU cell. Null = no broker configured (no token) → the cell is not drawn. */
export interface GpuStripCell {
  owner: "boş" | "LLM" | "iş" | "--";
  queued: number | null;
  daily: string;
  tone: GpuTone;
  title: string;
}
export function gpuStripCell(o: GpuOverview | null): GpuStripCell | null {
  if (!o?.tokenSet) return null;
  const s = o.authorized ? o.status : null;
  if (!s) return { owner: "--", queued: null, daily: "--", tone: "down", title: `GPU — ${connectionLine(o).text}` };
  const owner = s.runningJob ? "iş" : s.owner === "llm" ? "LLM" : !s.owner || /^(idle|none|free)$/.test(s.owner) ? "boş" : "iş";
  const daily = `${s.dailyUsed}/${s.dailyLimit}`;
  const who = owner === "LLM" ? `LLM'in${s.llmModel ? ` (${s.llmModel})` : ""}` : owner === "iş" ? "ajan işi" : "boş";
  return {
    owner,
    queued: s.queued,
    daily,
    tone: s.dailyLimit > 0 && s.dailyUsed >= s.dailyLimit ? "warn" : "ok",
    title: `GPU: ${who} · kuyrukta ${s.queued} iş · bugün ${daily} görsel`,
  };
}

/** #311: a worker's GPU job waits for the patron's LLM (`gpu-waiting` from dexd). */
export interface GpuWaiting {
  agent: string;
  agentName: string;
  teamId: string;
  jobId: string;
  waitReason: string | null;
  etaS: number | null;
}
export function waitingToastText(name: string, etaS: number | null): { title: string; body: string } {
  const eta = etaS !== null && etaS > 0 ? ` (en erken ~${Math.max(1, Math.round(etaS / 60))} dk)` : "";
  return { title: `${name} GPU bekliyor`, body: `LLM'in boşa çıkınca başlayacak${eta}` };
}
