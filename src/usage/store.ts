// #54: kullanım şeridinin durumu. Veri yerel çekirdeğin köprüsünden (usage_get + usage_snapshot olayı;
// poll yok); görünürlük bu cihazın tercihi (localStorage, varsayılan açık).
import { useSyncExternalStore } from "react";
import { callLocal, LOCAL, transportOf } from "../api/hosts";
import { hasLocalCore } from "../api/platform";
import { OFFLINE, type UsageSnapshot } from "./types";

const EVENT = "usage_snapshot";
const KEY = "dex.usageStrip";
const VISIBLE_EVENT = "dex:usage-strip";

let snap: UsageSnapshot = OFFLINE;
let started = false;
const subs = new Set<() => void>();
const set = (s: UsageSnapshot) => {
  snap = s;
  for (const f of subs) f();
};

function start() {
  if (started) return;
  started = true;
  // Olay önce: usage_get ile ilk olay arasında gelen veri kaybolmasın. Komut yoksa (tarayıcı/e2e
  // köprüsü, uzak çekirdek) şerit "TM bağlı değil"de kalır.
  if (!hasLocalCore) return;
  try {
    void transportOf(LOCAL).on<UsageSnapshot>(EVENT, (e) => set(e.payload)).catch(() => {});
  } catch { /* no Tauri: stays offline */ }
  callLocal<UsageSnapshot>("usage_get").then((s) => { if (snap === OFFLINE) set(s); }, () => {});
}

function subscribe(f: () => void) {
  start();
  subs.add(f);
  return () => void subs.delete(f);
}

export const useUsage = () => useSyncExternalStore(subscribe, () => snap);

export const setHubSecret = (secret: string) => callLocal<void>("usage_hub_secret_set", { secret });

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
