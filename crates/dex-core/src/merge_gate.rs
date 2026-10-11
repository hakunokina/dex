//! Target-committed policy opts a repository into fail-closed delegation merge checks.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::worktree::{git, hidden};

#[cfg(test)]
thread_local! {
    static BEFORE_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn before_scan(f: impl FnOnce(&Path) + 'static) {
    BEFORE_SCAN.with(|hook| *hook.borrow_mut() = Some(Box::new(f)));
}

pub(crate) const GITLEAKS_VERSION: &str = "8.30.0";
pub(crate) const GITLEAKS_DIR: &str = ".dex/tools/gitleaks";
// Executables extracted from checksum-verified official v8.30.0 release archives.
const GITLEAKS_HASHES: &[(&str, &str, &str)] = &[
    ("windows", "x86_64", "9d08e3f5cfb35a98f230b97bcda24f8d3fc66363c91868ffc98dac0afebdcb72"),
    ("macos", "aarch64", "095bc4be1624239f52ebb90ff91668d1d3e78e0f8c5286e2d981887905c27561"),
    ("linux", "x86_64", "8b6fd684fcd5b4ebe39b68abb072ce59e1063ce7ed4abd556157697845f1f088"),
];
/// Reserved by permit regardless of the worker's writable roots.
pub(crate) const SCRATCH_DIR: &str = "dex-core-merge-scan";

pub(crate) fn gitleaks_path(root: &Path, search: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "gitleaks.exe" } else { "gitleaks" };
    let local = root.join(GITLEAKS_DIR).join(GITLEAKS_VERSION).join(exe);
    if local.is_file() { return Some(local); }
    search.and_then(|p| std::env::split_paths(p).map(|d| d.join(exe)).find(|p| p.is_file()))
}

/// Reject object rewriting before callers compute diffs or ancestry.
pub(crate) fn check_repository(root: &Path, target: &str) -> Result<Option<Value>, String> {
    if git(root, &["ls-tree", "--name-only", target, "--", ".gitleaks.toml"])?.is_empty() {
        return Ok(None);
    }
    let refs = git(root, &["for-each-ref", "--format=%(refname)", "refs/replace/"])?;
    // --git-path also resolves the shared repository behind a linked worktree.
    let grafts = git(root, &["rev-parse", "--git-path", "info/grafts"])?;
    let grafts = root.join(grafts).try_exists().map_err(|_| "Git graft dosyası denetlenemedi; birleştirme durduruldu".to_string())?;
    if !refs.is_empty() || grafts {
        return Ok(Some(json!({ "merged": false, "code": "merge_git_rewrite", "note": "refs/replace veya info/grafts bulundu; birleştirme durduruldu" })));
    }
    Ok(None)
}

