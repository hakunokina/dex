// #1717 Android Paylaş → hızlı gönderim: hedef belirsiz değilse ShareScreen açılmadan `inbox_submit`.
// Karar saf mantıkta (shareLogic.ts `fastSendPlan`, birim testli); burası yalnız olguları toplar.
// Mevcut WebView/`callHost` yolu; yerel ikinci ağ bağlantısı yok. İçerik loglanmaz.
import { callHost, hostSupports, initHosts, rawId, remoteHosts, whenHostUp } from "../api/hosts";
import type { RawShare } from "../api/shareTarget";
import { listAgents, listTeams } from "../team/api";
import { prepareImage } from "./shareImage";
import {
  assistantName,
  assistantTeams,
  buildSubmit,
  fastSendPlan,
  initialText,
  sentToast,
  taskText,
  validateSubmit,
  type FastFacts,
  type ShareTask,
  type SubmitImage,
} from "./shareLogic";

/** Bağlantı + hedef araması için toplam süre; gönderimin kendisi ayrıca bu kadar bekler. */
export const FAST_TIMEOUT_MS = 5000;

export type FastResult = { sent: true; toast: string } | { sent: false; error: string | null };

const TIMEOUT = "Sunucuya bağlanılamadı (zaman aşımı)";

function within<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(TIMEOUT), Math.max(0, ms));
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}

/** Gönderilemiyorsa `error` (yalnız ShareScreen'in kendisi göstermediği hata) ile döner; ekran açılır. */
export async function tryFastShare(raw: RawShare, task: ShareTask, clientId: string): Promise<FastResult> {
  const deadline = Date.now() + FAST_TIMEOUT_MS;
  const left = () => deadline - Date.now();
  // Bilinmeyen olgular karamsar: plan ilk eksik koşulda durur.
  const facts: FastFacts = { hostIds: [], connected: false, supportsInbox: false, targets: [], problem: null };
  const fallback = (error: string | null = null): FastResult => ({ sent: false, error });

  try {
    await within(initHosts(), left());
    facts.hostIds = remoteHosts().map((h) => h.id);
    if (facts.hostIds.length !== 1) return fallback();
    const host = facts.hostIds[0];
    facts.connected = await within(whenHostUp(host), left()).then(() => true, () => false);
    if (!facts.connected) return fallback(TIMEOUT);
    facts.supportsInbox = hostSupports(host, "inbox_submit");
    if (!facts.supportsInbox) return fallback();

    const [teams, agents] = await within(Promise.all([listTeams(host), listAgents(host)]), left());
    facts.targets = assistantTeams(teams, agents).map((t) => ({ id: t.id, assistant: assistantName(t.id, agents) ?? "" }));

    let image: SubmitImage | null = null;
    if (raw.error) facts.problem = raw.error;
    else if (raw.image) image = await within(prepareImage(raw.image), left()).catch((e) => ((facts.problem = String(e)), null));
    const text = initialText(raw);
    facts.problem ??= validateSubmit(text, image, facts.targets[0]?.id ?? null, task);

    const plan = fastSendPlan(facts);
    if (!plan.send) return fallback();
    const r = await within(
      callHost<{ queued?: boolean }>(plan.host, "inbox_submit", { ...buildSubmit(rawId(plan.teamId), taskText(task, text), image, clientId) }),
      FAST_TIMEOUT_MS,
    );
    return { sent: true, toast: sentToast(plan.assistant, !!r?.queued) };
  } catch (e) {
    // Zaman aşımı ya da RPC hatası: ekran aynı clientId ile yeniden denemeye izin verir (çift teslim yok).
    return fallback(String(e));
  }
}
