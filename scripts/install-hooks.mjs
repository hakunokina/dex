import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { gitEnvironment, GITLEAKS_RELATIVE_PATH, GITLEAKS_VERSION, RELEASES, verifyScanner } from "./gitleaks.mjs";

const root = resolve(import.meta.dirname, "..");
const git = (...args) => execFileSync("git", args, { cwd: root, env: gitEnvironment(), encoding: "utf8" }).trim();
const current = spawnSync("git", ["config", "--get", "core.hooksPath"], { cwd: root, env: gitEnvironment(), encoding: "utf8" });
if (current.error || (current.status !== 0 && current.status !== 1)) {
  throw new Error("core.hooksPath okunamadi; hook kurulumu durduruldu.");
}
const previous = current.stdout.trim();
if (previous && previous !== "scripts/hooks") {
  throw new Error("Mevcut core.hooksPath var; otomatik olarak uzerine yazilmaz.");
}
for (const name of ["pre-commit", "commit-msg"]) {
  const hook = resolve(root, "scripts/hooks", name);
  if (!existsSync(hook)) throw new Error("Takipli Git hook dosyasi eksik; kurulum durduruldu.");
  if (process.platform !== "win32") chmodSync(hook, 0o755);
  if (!previous && existsSync(resolve(root, git("rev-parse", "--git-path", `hooks/${name}`)))) {
    throw new Error("Mevcut Git hook var; otomatik olarak uzerine yazilmaz.");
  }
}

const release = RELEASES[`${process.platform}-${process.arch}`];
if (!release) throw new Error("Desteklenen platformlar: Windows x64, macOS arm64, Linux x64.");
const [suffix, expected] = release;
const archiveName = `gitleaks_${GITLEAKS_VERSION}_${suffix}`;
const binary = resolve(root, GITLEAKS_RELATIVE_PATH);
mkdirSync(dirname(binary), { recursive: true });
const archive = resolve(dirname(binary), archiveName);
if (!existsSync(archive)) {
  console.log(`[Dex hooks] Resmi gitleaks ${GITLEAKS_VERSION} indiriliyor.`);
  const response = await fetch(`https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${archiveName}`, { signal: AbortSignal.timeout(120_000) });
  if (!response.ok) throw new Error(`Gitleaks indirme hatasi: HTTP ${response.status}`);
  const data = Buffer.from(await response.arrayBuffer());
  if (createHash("sha256").update(data).digest("hex") !== expected) throw new Error("Gitleaks SHA-256 uyusmuyor; kurulum durduruldu.");
  writeFileSync(archive, data);
}
if (createHash("sha256").update(readFileSync(archive)).digest("hex") !== expected) {
  throw new Error("Yerel gitleaks arsivi SHA-256 uyusmuyor; kurulum durduruldu.");
}
const scratch = resolve(root, ".dex/tmp");
mkdirSync(scratch, { recursive: true });
const unpack = mkdtempSync(resolve(scratch, "gitleaks-install-"));
try {
  // Windows: System32 bsdtar reads zip; Git Bash's GNU tar on PATH would treat "C:" as a remote host.
  const tar = process.platform === "win32" ? resolve(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe") : "tar";
  execFileSync(tar, ["-xf", archive, "-C", unpack, basename(binary)], { cwd: root, stdio: "pipe" });
  const extracted = resolve(unpack, basename(binary));
  if (process.platform !== "win32") chmodSync(extracted, 0o755);
  verifyScanner(extracted);
  copyFileSync(extracted, binary);
  if (process.platform !== "win32") chmodSync(binary, 0o755);
} finally {
  rmSync(unpack, { recursive: true, force: true });
}
// --local writes the common repository config, shared by all linked worktrees.
git("config", "--local", "core.hooksPath", "scripts/hooks");
console.log(`[Dex hooks] SHA-256 dogrulandi: ${GITLEAKS_RELATIVE_PATH}`);
console.log("[Dex hooks] Kuruldu: core.hooksPath=scripts/hooks (tekrar kurulum guvenli).");
console.log("[Dex hooks] Geri alma: git config --local --unset core.hooksPath");
