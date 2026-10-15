// #54/#1861: kullanım şeridinin durumu. Veri sunucudan (dexd usage_get + usage-snapshot olayı; poll
// yok), sunucu başına; şerit etkin takımın sunucusunu gösterir. Görünürlük bu cihazın tercihi
// (localStorage, varsayılan açık).
import { useEffect, useSyncExternalStore } from "react";
import { callHost, hostSupports, onHostUp } from "../api/hosts";
import { kept, listenKept } from "../hot";
import type { UsageSnapshot, UsageStatus } from "./types";

const KEY = "dex.usageStrip";
const VISIBLE_EVENT = "dex:usage-strip";
const hot = import.meta.hot;

export interface UsageView {
  status: UsageStatus;
  snap: UsageSnapshot | null;
}

const CONNECTING: UsageView = { status: "connecting", snap: null };
const views = kept(hot, "usageViews", () => new Map<string, UsageView>());
const watched = kept(hot, "usageWatched", () => new Map<string, number>());
const subs = kept(hot, "usageSubs", () => new Set<() => void>());
const set = (host: string, v: UsageView) => {
  views.set(host, v);
  subs.forEach((f) => f());
};

async function load(host: string) {
  if (!hostSupports(host, "usage_get")) return set(host, { status: "unsupported", snap: null });
  try {
    const snap = await callHost<UsageSnapshot>(host, "usage_get");
    // Yükleme sürerken gelen olay daha yeni: onu ezme.
    const cur = views.get(host)?.snap;
    if (!cur || cur.updatedAt <= snap.updatedAt) set(host, { status: "live", snap });
  } catch (e) {
    const old = views.get(host);
    set(host, String(e).includes("bilinmeyen metot") ? { status: "unsupported", snap: null } : { status: old?.snap ? "live" : "offline", snap: old?.snap ?? null });
  }
}

// usage_get'i bilmeyen (eski) sunucu bu olayı yaymaz; yükseltilmiş sunucu yeniden bağlanınca onHostUp yükler.
listenKept<UsageSnapshot>(hot, "usage-snapshot", ({ payload }, host) => {
  if (views.get(host)?.status !== "unsupported") set(host, { status: "live", snap: payload });
});
kept(hot, "usageUp", () => onHostUp((host) => { if (watched.has(host)) void load(host); }));

function subscribe(f: () => void) {
  subs.add(f);
  return () => void subs.delete(f);
}

/** `host`'un kullanım görüntüsü; izlenmeye ilk başlayan yükler. */
export function useUsage(host: string): UsageView {
  useEffect(() => {
    watched.set(host, (watched.get(host) ?? 0) + 1);
    if (!views.has(host) || views.get(host)!.status !== "live") void load(host);
    return () => {
      const n = (watched.get(host) ?? 1) - 1;
      if (n > 0) watched.set(host, n);
      else watched.delete(host);
    };
  }, [host]);
  return useSyncExternalStore(subscribe, () => views.get(host) ?? CONNECTING);
}

export function stripVisible(): boolean {
  try {
    return localStorage.getItem(KEY) !== "off";
  } catch { /* storage blocked: the default */ }
  return true;
}

export function setStripVisible(on: boolean) {
  try {
    localStorage.setItem(KEY, on ? "on" : "off");
  } catch { /* still applies for this run */ }
  window.dispatchEvent(new Event(VISIBLE_EVENT));
}

function subscribeVisible(f: () => void) {
  window.addEventListener(VISIBLE_EVENT, f);
  return () => window.removeEventListener(VISIBLE_EVENT, f);
}

export const useStripVisible = () => useSyncExternalStore(subscribeVisible, stripVisible);
