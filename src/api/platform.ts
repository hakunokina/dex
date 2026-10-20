// S5-A: build-time platform split. The Tauri CLI sets TAURI_ENV_PLATFORM for the frontend build
// (vite.config.ts maps it into `import.meta.env`); "" = plain vite / desktop. The Android build has
// no in-process core: every team lives on a dexd host, so the local core's commands are absent and
// stores must not call them (src/team/store.ts).
import { browserTokensAllowed } from "./webTokens";

export const PLATFORM: string = import.meta.env.TAURI_ENV_PLATFORM ?? "";
/** Plain browser (the vite dev server opened from a phone over the tailnet) — no Tauri at all. */
export const WEB = typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
/** Browser token persistence is restricted; desktop/APK use their native secret store. */
export const WEB_TOKEN_STORAGE = WEB && browserTokensAllowed(import.meta.env.DEV, window.location.origin);
export const MOBILE = PLATFORM === "android" || PLATFORM === "ios" || WEB;
/**
 * #2258: a leader pop-out window (`?pane=lead`, src/lead/LeadWindow.tsx). It shows one pane and
 * nothing else: the patron-side reactions (desktop notifications, sounds, pings pasted to a leader,
 * stall notes) stay with the main window, or every event would act twice.
 */
export const POPOUT = typeof window !== "undefined" && new URLSearchParams(window.location.search).get("pane") === "lead";
/** Whether the in-process core (dex-core commands) exists in this build. */
export const hasLocalCore = !MOBILE;
