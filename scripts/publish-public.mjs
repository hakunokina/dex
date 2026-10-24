import { mkdirSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { committedPolicy, git, requireNoreply, requireOriginalHistory } from "./commit-policy.mjs";
import { gitEnvironment, scanContext, scanner } from "./gitleaks.mjs";

const CANONICAL_URL = "https://github.com/hakunokina/dex.git";

function argumentsFor(args) {
  let dryRun = false, initial = false, source = "team/dev", target;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--dry-run") dryRun = true;
    else if (arg === "--initial") initial = true;
    else if (arg === "--source") source = args[++index];
    else if (arg.startsWith("-") || target) throw new Error("Kullanim: publish:public -- [dal] [--initial] [--dry-run]");
    else target = arg;
  }
  if (source !== "team/dev") throw new Error("Yalniz team/dev yayinlanabilir; archive ve master reddedilir.");
  target ??= "main";
  if (target === "master" || /^(?:archive|tags|refs)(?:\/|$)/.test(target)) throw new Error("Yasak public hedef dali.");
  if (initial && target !== "main") throw new Error("Ilk yayin yalniz main dalina yapilir.");
  return { dryRun, initial, target };
}

function publicUrl(root) {
  const override = process.env.DEX_PUBLIC_TEST_REMOTE;
  if (!override) return CANONICAL_URL;
  // Tests may use only a local bare repository inside this checkout's scratch directory.
  const url = new URL(override);
  if (url.protocol !== "file:" || url.hostname || url.search || url.hash) throw new Error("Test remote yalniz yerel file URL olabilir.");
  const path = fileURLToPath(url);
  const within = relative(resolve(root, ".dex/tmp"), path);
  if (!within || isAbsolute(within) || within === ".." || within.startsWith(`..${sep}`)) throw new Error("Test remote .dex/tmp icinde olmali.");
  return override;
}

function validateUrl(root, url) {
  const fetchUrl = git(root, ["ls-remote", "--get-url", url]);
  const rewrites = spawnSync("git", ["config", "--null", "--get-regexp", "^url\\..*\\.pushinsteadof$"], { cwd: root, env: gitEnvironment(), encoding: "utf8" });
  if (rewrites.error || ![0, 1].includes(rewrites.status)) throw new Error("Push URL rewrite okunamadi.");
  for (const entry of rewrites.stdout.split("\0").filter(Boolean)) {
    const split = entry.indexOf("\n");
    const prefix = entry.slice(split + 1);
    const replacement = entry.slice(4, split - ".pushinsteadof".length);
    if (url.startsWith(prefix) && replacement + url.slice(prefix.length) !== url) throw new Error("Public pushInsteadOf rewrite reddedildi.");
  }
  if (fetchUrl !== url) throw new Error("Public remote URL rewrite reddedildi.");
}

function privateIntersection(root, commits) {
  const outgoing = new Set(commits);
  for (const ref of ["refs/heads/archive/pre-public", "refs/heads/master"]) {
    const exists = spawnSync("git", ["show-ref", "--verify", "--quiet", ref], { cwd: root, env: gitEnvironment() });
    if (exists.status === 1) continue;
    if (exists.error || exists.status !== 0) throw new Error("Private ref kontrolu basarisiz.");
    const privateCommits = git(root, ["rev-list", ref]).split("\n");
    if (privateCommits.some((sha) => outgoing.has(sha))) throw new Error("Private archive/master gecmisi public'e gonderilemez.");
  }
}

