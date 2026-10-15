//! DEX-33 — test bütçesi: the whole Playwright suite is QA's job. A shell command that runs
//! `playwright test` with no spec (positional filter) and no `--grep` is denied at PreToolUse
//! for every role but `qa`. Covers npx / pnpm exec / yarn / bunx / node_modules/.bin, env
//! prefixes (`CI=1 …`, `cross-env …`) and package.json scripts (`npm run e2e`, `pnpm e2e`,
//! `npm test`) resolved from the hook's `cwd` (plus a leading `cd <dir> &&`), extra args
//! included. Not detected on purpose (false positives cost more than a miss): `npm --prefix`,
//! `pnpm -C/--filter`, workspaces, `bash script.sh`, `$(…)`, config-file `testMatch` tricks.
//! Godot GUT is also denied when a literal broad test directory and recursive discovery
//! are given without a file/test selector. Unknown wrappers and directory expressions pass.
//! Rust `cargo test` / `cargo nextest run` require a test-name filter or a single named
//! target (or `--doc`); unknown options pass rather than guessing which words are filters.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const DENY: &str = "Tüm e2e paketini koşma; ilgili spec'i ver ya da QA'ya bırak (Dex test bütçesi). \
    Örn. `npx playwright test tests/x.spec.ts --workers=1` ya da `--grep \"<test adı>\"`.";

pub const DENY_LEADER: &str = "Dex politikası: lider tarayıcı/e2e testi koşamaz — tip kontrolü yeter; e2e QA'nın işi.";

pub(crate) const DENY_GUT: &str = "Tüm GUT paketini koşma; ilgili test dosyasını ver ya da QA'ya bırak (Dex test bütçesi). \
    Örn. `godot --headless -s addons/gut/gut_cmdln.gd -gtest=res://tests/e2e/test_x.gd -gexit`.";

pub(crate) const DENY_RUST: &str = "Tüm Rust paketini koşma; dokunduğun modülün filtresini ekle: \
    `cargo test -p dex-core --lib <modül>`; tam paket QA'nın işi (Dex test bütçesi).";

struct Enforcer {
    segments: fn(&str, Option<crate::permit::Dialect>) -> Vec<Vec<String>>,
    recognises: fn(&[String]) -> bool,
    deny: &'static str,
}

struct Runner {
    name: &'static str,
    markers: fn(&crate::stack::TestMarkers) -> bool,
    enforcer: Option<Enforcer>,
}

const RUNNERS: &[Runner] = &[
    Runner { name: "Playwright", markers: playwright_markers, enforcer: Some(Enforcer { segments: e2e_command_words, recognises: unfiltered_playwright, deny: DENY }) },
    Runner { name: "Godot GUT", markers: |m| m.file("project.godot") && m.dir("addons/gut"), enforcer: Some(Enforcer { segments: e2e_command_words, recognises: unfiltered_gut, deny: DENY_GUT }) },
    Runner { name: "Rust", markers: |m| m.file("Cargo.toml"), enforcer: Some(Enforcer { segments: crate::permit::command_words, recognises: unfiltered_rust, deny: DENY_RUST }) },
    Runner { name: "Android (Gradle)", markers: android_markers, enforcer: None },
    Runner { name: "Maestro", markers: |m| m.dir(".maestro"), enforcer: None },
    Runner { name: "WebdriverIO (Electron)", markers: |m| m.root_config("wdio.conf."), enforcer: None },
    Runner { name: "Jest/Vitest e2e", markers: |m| m.root_file_matching(|n|
        (n.starts_with("jest.") || n.starts_with("vitest.")) && n.contains("config.") && (n.contains("e2e") || n.contains("integration"))), enforcer: None },
    Runner { name: "Cypress", markers: |m| m.root_config("cypress.config."), enforcer: None },
    Runner { name: "Flutter integration_test", markers: |m| m.dir("integration_test"), enforcer: None },
    Runner { name: "Unity Test Framework", markers: |m| m.dir("Assets") && m.text("Packages/manifest.json")
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|p| p.get("dependencies").and_then(|v| v.get("com.unity.test-framework")).is_some()), enforcer: None },
];

fn android_markers(m: &crate::stack::TestMarkers) -> bool {
    ["build.gradle", "build.gradle.kts"].iter().any(|path| m.file(path) &&
        (m.dir_named("androidTest") || m.text(path).is_some_and(|s| s.contains("connectedAndroidTest"))))
}

fn playwright_markers(m: &crate::stack::TestMarkers) -> bool {
    m.root_config("playwright.config.") || m.text("package.json")
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|p| ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].iter()
            .any(|key| p.get(key).and_then(|v| v.get("@playwright/test")).is_some()))
}

/// Advisory only; new enforcement requires a patron-approved plan. Called at prompt build.
pub(crate) fn unplanned_runners(root: &Path) -> Vec<&'static str> {
    let markers = crate::stack::TestMarkers::scan(root);
    RUNNERS.iter().filter(|r| r.enforcer.is_none() && (r.markers)(&markers)).map(|r| r.name).collect()
}

/// Hook side: Some(deny answer) when a non-QA agent's shell command would run the whole suite,
/// or a leader's would run any e2e test (#1876).
pub fn on_pre_tool(conn: &Connection, agent: &str, body: &Value) -> Option<String> {
    let tool = body.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if !matches!(tool, "Bash" | "PowerShell") {
        return None;
    }
    let command = body.get("tool_input")?.get("command")?.as_str()?;
    let cwd = body.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    let d = crate::permit::dialect(tool);
    let (kind, role): (Option<String>, Option<String>) = conn
        .query_row("SELECT kind, role_slug FROM agents WHERE id = ?1", [agent], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    let leader = if kind.as_deref() == Some("leader") { leader_e2e_denial(command, cwd.as_deref(), d) } else { None };
    let reason = match leader {
        Some(reason) => reason,
        None if role.as_deref() == Some("qa") => return None,
        None => check(command, cwd, 0, d)?,
    };
    Some(
        json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }})
        .to_string(),
    )
}

/// Whether `command` (run in `cwd`) starts a full registered suite.
pub fn runs_full_suite(command: &str, cwd: Option<&Path>) -> bool {
    full_suite_denial(command, cwd).is_some()
}

pub(crate) fn full_suite_denial(command: &str, cwd: Option<&Path>) -> Option<&'static str> {
    full_suite_denial_in_shell(command, cwd, crate::permit::dialect(""))
}

pub(crate) fn full_suite_denial_in_shell(command: &str, cwd: Option<&Path>, d: Option<crate::permit::Dialect>) -> Option<&'static str> {
    check(command, cwd.map(Path::to_path_buf), 0, d)
}

/// #1876: a leader runs no browser/e2e test at all — a spec or a grep included.
pub(crate) fn leader_e2e_denial(command: &str, cwd: Option<&Path>, d: Option<crate::permit::Dialect>) -> Option<&'static str> {
    walk(command, cwd.map(Path::to_path_buf), 0, d, &|line, d| {
        e2e_command_words(line, d).iter().any(|seg| runs_playwright_test(&strip_env(seg))).then_some(DENY_LEADER)
    })
}

