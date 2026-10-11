// Builds what every worker's dexd needs, once: the dexd binary and the fake `claude`.
// Nothing here touches the patron's data: dexd instances are started per worker (fixtures.ts)
// with their own data dir under the checkout's .dex/tmp folder.
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { BIN_DIR, FAKE_CLAUDE, REPO } from "./paths";

export default function globalSetup() {
  mkdirSync(BIN_DIR, { recursive: true });
  if (!process.env.DEX_E2E_SKIP_BUILD) {
    execFileSync("cargo", ["build", "-p", "dexd"], { cwd: REPO, stdio: "inherit" });
  }
  execFileSync("rustc", ["--edition", "2021", "-O", join(REPO, "e2e", "fake-claude.rs"), "-o", FAKE_CLAUDE], { stdio: "inherit" });
}