/// Use the target's committed policy and the entire incoming history, even deleted secrets.
/// Never return scanner stdout/stderr: reports include source lines and author metadata.
fn scan_incoming(root: &Path, target: &str, tip: &str, tool: Option<&Path>) -> Result<Option<Value>, String> {
    // Only absence in the target's tree opts out; Git errors must still fail closed.
    if git(root, &["ls-tree", "--name-only", target, "--", ".gitleaks.toml"])?.is_empty() {
        return Ok(None);
    }
    if let Some(refusal) = check_repository(root, target)? {
        return Ok(Some(refusal));
    }
    let refusal = |code: &str, note: &str, findings: Vec<Value>| {
        Some(json!({ "merged": false, "code": code, "note": note, "findings": findings }))
    };
    let Some(tool) = tool else {
        return Ok(refusal("gitleaks_missing", "gitleaks bulunamadı: npm run hooks:install", vec![]));
    };
    let expected = GITLEAKS_HASHES.iter().find(|(os, arch, _)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH).map(|(_, _, hash)| *hash);
    let hash = std::fs::read(tool).ok().map(|data| format!("{:x}", Sha256::digest(data)));
    if expected.is_none() || hash.as_deref() != expected {
        return Ok(refusal("gitleaks_integrity", "gitleaks ikili SHA-256 doğrulaması başarısız veya platform desteklenmiyor: npm run hooks:install", vec![]));
    }
    let version = hidden(Command::new(tool).arg("version")).output();
    if !matches!(version, Ok(ref o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == GITLEAKS_VERSION) {
        return Ok(refusal("gitleaks_unavailable", "gitleaks 8.30.0 çalıştırılamadı: npm run hooks:install", vec![]));
    }
    let base = git(root, &["merge-base", target, tip])?;
    let range = format!("{base}..{tip}");
    let identities = git(root, &["log", "--format=%H%x00%ae%x00%ce", &range])?;
    let mut invalid = Vec::new();
    for line in identities.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        if fields.len() != 3 { return Err("commit kimliği okunamadı; birleştirme durduruldu".into()); }
        for (role, email) in [("author", fields[1]), ("committer", fields[2])] {
            let noreply = email.to_ascii_lowercase().ends_with("@users.noreply.github.com") && !email.starts_with('@');
            if !noreply {
                invalid.push(json!({ "commit": fields[0], "role": role, "email": "[REDACTED]" }));
            }
        }
    }
    if !invalid.is_empty() {
        return Ok(refusal("merge_identity", "düzelt: author/committer e-postası *@users.noreply.github.com olmalı (redakte)", invalid));
    }
    let config = git(root, &["show", &format!("{target}:.gitleaks.toml")])
        .map_err(|_| "hedef dalda .gitleaks.toml okunamadı; birleştirme durduruldu".to_string())?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| "tarama için geçici dizin oluşturulamadı".to_string())?;
    // Policy and bare source stay outside the project's shared worker scratch.
    let scratch = std::env::temp_dir().join(SCRATCH_DIR);
    std::fs::create_dir_all(&scratch).map_err(|_| "tarama için geçici dizin oluşturulamadı".to_string())?;
    let dir = scratch.join(nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
    std::fs::create_dir(&dir).map_err(|_| "tarama için geçici dizin oluşturulamadı".to_string())?;
    let result = (|| {
        let policy = dir.join(".gitleaks.toml");
        std::fs::write(&policy, config).map_err(|_| "tarama politikası yazılamadı".to_string())?;
        // v8.30 also loads <source>/.gitleaksignore regardless of --gitleaks-ignore-path.
        // A bare shared clone reads the same objects without any working-tree overrides.
        let source = dir.join("repo.git");
        git(root, &["clone", "--bare", "--shared", "--quiet", "--", &root.to_string_lossy(), &source.to_string_lossy()])
            .map_err(|_| "tarama deposu oluşturulamadı; birleştirme durduruldu".to_string())?;
        let messages = dir.join("messages");
        std::fs::create_dir(&messages).map_err(|_| "commit mesajları için dizin oluşturulamadı".to_string())?;
        for line in identities.lines() {
            let commit = line.split('\0').next().unwrap();
            let message = git(&source, &["show", "-s", "--format=%B", commit])
                .map_err(|_| "commit mesajı okunamadı; birleştirme durduruldu".to_string())?;
            std::fs::write(messages.join(format!("{commit}.txt")), message)
                .map_err(|_| "commit mesajı taramaya hazırlanamadı".to_string())?;
        }
        #[cfg(test)]
        BEFORE_SCAN.with(|hook| {
            if let Some(f) = hook.borrow_mut().take() { f(&dir); }
        });
        for message_scan in [false, true] {
            let mut cmd = Command::new(tool);
            cmd.current_dir(&dir);
            if message_scan {
                cmd.arg("dir").arg(&messages);
            } else {
                cmd.arg("git").arg(&source).arg("--log-opts").arg(format!("--full-history --diff-merges=separate {range}"));
            }
            cmd.arg("--config").arg(&policy)
                .args(["--redact", "--no-banner", "--no-color", "--ignore-gitleaks-allow", "--report-format", "json", "--report-path", "-", "--exit-code", "42"])
                .arg("--gitleaks-ignore-path").arg(&dir);
            for (key, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_")) {
                cmd.env_remove(key);
            }
            cmd.env("GIT_NO_REPLACE_OBJECTS", "1");
            let out = hidden(&mut cmd).output().map_err(|_| "gitleaks çalıştırılamadı; birleştirme durduruldu".to_string())?;
            if !out.status.success() && out.status.code() != Some(42) {
                return Ok(refusal("gitleaks_failed", "gitleaks taraması başarısız; birleştirme durduruldu", vec![]));
            }
            let report: Vec<Value> = serde_json::from_slice(&out.stdout).map_err(|_| "gitleaks raporu okunamadı; birleştirme durduruldu".to_string())?;
            if !report.is_empty() || !out.status.success() {
                let findings = report.iter().map(|f| {
                    let commit = if message_scan {
                        json!(Path::new(f["File"].as_str().unwrap_or("")).file_stem().and_then(|s| s.to_str()))
                    } else { f["Commit"].clone() };
                    json!({ "rule": f["RuleID"], "commit": commit, "line": f["StartLine"] })
                }).collect();
                return Ok(refusal("merge_leaks", "düzelt: gelen commit aralığında veya mesajında sızıntı bulundu; birleştirme yapılmadı (bulgular redakte)", findings));
            }
        }
        Ok(None)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

pub(crate) fn check(root: &Path, target: &str, tip: &str) -> Result<Option<Value>, String> {
    let tool = gitleaks_path(root, std::env::var_os("PATH").as_deref());
    scan_incoming(root, target, tip, tool.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opted_in_target_rejects_repo_local_personal_email() {
        let root = crate::worktree::tests::repo("local-personal-email");
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let email = format!("synthetic-person@{}", "mail.test");
        git(&root, &["config", "user.email", &email]).unwrap();
        git(&root, &["commit", "--allow-empty", "-q", "-m", "local identity fixture"]).unwrap();
        let tip = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let out = check(&root, &target, &tip).unwrap().unwrap();
        assert_eq!(out["code"], "merge_identity", "{out}");
        assert!(!out.to_string().contains(&email));
    }

    #[test]
    fn target_without_policy_merges_personal_identity_without_gitleaks() {
        use crate::worktree::{self, tests::{commit, db_for, job, repo}};
        let root = repo("merge-without-policy");
        git(&root, &["rm", ".gitleaks.toml"]).unwrap();
        commit(&root, "remove opt-in policy fixture");
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let w = worktree::prepare(&db, &d).unwrap().unwrap();
        std::fs::write(w.path.join("b.txt"), "b\n").unwrap();
        git(&w.path, &["add", "b.txt"]).unwrap();
        let message = format!("synthetic-person@{}\n\n{}.{}", "mail.test", "a0".repeat(16), "B1".repeat(8));
        git(&w.path, &["-c", "user.email=person@example.invalid", "commit", "-q", "-m", &message]).unwrap();
        // Working-tree policy does not opt the target's committed tree in.
        std::fs::write(root.join(".gitleaks.toml"), "invalid policy fixture").unwrap();
        let tip = git(&w.path, &["rev-parse", "HEAD"]).unwrap();
        let replacement = git(&root, &["commit-tree", &format!("{target}^{{tree}}"), "-m", "opt-out replacement fixture"]).unwrap();
        git(&root, &["replace", &target, &replacement]).unwrap();
        let parent = git(&root, &["rev-parse", &format!("{target}^")]).unwrap();
        std::fs::write(root.join(".git/info/grafts"), format!("{target} {parent}\n")).unwrap();
        assert_eq!(scan_incoming(&root, &target, &tip, None).unwrap(), None);
        let leader = crate::db::with(&db, |c| {
            crate::delegate::set_status(c, d.id, "done")?;
            crate::delegate::caller(c, "timur")
        }).unwrap();
        let out = worktree::merge(&db, &leader, d.id, false, false, &|_| false).unwrap();
        assert_eq!(out["merged"], true, "{out}");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b\n");
        assert!(git(&root, &["merge-base", "--is-ancestor", &tip, "HEAD"]).is_ok());
    }

    #[test]
    fn concurrent_worker_cannot_weaken_scan_policy() {
        use crate::permit::{decide, Ctx, Verdict};
        let root = crate::worktree::tests::repo("scan-policy-race");
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        git(&root, &["checkout", "-q", "-b", "worker"]).unwrap();
        let secret = format!("{}.{}", "a0".repeat(16), "B1".repeat(8));
        std::fs::write(root.join("b.txt"), &secret).unwrap();
        crate::worktree::tests::commit(&root, "synthetic fixture");
        let tip = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let attack_root = root.clone();
        let fingerprint = format!("{tip}:b.txt:dex-zai-api-key:1\n");
        let blocked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let attempts = blocked.clone();
        before_scan(move |dir| {
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    // Even a broad write root must not open the core's scratch area.
                    let ctx = Ctx { cwd: Some(attack_root.clone()), write: vec![attack_root, std::env::temp_dir()], ..Default::default() };
                    for (path, content) in [
                        (dir.join(".gitleaks.toml"), "[[rules]]\nid = 'disabled'\nregex = 'never-matches-this-fixture'\n".to_string()),
                        (dir.join("repo.git/.gitleaksignore"), fingerprint),
                    ] {
                        let verdict = decide("apply_patch", &json!({ "patch": format!("*** Begin Patch\n*** Add File: {}\n+disabled\n*** End Patch", path.display()) }), &ctx);
                        if verdict == Verdict::Allow { std::fs::write(path, content).unwrap(); }
                        attempts.lock().unwrap().push(verdict);
                    }
                }).join().unwrap();
            });
        });
        let tool = gitleaks_path(&root, None).unwrap();
        let out = scan_incoming(&root, &target, &tip, Some(&tool)).unwrap();
        assert_eq!(out.as_ref().map(|v| v["code"].as_str().unwrap()), Some("merge_leaks"), "{out:?}");
        let blocked = blocked.lock().unwrap();
        assert_eq!(blocked.len(), 2);
        assert!(blocked.iter().all(|v| matches!(v, Verdict::Deny(_))));
    }

    #[test]
    fn altered_gitleaks_with_the_pinned_version_is_rejected() {
        let root = crate::worktree::tests::repo("altered-scanner");
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let tool = gitleaks_path(&root, None).unwrap();
        let mut altered = std::fs::read(&tool).unwrap();
        altered.extend_from_slice(b"integrity regression fixture");
        std::fs::write(&tool, altered).unwrap();
        // PE/ELF still reports the pinned version; macOS may reject the broken signature.
        #[cfg(not(target_os = "macos"))]
        {
            let version = hidden(Command::new(&tool).arg("version")).output().unwrap();
            assert!(version.status.success());
            assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), GITLEAKS_VERSION);
        }
        let out = scan_incoming(&root, &target, &target, Some(&tool)).unwrap().unwrap();
        assert_eq!(out["code"], "gitleaks_integrity", "{out}");
    }

    #[test]
    fn missing_gitleaks_refuses_the_merge() {
        let root = crate::worktree::tests::repo("policy-without-gitleaks");
        let target = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let out = scan_incoming(&root, &target, &target, None).unwrap().unwrap();
        assert_eq!(out["merged"], false);
        assert_eq!(out["code"], "gitleaks_missing");
        assert_eq!(out["note"], "gitleaks bulunamadı: npm run hooks:install");
    }

    #[test]
    fn repo_tool_precedes_path_and_missing_paths_return_none() {
        let root = crate::worktree::tests::repo("tool-resolution");
        let local = gitleaks_path(&root, None).unwrap();
        let fallback = root.join(".dex/fallback");
        std::fs::create_dir_all(&fallback).unwrap();
        let path_tool = fallback.join(local.file_name().unwrap());
        std::fs::copy(&local, &path_tool).unwrap();
        let search = std::env::join_paths([&fallback]).unwrap();
        assert_eq!(gitleaks_path(&root, Some(&search)), Some(local.clone()));
        std::fs::remove_file(&local).unwrap();
        assert_eq!(gitleaks_path(&root, Some(&search)), Some(path_tool));
        assert_eq!(gitleaks_path(&root, None), None);
        assert_eq!(gitleaks_path(&root, Some(root.join("absent").as_os_str())), None);
    }
}
