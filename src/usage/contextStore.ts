// #212: ajan başına güncel context doluluğu. Takım ilk izlendiğinde (ve sunucu yeniden bağlanınca)
// team_context ile bir kez yüklenir; sonrası `agent-context` olayıyla gelir — poll yok.
// Eski dexd (auth.hello listesinde team_context yok / "bilinmeyen metot") → "unsupported": gösterge gizli.
import { useEffect, useSyncExternalStore } from "react";
import { callHost, hostSupports, onHostUp, qid, splitId } from "../api/hosts";
import { kept, listenKept } from "../hot";
import type { AgentContext, AgentContextEvent, TeamContext } from "./costTypes";

const hot = import.meta.hot;

type TeamStatus = "loading" | "ready" | "unsupported" | "error";

const status = kept(hot, "status", () => new Map<string, TeamStatus>());
/** Nitelikli ajan kimliği → son ölçüm. */
const contexts = kept(hot, "contexts", () => new Map<string, AgentContext>());
/** Olay sırası: yükleme sürerken gelen olay, yüklemenin (daha eski) cevabınca ezilmesin. */
const touched = kept(hot, "touched", () => new Map<string, number>());
const clock = kept(hot, "clock", () => ({ seq: 0 }));
const watchers = kept(hot, "watchers", () => new Map<string, number>());
const subs = kept(hot, "subs", () => new Set<() => void>());
const emit = () => subs.forEach((f) => f());

async function load(teamId: string) {
  const [host, team] = splitId(teamId);
  if (!hostSupports(host, "team_context")) {
    status.set(teamId, "unsupported");
    return emit();
  }
  const started = ++clock.seq;
  try {
    const all = await callHost<TeamContext>(host, "team_context", { teamId: team });
    for (const [agent, c] of Object.entries(all)) {
      const id = qid(host, agent);
      if ((touched.get(id) ?? 0) < started) contexts.set(id, c);
    }
    status.set(teamId, "ready");
  } catch (e) {
    // Son iyi veri kalır; hata yalnız ilk yüklemede göstergeyi gizler.
    status.set(teamId, String(e).includes("bilinmeyen metot") ? "unsupported" : status.get(teamId) === "ready" ? "ready" : "error");
  }
  emit();
}

listenKept<AgentContextEvent>(hot, "agent-context", ({ payload }, host) => {
  const id = qid(host, payload.agentId);
  contexts.set(id, payload.context);
  touched.set(id, ++clock.seq);
  emit();
});
kept(hot, "contextUp", () => onHostUp((host) => {
  for (const id of watchers.keys()) if (splitId(id)[0] === host) void load(id);
}));

function watch(teamId: string): () => void {
  const n = watchers.get(teamId) ?? 0;
  watchers.set(teamId, n + 1);
  if (!n) void load(teamId);
  return () => {
    const left = (watchers.get(teamId) ?? 1) - 1;
    if (left > 0) watchers.set(teamId, left);
    else watchers.delete(teamId);
  };
}

const subscribe = (f: () => void) => {
  subs.add(f);
  return () => void subs.delete(f);
};

/** Ajanın güncel context'i; takım yüklenmeden, eski dexd'de ya da hata varsa null (gösterge gizli). */
export function useAgentContext(teamId: string, agentId: string): AgentContext | null {
  useEffect(() => watch(teamId), [teamId]);
  return useSyncExternalStore(subscribe, () => (status.get(teamId) === "ready" ? contexts.get(agentId) ?? null : null));
}
