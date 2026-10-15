// Builds the dexd binary (as e2e/global-setup.ts) and the demo `claude` stand-in (demo-claude.rs).
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { REPO } from "../../e2e/paths";

export const SHOTS_BIN = join(REPO, ".dex", "tmp", "shots-bin");
export const DEMO_CLAUDE = join(SHOTS_BIN, `claude${process.platform === "win32" ? ".exe" : ""}`);

export default function setup() {
  mkdirSync(SHOTS_BIN, { recursive: true });
  if (!process.env.DEX_E2E_SKIP_BUILD) execFileSync("cargo", ["build", "-p", "dexd"], { cwd: REPO, stdio: "inherit" });
  execFileSync("rustc", ["--edition", "2021", "-O", join(REPO, "scripts", "screenshots", "demo-claude.rs"), "-o", DEMO_CLAUDE], { stdio: "inherit" });
}