fn check(command: &str, cwd: Option<PathBuf>, depth: u8, d: Option<crate::permit::Dialect>) -> Option<&'static str> {
    walk(command, cwd, depth, d, &|line, d| {
        RUNNERS.iter().filter_map(|r| r.enforcer.as_ref()).find(|enforcer| {
            (enforcer.segments)(line, d).iter().any(|seg| (enforcer.recognises)(&strip_env(seg)))
        }).map(|enforcer| enforcer.deny)
    })
}

/// `hit` on the command line itself, then on the package.json scripts it calls (`cd <dir>` followed).
fn walk(command: &str, mut cwd: Option<PathBuf>, depth: u8, d: Option<crate::permit::Dialect>, hit: &dyn Fn(&str, Option<crate::permit::Dialect>) -> Option<&'static str>) -> Option<&'static str> {
    if let Some(reason) = hit(command, d) {
        return Some(reason);
    }
    for seg in segments(command) {
        let words = strip_env(&seg);
        if words.first().map(String::as_str) == Some("cd") {
            // Only a relative/absolute literal dir; anything fancy drops the cwd (→ no script lookup).
            cwd = match (words.get(1), &cwd) {
                (Some(d), _) if Path::new(d).is_absolute() => Some(PathBuf::from(d)),
                (Some(d), Some(c)) if !d.starts_with(['$', '~', '-']) => Some(c.join(d)),
                _ => None,
            };
            continue;
        }
        if depth < 3 {
            if let Some((script, extra)) = script_call(&words) {
                if let Some(body) = cwd.as_deref().and_then(|c| package_script(c, &script)) {
                    // npm/pnpm/yarn append the extra args to the script's command line.
                    let line = if extra.is_empty() { body } else { format!("{body} {}", extra.join(" ")) };
                    if let Some(reason) = walk(&line, cwd.clone(), depth + 1, d, hit) {
                        return Some(reason);
                    }
                }
            }
        }
    }
    None
}

fn unfiltered_rust(words: &[String]) -> bool {
    let Some(exe) = words.first() else { return false };
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe).to_ascii_lowercase();
    if !matches!(base.as_str(), "cargo" | "cargo.exe") {
        return false;
    }
    let mut args = words[1..].iter().peekable();
    if args.peek().is_some_and(|a| a.starts_with('+')) {
        args.next();
    }
    let nextest = match args.next().map(String::as_str) {
        Some("test") => false,
        Some("nextest") if args.next().map(String::as_str) == Some("run") => true,
        _ => return false,
    };
    let mut harness = false;
    let mut named_targets = 0;
    let mut broad_targets = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            harness = true;
            continue;
        }
        let (flag, value) = arg.split_once('=').map_or((arg.as_str(), None), |(f, v)| (f, Some(v)));
        if matches!(flag, "-h" | "--help")
            || !harness && matches!(flag, "--no-run" | "--doc")
            || harness && flag == "--list"
            || nextest && !harness && matches!(flag, "-E" | "--filter-expr")
        {
            return false;
        }
        if !harness && matches!(flag, "--test" | "--bin" | "--example" | "--bench") {
            let target = value.or_else(|| args.next().map(String::as_str));
            let Some(target) = target else { return false };
            if target.is_empty() || target.starts_with('-') {
                return false;
            }
            named_targets += 1;
            broad_targets |= target.contains(['*', '?', '[']);
            continue;
        }
        let takes_value = if harness {
            matches!(flag, "--test-threads" | "--skip" | "--format" | "--color" | "--logfile" | "-Z")
        } else {
            matches!(flag, "-p" | "--package" | "--exclude" | "-F" | "--features" | "-j" | "--jobs"
                | "--profile" | "--target" | "--target-dir" | "--manifest-path" | "--message-format"
                | "--color" | "--config" | "-Z")
                || nextest && matches!(flag, "--test-threads" | "--retries" | "--status-level"
                    | "--final-status-level" | "--failure-output" | "--success-output" | "--partition"
                    | "--no-tests")
        };
        if takes_value {
            if value.is_none() && args.next().is_none() {
                return false;
            }
            continue;
        }
        if !harness {
            broad_targets |= matches!(flag, "--lib" | "--bins" | "--examples" | "--tests" | "--benches" | "--all-targets");
            if matches!(flag, "--lib" | "--bins" | "--examples" | "--tests" | "--benches" | "--all-targets"
                | "--workspace" | "--all" | "--all-features" | "--no-default-features" | "--no-fail-fast"
                | "--future-incompat-report" | "-q" | "--quiet" | "-v" | "-vv" | "--verbose"
                | "-r" | "--release" | "--timings" | "--locked" | "--offline" | "--frozen" | "--ignore-rust-version")
                || ["-p", "-F", "-j", "-Z"].iter().any(|f| arg.starts_with(f) && arg.len() > f.len())
            {
                continue;
            }
        } else if matches!(flag, "--nocapture" | "--no-capture" | "--show-output" | "--exact"
            | "--ignored" | "--include-ignored" | "--quiet" | "-q") {
            continue;
        }
        // A nonempty positional name narrows the run. Unknown flags/expressions pass.
        if !arg.is_empty() {
            return false;
        }
    }
    named_targets != 1 || broad_targets
}

fn unfiltered_gut(words: &[String]) -> bool {
    let mut words = strip_env(words);
    // Only literal `timeout <duration>` wrappers; unfamiliar options remain allowed.
    while words.first().is_some_and(|w| w == "timeout") {
        let Some(duration) = words.get(1) else { return false };
        let number = duration.trim_end_matches(['s', 'm', 'h', 'd']);
        if number.parse::<f64>().is_err() {
            return false;
        }
        words = strip_env(&words[2..]);
    }
    let Some(exe) = words.first() else { return false };
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe).to_ascii_lowercase();
    if !(matches!(base.as_str(), "godot" | "godot.exe" | "godot_console" | "godot_console.exe")
        || base.starts_with("godot_v") && base.ends_with(".exe"))
    {
        return false;
    }
    let mut runner = false;
    let mut broad_dir = false;
    let mut recursive = false;
    let mut engine_args = true;
    let mut doctool = false;
    let mut gdscript_docs = false;
    let mut args = words[1..].iter();
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "--" | "++") {
            engine_args = false;
        }
        // Only an engine flag stops execution; script user arguments do not.
        if engine_args && arg == "--check-only" {
            return false;
        }
        if engine_args {
            doctool |= arg == "--doctool";
            gdscript_docs |= arg == "--gdscript-docs";
        }
        let (flag, value) = arg.split_once('=').unwrap_or((arg, ""));
        match flag {
            "-gtest" | "-gselect" | "-gunit_test_name" | "-h" | "--help" => return false,
            "-s" | "--script" => {
                let path = if value.is_empty() { args.next().map(String::as_str).unwrap_or("") } else { value };
                let path = path.replace('\\', "/");
                runner = matches!(path.as_str(), "addons/gut/gut_cmdln.gd" | "./addons/gut/gut_cmdln.gd" | "res://addons/gut/gut_cmdln.gd");
                if !runner {
                    return false;
                }
            }
            "-gdir" => {
                let dir = if value.is_empty() { args.next().map(String::as_str).unwrap_or("") } else { value };
                if !matches!(dir.trim_end_matches('/'), "res://tests" | "res://tests/e2e") {
                    return false;
                }
                broad_dir = true;
            }
            "-ginclude_subdirs" => recursive = value.is_empty() || value == "true",
            _ => {}
        }
    }
    // GDScript docs bypass the engine-doc early exit and can instantiate the script.
    runner && broad_dir && recursive && (!doctool || gdscript_docs)
}

