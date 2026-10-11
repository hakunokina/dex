// `include!`d by the build scripts of dexd and src-tauri (the leaves — a new commit must not
// rebuild dex-core): exports `DEX_COMMIT` (short sha) and `DEX_COMMIT_TIME` (unix secs, 0 =
// unknown) so a client can tell how old a host's build is. `DEX_BUILD_COMMIT` / `DEX_BUILD_COMMIT_TIME`
// in the environment win (a `git archive` build has no .git); no git at all → "unknown" / 0. Not the
// exported names: `cargo run` (tauri dev) puts `rustc-env`s into the running app's environment, so
// every agent and shell Dex starts saw a stale `DEX_COMMIT` and stamped its own builds with it (#203).

fn emit_commit() {
    use std::path::Path;
    use std::process::Command;

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !s.is_empty()).then_some(s)
    };
    println!("cargo:rerun-if-env-changed=DEX_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=DEX_BUILD_COMMIT_TIME");
    // HEAD moves on checkout; the branch ref (loose or packed) on commit. Worktree-safe: --git-path.
    // Only paths that exist (#400): cargo reruns a script whose watched file is missing on every
    // build — a repo without packed-refs relinked dexd each time, and a new exe looks like an update.
    let mut watch = vec!["HEAD".to_string(), "packed-refs".to_string()];
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch.push(r);
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    for w in watch {
        if let Some(p) = git(&["rev-parse", "--git-path", &w]).map(|p| Path::new(&dir).join(p)).filter(|p| p.exists()) {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    let commit = std::env::var("DEX_BUILD_COMMIT").ok().filter(|s| !s.is_empty()).or_else(|| git(&["rev-parse", "--short=10", "HEAD"]));
    let time = std::env::var("DEX_BUILD_COMMIT_TIME").ok().or_else(|| git(&["log", "-1", "--format=%ct"]));
    println!("cargo:rustc-env=DEX_COMMIT={}", commit.unwrap_or_else(|| "unknown".into()));
    println!("cargo:rustc-env=DEX_COMMIT_TIME={}", time.and_then(|t| t.parse::<i64>().ok()).unwrap_or(0));
}
