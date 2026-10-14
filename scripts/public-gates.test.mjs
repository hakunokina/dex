import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { basename, delimiter, dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { GITLEAKS_RELATIVE_PATH, GITLEAKS_VERSION, RELEASES } from "./gitleaks.mjs";

const root = resolve(import.meta.dirname, "..");
const scratch = resolve(root, ".dex/tmp/public-gates-tests");
mkdirSync(scratch, { recursive: true });
const identity = {
  GIT_AUTHOR_NAME: "Public Test", GIT_COMMITTER_NAME: "Public Test",
  GIT_AUTHOR_EMAIL: "test@users.noreply.github.com",
  GIT_COMMITTER_EMAIL: "test@users.noreply.github.com",
};
let installedScanner;
function run(repo, command, args, extra = {}) {
  const env = { ...process.env, ...identity, GIT_NO_REPLACE_OBJECTS: "1", ...extra };
  for (const key of ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"]) delete env[key];
  return spawnSync(command, args, { cwd: repo, env, encoding: "utf8" });
}
function git(repo, ...args) {
  const result = run(repo, "git", args);
  assert.equal(result.status, 0, `git ${args[0]} failed`);
  return result.stdout.trim();
}
function fixture() {
  const repo = mkdtempSync(resolve(scratch, "repo-"));
  cpSync(resolve(root, "scripts"), resolve(repo, "scripts"), { recursive: true });
  cpSync(resolve(root, ".gitleaks.toml"), resolve(repo, ".gitleaks.toml"));
  cpSync(resolve(root, ".gitignore"), resolve(repo, ".gitignore"));
  cpSync(resolve(root, ".gitattributes"), resolve(repo, ".gitattributes"));
  git(repo, "init", "-b", "team/dev");
  git(repo, "config", "user.name", "Public Test");
  git(repo, "config", "user.email", identity.GIT_AUTHOR_EMAIL);
  git(repo, "add", ".");
  git(repo, "update-index", "--chmod=+x", "scripts/hooks/pre-commit", "scripts/hooks/commit-msg");
  git(repo, "commit", "-m", "Clean baseline");
  return repo;
}

test("installed pre-commit rejects a missing scanner", () => {
  const repo = fixture();
  git(repo, "config", "--local", "core.hooksPath", "scripts/hooks");
  writeFileSync(resolve(repo, "sample.txt"), "clean content\n");
  git(repo, "add", "sample.txt");
  // Ortamda gitleaks kurulu olabilir (PATH'te); "tarayıcı yok" senaryosu yalıtılmış PATH ister.
  const exe = process.platform === "win32" ? "gitleaks.exe" : "gitleaks";
  const isolated = (process.env.PATH || "").split(delimiter)
    .filter((dir) => dir && !existsSync(resolve(dir, exe)));
  const nodeExe = process.platform === "win32" ? "node.exe" : "node";
  if (!isolated.some((dir) => existsSync(resolve(dir, nodeExe)))) isolated.unshift(dirname(process.execPath));
  const result = run(repo, "git", ["commit", "-m", "Clean change"], { PATH: isolated.join(delimiter) });
  assert.notEqual(result.status, 0, "missing gitleaks must block commit");
  assert.match(result.stderr, /gitleaks.*bulunamadi/);
});

test("installer downloads the pinned scanner and is repeatable across worktrees", () => {
  const repo = fixture();
  const install = () => run(repo, process.execPath, ["scripts/install-hooks.mjs"]);
  assert.equal(install().status, 0, "installer succeeds");
  const binary = resolve(repo, GITLEAKS_RELATIVE_PATH);
  assert.ok(existsSync(binary), "repo-local scanner installed");
  assert.equal(run(repo, binary, ["version"]).stdout.trim(), "8.30.0");
  assert.equal(install().status, 0, "repeated installation succeeds");
  installedScanner = binary;
  const worktree = resolve(repo, ".dex/tmp/linked");
  git(repo, "worktree", "add", "-b", "worker", worktree);
  assert.equal(git(worktree, "config", "--get", "core.hooksPath"), "scripts/hooks");
  const linkedBinary = resolve(worktree, GITLEAKS_RELATIVE_PATH);
  assert.equal(existsSync(linkedBinary), false, "worker has no local scanner");
  writeFileSync(resolve(worktree, "sample.txt"), "clean worktree change\n");
  git(worktree, "add", "sample.txt");
  const clean = run(worktree, "git", ["commit", "-m", "Clean worktree change"]);
  assert.equal(clean.status, 0, `common checkout scanner must work: ${clean.stderr}`);
  writeFileSync(resolve(worktree, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join(""));
  git(worktree, "add", "sample.txt");
  assert.match(run(worktree, "git", ["commit", "-m", "Unsafe worktree change"]).stderr, /Gitleaks/);
  const archive = resolve(dirname(binary), `gitleaks_${GITLEAKS_VERSION}_${RELEASES[`${process.platform}-${process.arch}`][0]}`);
  const data = readFileSync(archive);
  writeFileSync(archive, "corrupted archive");
  const corrupted = install();
  assert.notEqual(corrupted.status, 0, "unverified archive must not install");
  assert.match(corrupted.stderr, /SHA-256/);
  writeFileSync(archive, data);
});

test("diverged worktree scans staged changes against the worktree HEAD", () => {
  const repo = fixture();
  installScanner(repo);
  writeFileSync(resolve(repo, "main.txt"), "main checkout change\n");
  git(repo, "add", "main.txt");
  git(repo, "commit", "-m", "Main checkout moves on");
  const base = git(repo, "rev-parse", "HEAD~1");
  const worktree = resolve(repo, ".dex/tmp/diverged");
  git(repo, "worktree", "add", "--detach", worktree, base);
  writeFileSync(resolve(worktree, "sample.txt"), "clean diverged change\n");
  git(worktree, "add", "sample.txt");
  assert.equal(run(worktree, "git", ["commit", "-m", "Clean diverged change"]).status, 0,
    "clean staged diff against the worktree HEAD must pass");
  writeFileSync(resolve(worktree, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8), "\n"].join(""));
  git(worktree, "add", "sample.txt");
  const rejected = run(worktree, "git", ["commit", "-m", "Secret diverged change"]);
  assert.notEqual(rejected.status, 0, "staged secret must still be rejected");
  assert.match(rejected.stderr, /Gitleaks/);
});

test("split-index worktrees gate only the staged diff", () => {
  const repo = fixture();
  installScanner(repo);
  const worktree = resolve(repo, ".dex/tmp/splitter");
  git(repo, "worktree", "add", "-b", "splitter", worktree);
  git(worktree, "config", "core.splitIndex", "true");
  writeFileSync(resolve(worktree, "sample.txt"), "clean split-index change\n");
  git(worktree, "add", "sample.txt");
  const indexDir = dirname(git(worktree, "rev-parse", "--path-format=absolute", "--git-path", "index"));
  assert.ok(readdirSync(indexDir).some((entry) => entry.startsWith("sharedindex.")),
    "split index must be engaged for this test");
  assert.equal(run(worktree, "git", ["commit", "-m", "Clean split-index change"]).status, 0,
    "clean staged diff with split index must pass");
  writeFileSync(resolve(worktree, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8), "\n"].join(""));
  git(worktree, "add", "sample.txt");
  const rejected = run(worktree, "git", ["commit", "-m", "Secret split-index change"]);
  assert.notEqual(rejected.status, 0, "staged secret must still be rejected");
  assert.match(rejected.stderr, /Gitleaks/);
});

function installScanner(repo) {
  if (!installedScanner) {
    assert.equal(run(repo, process.execPath, ["scripts/install-hooks.mjs"]).status, 0);
    installedScanner = resolve(repo, GITLEAKS_RELATIVE_PATH);
  } else {
    const target = resolve(repo, GITLEAKS_RELATIVE_PATH);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(installedScanner, target);
    git(repo, "config", "--local", "core.hooksPath", "scripts/hooks");
  }
}

test("repo-local scanner gates staged secrets, identities and messages", () => {
  const repo = fixture();
  installScanner(repo);
  writeFileSync(resolve(repo, "sample.txt"), "clean content\n");
  git(repo, "add", "sample.txt");
  const rejected = (message, env = {}) => {
    const result = run(repo, "git", ["commit", "-m", message], { PATH: `${dirname(installedScanner)}${delimiter}${process.env.PATH}`, ...env });
    assert.notEqual(result.status, 0, "unsafe commit must be rejected");
  };
  const personal = ["person", "mail.test"].join("@");
  rejected("Clean change", { GIT_AUTHOR_EMAIL: personal });
  rejected("Clean change", { GIT_COMMITTER_EMAIL: personal });
  rejected(`Contact ${personal}`);
  rejected(["Host: model", "vm"].join("-"));
  git(repo, "commit", "-m", "Clean change");
  writeFileSync(resolve(repo, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8), "\n"].join(""));
  git(repo, "add", "sample.txt");
  rejected("Secret change");
});

