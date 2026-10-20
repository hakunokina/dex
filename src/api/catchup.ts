// Plan #124: "sen yokken" özeti — veri sözleşmesi + istemci çağrıları (dex-core::catchup,
// dexd `catchup` / `catchup_ack`). Panel UI'ı ayrı iş; burası yalnız tip + taşıma.
//
// Döngü: panel açılırken `fetchCatchup(team)` (since yok → sunucu imleci; imleç yoksa son 24
// saat, üst sınır 7 gün). Gelen `since` panel açık kaldıkça tutulur ve sonraki çekişlerde geri
// verilir (snapshot — kalemler kaybolmaz). Panel görünür + pencere odaklıyken 10 sn sonra ya da
// "Okudum" ile `ackCatchup(team, gösterilen verinin generatedAt'i)` çağrılır — `Date.now()`
// DEĞİL, yoksa çekiş ile ack arasında biten iş hiç görünmeden imlecin gerisinde kalır. Ack yalnız
// DB imlecini ilerletir, ekrandaki listeyi değiştirmez. Eski host (metot yok) → `null`, UI paneli göstermez.
import { callHost, hostSupports, splitId } from "./hosts";

export type CatchupKind =
  | "question_escalated"
  | "question_open"
  | "member_proposal"
  | "model_approval"
  | "job_change_approval"
  | "gpu_approval"
  | "merge_waiting"
  | "review_fix"
  | "delegation_done"
  | "delegation_done_no_report"
  | "delegation_failed"
  | "delegation_merged"
  | "plan_done"
  | "plan_cancelled"
  | "memory";

export interface CatchupItem {
  kind: CatchupKind;
  /** Takım içinde kararlı: `"{kind}:{ref}"`. */
  id: string;
  /** Kısa başlık/durum; rapor ya da prompt gövdesi taşınmaz. */
  title: string;
  /** Epoch ms. */
  at: number;
  /**
   * Kaynağın id'si (UI bunu açar): soru/öneri/devretme/plan/GPU isteği için sayı,
   * hafıza için graph id'si (örn. `shared/<ad>`).
   */
  ref: number | string;
}

export interface CatchupBucket {
  /** Yeniden eskiye, en çok 50. */
  items: CatchupItem[];
  /** 50'yi aşan kalem sayısı. */
  more: number;
}

export interface Catchup {
  /** Host'un çıplak takım id'si. */
  teamId: string;
  /** Kullanılan pencere başlangıcı (ms); panel bunu tutup geri verir. */
  since: number;
  /** Kayıtlı imleç; hiç ack edilmediyse null. */
  cursor: number | null;
  generatedAt: number;
  /** (a) Durum tabanlı: escalate/soru, öneri ve onaylar, birleştirme/düzelt bekleyenler. Önce bu. */
  waiting: CatchupBucket;
  /** (b) `since`'den beri biten/başarısız/raporsuz/birleşen işler + plan sonuçları. */
  changed: CatchupBucket;
  /** (c) `since`'den beri eklenen/güncellenen hafıza kayıtları (ad — tek satır açıklama). */
  memories: CatchupBucket;
}

/** `teamId` nitelikli olabilir (`<host>:<id>`); eski hostta `null`. */
export async function fetchCatchup(teamId: string, since?: number): Promise<Catchup | null> {
  const [host, team] = splitId(teamId);
  if (!hostSupports(host, "catchup")) return null;
  try {
    return await callHost<Catchup>(host, "catchup", since === undefined ? { teamId: team } : { teamId: team, since });
  } catch (e) {
    if (String(e).includes("bilinmeyen metot")) return null;
    throw e;
  }
}

/** İmleci `at`'e (epoch ms) ilerletir; geriye gitmez, gelecek sunucuda "şimdi"ye kısılır. Kaydedileni döner. */
export async function ackCatchup(teamId: string, at: number): Promise<number | null> {
  const [host, team] = splitId(teamId);
  if (!hostSupports(host, "catchup_ack")) return null;
  try {
    const r = await callHost<{ seenAt: number }>(host, "catchup_ack", { teamId: team, at });
    return r.seenAt;
  } catch (e) {
    if (String(e).includes("bilinmeyen metot")) return null;
    throw e;
  }
}
