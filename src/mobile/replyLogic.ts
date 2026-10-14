// #1719 Echo cevapları (telefon): bildirim derin bağlantısı, push jetonu kayıt kuralı ve cevap
// markdown'ının güvenli görünümü. Saf mantık (birim testli: replyLogic.test.ts). Sözleşme (Kernel-1):
// `inbox_replies { teamId, limit? }`, `inbox_reply_get { id }`, `push_register { token, platform }`,
// olay `inbox-reply { id, teamId }`, FCM data `{ type: "inbox_reply", id }`. İçerik loglanmaz.
import { inline, renderMarkdown } from "../chat/md.ts";
import type { ReplySummary, ReplyTask, ReplyId } from "../api/replies.ts";

const TASK_LABELS: Record<ReplyTask, string> = { repo: "Repo incele", verify: "Kaynak doğrula", ask: "Soru" };

export const replyTaskLabel = (t: unknown): string | null =>
  typeof t === "string" && Object.prototype.hasOwnProperty.call(TASK_LABELS, t) ? TASK_LABELS[t as ReplyTask] : null;

// ---- bildirim derin bağlantısı ----

/** Kotlin `DexMessagingService` bildirimi `dex://reply/<id>` ile açar (`PushPlugin.take`). */
export const REPLY_LINK_PREFIX = "dex://reply/";
const ID = /^[A-Za-z0-9_-]{1,64}$/;

/** `dex://reply/<id>` → id (tam sayı ise number, çekirdekte satır kimliği); başka her şey null. */
export function parseReplyLink(uri: unknown): ReplyId | null {
  if (typeof uri !== "string" || !uri.startsWith(REPLY_LINK_PREFIX)) return null;
  const id = uri.slice(REPLY_LINK_PREFIX.length);
  if (!ID.test(id)) return null;
  return /^\d{1,15}$/.test(id) ? Number(id) : id;
}

// ---- push jetonu kaydı ----

/**
 * Bu hosta `push_register` gönderilmeli mi? Uzak host, jeton var, host metodu ilan etmiş (ya da
 * metot listesi bilinmiyor — eski dexd "bilinmeyen metot" der, sessizce yutulur) ve bu oturumda o
 * jeton o hosta henüz gitmedi.
 */
export function pushRegisterNeeded(f: {
  host: string;
  token: string | null;
  methods: readonly string[] | null;
  sentToken: string | undefined;
}): boolean {
  if (f.host === "local" || !f.token) return false;
  if (f.methods && !f.methods.includes("push_register")) return false;
  return f.sentToken !== f.token;
}

// ---- cevap görünümü ----

/** Yalnız http/https bağlantı kalır; `md.ts`'in izin verdiği `mailto:` metne döner. */
const NON_WEB_LINK = /<a href="(?!https?:\/\/)[^"]*"[^>]*>(.*?)<\/a>/gi;
export const isWebUrl = (u: string | null | undefined) => !!u && /^https?:\/\//i.test(u);

/**
 * Cevabın ilk anlamlı satırı (tek cümlelik sonuç) vurgulu kart, gerisi gövde. Ham HTML hiçbir yerde
 * çalışmaz: `md.ts` her şeyi önce kaçışlar; bağlantılar yalnız http/https.
 */
export function replyView(markdown: string): { lead: string | null; body: string } {
  const lines = markdown.replace(/\r\n?/g, "\n").split("\n");
  const first = lines.findIndex((l) => l.trim() !== "");
  let lead: string | null = null;
  let rest = markdown;
  // Kod bloğu, tablo ya da liste ile başlıyorsa kart yok: bütün metin gövdede.
  if (first >= 0 && !/^\s*(```|\||[-*+]\s|\d+[.)]\s|[+┌│])/.test(lines[first])) {
    const text = lines[first].replace(/^\s*(#{1,6}\s+|>\s?)/, "").trim();
    if (text) {
      lead = safeLinks(inline(text));
      rest = lines.slice(first + 1).join("\n");
    }
  }
  return { lead, body: safeLinks(renderMarkdown(rest)) };
}

const safeLinks = (html: string) => html.replace(NON_WEB_LINK, "$1");

/** `createdAt` saniye ya da ms gelebilir → ms. */
export const toMs = (t: number) => (t > 0 && t < 1e12 ? t * 1000 : t);

/** Liste/başlık zamanı: bugün "14:05", dün "Dün 14:05", daha eski "12 Eki 14:05". */
export function replyTime(createdAt: number, now: number = Date.now()): string {
  const d = new Date(toMs(createdAt));
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(new Date(now)) - day(d)) / 86_400_000);
  if (diff === 0) return hm;
  if (diff === 1) return `Dün ${hm}`;
  const MONTHS = ["Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara"];
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${hm}`;
}

/** Birden çok host/takımdan gelen listeleri tek, yeni → eski listeye çevirir (aynı host+id bir kez). */
export function mergeReplies<T extends ReplySummary & { host: string }>(lists: readonly T[][], limit = 50): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const r of lists.flat().sort((a, b) => toMs(b.createdAt) - toMs(a.createdAt))) {
    const k = `${r.host}\u0000${r.id}`;
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(r);
  }
  return out.slice(0, limit);
}
