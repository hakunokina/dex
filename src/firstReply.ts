// FIX-3: the backend's first-reply watchdog (crates/dex-core/src/first_reply.rs) says a turn got no
// event within its provider's threshold. Here: the mark (pane line with retry/cancel, amber hourglass
// on the dock bar and hover card), a desktop notification, and a ping to the leader when the agent
// is on a delegated job (the backend already left it a note). The backend clears the mark.
import { useSyncExternalStore } from "react";
import { callHost, qid, qOpt, splitId } from "./api/hosts";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { findAgent, markPromptUnsure } from "./agents";
import { pastePing, pingable } from "./delegations";
import { POPOUT } from "./api/platform";
import { kept, listenKept } from "./hot";

const hot = import.meta.hot;

interface Mark {
  agentId: string;
  late: boolean;
  seconds: number;
  text: string | null;
  leader: string | null;
}

const marks = kept(hot, "marks", () => new Map<string, string>());
const box = kept(hot, "version", () => ({ v: 0 }));
const listeners = kept(hot, "listeners", () => new Set<() => void>());

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
/** Re-renders on every mark change (read with `slowReply`). */
export const useSlowReply = () => useSyncExternalStore(subscribe, () => box.v);

/** Plain-Turkish line while the agent's first reply is late, else null. */
export const slowReply = (id: string) => marks.get(id) ?? null;

export async function retryTurn(id: string) {
  const [host, raw] = splitId(id);
  return callHost(host, "agent_first_reply_retry", { id: raw });
}
export async function cancelTurn(id: string) {
  const [host, raw] = splitId(id);
  await callHost(host, "agent_first_reply_cancel", { id: raw });
  markPromptUnsure(id); // the put-back prompt is text the pane never saw typed
}

listenKept<Mark>(hot, "dex-first-reply", ({ payload }, host) => {
  const m = { ...payload, agentId: qid(host, payload.agentId), leader: qOpt(host, payload.leader) };
  const had = marks.has(m.agentId);
  if (m.late && m.text) marks.set(m.agentId, m.text);
  else marks.delete(m.agentId);
  if (had !== marks.has(m.agentId) || m.late) {
    box.v++;
    for (const l of listeners) l();
  }
  if (m.late && m.text) void tell(m);
});

async function tell(m: Mark) {
  if (POPOUT) return; // #2258: the main window tells
  const name = findAgent(m.agentId)?.name ?? m.agentId;
  if (m.leader && pingable(m.leader)) pastePing(m.leader, `[Dex] ${name} ilk yanıtı alamadı: ${m.text} Ayrıntı bu mesajın ekinde.`);
  let ok = await isPermissionGranted();
  if (!ok) ok = (await requestPermission()) === "granted";
  if (ok) sendNotification({ title: `${name}: yanıt gecikiyor`, body: `${m.text} Pane'den yeniden deneyebilir ya da iptal edebilirsin.` });
}
