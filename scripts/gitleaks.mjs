import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { delimiter, dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const GITLEAKS_VERSION = "8.30.0";
export const GITLEAKS_RELATIVE_PATH = `.dex/tools/gitleaks/${GITLEAKS_VERSION}/gitleaks${process.platform === "win32" ? ".exe" : ""}`;
// Official v8.30.0 release checksums; Windows is also pinned in ci.yml.
export const RELEASES = {
  "win32-x64": ["windows_x64.zip", "54fe94f644b832dd08e8c3a5915efb3bfa862386d59fb27ca0792cb687a83573"],
  "darwin-arm64": ["darwin_arm64.tar.gz", "b251ab2bcd4cd8ba9e56ff37698c033ebf38582b477d21ebd86586d927cf87e7"],
  "linux-x64": ["linux_x64.tar.gz", "79a3ab579b53f71efd634f3aaf7e04a0fa0cf206b7ed434638d1547a2470a66e"],
};
// SHA-256 of the executable extracted from each checksum-verified official archive.
export const BINARY_SHA256 = {
  "win32-x64": "9d08e3f5cfb35a98f230b97bcda24f8d3fc66363c91868ffc98dac0afebdcb72",
  "darwin-arm64": "095bc4be1624239f52ebb90ff91668d1d3e78e0f8c5286e2d981887905c27561",
  "linux-x64": "8b6fd684fcd5b4ebe39b68abb072ce59e1063ce7ed4abd556157697845f1f088",
};

export function gitEnvironment(env = process.env) {
  return { ...env, GIT_NO_REPLACE_OBJECTS: "1" };
}

export function verifyScanner(binary) {
  const expected = BINARY_SHA256[`${process.platform}-${process.arch}`];
  if (!expected) throw new Error("Gitleaks platformu desteklenmiyor.");
  if (createHash("sha256").update(readFileSync(binary)).digest("hex") !== expected) {
    throw new Error("Gitleaks SHA-256 uyusmuyor; islem durduruldu.");
  }
}

export function scanner(root) {
  const git = (args) => {
    const result = spawnSync("git", args, { cwd: root, env: gitEnvironment(), encoding: "utf8" });
    if (result.error || result.status !== 0) throw new Error("Gitleaks aracinin Git kokleri okunamadi.");
    return result.stdout.trim();
  };
  const common = git(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
  const worktrees = git(["worktree", "list", "--porcelain", "-z"]).split("\0")
    .filter((field) => field.startsWith("worktree ")).map((field) => field.slice(9));
  const exe = process.platform === "win32" ? "gitleaks.exe" : "gitleaks";
  const candidates = [root, dirname(common), ...worktrees].map((path) => resolve(path, GITLEAKS_RELATIVE_PATH));
  candidates.push(...(process.env.PATH || "").split(delimiter).filter(Boolean).map((path) => resolve(path, exe)));
  let invalid = false;
  for (const binary of new Set(candidates)) {
    if (!existsSync(binary)) continue;
    try { verifyScanner(binary); return binary; } catch { invalid = true; }
  }
  if (invalid) throw new Error("Gitleaks SHA-256 dogrulanamadi; npm run hooks:install calistirin.");
  throw new Error(`gitleaks ${GITLEAKS_VERSION} bulunamadi; npm run hooks:install calistirin.`);
}

export function scanContext(root, config) {
  const parent = resolve(root, ".dex/tmp");
  mkdirSync(parent, { recursive: true });
  const dir = mkdtempSync(resolve(parent, "gitleaks-"));
  const configPath = resolve(dir, "policy.toml");
  const ignorePath = resolve(dir, "empty.gitleaksignore");
  const emptyGitConfig = resolve(dir, "empty.gitconfig");
  writeFileSync(configPath, config);
  writeFileSync(ignorePath, "");
  writeFileSync(emptyGitConfig, "");
  const env = { ...process.env };
  // Hooks inherit Git's index/worktree environment; the scanner must read only its own source.
  for (const key of Object.keys(env)) if (/^(?:GIT_|GITLEAKS_)/i.test(key)) delete env[key];
  Object.assign(env, { GIT_NO_REPLACE_OBJECTS: "1", GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: emptyGitConfig });
  let source;
  const scanGit = (args) => {
    const result = spawnSync("git", args, { cwd: dir, env, encoding: "utf8" });
    if (result.error || result.status !== 0) throw new Error("Izole tarama deposu olusturulamadi.");
    return result.stdout.trim();
  };
  return {
    dir,
    scan(binary, args, input) {
      let cwd = dir;
      if (args[0] === "git") {
        if (!source) {
          source = resolve(dir, "repo.git");
          // v8.30 also reads <source>/.gitleaksignore; a fresh bare clone has no such file.
          scanGit(["clone", "--bare", "--shared", "--quiet", "--template=", "--", root, source]);
        }
        if (args.includes("--staged")) {
          // Pin the isolated copy to the worktree HEAD so --staged always diffs the
          // staged index against the true base, never against the clone default.
          // An unborn HEAD has no target to pin; keep the clone default as before.
          const head = spawnSync("git", ["rev-parse", "--verify", "HEAD"],
            { cwd: root, env: gitEnvironment(), encoding: "utf8" });
          if (!head.error && head.status === 0) {
            const pin = spawnSync("git", ["update-ref", "--no-deref", "HEAD", head.stdout.trim()],
              { cwd: source, env, encoding: "utf8" });
            if (pin.error || pin.status !== 0) throw new Error("Izole tarama deposu olusturulamadi.");
          }
          const index = spawnSync("git", ["rev-parse", "--path-format=absolute", "--git-path", "index"],
            { cwd: root, env: gitEnvironment(), encoding: "utf8" });
          if (index.error || index.status !== 0) throw new Error("Staged index okunamadi.");
          const indexPath = index.stdout.trim();
          copyFileSync(indexPath, resolve(source, "index"));
          // Split index (core.splitIndex) keeps base entries in a companion file; without it
          // the isolated copy sees a partial index and --staged misses staged content.
          for (const entry of readdirSync(dirname(indexPath))) {
            if (entry.startsWith("sharedindex.")) copyFileSync(resolve(dirname(indexPath), entry), resolve(source, entry));
          }
        }
        args = ["git", source, ...args.slice(1)];
      } else if (args[0] === "dir") {
        cwd = resolve(args[1]);
        if (existsSync(resolve(cwd, ".gitleaksignore"))) throw new Error("Tarama agacinda .gitleaksignore reddedildi.");
        // Use repository-relative paths for consistent findings and file exceptions.
        args = ["dir", ".", ...args.slice(2)];
      }
      const result = spawnSync(binary, [...args, "--config", configPath, "--gitleaks-ignore-path", ignorePath,
        "--redact", "--no-banner", "--ignore-gitleaks-allow"], { cwd, env, input, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
      // Do not echo scanner output: git metadata and filenames can contain personal data.
      if (result.error || result.status !== 0) throw new Error(`Gitleaks taramasi reddetti (exit ${result.status ?? "error"}).`);
    },
    close() { rmSync(dir, { recursive: true, force: true }); },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.argv.slice(2).join(" ") !== "--path") throw new Error("Kullanim: node scripts/gitleaks.mjs --path");
  console.log(GITLEAKS_RELATIVE_PATH);
}
