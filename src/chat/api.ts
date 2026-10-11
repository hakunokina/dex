// S5-B: chat over the agent's own session transcript (crates/dex-core/src/chat.rs).
// Same door as everything else: `callHost` + `onEvent` (src/api/hosts.ts). Ids are qualified
// (`<host>:<id>`); the host is always called with its bare id.
import { callHost, hostName, hostSupports, onEvent, splitId } from "../api/hosts";
import type { Unlisten } from "../api/transport";

export interface ToolCard {
  name: string;
  input: string;
  result?: string | null;
  /** `undefined` = running, `true` = ✓, `false` = ✗. */
  ok?: boolean | null;
}

export interface ChatMessage {
  id: string;
  /** 1-based transcript row — the paging cursor. */
  seq: number;
  role: string;
  text: string;
  thinking?: boolean;
  ts?: number | null;
  tool?: ToolCard | null;
}

export interface ChatPage {
  messages: ChatMessage[];
  cursor?: number | null;
  hasMore: boolean;
}

/** `chat.send` outcome: `started` (agent was offline), `sent` (pasted), `queued` (busy). */
export type ChatSendResult = "started" | "sent" | "queued";

/** #142: the host's `auth.hello` lists its methods; an older dexd has no chat (pre-S5-B). */
export const chatSupported = (agentId: string) => hostSupports(splitId(agentId)[0], "chat_history");

export function chatHistory(agentId: string, before?: number, limit = 50): Promise<ChatPage> {
  const [host, raw] = splitId(agentId);
  if (!chatSupported(agentId)) return Promise.reject(`${hostName(host)}: bu sunucu sohbeti desteklemiyor (eski dexd) — Terminal görünümünü kullanın`);
  return callHost<ChatPage>(host, "chat_history", { agentId: raw, before: before ?? null, limit });
}

export function chatSend(agentId: string, text: string): Promise<ChatSendResult> {
  const [host, raw] = splitId(agentId);
  return callHost<ChatSendResult>(host, "chat_send", { agentId: raw, text });
}

export function chatScreen(agentId: string): Promise<string> {
  const [host, raw] = splitId(agentId);
  return callHost<string>(host, "chat_screen", { agentId: raw });
}

export interface ChatIncoming {
  agentId: string;
  message: ChatMessage;
}

/** Live transcript rows (`chat-message`), tool patches included. Qualified agent id. */
export function onChatMessage(fn: (m: ChatIncoming, host: string) => void): Unlisten {
  return onEvent<{ agentId: string; message: ChatMessage }>("chat-message", (e, host) => {
    const p = e.payload;
    fn({ agentId: host === "local" ? p.agentId : `${host}:${p.agentId}`, message: p.message }, host);
  });
}

/** The agent switched transcripts (new session / truncation) — refetch history. */
export function onChatReset(fn: (agentId: string, host: string) => void): Unlisten {
  return onEvent<{ agentId: string }>("chat-reset", (e, host) => {
    const raw = e.payload.agentId;
    fn(host === "local" ? raw : `${host}:${raw}`, host);
  });
}

/** Queued outgoing texts (`chat-queue`) — the "sırada" view. */
export function onChatQueue(fn: (agentId: string, messages: string[], host: string) => void): Unlisten {
  return onEvent<{ agentId: string; messages: string[] }>("chat-queue", (e, host) => {
    const p = e.payload;
    fn(host === "local" ? p.agentId : `${host}:${p.agentId}`, p.messages ?? [], host);
  });
}
