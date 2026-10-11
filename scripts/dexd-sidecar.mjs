// #150 (B): the release ships `dexd` beside the app (the core runs as `dexd --local`). Builds it
// and puts it where `bundle.externalBin` (src-tauri/tauri.release.conf.json) looks for it:
// src-tauri/binaries/dexd-<host triple>[.exe]. `pnpm release` runs this, then `tauri build` with
// that config. Not in the main config: tauri-build would then require the file for every
// `cargo check` / `tauri dev`.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
execFileSync("cargo", ["build", "--release", "-p", "dexd"], { cwd: root, stdio: "inherit" });
const host = /host: (\S+)/.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))?.[1];
if (!host) throw new Error("rustc -vV: host triple yok");
const exe = process.platform === "win32" ? ".exe" : "";
const built = join(process.env.CARGO_TARGET_DIR || join(root, "target"), "release", `dexd${exe}`);
const dir = join(root, "src-tauri", "binaries");
mkdirSync(dir, { recursive: true });
copyFileSync(built, join(dir, `dexd-${host}${exe}`));
console.log(`dexd-sidecar: ${built} → ${join(dir, `dexd-${host}${exe}`)}`);