test("hooks fall back to PATH and refuse the wrong scanner version", () => {
  const repo = fixture();
  installScanner(repo);
  const local = resolve(repo, GITLEAKS_RELATIVE_PATH);
  const fallback = resolve(repo, ".dex/tmp/path-scanner", basename(local));
  mkdirSync(dirname(fallback), { recursive: true });
  cpSync(installedScanner, fallback);
  if (installedScanner === local) installedScanner = fallback;
  rmSync(local);
  const path = `${dirname(fallback)}${delimiter}${process.env.PATH}`;
  writeFileSync(resolve(repo, "sample.txt"), "clean\n");
  git(repo, "add", "sample.txt");
  assert.equal(run(repo, "git", ["commit", "-m", "PATH scanner"], { PATH: path }).status, 0);
  if (process.platform === "win32") {
    const data = readFileSync(fallback);
    const offset = data.indexOf(Buffer.from(GITLEAKS_VERSION));
    assert.notEqual(offset, -1, "release binary contains its version");
    Buffer.from("8.29.0").copy(data, offset);
    writeFileSync(local, data);
  } else {
    writeFileSync(local, "#!/bin/sh\necho 8.29.0\n");
    chmodSync(local, 0o755);
  }
  const result = run(repo, "git", ["commit", "--allow-empty", "-m", "Wrong version"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /SHA-256/);
});

test("hooks and publication reject a tampered scanner reporting the pinned version", () => {
  const repo = fixture();
  installScanner(repo);
  const local = resolve(repo, GITLEAKS_RELATIVE_PATH);
  // Appended bytes preserve the real executable's version output.
  writeFileSync(local, Buffer.concat([readFileSync(local), Buffer.from("tampered")]));
  assert.equal(run(repo, local, ["version"]).stdout.trim(), GITLEAKS_VERSION);
  const hook = run(repo, "git", ["commit", "--allow-empty", "-m", "Integrity check"]);
  assert.notEqual(hook.status, 0, "tampered scanner must not gate commits");
  assert.match(hook.stderr, /SHA-256/);
  const { env } = publicRemote(repo);
  const result = publish(repo, env, "--initial");
  assert.notEqual(result.status, 0, "tampered scanner must not gate publication");
  assert.match(result.stderr, /SHA-256/);
});

test("runtime env, browser snapshots, release and tool outputs are ignored", () => {
  const repo = fixture();
  for (const file of ["app.env", "nested/production.env", ".playwright-mcp/page.yml", "release/sample.exe", ".dex/tools/scanner"]) {
    assert.equal(run(repo, "git", ["check-ignore", "--no-index", file]).status, 0);
  }
  assert.equal(run(repo, "git", ["check-ignore", "--no-index", ".env.example"]).status, 1);
});

test("privacy rules reject real endpoints and allow documentation fixtures", () => {
  const repo = fixture();
  installScanner(repo);
  const unsafe = [
    ["D:", "Users", "x"].join("\\"),
    ["E:", "Users", "x"].join("/"),
    ["8", "8", "4", "4"].join("."),
    ["user", "host:"].join("@"),
    "ssh://" + ["user", "host"].join("@"),
    "ssh " + ["synthetic", "fixture-host"].join("@"),
    ["model", "vm"].join("-"),
    ["person", "mail.test"].join("@"),
  ];
  for (const [index, content] of unsafe.entries()) {
    writeFileSync(resolve(repo, "sample.txt"), `${content}\n`);
    git(repo, "add", "sample.txt");
    assert.notEqual(run(repo, "git", ["commit", "-m", "Privacy check"]).status, 0, `private content case ${index} blocked`);
  }
  writeFileSync(resolve(repo, "sample.txt"), "192.0.2.1 198.51.100.2 203.0.113.3 127.0.0.1 person@example.com person@demo.invalid\n");
  git(repo, "add", "sample.txt");
  git(repo, "commit", "-m", "Documented fixtures");
});

function publicRemote(repo, commit) {
  const remote = resolve(repo, ".dex/tmp/public.git");
  mkdirSync(dirname(remote), { recursive: true });
  git(repo, "init", "--bare", remote);
  if (commit) git(repo, "--git-dir", remote, "fetch", "--no-tags", repo, `${commit}:refs/heads/main`);
  return { remote, env: { DEX_PUBLIC_TEST_REMOTE: pathToFileURL(remote).href } };
}
function publish(repo, env, ...args) {
  return run(repo, process.execPath, ["scripts/publish-public.mjs", "--dry-run", ...args], env);
}

function scanAbsoluteFixture(repo, path) {
  const tree = resolve(repo, ".dex/tmp/absolute-tree");
  mkdirSync(dirname(resolve(tree, path)), { recursive: true });
  cpSync(resolve(repo, path), resolve(tree, path));
  const ignore = resolve(repo, ".dex/tmp/empty.ignore");
  writeFileSync(ignore, "");
  const result = run(repo, resolve(repo, GITLEAKS_RELATIVE_PATH), ["dir", tree,
    "--config", resolve(repo, ".gitleaks.toml"), "--gitleaks-ignore-path", ignore,
    "--redact", "--no-banner", "--ignore-gitleaks-allow"]);
  assert.equal(result.status, 0, "absolute directory scan must accept exact public fixtures");
}

for (const initial of [false, true]) {
  for (const personalData of ["email", "public-ipv4"]) {
    test(`publication uses source policy with weaker HEAD: ${personalData}, initial=${initial}`, () => {
      const repo = fixture();
      const base = git(repo, "rev-parse", "HEAD");
      const content = personalData === "email" ? ["person", "mail.test"].join("@") : ["8", "8", "4", "4"].join(".");
      writeFileSync(resolve(repo, "sample.txt"), `${content}\n`);
      git(repo, "add", "sample.txt");
      git(repo, "commit", "-m", "Outgoing change");
      const source = git(repo, "rev-parse", "team/dev");
      git(repo, "checkout", "-b", "worker", base);
      const policy = readFileSync(resolve(repo, ".gitleaks.toml"), "utf8");
      const weakPolicy = policy.replace(/\[\[rules\]\]\r?\nid = "dex-(?:personal-email|public-ipv4)"[\s\S]*?(?=\r?\n\[\[rules\]\]|$)/g, "");
      assert.doesNotMatch(weakPolicy, /id = "dex-(?:personal-email|public-ipv4)"/);
      writeFileSync(resolve(repo, ".gitleaks.toml"), weakPolicy);
      git(repo, "add", ".gitleaks.toml");
      git(repo, "commit", "-m", "Checkout without privacy rules");
      installScanner(repo);
      const { remote, env } = publicRemote(repo, initial ? undefined : base);
      const before = git(repo, "--git-dir", remote, "for-each-ref");
      const result = run(repo, process.execPath, ["scripts/publish-public.mjs", ...(initial ? ["--initial"] : ["sync-policy"])], env);
      assert.notEqual(result.status, 0, "source personal data must block publication despite weak HEAD policy");
      assert.match(result.stderr, /Gitleaks taramasi reddetti/);
      assert.ok(result.stdout.includes(`kaynak politikasi kullaniliyor (${source}:.gitleaks.toml)`));
      assert.equal(git(repo, "--git-dir", remote, "for-each-ref"), before, "rejection leaves public refs unchanged");
    });
  }
}

test("initial dry-run creates one clean root without updating public refs", () => {
  const repo = fixture();
  installScanner(repo);
  const { remote, env } = publicRemote(repo);
  const result = publish(repo, env, "--initial");
  assert.equal(result.status, 0, `initial publication passes: ${result.stderr}`);
  const sha = result.stdout.match(/root=([a-f0-9]{40})/)?.[1];
  assert.ok(sha, "new root SHA is reported");
  assert.equal(git(repo, "rev-list", "--count", sha), "1");
  assert.equal(git(repo, "rev-parse", `${sha}^{tree}`), git(repo, "rev-parse", "team/dev^{tree}"));
  assert.match(git(repo, "ls-tree", sha, "scripts/hooks/commit-msg"), /^100755/);
  assert.equal(git(repo, "--git-dir", remote, "for-each-ref"), "", "dry-run leaves remote empty");
  assert.match(result.stdout, /Do not use this SHA for migration/);
  assert.match(result.stdout, /Next steps/);
});

test("initial metadata uses one current UTC instant instead of inherited dates", () => {
  const repo = fixture();
  installScanner(repo);
  const { env } = publicRemote(repo);
  const before = Math.floor(Date.now() / 1000);
  const result = publish(repo, { ...env,
    GIT_AUTHOR_DATE: "2001-01-01T00:00:00+0300",
    GIT_COMMITTER_DATE: "2002-01-01T00:00:00-0700",
  }, "--initial");
  const after = Math.floor(Date.now() / 1000);
  assert.equal(result.status, 0, `initial publication passes: ${result.stderr}`);
  const sha = result.stdout.match(/root=([a-f0-9]{40})/)?.[1];
  assert.ok(sha);
  const metadata = git(repo, "cat-file", "commit", sha);
  const author = metadata.match(/^author hakunokina <hakunokina@users\.noreply\.github\.com> (\d+) ([+-]\d{4})$/m);
  const committer = metadata.match(/^committer hakunokina <hakunokina@users\.noreply\.github\.com> (\d+) ([+-]\d{4})$/m);
  assert.ok(author, "fixed public author is present");
  assert.ok(committer, "fixed public committer is present");
  assert.equal(author[2], "+0000");
  assert.equal(committer[2], "+0000");
  assert.equal(author[1], committer[1], "author and committer share the controlled instant");
  assert.ok(Number(author[1]) >= before && Number(author[1]) <= after, "timestamp comes from this run");
});

test("initial push supports migration to the verified root and normal publication", () => {
  const repo = fixture();
  const oldTip = git(repo, "rev-parse", "team/dev");
  git(repo, "branch", "archive/pre-public", oldTip);
  installScanner(repo);
  const { remote, env } = publicRemote(repo);
  const initial = run(repo, process.execPath, ["scripts/publish-public.mjs", "--initial"], env);
  assert.equal(initial.status, 0, `initial push passes: ${initial.stderr}`);
  const sha = initial.stdout.match(/root=([a-f0-9]{40})/)?.[1];
  assert.ok(sha, "published root is reported");
  assert.equal(git(repo, "--git-dir", remote, "rev-parse", "main"), sha);
  assert.equal(git(repo, "rev-parse", "team/dev"), oldTip, "publication never moves the source branch");
  assert.notEqual(publish(repo, env).status, 0, "old source must fail the ancestry gate");
  git(repo, "fetch", "--no-tags", env.DEX_PUBLIC_TEST_REMOTE, "refs/heads/main");
  assert.equal(git(repo, "rev-parse", "FETCH_HEAD"), sha);
  assert.equal(git(repo, "rev-list", "--parents", "-n", "1", "FETCH_HEAD"), sha);
  git(repo, "diff", "--exit-code", "team/dev", "FETCH_HEAD", "--");
  assert.equal(git(repo, "status", "--short"), "");
  git(repo, "branch", "archive/pre-initial-1456", "team/dev");
  git(repo, "reset", "--soft", sha);
  assert.equal(git(repo, "status", "--short"), "");
  assert.equal(git(repo, "rev-parse", "archive/pre-public"), oldTip);
  assert.equal(git(repo, "rev-parse", "archive/pre-initial-1456"), oldTip);
  assert.equal(publish(repo, env).status, 0, "normal dry-run accepts the migrated root");
  writeFileSync(resolve(repo, "sample.txt"), "clean change after migration\n");
  git(repo, "add", "sample.txt");
  git(repo, "commit", "-m", "Change after public migration");
  const source = git(repo, "rev-parse", "team/dev");
  const normal = run(repo, process.execPath, ["scripts/publish-public.mjs"], env);
  assert.equal(normal.status, 0, `normal push passes: ${normal.stderr}`);
  assert.equal(git(repo, "--git-dir", remote, "rev-parse", "main"), source);
  assert.equal(git(repo, "rev-list", "--count", "team/dev"), "2", "old history is excluded");
  const repeated = run(repo, process.execPath, ["scripts/publish-public.mjs"], env);
  assert.equal(repeated.status, 0, `repeated publication passes: ${repeated.stderr}`);
  assert.equal(git(repo, "--git-dir", remote, "rev-parse", "main"), source);
  assert.match(initial.stdout, /Next steps/);
  assert.ok(initial.stdout.includes(`git fetch --no-tags ${env.DEX_PUBLIC_TEST_REMOTE} refs/heads/main`));
  assert.match(initial.stdout, /archive\/pre-initial-1456/);
  assert.match(initial.stdout, /Do not merge old worker branches/);
});

for (const scenario of ["removed-secret", "author", "committer", "message", "private-archive", "private-master"]) {
  test(`publication rejects ${scenario} in the outgoing history`, () => {
    const repo = fixture();
    const base = git(repo, "rev-parse", "HEAD");
    const personal = ["person", "mail.test"].join("@");
    if (scenario.startsWith("private-")) {
      const sha = git(repo, "commit-tree", `${base}^{tree}`, "-m", "Private history root");
      git(repo, "update-ref", scenario === "private-archive" ? "refs/heads/archive/pre-public" : "refs/heads/master", sha);
      git(repo, "merge", "--no-ff", "--allow-unrelated-histories", sha, "-m", "Unsafe history merge");
    } else {
      writeFileSync(resolve(repo, "sample.txt"), scenario === "removed-secret" ? ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join("") : "clean\n");
      git(repo, "add", "sample.txt");
      const env = scenario === "author" ? { GIT_AUTHOR_EMAIL: personal } : scenario === "committer" ? { GIT_COMMITTER_EMAIL: personal } : {};
      assert.equal(run(repo, "git", ["commit", "-m", scenario === "message" ? `Contact ${personal}` : "Outgoing change"], env).status, 0);
      if (scenario === "removed-secret") {
        writeFileSync(resolve(repo, "sample.txt"), "clean\n");
        git(repo, "add", "sample.txt");
        git(repo, "commit", "-m", "Remove leaked content");
      }
    }
    installScanner(repo);
    const { env } = publicRemote(repo, base);
    const result = publish(repo, env, "sync-3");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, scenario.startsWith("private-") ? /Private archive\/master/ : scenario === "author" || scenario === "committer" ? /noreply/ : /Gitleaks/);
  });
}

