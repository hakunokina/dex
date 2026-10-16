// `pnpm dev:ui` / `pnpm dev:core [--now]` — one-step dev shell refresh.
// dev:ui: close the running dev shell (only this checkout's target\debug\dex.exe, never the installed
// release) and start `tauri dev`. dev:core: first build dexd and have the local core come back as that
// build (scripts/dexd-dev.mjs; default once drained, `--now` cuts running jobs), then the same.
import { execFileSync, spawn } from "node:child_process";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const core = process.argv.includes("--core");
const now = process.argv.includes("--now");

if (core) {
  execFileSync(process.execPath, [join(root, "scripts", "dexd-dev.mjs"), ...(now ? ["--now"] : [])], { cwd: root, stdio: "inherit" });
}

if (process.platform === "win32") {
  const target = (process.env.CARGO_TARGET_DIR || join(root, "target")).replace(/'/g, "''");
  const ps = `$t = [IO.Path]::GetFullPath('${target}\\debug\\dex.exe'); `
    + `Get-CimInstance Win32_Process -Filter "Name='dex.exe'" | Where-Object { $_.ExecutablePath -and ([IO.Path]::GetFullPath($_.ExecutablePath) -ieq $t) } | `
    + `ForEach-Object { Stop-Process -Id $_.ProcessId -Force; "dev: açık dev kabuğu kapatıldı (pid $($_.ProcessId))" }`;
  const out = execFileSync("powershell", ["-NoProfile", "-Command", ps], { encoding: "utf8" }).trim();
  console.log(out || "dev: açık dev kabuğu yok");
}

const child = spawn("pnpm", ["tauri", "dev"], { cwd: root, stdio: "inherit", shell: process.platform === "win32" });
child.on("exit", (code) => process.exit(code ?? 0));
