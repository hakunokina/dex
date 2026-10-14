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

/** Gönderilebilir mi; değilse kullanıcıya gösterilecek neden. */
export function validateSubmit(text: string, image: SubmitImage | null, teamId: string | null): string | null {
  if (!teamId) return "Bu sunucuda asistan yok";
  if (!text.trim() && !image) return "Gönderilecek bir şey yok";
  if (utf8Bytes(text.trim()) > TEXT_MAX_BYTES) return "Metin çok uzun (en fazla 8 KB)";
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

/** `inbox_submit` cevabı → kısa sonuç metni. */
export function resultText(r: { queued?: boolean } | null | undefined): string {
  return r?.queued ? "Sıraya alındı" : "Asistana gönderildi";
}
