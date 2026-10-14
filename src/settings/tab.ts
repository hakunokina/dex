// Ayarlar sayfaları: Settings.tsx'ten ayrı ki üst çubuk ve palet sekmeyi
// ayar görünümünü ilk açılış parçasına çekmeden açabilsin.
export type SettingsTab =
  | "general"       // Bu cihaz › Genel
  | "notifications" // Bu cihaz › Bildirimler
  | "connections"   // Bu cihaz › Bağlantılar
  | "providers"     // Sunucu › Sağlayıcılar
  | "chain"         // Sunucu › Kota zinciri
  | "resources"     // Sunucu › Kaynak sınırları
  | "gpu";          // Sunucu › GPU

export const TAB_KEY = "dex.settingsTab";
export const TAB_EVENT = "dex:settings-tab";
/** Derin bağlantı: sayfa açıldıktan sonra `[data-setting="<id>"]` odağı. */
export const FOCUS_EVENT = "dex:settings-focus";

/** Mount'tan önce gelen odak isteği burada bekler; görünüm açılışta tüketir. */
let pendingFocus: string | null = null;

/** Focus `setting` now (mounted view) or on the next mount (opening view). */
export function focusSetting(setting: string) {
  pendingFocus = setting;
  window.dispatchEvent(new CustomEvent(FOCUS_EVENT, { detail: setting }));
}

export function takePendingFocus(): string | null {
  const s = pendingFocus;
  pendingFocus = null;
  return s;
}

/** Eski kayıtlı sekmeler yeni sayfalara göçer (gateway "providers" Sağlayıcılar'a). */
export function migrateTab(raw: string | null): SettingsTab {
  switch (raw) {
    case "general": case "notifications": case "connections":
    case "providers": case "chain": case "resources": case "gpu":
      return raw;
    case "agents": return "resources";
    case "hosts": return "connections";
    case "advanced": return "general";
    default: return "general";
  }
}

/** Remember `tab` (a view mounted later reads it) and switch a mounted view to it. */
export function selectSettingsTab(tab: SettingsTab) {
  try { localStorage.setItem(TAB_KEY, tab); } catch { /* the event still switches a mounted view */ }
  window.dispatchEvent(new CustomEvent(TAB_EVENT, { detail: tab }));
}