test("publication rejects a source without public ancestry and both URL rewrites", () => {
  const repo = fixture();
  installScanner(repo);
  const orphan = git(repo, "commit-tree", "HEAD^{tree}", "-m", "Unrelated public root");
  const { env } = publicRemote(repo, orphan);
  assert.match(publish(repo, env).stderr, /merge-base/);
  for (const kind of ["insteadOf", "pushInsteadOf"]) {
    git(repo, "config", `url.file:///blocked/.${kind}`, env.DEX_PUBLIC_TEST_REMOTE);
    const result = publish(repo, env);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /rewrite/);
    git(repo, "config", "--unset", `url.file:///blocked/.${kind}`);
  }
  assert.notEqual(publish(repo, { DEX_PUBLIC_TEST_REMOTE: "https://example.com/test.git" }).status, 0);
});

test("commit hooks ignore unstaged policy weakening and ignore fingerprints", () => {
  const repo = fixture();
  installScanner(repo);
  const secret = ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join("");
  writeFileSync(resolve(repo, "sample.txt"), `${secret}\n`);
  git(repo, "add", "sample.txt");
  writeFileSync(resolve(repo, ".gitleaks.toml"), 'title = "Weakened unstaged policy"\n');
  writeFileSync(resolve(repo, ".gitleaksignore"), "sample.txt:dex-zai-api-key:1\n");
  const result = run(repo, "git", ["commit", "-m", "Secret change"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Gitleaks/);
});

test("hooks reject uppercase hex in Z.ai keys", () => {
  const repo = fixture();
  installScanner(repo);
  writeFileSync(resolve(repo, "sample.txt"), ["ABCDEF0123456789".repeat(2), ".", "A1".repeat(8)].join(""));
  git(repo, "add", "sample.txt");
  const result = run(repo, "git", ["commit", "-m", "Uppercase key check"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Gitleaks/);
});

test("permit fixtures are exempt only at their exact file and endpoint values", () => {
  const repo = fixture();
  installScanner(repo);
  const path = "crates/dex-core/src/permit.rs";
  mkdirSync(dirname(resolve(repo, path)), { recursive: true });
  cpSync(resolve(root, path), resolve(repo, path));
  git(repo, "add", path);
  git(repo, "commit", "-m", "Synthetic permit fixtures");
  const { env } = publicRemote(repo);
  assert.equal(publish(repo, env, "--initial").status, 0, "permit fixtures must also pass tree scanning");
  scanAbsoluteFixture(repo, path);
  const endpoint = ["deploy", "192.0.2.10:/srv/git/dex.git"].join("@");
  writeFileSync(resolve(repo, "sample.txt"), endpoint);
  git(repo, "add", "sample.txt");
  assert.notEqual(run(repo, "git", ["commit", "-m", "Copied fixture"]).status, 0);
  git(repo, "reset", "--", "sample.txt");
  for (const content of [endpoint.replace("dex.git", "real.git"), ["git", "personal.test"].join("@")]) {
    writeFileSync(resolve(repo, path), `${content}\n`);
    git(repo, "add", path);
    assert.notEqual(run(repo, "git", ["commit", "-m", "Changed fixture"]).status, 0);
  }
});

test("publication ignores source fingerprints for a removed secret", () => {
  const repo = fixture();
  const base = git(repo, "rev-parse", "HEAD");
  writeFileSync(resolve(repo, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join(""));
  git(repo, "add", "sample.txt");
  git(repo, "commit", "-m", "Synthetic historical leak");
  const bad = git(repo, "rev-parse", "HEAD");
  git(repo, "rm", "sample.txt");
  git(repo, "commit", "-m", "Remove synthetic leak");
  // Untracked ignore must not affect scanning, even when it holds the exact fingerprint.
  writeFileSync(resolve(repo, ".gitleaksignore"), `${bad}:sample.txt:dex-zai-api-key:1\n`);
  installScanner(repo);
  const { env } = publicRemote(repo, base);
  const result = publish(repo, env);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Gitleaks/);
});

for (const attribute of ["-diff", "binary"]) {
  test(`hooks scan staged text despite ${attribute} attributes`, () => {
    const repo = fixture();
    installScanner(repo);
    writeFileSync(resolve(repo, ".gitattributes"), `sample.txt ${attribute}\n`);
    writeFileSync(resolve(repo, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join(""));
    git(repo, "add", ".gitattributes", "sample.txt");
    const result = run(repo, "git", ["commit", "-m", "Attribute check"]);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Gitleaks/);
  });
}

test("publication scans removed text despite repository and global attributes", () => {
  const repo = fixture();
  const base = git(repo, "rev-parse", "HEAD");
  writeFileSync(resolve(repo, ".gitattributes"), "sample.txt binary\n");
  writeFileSync(resolve(repo, "sample.txt"), ["0123456789abcdef".repeat(2), ".", "A1".repeat(8)].join(""));
  git(repo, "add", ".gitattributes", "sample.txt");
  git(repo, "commit", "-m", "Synthetic hidden history");
  git(repo, "rm", "sample.txt");
  git(repo, "commit", "-m", "Remove synthetic history");
  const attributes = resolve(repo, ".dex/tmp/attributes");
  mkdirSync(dirname(attributes), { recursive: true });
  writeFileSync(attributes, "sample.txt -diff\n");
  git(repo, "config", "core.attributesFile", attributes);
  writeFileSync(resolve(repo, ".git/info/attributes"), "sample.txt -diff\n");
  installScanner(repo);
  const { env } = publicRemote(repo, base);
  const result = publish(repo, env);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Gitleaks/);
});

for (const scenario of ["replace", "grafts"]) {
  test(`hooks and publication refuse ${scenario} history overrides`, () => {
    const repo = fixture();
    if (scenario === "replace") {
      const replacement = git(repo, "commit-tree", "HEAD^{tree}", "-m", "Synthetic replacement");
      git(repo, "replace", "HEAD", replacement);
    } else {
      writeFileSync(resolve(repo, ".git/info/grafts"), "");
    }
    installScanner(repo);
    const hook = run(repo, "git", ["commit", "--allow-empty", "-m", "History check"], { GIT_NO_REPLACE_OBJECTS: "0" });
    assert.notEqual(hook.status, 0);
    assert.match(hook.stderr, /replace|grafts/);
    const { env } = publicRemote(repo);
    const result = publish(repo, { ...env, GIT_NO_REPLACE_OBJECTS: "0" }, "--initial");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /replace|grafts/);
  });
}