fn e2e_command_words(command: &str, d: Option<crate::permit::Dialect>) -> Vec<Vec<String>> {
    // Rd splits braces into synthetic commands, losing narrow brace-expanded selectors.
    // Prefer the legacy path here; braces in comments also fall back (budget guard, not security).
    if command.contains(['{', '}']) {
        return segments(command);
    }
    let mut words = crate::permit::command_words(command, d);
    // Keep every legacy denial, including unknown dialects and incomplete Rd parses.
    words.extend(segments(command));
    words
}

/// Shell-ish split into command segments (`&&`, `||`, `;`, `|`, `&`, newline), words unquoted.
fn segments(command: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut word = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let flush = |out: &mut Vec<Vec<String>>, word: &mut String, has: &mut bool| {
        if *has {
            out.last_mut().unwrap().push(std::mem::take(word));
            *has = false;
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has_word = true;
            }
            // Escape only before a shell-special char; otherwise literal (Windows paths, PowerShell).
            (None, '\\') => match chars.peek() {
                Some('\n') => {
                    chars.next();
                }
                Some(&n) if n.is_whitespace() || "\"'\\&|;()$`".contains(n) => {
                    word.push(n);
                    chars.next();
                    has_word = true;
                }
                _ => {
                    word.push('\\');
                    has_word = true;
                }
            },
            (None, c) if c.is_whitespace() && c != '\n' => flush(&mut out, &mut word, &mut has_word),
            (None, '&' | '|' | ';' | '\n' | '(' | ')') => {
                flush(&mut out, &mut word, &mut has_word);
                if !out.last().unwrap().is_empty() {
                    out.push(Vec::new());
                }
            }
            (None, c) => {
                word.push(c);
                has_word = true;
            }
        }
    }
    flush(&mut out, &mut word, &mut has_word);
    out.retain(|s| !s.is_empty());
    out
}

/// Drops leading `VAR=value` assignments and `env` / `cross-env` wrappers.
fn strip_env(words: &[String]) -> Vec<String> {
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let assign = w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if assign || w == "env" || w == "cross-env" {
            i += 1;
        } else {
            break;
        }
    }
    words[i..].to_vec()
}

fn is_playwright(word: &str) -> bool {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    matches!(base.as_str(), "playwright" | "playwright.cmd" | "playwright.exe" | "playwright.ps1" | "cli.js")
        // `node node_modules/playwright/cli.js` or `…/@playwright/test/cli.js`
        && (base != "cli.js" || word.replace('\\', "/").contains("playwright/"))
}

/// Flags of `playwright test` that take a separate value (`--workers 1`), so that value is not
/// mistaken for a spec filter.
const VALUE_FLAGS: &[&str] = &[
    "-c", "--config", "-j", "--workers", "--project", "--reporter", "--retries", "--timeout",
    "--repeat-each", "--max-failures", "--output", "--shard", "--trace", "--grep-invert", "--global-timeout",
    "--browser", "--update-snapshots", "--tsconfig", "--ignore-snapshots-dir",
];

/// `… playwright test …` in the segment, filtered or not.
fn runs_playwright_test(words: &[String]) -> bool {
    words.windows(2).any(|w| is_playwright(&w[0]) && w[1] == "test")
}

/// `… playwright test …` in the segment with neither a positional filter nor a grep.
fn unfiltered_playwright(words: &[String]) -> bool {
    let Some(at) = words.windows(2).position(|w| is_playwright(&w[0]) && w[1] == "test") else {
        return false;
    };
    let mut args = words[at + 2..].iter();
    while let Some(a) = args.next() {
        let a = a.as_str();
        if a == "--" {
            continue;
        }
        if matches!(a, "-g" | "--grep") || a.starts_with("--grep=") || a.starts_with("-g=") {
            return false;
        }
        // Narrowed by something other than a spec: fine.
        if matches!(a, "--last-failed" | "--only-changed" | "--list" | "-h" | "--help") || a.starts_with("--only-changed=") {
            return false;
        }
        if VALUE_FLAGS.contains(&a) {
            args.next();
            continue;
        }
        if !a.starts_with('-') {
            return false; // spec file / path filter
        }
    }
    true
}

/// `npm run x …`, `npm test`, `pnpm [run] x …`, `yarn [run] x …`, `bun run x …` → (script, extra args).
fn script_call(words: &[String]) -> Option<(String, Vec<String>)> {
    let pm = words.first()?.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let pm = pm.trim_end_matches(".cmd").trim_end_matches(".exe");
    let rest = &words[1..];
    let (script, extra) = match (pm, rest.first().map(String::as_str)) {
        ("npm", Some("run" | "run-script" | "rum" | "urn")) | ("pnpm" | "yarn" | "bun", Some("run")) => (rest.get(1)?.as_str(), &rest[2..]),
        ("npm", Some("test" | "t" | "tst")) => ("test", &rest[1..]),
        // Bare `pnpm e2e` / `yarn e2e`: a script name unless it is a flag; unknown builtins just
        // find no such script in package.json.
        ("pnpm" | "yarn", Some(s)) if !s.starts_with('-') => (s, &rest[1..]),
        _ => return None,
    };
    if script.starts_with('-') {
        return None;
    }
    let extra = extra.iter().skip_while(|a| *a == "--").cloned().collect();
    Some((script.to_string(), extra))
}

