import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { gitEnvironment, scanContext, scanner } from "./gitleaks.mjs";

export function git(root, args, options = {}) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options,
    env: gitEnvironment(options.env) });
  if (result.error || result.status !== 0) throw new Error(`Git ${args[0]} basarisiz; islem durduruldu.`);
  return result.stdout.trim();
}

export function requireNoreply(email) {
  if (!/^[a-z0-9][a-z0-9+._-]*@(?:users\.)?noreply\.github\.com$/i.test(email)) {
    throw new Error("Author ve committer GitHub noreply e-postasi kullanmali.");
  }
}

export function requireOriginalHistory(root) {
  if (git(root, ["for-each-ref", "--format=%(refname)", "refs/replace/"])) {
    throw new Error("Git refs/replace gecmis override reddedildi.");
  }
  const grafts = git(root, ["rev-parse", "--path-format=absolute", "--git-path", "info/grafts"]);
  if (existsSync(grafts) || (process.env.GIT_GRAFT_FILE && existsSync(resolve(root, process.env.GIT_GRAFT_FILE)))) {
    throw new Error("Git info/grafts gecmis override reddedildi.");
  }
}

export function committedPolicy(root, revision) {
  if (revision !== undefined) return git(root, ["show", `${revision}:.gitleaks.toml`]);
  const head = spawnSync("git", ["rev-parse", "--verify", "HEAD"], { cwd: root, env: gitEnvironment(), encoding: "utf8" });
  // An unborn repository must stage its policy before its first commit.
  return git(root, ["show", head.status === 0 ? "HEAD:.gitleaks.toml" : ":.gitleaks.toml"]);
}

function main() {
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]);
  const [mode, messageFile] = process.argv.slice(2);
  if (mode !== "staged" && mode !== "message") throw new Error("Bilinmeyen commit kontrolu.");
  requireOriginalHistory(root);
  const binary = scanner(root);
  const context = scanContext(root, committedPolicy(root));
  try {
    if (mode === "staged") {
      for (const field of ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"]) {
        const ident = git(root, ["var", field]).match(/<([^<>]+)> \d+ [+-]\d{4}$/);
        if (!ident) throw new Error("Git kimligi okunamadi.");
        requireNoreply(ident[1]);
      }
      context.scan(binary, ["git", "--pre-commit", "--staged"]);
    } else {
      if (!messageFile) throw new Error("Commit mesaji dosyasi gerekli.");
      context.scan(binary, ["stdin"], readFileSync(resolve(messageFile), "utf8"));
    }
    console.log(`[Dex hook] ${mode}: kontrol gecti.`);
  } finally {
    context.close();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { main(); } catch (error) {
    console.error(`[Dex hook] HATA: ${error.message}`);
    process.exitCode = 1;
  }
}
