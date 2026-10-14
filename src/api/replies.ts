// #1719 Echo cevapları — host RPC'leri (Kernel-1 sözleşmesi, dexd): `inbox_replies`, `inbox_reply_get`,
// `push_register`; canlı olay `inbox-reply { id, teamId }`. `teamId` hosttaki ham kimliktir. İçerik loglanmaz.
import { callHost } from "./hosts";

export type ReplyTask = "repo" | "verify" | "ask";
/** Çekirdekte satır kimliği (bildirimden gelen derin bağlantı da aynı değeri taşır). */
export type ReplyId = number | string;

export interface ReplySummary {
  id: ReplyId;
  task: ReplyTask | null;
  /** Paylaşılan içeriğin kısa önizlemesi. */
  preview: string;
  createdAt: number;
  agentId: string;
  agentName: string;
}

export interface Reply extends ReplySummary {
  markdown: string;
}

export const INBOX_REPLY_EVENT = "inbox-reply";

export const listReplies = (host: string, teamId: string, limit = 30) =>
  callHost<ReplySummary[]>(host, "inbox_replies", { teamId, limit });

export const getReply = (host: string, id: ReplyId) => callHost<Reply>(host, "inbox_reply_get", { id });

export const pushRegister = (host: string, token: string) =>
  callHost<unknown>(host, "push_register", { token, platform: "android" });