fn package_script(dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("scripts")?.get(name)?.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker_fixture(files: &[(&str, &str)], dirs: &[&str]) -> crate::projects::tests::Fixture {
        let f = crate::projects::tests::Fixture::new();
        for (path, body) in files {
            let path = f.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        for path in dirs {
            std::fs::create_dir_all(f.0.join(path)).unwrap();
        }
        f
    }

    #[test]
    fn unplanned_runners_require_known_file_markers() {
        for (files, dirs, name) in [
            (vec![("build.gradle", "")], vec!["app/src/androidTest"], "Android (Gradle)"),
            (vec![("build.gradle.kts", "tasks.named(\"connectedAndroidTest\")")], vec![], "Android (Gradle)"),
            (vec![], vec![".maestro"], "Maestro"),
            (vec![("wdio.conf.ts", "")], vec![], "WebdriverIO (Electron)"),
            (vec![("jest.e2e.config.js", "")], vec![], "Jest/Vitest e2e"),
            (vec![("vitest.config.integration.ts", "")], vec![], "Jest/Vitest e2e"),
            (vec![("cypress.config.ts", "")], vec![], "Cypress"),
            (vec![], vec!["integration_test"], "Flutter integration_test"),
            (vec![("Packages/manifest.json", r#"{"dependencies":{"com.unity.test-framework":"1.0"}}"#)], vec!["Assets"], "Unity Test Framework"),
        ] {
            let f = marker_fixture(&files, &dirs);
            assert_eq!(unplanned_runners(&f.0), vec![name], "{name}");
        }
        for (files, dirs) in [
            (vec![("build.gradle", "")], vec![]),
            (vec![], vec!["app/src/androidTest"]),
            (vec![("jest.config.js", ""), ("vitest.config.ts", ""), ("e2e.txt", "cypress wdio Flutter")], vec![]),
            (vec![("Packages/manifest.json", "{}")], vec!["Assets"]),
            (vec![("Packages/manifest.json", r#"{"dependencies":{"com.unity.test-framework":"1"}}"#)], vec![]),
        ] {
            let f = marker_fixture(&files, &dirs);
            assert!(unplanned_runners(&f.0).is_empty());
        }
    }

    #[test]
    fn registered_runner_markers_do_not_hide_unplanned_tools() {
        for (files, dirs, name) in [
            (vec![("playwright.config.ts", "")], vec![], "Playwright"),
            (vec![("package.json", r#"{"devDependencies":{"@playwright/test":"1"}}"#)], vec![], "Playwright"),
            (vec![("project.godot", "")], vec!["addons/gut"], "Godot GUT"),
            (vec![("Cargo.toml", "")], vec![], "Rust"),
        ] {
            let f = marker_fixture(&files, &dirs);
            let runner = RUNNERS.iter().find(|r| r.name == name).unwrap();
            assert!(runner.enforcer.is_some());
            assert!((runner.markers)(&crate::stack::TestMarkers::scan(&f.0)), "{name}");
            assert!(unplanned_runners(&f.0).is_empty());
            std::fs::write(f.0.join("cypress.config.js"), "").unwrap();
            assert_eq!(unplanned_runners(&f.0), vec!["Cypress"]);
        }
        for (files, dirs, name) in [
            (vec![("package.json", r#"{"scripts":{"test":"playwright test"}}"#)], vec![], "Playwright"),
            (vec![("project.godot", "")], vec![], "Godot GUT"),
            (vec![], vec!["addons/gut"], "Godot GUT"),
        ] {
            let f = marker_fixture(&files, &dirs);
            let runner = RUNNERS.iter().find(|r| r.name == name).unwrap();
            assert!(!(runner.markers)(&crate::stack::TestMarkers::scan(&f.0)));
        }
        for (i, runner) in RUNNERS.iter().enumerate() {
            assert!(!RUNNERS[..i].iter().any(|r| r.name == runner.name));
        }
    }

    #[test]
    fn marker_scan_ignores_nested_configs_heavy_dirs_and_deep_android_tests() {
        let f = marker_fixture(&[("build.gradle", ""), ("nested/cypress.config.ts", "")], &["a/b/c/androidTest"]);
        for dir in [".git", ".dex", "node_modules", "target", "build", "dist", ".gradle", ".godot", "Library", "Temp", "Pods", ".dart_tool"] {
            std::fs::create_dir_all(f.0.join(dir).join("src/androidTest")).unwrap();
        }
        assert!(unplanned_runners(&f.0).is_empty());
        std::fs::create_dir_all(f.0.join("app/src/androidTest")).unwrap();
        assert_eq!(unplanned_runners(&f.0), vec!["Android (Gradle)"]);
    }

    fn full(cmd: &str) -> bool {
        runs_full_suite(cmd, None)
    }

    fn e2e_shell_budget(tool: &str, cmd: &str, cwd: Option<&Path>, reason: Option<&str>) {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let input = json!({ "command": cmd });
        let body = json!({ "tool_name": tool, "tool_input": input, "cwd": cwd });
        // #1876: the leader (timur) is also denied a filtered e2e run, with its own reason.
        let leader = leader_e2e_denial(cmd, cwd, crate::permit::dialect(tool)).or(reason);
        for (agent, reason) in [("kaan", reason), ("ayla", reason), ("timur", leader), ("unknown", reason)] {
            let out = on_pre_tool(&c, agent, &body);
            assert_eq!(out.is_some(), reason.is_some(), "{tool}: {agent}: {cmd}");
            if let Some(out) = out {
                let out: Value = serde_json::from_str(&out).unwrap();
                assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
                assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], reason.unwrap());
            }
        }
        assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {tool}: {cmd}");
        let root = cwd.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let mut ctx = crate::permit::Ctx { cwd: Some(root.clone()), write: vec![root], ..Default::default() };
        let budget_reason = |out: crate::permit::Verdict| match out {
            crate::permit::Verdict::Deny(why) if why.contains(DENY) => Some(DENY),
            crate::permit::Verdict::Deny(why) if why.contains(DENY_GUT) => Some(DENY_GUT),
            crate::permit::Verdict::Deny(why) if why.contains(DENY_LEADER) => Some(DENY_LEADER),
            _ => None,
        };
        assert_eq!(budget_reason(crate::permit::decide(tool, &input, &ctx)), reason, "permit: {tool}: {cmd}");
        ctx.leader = true;
        assert_eq!(budget_reason(crate::permit::decide(tool, &input, &ctx)), leader, "leader permit: {tool}: {cmd}");
        ctx.leader = false;
        ctx.qa = true;
        assert_eq!(budget_reason(crate::permit::decide(tool, &input, &ctx)), None, "QA permit: {tool}: {cmd}");
    }

    fn playwright_shell_table(tool: &str, cwd: Option<&Path>, prefix: &str) {
        for suffix in ["# kontrol", "2> err.txt", "2>&1 | tail -20", ">> out.txt"] {
            e2e_shell_budget(tool, &format!("{prefix} {suffix}"), cwd, Some(DENY));
        }
        for suffix in ["e2e/x.spec.ts --workers=1", "e2e/x.spec.ts # not", "--grep \"ad\"", "--grep \"ad\" 2> err.txt", "\"e2e/a#b.spec.ts\"", "e2e/a#b.spec.ts", "e2e/x.spec.ts 2>&1 | tail -20", "e2e/x.spec.ts >> out.txt"] {
            e2e_shell_budget(tool, &format!("{prefix} {suffix}"), cwd, None);
        }
        if tool == "PowerShell" {
            e2e_shell_budget(tool, &format!("{prefix} *> log.txt"), cwd, Some(DENY));
            e2e_shell_budget(tool, &format!("{prefix} --grep \"ad\" *> log.txt"), cwd, None);
        }
    }

    fn gut_shell_table(tool: &str, cwd: Option<&Path>, prefix: &str) {
        for suffix in ["# -gtest=x", "# --check-only", "> --help", "# kontrol", "2> err.txt", "2>&1 | tail -20", ">> out.txt"] {
            e2e_shell_budget(tool, &format!("{prefix} {suffix}"), cwd, Some(DENY_GUT));
        }
        for suffix in ["-gtest=x # not", "-gselect=test_x # not", "-gtest=x > --help", "-gselect=test_x 2> err.txt", "--check-only # not"] {
            e2e_shell_budget(tool, &format!("{prefix} {suffix}"), cwd, None);
        }
        if tool == "PowerShell" {
            e2e_shell_budget(tool, &format!("{prefix} *> log.txt"), cwd, Some(DENY_GUT));
            e2e_shell_budget(tool, &format!("{prefix} -gtest=x *> log.txt"), cwd, None);
        }
    }

    #[test]
    fn playwright_shell_bash() {
        playwright_shell_table("Bash", None, "npx playwright test");
    }

    #[test]
    fn playwright_shell_brace_filters() {
        for suffix in ["{e2e/a,e2e/b}.spec.ts --workers=1", "e2e/{a,b}.spec.ts"] {
            e2e_shell_budget("Bash", &format!("npx playwright test {suffix}"), None, None);
        }
    }

    fn e2e_shell_package_brace_table(script_body: bool) {
        let f = marker_fixture(&[], &[]);
        let gut = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs";
        let mut scripts = serde_json::Map::new();
        scripts.insert("e2e".into(), json!("npx playwright test"));
        scripts.insert("gut".into(), json!(gut));
        for (name, prefix, suffixes) in [
            ("e2e", "npx playwright test", vec!["{e2e/a,e2e/b}.spec.ts --workers=1", "e2e/{a,b}.spec.ts"]),
            ("gut", gut, vec!["-gselect=test_{jump,run}"]),
        ] {
            for (i, suffix) in suffixes.iter().enumerate() {
                scripts.insert(format!("{name}-brace-{i}"), json!(format!("{prefix} {suffix}")));
            }
        }
        std::fs::write(f.0.join("package.json"), json!({ "scripts": scripts }).to_string()).unwrap();
        for (name, suffixes) in [
            ("e2e", vec!["{e2e/a,e2e/b}.spec.ts --workers=1", "e2e/{a,b}.spec.ts"]),
            ("gut", vec!["-gselect=test_{jump,run}"]),
        ] {
            for (i, suffix) in suffixes.iter().enumerate() {
                let cmd = if script_body { format!("npm run {name}-brace-{i}") } else { format!("npm run {name} -- {suffix}") };
                e2e_shell_budget("Bash", &cmd, Some(&f.0), None);
            }
        }
    }

    #[test]
    fn e2e_shell_package_brace_filters() {
        e2e_shell_package_brace_table(false);
    }

    #[test]
    fn e2e_shell_package_brace_bodies() {
        e2e_shell_package_brace_table(true);
    }

    #[test]
    fn gut_shell_brace_selector() {
        e2e_shell_budget("Bash", "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs -gselect=test_{jump,run}", None, None);
    }

    #[test]
    fn rust_shell_grouped_filters_stay_allowed() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (tool, cmd) in [
            ("Bash", "{ cargo test -p dex-core --lib chat; }"),
            ("Bash", "(cargo test -p dex-core --lib chat)"),
            ("PowerShell", "& { cargo test -p dex-core --lib chat }"),
        ] {
            let input = json!({ "command": cmd });
            let body = json!({ "tool_name": tool, "tool_input": input });
            assert_eq!(full_suite_denial_in_shell(cmd, None, crate::permit::dialect(tool)), None, "{tool}: {cmd}");
            assert!(on_pre_tool(&c, "kaan", &body).is_none(), "{tool}: {cmd}");
            assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {tool}: {cmd}");
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let mut ctx = crate::permit::Ctx { cwd: Some(root.clone()), write: vec![root], ..Default::default() };
            for qa in [false, true] {
                ctx.qa = qa;
                let out = crate::permit::decide(tool, &input, &ctx);
                assert!(!matches!(out, crate::permit::Verdict::Deny(ref why) if why.contains(DENY_RUST)), "permit: QA={qa}: {tool}: {cmd}: {out:?}");
            }
        }
    }

    #[test]
    fn playwright_shell_powershell() {
        playwright_shell_table("PowerShell", None, "npx playwright test");
    }

    #[test]
    fn gut_shell_bash() {
        gut_shell_table("Bash", None, "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs");
    }

    #[test]
    fn gut_shell_powershell() {
        gut_shell_table("PowerShell", None, "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs");
    }

    fn e2e_shell_package_table(tool: &str) {
        let f = marker_fixture(&[], &[]);
        let gut = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs";
        let mut scripts = serde_json::Map::new();
        for (name, command) in [("e2e", "npx playwright test"), ("gut", gut)] {
            scripts.insert(name.into(), json!(command));
            for (index, suffix) in ["# kontrol", "2> err.txt", "2>&1 | tail -20", ">> out.txt", "# -gtest=x", "# --check-only", "> --help"].iter().enumerate() {
                scripts.insert(format!("{name}-{index}"), json!(format!("{command} {suffix}")));
            }
        }
        scripts.insert("nested".into(), json!("npm run e2e-0"));
        scripts.insert("test".into(), json!("npm run nested"));
        scripts.insert("stream".into(), json!("npx playwright test *> log.txt"));
        scripts.insert("gut-stream".into(), json!(format!("{gut} *> log.txt")));
        std::fs::write(f.0.join("package.json"), json!({ "scripts": scripts }).to_string()).unwrap();
        playwright_shell_table(tool, Some(&f.0), "npm run e2e --");
        gut_shell_table(tool, Some(&f.0), "npm run gut --");
        for (name, reason) in [("e2e", DENY), ("gut", DENY_GUT)] {
            for index in 0..7 {
                for pm in ["npm run", "pnpm", "yarn", "bun run"] {
                    e2e_shell_budget(tool, &format!("{pm} {name}-{index}"), Some(&f.0), Some(reason));
                }
            }
        }
        e2e_shell_budget(tool, "npm test", Some(&f.0), Some(DENY));
        // Leader #675: script replay loses the quotes around a leading '#'.
        e2e_shell_budget(tool, "npm run e2e -- \"#\"", Some(&f.0), Some(DENY));
        e2e_shell_budget(tool, "npx playwright test \"#\"", Some(&f.0), None);
        if tool == "PowerShell" {
            e2e_shell_budget(tool, "npm run stream", Some(&f.0), Some(DENY));
            e2e_shell_budget(tool, "npm run gut-stream", Some(&f.0), Some(DENY_GUT));
        }
    }

    #[test]
    fn e2e_shell_package_bash() {
        e2e_shell_package_table("Bash");
    }

    #[test]
    fn e2e_shell_package_powershell() {
        e2e_shell_package_table("PowerShell");
    }

    #[test]
    fn playwright_shell_redirection() {
        e2e_shell_budget("PowerShell", "npx playwright test *> log.txt", None, Some(DENY));
        for tool in ["Bash", "PowerShell"] {
            e2e_shell_budget(tool, "npx playwright test 2> err.txt", None, Some(DENY));
        }
    }

    #[test]
    fn gut_shell_redirect_target_is_not_a_flag() {
        for tool in ["Bash", "PowerShell"] {
            e2e_shell_budget(tool, "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs > --help", None, Some(DENY_GUT));
        }
    }

    #[test]
    fn e2e_shell_legacy_denials_and_unknown_dialect() {
        let gut = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs";
        for d in [Some(crate::permit::Dialect::Bash), Some(crate::permit::Dialect::Pwsh), None] {
            for (cmd, reason) in [
                ("npx playwright test".into(), DENY),
                ("npx playwright test ; echo 'unterminated".into(), DENY),
                (r"node_modules\playwright\cli.js test".into(), DENY),
                (gut.into(), DENY_GUT),
                (format!("{gut} ; echo 'unterminated"), DENY_GUT),
                (r"C:\Godot\godot_console.exe -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs".into(), DENY_GUT),
            ] {
                assert_eq!(full_suite_denial_in_shell(&cmd, None, d), Some(reason), "{d:?}: {cmd}");
            }
            for cmd in ["npx playwright test e2e/x.spec.ts --workers=1".into(), "npx playwright test --grep \"ad\"".into(), format!("{gut} -gtest=x"), format!("{gut} -gselect=test_x")] {
                assert_eq!(full_suite_denial_in_shell(&cmd, None, d), None, "{d:?}: {cmd}");
            }
        }
    }

    fn rust_shell_budget(tool: &str, cmd: &str, denied: bool) {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let input = json!({ "command": cmd });
        let body = json!({ "tool_name": tool, "tool_input": input });
        for agent in ["kaan", "ayla", "timur", "unknown"] {
            let out = on_pre_tool(&c, agent, &body);
            assert_eq!(out.is_some(), denied, "{tool}: {agent}: {cmd}");
            if let Some(out) = out {
                let out: Value = serde_json::from_str(&out).unwrap();
                assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
                assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], DENY_RUST);
            }
        }
        assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {tool}: {cmd}");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut ctx = crate::permit::Ctx { cwd: Some(root.clone()), write: vec![root], ..Default::default() };
        let out = crate::permit::decide(tool, &input, &ctx);
        if denied {
            assert!(matches!(out, crate::permit::Verdict::Deny(ref why) if why.contains(DENY_RUST)), "{tool}: {cmd}: {out:?}");
        } else {
            assert_eq!(out, crate::permit::Verdict::Allow, "{tool}: {cmd}");
        }
        ctx.qa = true;
        assert_eq!(crate::permit::decide(tool, &input, &ctx), crate::permit::Verdict::Allow, "QA: {tool}: {cmd}");
    }

    #[test]
    fn rust_shell_stream_redirection() {
        rust_shell_budget("PowerShell", "cargo test -p dex-core --lib *> .dex/tmp/rust.log", true);
        for tool in ["Bash", "PowerShell"] {
            for suffix in ["2> err.txt", "2>&1 > out.txt", "3>&1", ">> out.txt"] {
                rust_shell_budget(tool, &format!("cargo test -p dex-core --lib {suffix}"), true);
            }
        }
    }

    #[test]
    fn rust_shell_comment_bash() {
        rust_shell_budget("Bash", "cargo test -p dex-core --lib # kontrol", true);
    }

    #[test]
    fn rust_shell_comment_powershell() {
        rust_shell_budget("PowerShell", "cargo test -p dex-core --lib # kontrol", true);
    }

    #[test]
    fn rust_shell_quoted_filters_stay_allowed() {
        for tool in ["Bash", "PowerShell"] {
            for suffix in [r#""pty::tests::x""#, "'chat' # not", "'#'", "'*'", r#""chat#*""#, "'chat' 2> err.txt", "'chat' 2>&1 > out.txt", "'chat' >> out.txt"] {
                rust_shell_budget(tool, &format!("cargo test -p dex-core --lib {suffix}"), false);
            }
        }
        rust_shell_budget("PowerShell", "cargo test -p dex-core --lib chat *> log.txt", false);
    }

    #[test]
    fn rust_shell_explicit_dialects() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (tool, cmd, denied) in [
            ("Bash", "cargo test -p dex-core --lib # kontrol", true),
            ("PowerShell", "cargo test -p dex-core --lib # kontrol", true),
            ("PowerShell", "cargo test -p dex-core --lib *> .dex/tmp/rust.log", true),
            ("Bash", "cargo test -p dex-core --lib '*' > log.txt", false),
            ("PowerShell", "cargo test -p dex-core --lib chat *> log.txt", false),
        ] {
            let input = json!({ "command": cmd });
            let body = json!({ "tool_name": tool, "tool_input": input });
            assert_eq!(on_pre_tool(&c, "kaan", &body).is_some(), denied, "{tool}: {cmd}");
            assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {tool}: {cmd}");
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let mut ctx = crate::permit::Ctx { cwd: Some(root.clone()), write: vec![root], ..Default::default() };
            let budget_denied = |out| matches!(out, crate::permit::Verdict::Deny(ref why) if why.contains(DENY_RUST));
            assert_eq!(budget_denied(crate::permit::decide(tool, &input, &ctx)), denied, "{tool}: {cmd}");
            ctx.qa = true;
            assert!(!budget_denied(crate::permit::decide(tool, &input, &ctx)), "QA: {tool}: {cmd}");
        }
    }

    #[test]
    fn rust_shell_bash_executable_paths() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (cmd, denied) in [
            (r#""C:\Rust\bin\cargo.exe" test --lib"#, true),
            ("C:/Rust/bin/cargo.exe test --lib", true),
            ("/c/Rust/bin/cargo.exe test --lib", true),
            (r"C:\Rust\bin\cargo.exe test --lib", false),
        ] {
            let body = json!({ "tool_name": "Bash", "tool_input": { "command": cmd } });
            assert_eq!(on_pre_tool(&c, "kaan", &body).is_some(), denied, "Bash: {cmd}");
            assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {cmd}");
        }
    }

    #[test]
    fn rust_hook_budget_table() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (cmd, denied) in [
            ("cargo test", true),
            ("cargo test --workspace", true),
            ("cargo test -p dex-core", true),
            ("cargo test -p dex-core --lib", true),
            ("cargo test 2>&1 | tail -20", true),
            ("cargo test > out.txt", true),
            ("cargo test -p dex-core --lib | Select-String FAILED", true),
            ("cargo test -p dex-core --lib -- --nocapture", true),
            ("cargo test -- --test-threads=1", true),
            ("cargo test -- --test-threads 1 --skip slow", true),
            ("cargo.exe test --package=dex-core --lib", true),
            (r"C:\Rust\bin\cargo.exe test --lib", true),
            (r#"& "C:\Program Files\Rust\cargo.exe" test --lib"#, true),
            ("CARGO_TARGET_DIR=.dex/tmp/target cargo test --lib", true),
            ("env CARGO_TARGET_DIR=.dex/tmp/target cargo test --lib", true),
            ("env cargo test", true),
            ("$env:CARGO_TARGET_DIR='.dex/tmp/target'; cargo test --lib", true),
            ("cd crates/dex-core && cargo test --lib", true),
            ("cargo +nightly test --lib --features foo --target x86_64-pc-windows-msvc", true),
            ("cargo test -pdex-core --lib -j2 --profile dev --manifest-path Cargo.toml", true),
            ("cargo test --test one --test two", true),
            ("cargo test --test one --lib", true),
            ("cargo nextest run", true),
            ("cargo nextest run --workspace", true),
            ("cargo nextest run -p dex-core --lib", true),
            ("cargo nextest run --profile ci --test-threads 1", true),
            ("cargo test -p dex-core --lib chat", false),
            ("cargo test -p dex-core --lib chat 2>&1 | tail -20", false),
            ("cargo test \"pty::tests::x\"", false),
            ("cargo test -p dex-core --lib pty::tests::x -- --exact", false),
            ("cargo test --lib -- --exact pty::tests::x", false),
            ("cargo test -- --test-threads 1 chat", false),
            ("cargo test --test api", false),
            ("cargo test --test=api", false),
            ("cargo test --doc", false),
            ("cargo test --bin dexd", false),
            ("cargo test --example demo", false),
            ("cargo test --no-run", false),
            ("cargo test --help", false),
            ("cargo test -- --list", false),
            ("cargo check", false),
            ("cargo clippy -p dex-core -- -D warnings", false),
            ("cargo build", false),
            ("cargo nextest run chat", false),
            ("cargo nextest run -E 'test(chat)'", false),
            ("cargo nextest run --filter-expr='test(chat)'", false),
            ("cargo nextest run --test api", false),
            ("cargo nextest list", false),
            ("cargo test --unknown-option value", false),
            ("bash script.sh", false),
            ("bash -lc 'cargo test'", false),
            ("echo 'cargo test'", false),
            ("npm test", false),
        ] {
            assert_eq!(full(cmd), denied, "{cmd}");
            // Bash consumes unquoted backslashes; its actual executable-path cases are above.
            let tools: &[&str] = if cmd == r"C:\Rust\bin\cargo.exe test --lib" {
                &["PowerShell"]
            } else {
                &["Bash", "PowerShell"]
            };
            for tool in tools {
                let body = json!({ "tool_name": tool, "tool_input": { "command": cmd } });
                for agent in ["kaan", "ayla", "timur", "unknown"] {
                    let out = on_pre_tool(&c, agent, &body);
                    assert_eq!(out.is_some(), denied, "{tool}: {agent}: {cmd}");
                    if let Some(out) = out {
                        let out: Value = serde_json::from_str(&out).unwrap();
                        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
                        let reason = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
                        assert!(reason.contains("cargo test -p dex-core --lib <modül>"), "{reason}");
                        assert!(reason.contains("tam paket QA'nın işi"), "{reason}");
                    }
                }
                assert!(on_pre_tool(&c, "bora", &body).is_none(), "QA: {cmd}");
            }
        }
    }

    #[test]
    fn rust_test_budget_permit_message() {
        let mut ctx = crate::permit::Ctx::default();
        let input = json!({ "command": "cargo test -p dex-core --lib" });
        let out = crate::permit::decide("Bash", &input, &ctx);
        assert!(matches!(out, crate::permit::Verdict::Deny(ref reason)
            if reason.contains("cargo test -p dex-core --lib <modül>") && reason.contains("tam paket QA'nın işi")), "{out:?}");
        ctx.qa = true;
        assert_eq!(crate::permit::decide("Bash", &input, &ctx), crate::permit::Verdict::Allow);
    }

    #[test]
    fn gut_full_suite() {
        for exe in ["godot", "godot_console", "godot.exe", "godot_console.exe", "Godot_v4.5-stable_win64_console.exe", r"C:\Godot\godot_console.exe"] {
            for prefix in ["", "timeout 1200 ", "CI=1 ", "env CI=1 timeout 1200 ", "timeout 20m env CI=1 ", "cross-env CI=1 "] {
                for dir in ["res://tests", "res://tests/e2e/"] {
                    let cmd = format!("{prefix}{exe} --headless --path . -s addons/gut/gut_cmdln.gd -gdir={dir} -ginclude_subdirs -gexit");
                    assert!(full(&cmd), "{cmd}");
                }
            }
        }
        assert!(full(r#"& "C:\Program Files\Godot\Godot_v4.5.exe" --headless --script res://addons/gut/gut_cmdln.gd -gdir res://tests/e2e -ginclude_subdirs=true"#));
    }

    #[test]
    fn gut_check_only_is_allowed() {
        let cmd = "godot --headless --check-only -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e -ginclude_subdirs";
        assert!(!full(cmd));
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let body = json!({ "tool_name": "Bash", "tool_input": { "command": cmd } });
        assert!(on_pre_tool(&c, "kaan", &body).is_none());
        let broad = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e -ginclude_subdirs";
        assert!(!full(&format!("timeout 1200 env CI=1 {broad} --check-only")));
        assert!(full(&format!("{cmd} && {broad}")), "the next suite still runs");
        for suffix in ["--check-only=false", "-- --check-only", "++ --check-only"] {
            assert!(full(&format!("{broad} {suffix}")), "{suffix}");
        }
    }

    #[test]
    fn gut_doctool_is_allowed() {
        let broad = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e -ginclude_subdirs";
        for cmd in [format!("{broad} --doctool ."), format!("{broad} --doctool")] {
            assert!(!full(&cmd), "{cmd}");
        }
        assert!(full(&format!("{broad} -- --doctool .")));
        assert!(full(&format!("{broad} --doctool . --gdscript-docs res://")));
        assert!(full(&format!("{broad} --export-release Linux .dex/tmp/game")));
    }

    #[test]
    fn gut_selected_or_uncertain_runs_are_allowed() {
        let broad = "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e -ginclude_subdirs";
        for selector in ["-gtest=res://tests/e2e/test_x.gd", "-gselect=test_x", "-gunit_test_name=test_jump", "-gtest res://tests/e2e/test_x.gd", "-gselect test_x", "-gunit_test_name test_jump", "-gtest=", "--help"] {
            assert!(!full(&format!("{broad} {selector}")), "{selector}");
        }
        for cmd in [
            "godot -s addons/gut/gut_cmdln.gd -gtest=res://tests/e2e/test_x.gd -gexit",
            "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e",
            "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs=false",
            "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e/login -ginclude_subdirs",
            "godot -s addons/gut/gut_cmdln.gd -gdir=$TEST_DIR -ginclude_subdirs",
            "godot -s addons/gut/gut_cmdln.gd -gdir=res://tests,res://other -ginclude_subdirs",
            "godot -s addons/gut/gut_cmdln.gd -ginclude_subdirs",
            "godot -s other.gd -gdir=res://tests -ginclude_subdirs",
            "godot --headless -gdir=res://tests -ginclude_subdirs",
            "other.exe -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs",
            "timeout --signal=TERM 1200 godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs",
            "echo godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs",
        ] {
            assert!(!full(cmd), "{cmd}");
        }
    }

    #[test]
    fn gut_hook_denies_everyone_but_qa_with_file_example() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let cmd = "timeout 1200 godot_console.exe --headless --path . -s addons/gut/gut_cmdln.gd -gdir=res://tests/e2e -ginclude_subdirs -gexit";
        for tool in ["Bash", "PowerShell"] {
            let body = json!({ "tool_name": tool, "tool_input": { "command": cmd } });
            for agent in ["kaan", "ayla", "timur", "unknown"] {
                let out: Value = serde_json::from_str(&on_pre_tool(&c, agent, &body).expect(agent)).unwrap();
                assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
                assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], DENY_GUT);
            }
            assert!(on_pre_tool(&c, "bora", &body).is_none(), "qa runs GUT");
            let selected = json!({ "tool_name": tool, "tool_input": { "command": format!("{cmd} -gtest=res://tests/e2e/test_x.gd") } });
            assert!(on_pre_tool(&c, "kaan", &selected).is_none());
        }
    }

    #[test]
    fn direct_invocations() {
        for c in [
            "npx playwright test",
            "npx playwright test --workers=2",
            "npx playwright test --workers 1 --project chromium",
            "pnpm exec playwright test --reporter=list",
            "yarn playwright test",
            "bunx playwright test -j 4",
            "CI=1 npx playwright test",
            "npx cross-env CI=1 playwright test",
            "cd app && npx playwright test",
            "npm run build && npx playwright test --trace on",
            "./node_modules/.bin/playwright test",
            r"node_modules\.bin\playwright.cmd test",
            "node node_modules/@playwright/test/cli.js test",
            "npx playwright test -- --workers=1",
        ] {
            assert!(full(c), "{c}");
        }
        for c in [
            "npx playwright test tests/login.spec.ts",
            "npx playwright test login --workers=1",
            "npx playwright test --grep \"giriş akışı\"",
            "npx playwright test -g login",
            "npx playwright test --grep=@smoke",
            "npx playwright test --workers 1 e2e/dock.spec.ts",
            "npx playwright test --last-failed",
            "npx playwright test --list",
            "npx playwright show-report",
            "npx playwright install chromium",
            "echo 'npx playwright test'",
            "grep -r \"playwright test\" docs",
            "cargo check --workspace",
            "npx vitest run",
        ] {
            assert!(!full(c), "{c}");
        }
    }

    #[test]
    fn package_scripts() {
        let dir = std::env::temp_dir().join(format!("dex-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{ "scripts": {
                "e2e": "playwright test --workers=2",
                "test:e2e": "npm run build && npx playwright test",
                "test": "npm run e2e",
                "e2e:smoke": "playwright test --grep @smoke",
                "unit": "vitest run"
            } }"#,
        )
        .unwrap();
        std::fs::write(dir.join("web/package.json"), r#"{ "scripts": { "e2e": "playwright test" } }"#).unwrap();
        let full = |c: &str| runs_full_suite(c, Some(&dir));
        for c in ["npm run e2e", "pnpm e2e", "pnpm run test:e2e", "yarn e2e", "bun run e2e", "npm test", "npm t", "cd web && npm run e2e", "npm run e2e -- --project=chromium"] {
            assert!(full(c), "{c}");
        }
        for c in [
            "npm run e2e -- tests/x.spec.ts",
            "pnpm e2e tests/x.spec.ts",
            "npm run e2e -- --grep login",
            "pnpm run test:e2e -- -g login",
            "npm run e2e:smoke",
            "npm run unit",
            "pnpm install",
            "npm run nope",
        ] {
            assert!(!full(c), "{c}");
        }
        // #1876: a leader's e2e run is denied filtered or not, through scripts as well.
        let leader = |c: &str| leader_e2e_denial(c, Some(&dir), None).is_some();
        for c in ["npx playwright test e2e/x.spec.ts --workers=1", "npx playwright test --grep \"ad\"", "npm run e2e -- tests/x.spec.ts", "npm run e2e:smoke", "cd web && npm run e2e", "CI=1 pnpm exec playwright test a.spec.ts"] {
            assert!(leader(c), "{c}");
        }
        for c in ["npx tsc --noEmit", "cargo test -p dex-core --lib x", "npm run unit", "npx playwright install chromium", "echo 'npx playwright test'"] {
            assert!(!leader(c), "{c}");
        }
        // No cwd → scripts can't be resolved: allowed (documented miss, not a false positive).
        assert!(!runs_full_suite("npm run e2e", None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hook_denies_everyone_but_qa() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let body = json!({ "tool_name": "Bash", "tool_input": { "command": "npx playwright test" } });
        for (agent, reason) in [("kaan", DENY), ("ayla", DENY), ("timur", DENY_LEADER)] {
            let out: Value = serde_json::from_str(&on_pre_tool(&c, agent, &body).expect(agent)).unwrap();
            assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
            assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], reason, "{agent}");
        }
        assert!(on_pre_tool(&c, "bora", &body).is_none(), "qa runs the suite");
        let spec = json!({ "tool_name": "Bash", "tool_input": { "command": "npx playwright test a.spec.ts" } });
        assert!(on_pre_tool(&c, "kaan", &spec).is_none());
        // #1876: a leader runs no e2e test at all, a single spec included; type checks stay free.
        let out: Value = serde_json::from_str(&on_pre_tool(&c, "timur", &spec).expect("leader spec")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], DENY_LEADER);
        for command in ["npx tsc --noEmit", "cargo test -p dex-core --lib x", "npx playwright show-report"] {
            let body = json!({ "tool_name": "Bash", "tool_input": { "command": command } });
            assert!(on_pre_tool(&c, "timur", &body).is_none(), "{command}");
        }
        let other = json!({ "tool_name": "Write", "tool_input": { "command": "npx playwright test" } });
        assert!(on_pre_tool(&c, "kaan", &other).is_none());
    }
}
