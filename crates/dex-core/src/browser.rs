//! Browser MCP for roles flagged `"browser": true` in the catalog (DEX-20). Playwright MCP over
//! stdio, driving the browser already on the machine (Edge on Windows): no browser download,
//! accessibility-tree snapshots instead of screenshots per step (far fewer tokens).
//! `--isolated` keeps each session's profile in memory, so agents share no cookies or logins;
//! screenshots land in `<agent cwd>/.playwright-mcp`. Each claude process starts its own server,
//! and the pane's process tree dies with the pane (`proc::Tree`).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Pinned: an upstream release cannot change what agents get silently.
const PACKAGE: &str = "@playwright/mcp@0.0.79";
const VERSION: &str = "0.0.79";

#[cfg(windows)]
const BROWSER: &str = "msedge";
#[cfg(target_os = "macos")]
const BROWSER: &str = "chrome";
// Servers rarely have Chrome/Edge; Playwright's own Chromium (`npx playwright install chromium`).
#[cfg(all(unix, not(target_os = "macos")))]
const BROWSER: &str = "chromium";

pub struct Browser {
    pub config: PathBuf,
}

/// Writes `<config_dir>/dex-browser-<pid>.json` (sweeping files of dead processes first).
pub fn start(config_dir: &Path) -> Result<Browser, String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    sweep_stale(config_dir);
    let config = config_dir.join(format!("dex-browser-{}.json", std::process::id()));
    let text = serde_json::to_vec_pretty(&config_json()).map_err(|e| e.to_string())?;
    std::fs::write(&config, text).map_err(|e| e.to_string())?;
    Ok(Browser { config })
}

impl Browser {
    pub fn remove_config(&self) {
        let _ = std::fs::remove_file(&self.config);
    }
}

fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_string_lossy().strip_prefix("dex-browser-").and_then(|r| r.strip_suffix(".json")).and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|p| !crate::proc::alive(p)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The stdio server as (command, args); codex profiles (DEX-26) embed the same server.
pub fn stdio() -> (&'static str, Vec<&'static str>) {
    // Keep the borrowed argv contract shared with Codex. Cache successes only: the first
    // npx fallback may populate the cache for the next session.
    static CLI: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if CLI.get().is_none() {
        if let Some(cli) = npm_cache().and_then(|cache| cached_cli(&cache)) {
            let _ = CLI.set(cli);
        }
    }
    stdio_for_cli(CLI.get().map(String::as_str).filter(|cli| Path::new(cli).is_file()))
}

fn stdio_for_cli(cli: Option<&str>) -> (&str, Vec<&str>) {
    if let Some(cli) = cli {
        return ("node", vec![cli, "--browser", BROWSER, "--headless", "--isolated"]);
    }
    let args = ["-y", PACKAGE, "--browser", BROWSER, "--headless", "--isolated"];
    // Claude Code on Windows cannot exec `npx` (a .cmd shim) directly.
    if cfg!(windows) { ("cmd", [&["/c", "npx"][..], &args].concat()) } else { ("npx", args.to_vec()) }
}

fn config_json() -> Value {
    let (command, args) = stdio();
    json!({ "mcpServers": { "browser": { "type": "stdio", "command": command, "args": args } } })
}

fn npm_cache() -> Option<PathBuf> {
    let cache = std::env::var_os("npm_config_cache").or_else(|| std::env::var_os("NPM_CONFIG_CACHE"));
    let cache = match cache {
        Some(cache) if !cache.is_empty() => PathBuf::from(cache),
        _ if cfg!(windows) => PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("npm-cache"),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".npm"),
    };
    if cache.is_absolute() { Some(cache) } else { Some(std::env::current_dir().ok()?.join(cache)) }
}

fn cached_cli(cache: &Path) -> Option<String> {
    // npm's hash is not a stable contract. Inspect the installed manifest, not the hash.
    let entries = std::fs::read_dir(cache.join("_npx")).ok()?;
    let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let package = path.join("node_modules/@playwright/mcp");
        let manifest = std::fs::read(package.join("package.json")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let Some(manifest) = manifest else { continue };
        let cli = package.join("cli.js");
        if manifest["name"] == "@playwright/mcp" && manifest["version"] == VERSION && cli.is_file() {
            if let Some(cli) = cli.to_str() {
                return Some(cli.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn cache_accepts_only_pinned_mcp_with_cli() {
        let dir = std::env::current_dir().unwrap().join(".dex/tmp").join(format!("browser-cache-{}", std::process::id()));
        let package = dir.join("_npx/hash/node_modules/@playwright/mcp");
        std::fs::create_dir_all(&package).unwrap();
        let manifest = package.join("package.json");
        let cli = package.join("cli.js");
        assert_eq!(super::cached_cli(&dir), None);
        std::fs::write(&manifest, r#"{"name":"@playwright/mcp","version":"0.0.79"}"#).unwrap();
        assert_eq!(super::cached_cli(&dir), None, "missing cli");
        std::fs::write(&cli, "// fixture").unwrap();
        let found = super::cached_cli(&dir);
        std::fs::write(&manifest, r#"{"name":"@playwright/mcp","version":"0.0.80"}"#).unwrap();
        let wrong_version = super::cached_cli(&dir);
        std::fs::write(&manifest, "invalid json").unwrap();
        let invalid = super::cached_cli(&dir);
        std::fs::write(&manifest, r#"{"name":"other","version":"0.0.79"}"#).unwrap();
        let wrong_name = super::cached_cli(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.map(std::path::PathBuf::from), Some(cli));
        assert_eq!(wrong_version, None);
        assert_eq!(invalid, None);
        assert_eq!(wrong_name, None);
        assert_eq!(super::cached_cli(&dir), None, "missing cache");
    }

    #[test]
    fn cached_cli_runs_node_without_shell_or_npx() {
        let cli = r"C:\cache with spaces\_npx\hash\node_modules\@playwright\mcp\cli.js";
        let (command, args) = super::stdio_for_cli(Some(cli));
        assert_eq!(command, "node");
        assert_eq!(args, [cli, "--browser", super::BROWSER, "--headless", "--isolated"]);
    }

    #[test]
    fn missing_cache_keeps_pinned_npx_fallback() {
        let (command, args) = super::stdio_for_cli(None);
        assert_eq!(command, if cfg!(windows) { "cmd" } else { "npx" });
        let mut want = if cfg!(windows) { vec!["/c", "npx"] } else { vec![] };
        want.extend(["-y", super::PACKAGE, "--browser", super::BROWSER, "--headless", "--isolated"]);
        assert_eq!(args, want);
    }

    #[test]
    fn config_is_headless_isolated_and_pinned() {
        let c = super::config_json();
        let s = &c["mcpServers"]["browser"];
        let args: Vec<&str> = s["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        for want in ["--headless", "--isolated", super::BROWSER] {
            assert!(args.contains(&want), "{want} in {args:?}");
        }
        if s["command"] == "node" {
            let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(args[0]).with_file_name("package.json")).unwrap()).unwrap();
            assert_eq!(manifest["version"], super::VERSION);
        } else {
            assert!(args.contains(&super::PACKAGE));
        }
        assert_eq!(s["type"], "stdio");
    }

    #[test]
    fn start_writes_and_sweeps() {
        let dir = std::env::current_dir().unwrap().join(".dex/tmp").join(format!("dex-browser-test-{}", std::process::id()));
        let dead = dir.join("dex-browser-4294967294.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&dead, "{}").unwrap();
        let b = super::start(&dir).unwrap();
        assert!(b.config.exists());
        assert!(!dead.exists());
        b.remove_config();
        assert!(!b.config.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
