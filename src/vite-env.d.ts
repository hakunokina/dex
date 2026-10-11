/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Set by vite.config.ts from the Tauri CLI's TAURI_ENV_PLATFORM ("" when run outside tauri). */
  readonly TAURI_ENV_PLATFORM?: string;
  /** #151: this UI's commit (short sha, "unknown") and its unix time (0 = unknown) — vite.config.ts. */
  readonly DEX_COMMIT?: string;
  readonly DEX_COMMIT_TIME?: number;
}
