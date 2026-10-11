import { join, resolve } from "node:path";

export const REPO = resolve(import.meta.dirname, "..");
const exe = process.platform === "win32" ? ".exe" : "";
/** Görev #144: a Dex job worktree (`<team>/.dex/wt/<agent>-<id>`) runs beside the team checkout and others. */
const LINKED = /[\\/]\.dex[\\/]wt[\\/]/.test(REPO);
const repoHash = [...REPO.toLowerCase()].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
/** Built by global-setup in checkout scratch, shared by the run's workers. */
export const BIN_DIR = join(REPO, ".dex", "tmp", "e2e-bin");
export const FAKE_CLAUDE = join(BIN_DIR, `claude${exe}`);
/** A job worktree builds into its agent's CARGO_TARGET_DIR. */
export const DEXD = process.env.DEX_E2E_DEXD ?? join(process.env.CARGO_TARGET_DIR || join(REPO, "target"), "debug", `dexd${exe}`);
/** The vite dev server of this run (not 1420: `tauri dev` may be using it). The main checkout
 *  keeps 5199; each job worktree gets its own in 5200–5899. */
export const VITE_PORT = Number(process.env.DEX_E2E_VITE_PORT ?? (LINKED ? 5200 + (repoHash % 700) : 5199));
