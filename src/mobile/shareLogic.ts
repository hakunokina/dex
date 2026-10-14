// Android "Paylaş" hedefi: paylaşılan veriyi `inbox_submit` girdisine çeviren, sınırları denetleyen
// saf mantık (birim testli: shareLogic.test.ts). Sözleşme: dexd `inbox_submit` (Kernel-1) —
// { teamId, text?, image?: { mime, base64 }, clientId? }; en az biri metin/görsel; WS mesajı ≤ 1 MB.
// İçerik hiçbir yerde loglanmaz.

export const TEXT_MAX_BYTES = 8 * 1024;
/** Ham görsel üst sınırı; base64'lü tüm mesaj WS'in 1 MB sınırına sığmalı (lider notu #33). */
export const IMAGE_MAX_BYTES = 700 * 1024;
export const IMAGE_MAX_EDGE = 1600;
/** Küçültmede denenen JPEG kaliteleri (ilki ilk deneme, sonrakiler "bir kez daha" düşürme). */
export const IMAGE_QUALITIES: readonly number[] = [0.8, 0.6, 0.4];
export const SUBMIT_MIMES = ["image/png", "image/jpeg", "image/webp"] as const;
export type SubmitMime = (typeof SUBMIT_MIMES)[number];

/** Kotlin `ShareTargetPlugin.take` cevabı; hepsi isteğe bağlı, paylaşım yoksa `{}`. */
export interface RawShare {
  text?: string;
  subject?: string;
  image?: { mime: string; base64: string };
  error?: string;
  /** #1717: hangi Paylaş girişinden gelindi (manifest activity-alias'ı); yok/bilinmeyen = "ask". */
  task?: string;
}

/** #1717: Paylaş menüsündeki üç Dex girişi. `line` gönderilen metnin ilk satırıdır (asistanın rol metni bunu okur). */
export type ShareTask = "repo" | "verify" | "ask";
export const SHARE_TASKS: readonly { id: ShareTask; label: string; line: string }[] = [
  { id: "repo", label: "Repo incele", line: "Görev: Repo incele" },
  { id: "verify", label: "Kaynak doğrula", line: "Görev: Kaynak doğrula" },
  { id: "ask", label: "Asistana sor", line: "Görev: Soru" },
];

export const shareTask = (t: unknown): ShareTask => (t === "repo" || t === "verify" ? t : "ask");

/** Gönderilecek metin: görev satırı + (varsa) kullanıcının metni. */
export function taskText(task: ShareTask, text: string): string {
  const line = SHARE_TASKS.find((t) => t.id === task)!.line;
  const t = text.trim();
  return t ? `${line}\n${t}` : line;
}

export interface SubmitImage {
  mime: SubmitMime;
  base64: string;
}

export interface InboxSubmit {
  teamId: string;
  text?: string;
  image?: SubmitImage;
  clientId: string;
}

export const hasShare = (r: RawShare | null | undefined): r is RawShare =>
  !!r && !!(r.text || r.subject || r.image || r.error);

/** Düzenlenebilir başlangıç metni: paylaşılan metin; yoksa konu satırı. */
export function initialText(r: RawShare): string {
  return (r.text?.trim() || r.subject?.trim() || "").trim();
}

export const utf8Bytes = (s: string) => new TextEncoder().encode(s).length;

/** Base64 dizgesinin çözülmüş bayt sayısı. */
export function base64Bytes(b64: string): number {
  const pad = b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0;
  return Math.floor((b64.length * 3) / 4) - pad;
}

export const isSubmitMime = (m: string): m is SubmitMime => (SUBMIT_MIMES as readonly string[]).includes(m);

/** Uzun kenarı `max`'a indirir (oranı korur); zaten sığıyorsa aynı boyut. */
export function scaleToFit(w: number, h: number, max = IMAGE_MAX_EDGE): { width: number; height: number } {
  const long = Math.max(w, h);
  if (long <= max) return { width: w, height: h };
  const k = max / long;
  return { width: Math.max(1, Math.round(w * k)), height: Math.max(1, Math.round(h * k)) };
}

/** Görsel olduğu gibi gidebilir mi: izinli MIME, bayt sınırı içinde, kenar sınırı içinde. */
export function imageFitsAsIs(mime: string, rawBytes: number, w: number, h: number): boolean {
  return isSubmitMime(mime) && rawBytes <= IMAGE_MAX_BYTES && Math.max(w, h) <= IMAGE_MAX_EDGE;
}

/**
 * Kalite merdiveninde ilk sınır altı sonucu döner; hiçbiri sığmazsa null ("görsel çok büyük").
 * `encode(q)` kodlanmış sonucu ve ham bayt sayısını verir.
 */
export async function fitUnderLimit<T>(
  encode: (quality: number) => Promise<{ value: T; bytes: number }>,
  limit = IMAGE_MAX_BYTES,
  qualities: readonly number[] = IMAGE_QUALITIES,
): Promise<T | null> {
  for (const q of qualities) {
    const r = await encode(q);
    if (r.bytes <= limit) return r.value;
  }
  return null;
}

/**
 * Gönderilebilir mi; değilse kullanıcıya gösterilecek neden. `task` verilirse boyut sınırı görev
 * satırıyla birlikte gidecek metne uygulanır (boşluk denetimi yine kullanıcının metnine).
 */