test("linked worktree hooks refuse common replacements and grafts", () => {
  const repo = fixture();
  installScanner(repo);
  const worktree = resolve(repo, ".dex/tmp/worker");
  git(repo, "worktree", "add", "-b", "worker", worktree);
  const replacement = git(repo, "commit-tree", "HEAD^{tree}", "-m", "Synthetic replacement");
  git(repo, "replace", "HEAD", replacement);
  const replaced = run(worktree, "git", ["commit", "--allow-empty", "-m", "History check"]);
  assert.notEqual(replaced.status, 0);
  assert.match(replaced.stderr, /replace/);
  git(repo, "replace", "-d", "HEAD");
  writeFileSync(resolve(repo, ".git/info/grafts"), "");
  const grafted = run(worktree, "git", ["commit", "--allow-empty", "-m", "History check"]);
  assert.notEqual(grafted.status, 0);
  assert.match(grafted.stderr, /grafts/);
});

for (const initial of [false, true]) {
  test(`publication refuses a tracked ignore file, initial=${initial}`, () => {
    const repo = fixture();
    const base = git(repo, "rev-parse", "HEAD");
    writeFileSync(resolve(repo, ".gitleaksignore"), "");
    git(repo, "add", ".gitleaksignore");
    git(repo, "commit", "-m", "Suppression file");
    installScanner(repo);
    const { env } = publicRemote(repo, initial ? undefined : base);
    const result = publish(repo, env, ...(initial ? ["--initial"] : []));
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /\.gitleaksignore/);
  });
}

