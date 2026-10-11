// Chat feed shaping: transcript rows → display items. Pure.
import type { ChatMessage } from "./api.ts";

export type FeedItem =
  | { kind: "day"; key: string; ts: number }
  | { kind: "msg"; key: string; m: ChatMessage }
  /** A run of consecutive tool calls (and thinking) collapsed into one "N araç" group. */
  | { kind: "activity"; key: string; tools: ChatMessage[]; thoughts: ChatMessage[]; running: boolean; failed: number };

const isActivity = (m: ChatMessage) => !!m.tool || !!m.thinking;

export function dayKey(ts: number): string {
  const d = new Date(ts);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

export function buildFeed(messages: ChatMessage[]): FeedItem[] {
  const out: FeedItem[] = [];
  let day = "";
  let run: Extract<FeedItem, { kind: "activity" }> | null = null;
  for (const m of messages) {
    if (m.ts) {
      const k = dayKey(m.ts);
      if (k !== day) {
        day = k;
        run = null;
        out.push({ kind: "day", key: `day-${k}`, ts: m.ts });
      }
    }
    if (isActivity(m)) {
      if (!run) {
        run = { kind: "activity", key: `act-${m.id}`, tools: [], thoughts: [], running: false, failed: 0 };
        out.push(run);
      }
      if (m.tool) {
        run.tools.push(m);
        if (m.tool.ok == null) run.running = true;
        else if (m.tool.ok === false) run.failed++;
      } else run.thoughts.push(m);
      continue;
    }
    run = null;
    out.push({ kind: "msg", key: m.id, m });
  }
  return out;
}

/** The tool the agent is on right now (running one, else the latest call) — "çalışıyor · Bash". */
export function currentTool(messages: ChatMessage[]): { name: string; input: string } | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const t = messages[i].tool;
    if (t) return { name: t.name, input: t.input };
    if (messages[i].role === "user" && !messages[i].thinking) return null;
  }
  return null;
}

/** An optimistic outgoing message, shown until the transcript echoes it. */
export interface Outgoing {
  id: string;
  text: string;
  /** Highest transcript `seq` when sent — only later user rows can echo it. */
  afterSeq: number;
  at: number;
  state: "sending" | "sent" | "started" | "failed";
}

/** How long an echo-less `sent`/`started` bubble stays (transcript may not record the paste). */
export const OUTGOING_TTL_MS = 30_000;

const norm = (s: string) => s.replace(/\s+/g, " ").trim();

/** Drops outgoing messages the transcript now contains (or that outlived their TTL). */
export function reconcileOutgoing(out: Outgoing[], messages: ChatMessage[], now: number): Outgoing[] {
  const users = messages.filter((m) => m.role === "user" && !m.tool && !m.thinking);
  const taken = new Set<string>();
  return out.filter((o) => {
    const echo = users.find((m) => m.seq > o.afterSeq && !taken.has(m.id) && norm(m.text) === norm(o.text));
    if (echo) {
      taken.add(echo.id);
      return false;
    }
    return o.state === "sending" || o.state === "failed" || now - o.at < OUTGOING_TTL_MS;
  });
}

export function timeLabel(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function dayLabel(ts: number, now = Date.now()): string {
  if (dayKey(ts) === dayKey(now)) return "Bugün";
  if (dayKey(ts) === dayKey(now - 86_400_000)) return "Dün";
  return new Date(ts).toLocaleDateString("tr-TR", { day: "numeric", month: "long" });
}
