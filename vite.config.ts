import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";
// @ts-expect-error type error without @types/node package
import { execFileSync } from "node:child_process";
// @ts-expect-error type error without @types/node package
import { fileURLToPath } from "node:url";
const host = process.env.TAURI_DEV_HOST;

// #151: this UI's build, as crates/dex-core/build_commit.rs stamps dexd's (DEX_BUILD_COMMIT* env
// wins — not DEX_COMMIT, which leaks from `tauri dev` into agents; no git → "unknown" / 0) — the
// "Çekirdek güncel değil" band compares the local core's hello against it.
const git = (...args: string[]) => {
  try {
    return String(execFileSync("git", args, { encoding: "utf8" })).trim();
  } catch {
    return "";
  }
};
const commit = process.env.DEX_BUILD_COMMIT || git("rev-parse", "--short=10", "HEAD") || "unknown";
const commitTime = Number(process.env.DEX_BUILD_COMMIT_TIME || git("log", "-1", "--format=%ct")) || 0;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react()],

  // S5-A: the Tauri CLI exports TAURI_ENV_PLATFORM for its frontend build ("android", "ios", "windows",
  // …); vite only forwards VITE_* on its own, so route it through define. "" = plain vite (desktop web).
  define: {
    "import.meta.env.TAURI_ENV_PLATFORM": JSON.stringify(process.env.TAURI_ENV_PLATFORM ?? ""),
    "import.meta.env.DEX_COMMIT": JSON.stringify(commit),
    "import.meta.env.DEX_COMMIT_TIME": JSON.stringify(commitTime),
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // IPv4 loopback: with a Tailscale exit node on, Windows blocks ::1 (EACCES) and `localhost` resolves there
    // first, so `tauri dev` waits forever. devUrl stays localhost:1420 — dexd's LOCAL_ORIGINS trusts that origin.
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching the Rust side (workspace: src-tauri, crates, root target)
      // #2021: and this root's .dex (worktrees' tsconfig.json add/unlink → full page reload on every
      // delegate/merge). Absolute, not "**/.dex/**": e2e's Vite runs from inside .dex/wt/<x>.
      ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**", `${fileURLToPath(new URL("./.dex", import.meta.url)).replace(/\\/g, "/")}/**`],
    },
  },
}));