test("publication refuses an ignore file nested in the source tree", () => {
  const repo = fixture();
  mkdirSync(resolve(repo, "nested"));
  writeFileSync(resolve(repo, "nested/.gitleaksignore"), "");
  git(repo, "add", "nested/.gitleaksignore");
  git(repo, "commit", "-m", "Nested suppression file");
  installScanner(repo);
  const { env } = publicRemote(repo);
  const result = publish(repo, env, "--initial");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /\.gitleaksignore/);
});

test("CI selects the base policy and bootstraps only when the base file is absent", () => {
  const workflow = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8").replaceAll("\r\n", "\n");
  const block = workflow.split("- name: Select trusted gitleaks policy")[1].split("- name: Scan HEAD tree")[0];
  const script = block.split("run: |\n")[1].replace(/^ {10}/gm, "");
  for (const hasPolicy of [true, false]) {
    const repo = fixture();
    const original = readFileSync(resolve(repo, ".gitleaks.toml"), "utf8");
    if (!hasPolicy) {
      git(repo, "rm", ".gitleaks.toml");
      git(repo, "commit", "-m", "Base predates policy");
    }
    const { remote } = publicRemote(repo, git(repo, "rev-parse", "HEAD"));
    git(repo, "remote", "add", "origin", remote);
    writeFileSync(resolve(repo, ".gitleaks.toml"), hasPolicy ? 'title = "Untrusted PR policy"\n' : original);
    const temp = resolve(repo, ".dex/tmp/ci");
    mkdirSync(temp, { recursive: true });
    const file = resolve(temp, "select-policy.ps1");
    writeFileSync(file, script);
    const env = { GITHUB_EVENT_NAME: "pull_request", BASE_REF: "main", RUNNER_TEMP: temp, GITHUB_ENV: resolve(temp, "github-env") };
    const shell = process.platform === "win32" ? "powershell.exe" : "pwsh";
    const result = run(repo, shell, ["-NoProfile", "-NonInteractive", "-File", file], env);
    assert.equal(result.status, 0, `CI policy selection succeeds: ${result.stderr}`);
    assert.equal(readFileSync(resolve(temp, "trusted.gitleaks.toml"), "utf8").replaceAll("\r\n", "\n"), original.replaceAll("\r\n", "\n"));
    assert.equal(readFileSync(resolve(temp, "empty.gitleaksignore"), "utf8"), "");
  }
});

test("normal dry-run checks public ancestry and rejects forbidden sources", () => {
  const repo = fixture();
  const base = git(repo, "rev-parse", "HEAD");
  writeFileSync(resolve(repo, "sample.txt"), "clean content\n");
  git(repo, "add", "sample.txt");
  git(repo, "commit", "-m", "Clean outgoing change");
  installScanner(repo);
  const { remote, env } = publicRemote(repo, base);
  const clean = publish(repo, env, "sync-2");
  assert.equal(clean.status, 0, `clean range passes: ${clean.stderr}`);
  assert.notEqual(publish(repo, env, "--source", "archive/pre-public").status, 0);
  assert.notEqual(publish(repo, env, "master").status, 0);
  assert.notEqual(publish(repo, env, "--all").status, 0);
  assert.notEqual(publish(repo, env, "--initial").status, 0, "initial mode rejects nonempty remote");
  assert.equal(git(repo, "--git-dir", remote, "rev-parse", "main"), base);
});
