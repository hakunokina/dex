// #44: which Ayarlar tab is shown. Kept apart from `Settings.tsx` so the topbar and the palette can
// open Ayarlar on a given tab without pulling the settings view into the first-open chunk.
export type SettingsTab = "general" | "agents" | "providers" | "hosts" | "advanced";

export const TAB_KEY = "dex.settingsTab";
export const TAB_EVENT = "dex:settings-tab";

/** Remember `tab` (a view mounted later reads it) and switch a mounted view to it. */
export function selectSettingsTab(tab: SettingsTab) {
  try { localStorage.setItem(TAB_KEY, tab); } catch { /* the event still switches a mounted view */ }
  window.dispatchEvent(new CustomEvent(TAB_EVENT, { detail: tab }));
}