export function validateSubmit(text: string, image: SubmitImage | null, teamId: string | null, task?: ShareTask): string | null {
  if (!teamId) return "Bu sunucuda asistan yok";
  if (!text.trim() && !image) return "Gönderilecek bir şey yok";
  if (utf8Bytes(task ? taskText(task, text) : text.trim()) > TEXT_MAX_BYTES) return "Metin çok uzun (en fazla 8 KB)";
  if (image) {
    if (!isSubmitMime(image.mime)) return "Görsel biçimi desteklenmiyor";
    if (base64Bytes(image.base64) > IMAGE_MAX_BYTES) return "Görsel çok büyük";
  }
  return null;
}

/** `inbox_submit` girdisi. `clientId` paylaşım başına bir kez üretilir; yeniden denemede aynı gider. */
export function buildSubmit(teamId: string, text: string, image: SubmitImage | null, clientId: string): InboxSubmit {
  const t = text.trim();
  const out: InboxSubmit = { teamId, clientId };
  if (t) out.text = t;
  if (image) out.image = image;
  return out;
}

/** Asistanı (role_slug "assistant") olan takımlar: `teams` sırasıyla, tekrarsız. */
export function assistantTeams<T extends { id: string }>(
  teams: readonly T[],
  agents: readonly { teamId: string; roleSlug: string | null }[],
): T[] {
  const withAssistant = new Set(agents.filter((a) => a.roleSlug === "assistant").map((a) => a.teamId));
  return teams.filter((t) => withAssistant.has(t.id));
}

/** Takımın (ilk) asistanının adı; yoksa null. */
export function assistantName(teamId: string, agents: readonly { teamId: string; roleSlug: string | null; name: string }[]): string | null {
  return agents.find((a) => a.teamId === teamId && a.roleSlug === "assistant")?.name ?? null;
}

/**
 * #1717 hızlı gönderim: hedef belirsiz değilse ekran açılmadan gönderilir. Sıra önemli — ilk eksik
 * koşul döner; sürücü (shareFast.ts) bilinmeyen alanları karamsar varsayılanla verir.
 * `offline` dışındaki nedenleri ShareScreen kendisi gösterir (seçici, "asistan yok", doğrulama).
 */
export type FastWhy = "no-host" | "many-hosts" | "offline" | "unsupported" | "no-assistant" | "many-teams" | "invalid";
export interface FastFacts {
  hostIds: readonly string[];
  /** Tek host süre içinde bağlandı mı. */
  connected: boolean;
  supportsInbox: boolean;
  /** Asistanlı takımlar (asistan adıyla). */
  targets: readonly { id: string; assistant: string }[];
  /** validateSubmit ya da görsel hazırlama hatası; null = gönderilebilir. */
  problem: string | null;
}
export type FastPlan = { send: true; host: string; teamId: string; assistant: string } | { send: false; why: FastWhy };

export function fastSendPlan(f: FastFacts): FastPlan {
  if (f.hostIds.length === 0) return { send: false, why: "no-host" };
  if (f.hostIds.length > 1) return { send: false, why: "many-hosts" };
  if (!f.connected) return { send: false, why: "offline" };
  if (!f.supportsInbox) return { send: false, why: "unsupported" };
  if (f.targets.length === 0) return { send: false, why: "no-assistant" };
  if (f.targets.length > 1) return { send: false, why: "many-teams" };
  if (f.problem) return { send: false, why: "invalid" };
  return { send: true, host: f.hostIds[0], teamId: f.targets[0].id, assistant: f.targets[0].assistant };
}

const BACK_VOWELS = "aıouâû";
const VOWELS = BACK_VOWELS + "eiöüî";

/** Türkçe yönelme hali, özel adla: Echo → Echo'ya, Kaan → Kaan'a, Defne → Defne'ye. Ünlü yoksa null. */
export function dative(name: string): string | null {
  const n = name.trim();
  const lower = n.toLocaleLowerCase("tr");
  const vowels = [...lower].filter((c) => VOWELS.includes(c));
  if (!vowels.length) return null;
  const suffix = BACK_VOWELS.includes(vowels[vowels.length - 1]) ? "a" : "e";
  const buffer = VOWELS.includes(lower[lower.length - 1]) ? "y" : "";
  return `${n}'${buffer}${suffix}`;
}

/** Hızlı gönderim sonrası Toast metni; içerik taşımaz, kısa (Kotlin ayrıca 80 karakterde keser). */
export function sentToast(assistant: string, queued: boolean): string {
  const to = dative(assistant.trim().slice(0, 30)) ?? "Asistana";
  return queued ? `${to} gönderildi (sıraya alındı)` : `${to} gönderildi`;
}

/**
 * Paylaşım sunucusu: kullanıcının açık seçimi (hâlâ kayıtlıysa) > güncel varsayılan (aktif takımın
 * sunucusu; soğuk açılışta sonradan gelebilir) > ilk kayıtlı. Seçim state'te tutulmaz, her render türetilir.
 */
export function resolveShareHost(hostIds: readonly string[], chosen: string | null, defaultHost: string | null): string | null {
  if (chosen && hostIds.includes(chosen)) return chosen;
  if (defaultHost && hostIds.includes(defaultHost)) return defaultHost;
  return hostIds[0] ?? null;
}

/** Hedef araması başarısızken seçili sunucu (yeniden) bağlandıysa listeyi yeniden yükle. */
export function shouldReloadTargets(upHost: string, selectedHost: string | null, lookupFailed: boolean): boolean {
  return lookupFailed && upHost === selectedHost;
}

/** `inbox_submit` cevabı → kısa sonuç metni. */
export function resultText(r: { queued?: boolean } | null | undefined): string {
  return r?.queued ? "Sıraya alındı" : "Asistana gönderildi";
}