function main() {
  const options = argumentsFor(process.argv.slice(2));
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]);
  requireOriginalHistory(root);
  git(root, ["check-ref-format", `refs/heads/${options.target}`]);
  const url = publicUrl(root);
  validateUrl(root, url);
  const binary = scanner(root);
  const source = git(root, ["rev-parse", "--verify", "refs/heads/team/dev^{commit}"]);
  const paths = git(root, ["ls-tree", "-r", "--name-only", "-z", source]).split("\0");
  if (paths.some((path) => path === ".gitleaksignore" || path.endsWith("/.gitleaksignore"))) {
    throw new Error("Public agacta .gitleaksignore reddedildi.");
  }
  const refs = git(root, ["ls-remote", "--refs", url]);
  const publicMain = refs.split("\n").find((line) => line.endsWith("\trefs/heads/main"))?.split("\t")[0];
  if (options.initial ? !!refs : !publicMain) throw new Error(options.initial ? "Ilk yayin icin public repo bos olmali." : "Public main yok; bos repo icin --initial kullanin.");

  const policy = committedPolicy(root, source);
  if (git(root, ["rev-parse", "--verify", "HEAD"]) !== source) {
    console.log(`[Dex publish] HEAD != kaynak; kaynak politikasi kullaniliyor (${source}:.gitleaks.toml).`);
  }
  const context = scanContext(root, policy);
  try {
    const indexEnv = { ...process.env, GIT_INDEX_FILE: resolve(context.dir, "index") };
    // Read Git's tree into an isolated index. No archive, CRLF conversion or mode loss in the published tree.
    git(root, ["read-tree", source], { env: indexEnv });
    const tree = git(root, ["write-tree"], { env: indexEnv });
    // The scratch index is the only input of the published tree (--initial commit-tree below) and of
    // the scan checkout; it must still be exactly the source tree.
    if (tree !== git(root, ["rev-parse", "--verify", `${source}^{tree}`])) throw new Error("Yayin agaci kaynak agacla uyusmuyor; islem durduruldu.");
    let candidate = source, range;
    if (options.initial) {
      const date = `${Math.floor(Date.now() / 1000)} +0000`;
      const identity = { ...process.env, GIT_AUTHOR_NAME: "hakunokina", GIT_COMMITTER_NAME: "hakunokina",
        GIT_AUTHOR_DATE: date, GIT_COMMITTER_DATE: date,
        GIT_AUTHOR_EMAIL: "hakunokina@users.noreply.github.com", GIT_COMMITTER_EMAIL: "hakunokina@users.noreply.github.com" };
      candidate = git(root, ["commit-tree", tree], { env: identity, input: "Initial public release\n" });
      range = candidate;
    } else {
      // Fetch exactly the advertised public main, without fetching tags or moving local branches.
      git(root, ["fetch", "--no-tags", "--no-write-fetch-head", "--no-recurse-submodules", url, publicMain]);
      git(root, ["merge-base", "--is-ancestor", publicMain, source]);
      range = `${publicMain}..${source}`;
    }
    const commits = git(root, ["rev-list", "--reverse", range]).split("\n").filter(Boolean);
    privateIntersection(root, commits);
    for (const sha of commits) {
      const [author, committer, message] = git(root, ["show", "-s", "--format=%ae%x00%ce%x00%B", sha]).split("\0");
      requireNoreply(author);
      requireNoreply(committer);
      context.scan(binary, ["stdin"], message);
    }
    // -m catches merge-parent changes; the range includes removed secrets in earlier commits.
    context.scan(binary, ["git", `--log-opts=${range} --full-history -m --root`]);
    const scanTree = resolve(context.dir, "tree");
    mkdirSync(scanTree);
    // This checkout is scan-only; the root commit above retains the original Git tree and modes.
    git(root, ["checkout-index", "--all", `--prefix=${scanTree.replaceAll("\\", "/")}/`], { env: indexEnv });
    context.scan(binary, ["dir", scanTree]);
    // What the scanner read must be what is published: re-hash the scanned checkout (refresh hashes
    // every stat-dirty entry; diff-index alone would only report them) and compare it with the tree.
    for (const args of [["update-index", "--refresh", "-q"], ["diff-index", "--quiet", tree, "--"]]) {
      const same = spawnSync("git", ["--work-tree", scanTree, ...args], { cwd: root, env: gitEnvironment(indexEnv), encoding: "utf8", stdio: ["ignore", "ignore", "ignore"] });
      if (same.error || same.status !== 0) throw new Error("Taranan agac yayin agaciyla uyusmuyor; islem durduruldu.");
    }
    console.log(`[Dex publish] ${commits.length} commit + agac + kimlik + mesaj: gecti.`);
    if (options.initial) console.log(`[Dex publish] root=${candidate}`);
    const pushArgs = ["-c", "push.followTags=false", "push", "--no-follow-tags", "--recurse-submodules=no", "--", url, `${candidate}:refs/heads/${options.target}`];
    if (options.dryRun) console.log(`[Dex publish] DRY-RUN: git ${pushArgs.join(" ")}`);
    else {
      validateUrl(root, url);
      const latest = git(root, ["ls-remote", "--refs", url]);
      if (options.initial ? !!latest : !latest.split("\n").includes(`${publicMain}\trefs/heads/main`)) throw new Error("Public taban degisti; yeniden dogrulayin.");
      git(root, pushArgs);
      console.log(`[Dex publish] Yayinlandi: ${options.target} (${candidate}).`);
    }
    if (options.initial) {
      const verifiedRoot = options.dryRun ? "<verified-R>" : candidate;
      if (options.dryRun) console.log("[Dex publish] Do not use this SHA for migration: dry-run did not push; the real run may create a different root.");
      console.log(`[Dex publish] Next steps (manual, with explicit approval; see docs/public-release.md):
${options.dryRun ? "After the approved real --initial push, use its reported root R." : `Verify the remote main SHA equals the published root R=${candidate}.`}
1. Stop workers and new jobs; require a clean team/dev checkout and index.
2. git fetch --no-tags ${url} refs/heads/main
   git rev-parse FETCH_HEAD
   git rev-list --parents -n 1 FETCH_HEAD
   git diff --exit-code team/dev FETCH_HEAD --
   Stop unless FETCH_HEAD equals the real published R, has no parents, and has the same tree.
3. git branch archive/pre-initial-1456 team/dev
   Do not overwrite an existing archive branch or change archive/pre-public.
4. With the verified literal R and explicit approval: git reset --soft ${verifiedRoot}
   Require empty git status --short and git rev-parse HEAD equal to R.
5. Do not merge old worker branches; recreate them from the new team/dev root.
6. npm run publish:public -- --dry-run
   Check the first public CI results; subsequent releases use npm run publish:public.`);
    }
  } finally {
    context.close();
  }
}

try { main(); } catch (error) {
  console.error(`[Dex publish] HATA: ${error.message}`);
  process.exitCode = 1;
}
