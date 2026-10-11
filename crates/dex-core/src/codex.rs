//! DEX-26 — Codex motoru: `codex` CLI'ı kendi girişiyle (ChatGPT aboneliği, `~/.codex/auth.json`)
//! ikinci motor olarak çalıştırır. Sağlayıcı yönlendirmesi (GLM/Muse/Yerel) yalnız claude'da.
//!
//! Kablolama (CDX-W §3–§4, codex-cli 0.154 ölçümü): claude'un `--settings` / `--mcp-config` /
//! `--append-system-prompt` üçlüsü codex'te tek bir OTURUM PROFİLİ dosyasıdır —
//! `$CODEX_HOME/dex-<ajan>-<pid>.config.toml`, `-p` ile verilir, kullanıcının config.toml'u
//! değişmez (katmanlama additive). Dosya süreç başına: ikinci bir Dex örneği birincinin
//! oturumlarını kendi portuna çeviremez; çıkışta silinir, çökenlerinki açılışta süpürülür.
//! Profilde sır YOK: jeton/ajan/anahtar pane env'inden okunur (MCP: `bearer_token_env_var` +
//! `env_http_headers`; hook: curl `--variable %VAR`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Hook'ların stable olduğu ölçülen ilk sürüm (CDX-W); daha eskisinde ajan başlatılmaz.
pub const MIN_VERSION: (u32, u32, u32) = (0, 154, 0);

/// Claude ile ortak olay kümesi. `Notification` / `StopFailure` codex'te yok (durum bunlara dayanmaz).
const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];

/// Pane'in codex'e özgü isteği (frontend claude argv'si yerine bunu yollar; argv burada kurulur).
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub model: Option<String>,
    #[serde(default)]
    pub add_dirs: Vec<String>,
    pub resume: Option<String>,
    pub prompt: Option<String>,
    /// #671: the job's effort as `-c model_reasoning_effort=` (fresh and resume alike — codex does
    /// not carry it over). Dex's own, never from IPC.
    #[serde(skip)]
    pub effort: Option<crate::effort::Effort>,
}

pub fn is_codex(cmd: &str) -> bool {
    Path::new(cmd).file_stem().is_some_and(|s| s.eq_ignore_ascii_case("codex"))
}

/// The real codex binary. On this setup PATH's `codex` is the claude-route shim (a second job +
/// CPU cap on top of Dex's own, DEX-27); its `routes.conf` names the real one under
/// `[slot-binaries] codex = …`. Otherwise the first `codex` on PATH outside `.claude-route`,
/// else `cmd` unchanged. Mirrors `providers::resolve_claude`.
pub fn resolve(cmd: &str) -> String {
    if !is_codex(cmd) || Path::new(cmd).is_absolute() {
        return cmd.to_string();
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    let route = home.join(".claude-route");
    if let Ok(text) = std::fs::read_to_string(route.join("routes.conf")) {
        if let Some(p) = slot_binary(&text).filter(|p| Path::new(p).is_file()) {
            return p;
        }
    }
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|d| !d.starts_with(&route))
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmd.to_string())
}

fn slot_binary(conf: &str) -> Option<String> {
    let mut in_slots = false;
    for line in conf.lines().map(str::trim) {
        if line.starts_with('[') {
            in_slots = line == "[slot-binaries]";
        } else if let Some((k, v)) = line.split_once('=').filter(|_| in_slots) {
            if k.trim() == "codex" {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("CODEX_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join(".codex")
}

pub fn profile_name(agent: &str) -> String {
    format!("dex-{agent}-{}", std::process::id())
}

fn profile_path(home: &Path, name: &str) -> PathBuf {
    home.join(format!("{name}.config.toml"))
}

/// Profile files of dead Dex processes (`dex-<ajan>-<pid>.config.toml`).
pub fn sweep_stale(home: &Path) {
    sweep(home, |pid| !crate::proc::alive(pid));
}

/// This process's profiles; the host calls it on exit (via `Core::shutdown`).
pub fn remove_own(home: &Path) {
    let me = std::process::id();
    sweep(home, |pid| pid == me);
}

fn sweep(home: &Path, gone: impl Fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(home) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name
            .to_string_lossy()
            .strip_prefix("dex-")
            .and_then(|r| r.strip_suffix(".config.toml"))
            .and_then(|r| r.rsplit_once('-'))
            .and_then(|(_, pid)| pid.parse::<u32>().ok());
        if pid.is_some_and(&gone) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub struct Profile<'a> {
    /// Workers deny tool calls when the Dex hook receiver is unavailable (#191).
    pub worker: bool,
    pub instructions: &'a str,
    /// Hook receiver / Dex MCP port.
    pub port: u16,
    /// Browser MCP (DEX-20) as a stdio server: (command, args).
    pub browser: Option<(&'a str, Vec<&'a str>)>,
    /// Folders marked trusted: codex's TUI otherwise opens on "Do you trust the contents of this
    /// directory?" and a delegated (background) worker waits there forever. The pane's workdir and
    /// a worker's job worktree, which a resumed session returns to (#395). The patron picked the
    /// team folder, which is the trust decision.
    pub trust: Vec<&'a str>,
    /// `(model, successor)`: marks codex's "switch model?" startup dialog as already answered —
    /// the patron chose this model in Dex, and a background worker cannot answer a dialog.
    pub migration: Option<(String, String)>,
}

/// #395: the folders a codex pane trusts — its workdir and, for a worker, the worktree of its
/// job: a plain start (the patron opens the pane, a chain hand-back) may run outside it but
/// resumes that job's session, and codex takes the session back to the worktree it ran in.
/// #838 D: `job` is the session's launch-bound job — only its worktree, never another project's
/// (a job just starting is still queued, so the latest started one may be a different project's).
/// Unbound (a pre-project session): the latest started job's, as before.
pub fn trust_dirs(conn: &rusqlite::Connection, agent: &str, worker: bool, cwd: Option<&str>, job: Option<i64>) -> Result<Vec<String>, String> {
    let mut dirs: Vec<String> = cwd.into_iter().map(String::from).collect();
    if worker {
        let wt = match job {
            Some(id) => crate::worktree::get(conn, id)?.filter(|w| w.removed_at.is_none() && w.agent_id == agent),
            None => crate::worktree::active(conn, agent)?,
        };
        if let Some(w) = wt {
            let w = w.path.to_string_lossy().into_owned();
            if !dirs.contains(&w) {
                dirs.push(w);
            }
        }
    }
    Ok(dirs)
}

/// TOML basic string (quotes, backslashes and control characters escaped; no toml dependency).
fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Hook command string. codex runs it through PowerShell on Windows and `sh` elsewhere: single
/// quotes are literal in both (`{{…}}` would be a script block and `@-` a splat in PowerShell).
/// curl reads token/agent from the pane env itself (`--variable %VAR`, curl ≥ 8.3), like the
/// claude SessionStart hook; the JSON event arrives on stdin, the answer goes to stdout.
fn hook_command(port: u16) -> String {
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    format!(
        "{curl} -s -m 3 -H 'Content-Type: application/json' --variable %DEX_HOOK_TOKEN --variable %DEX_AGENT_ID --variable %DEX_MCP_KEY \
         --expand-header 'Authorization: Bearer {{{{DEX_HOOK_TOKEN}}}}' --expand-header 'X-Dex-Agent: {{{{DEX_AGENT_ID}}}}' \
         --expand-header 'X-Dex-Key: {{{{DEX_MCP_KEY}}}}' \
         --data-binary '@-' http://127.0.0.1:{port}/hook"
    )
}

fn worker_hook_command(port: u16) -> String {
    let curl = hook_command(port).replacen(" -s -m 3 ", " -sS -f -m 8 ", 1);
    // Keep stdout ASCII: PowerShell 5.1 may use a legacy code page, while Codex reads UTF-8.
    let deny = concat!(
        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":""#,
        r#"Dex izin denetimi yan\u0131t vermedi; ara\u00e7 engellendi. Ayn\u0131 ad\u0131m\u0131 daha sonra yeniden dene."}}"#,
    );
    if cfg!(windows) {
        // PowerShell 5.1 has no `||`. Emit a normal deny response: hook exit codes are not
        // required for enforcement, and the curl timeout precedes the hook's own timeout.
        format!("$answer = {curl}; if ($LASTEXITCODE -ne 0) {{ Write-Output '{deny}' }} else {{ $answer }}")
    } else {
        format!("{curl} || printf '%s' '{deny}'")
    }
}

pub fn profile_toml(p: &Profile) -> String {
    let mut t = String::from("# Dex (DEX-26) — otomatik üretildi, elle düzenlemeyin. Yalnız `-p` ile okunur; Dex kapanınca silinir.\n");
    t.push_str(&format!("developer_instructions = {}\n", q(p.instructions)));
    // Dex tools run without a prompt, like claude's `--allowedTools mcp__dex`.
    t.push_str(&format!(
        "\n[mcp_servers.dex]\nurl = {}\nbearer_token_env_var = \"DEX_HOOK_TOKEN\"\n\
         env_http_headers = {{ \"X-Dex-Agent\" = \"DEX_AGENT_ID\", \"X-Dex-Key\" = \"DEX_MCP_KEY\" }}\n\
         default_tools_approval_mode = \"approve\"\n",
        q(&format!("http://127.0.0.1:{}/mcp", p.port))
    ));
    if let Some((cmd, args)) = &p.browser {
        let args: Vec<String> = args.iter().map(|a| q(a)).collect();
        t.push_str(&format!(
            "\n[mcp_servers.browser]\ncommand = {}\nargs = [{}]\ndefault_tools_approval_mode = \"approve\"\n",
            q(cmd),
            args.join(", ")
        ));
    }
    for dir in &p.trust {
        // codex keys `[projects]` by the lowercased path on Windows (see the user's config.toml).
        let key = if cfg!(windows) { dir.to_lowercase() } else { dir.to_string() };
        t.push_str(&format!("\n[projects.{}]\ntrust_level = \"trusted\"\n", q(&key)));
    }
    if let Some((from, to)) = &p.migration {
        t.push_str(&format!("\n[notice.model_migrations]\n{} = {}\n", q(from), q(to)));
    }
    let cmd = q(&hook_command(p.port));
    for ev in EVENTS {
        let worker = p.worker && *ev == "PreToolUse";
        let command = if worker { q(&worker_hook_command(p.port)) } else { cmd.clone() };
        let timeout = if worker { 15 } else { 5 };
        t.push_str(&format!("\n[[hooks.{ev}]]\n[[hooks.{ev}.hooks]]\ntype = \"command\"\ncommand = {command}\ntimeout = {timeout}\n"));
    }
    t
}

/// Switches off every MCP server in the user's config.toml except Dex's own (`own`): codex
/// layers the profile over config.toml, so without this an agent also starts the patron's
/// personal servers. Returns the `-c` overrides and the TOML to append to the profile.
///
/// Not as a bare `[mcp_servers.<ad>] enabled = false` table in the profile (#395): `-p` makes the
/// profile the user layer codex writes to, and every write (the TUI's folder trust) validates
/// that file alone — a table without command/url is "invalid transport" and the write fails.
/// A bare-key name goes as `-c mcp_servers.<ad>.enabled=false`. `-c` keys are split on `.`
/// unquoted, so any other name (`"a.b"`, `"my server"`) is copied whole into the profile —
/// transport, env and sub-tables included — with `enabled = false` (#425). A config that does not
/// parse stops the spawn: guessing the servers would leave some of them silently on.
fn disable_user_mcp(user_config: &str, own: &[&str]) -> Result<(Vec<String>, String), String> {
    let user: toml::Table = user_config.parse().map_err(|e: toml::de::Error| {
        // Only the position: the parser's snippet could show a token from the user's config.
        let line = e.span().map_or(0, |s| user_config[..s.start].matches('\n').count() + 1);
        format!("Codex config.toml ayrıştırılamadı (satır {line}): {}; kişisel MCP sunucuları kapatılamadığı için ajan başlatılmadı.", e.message())
    })?;
    let mut overrides = Vec::new();
    let mut copies = toml::Table::new();
    let servers = user.get("mcp_servers").and_then(toml::Value::as_table);
    for (name, server) in servers.into_iter().flatten() {
        let Some(server) = server.as_table().filter(|_| !own.contains(&name.as_str())) else {
            continue;
        };
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            overrides.extend(["-c".to_string(), format!("mcp_servers.{name}.enabled=false")]);
        } else {
            let mut off = server.clone();
            off.insert("enabled".into(), false.into());
            copies.insert(name.clone(), off.into());
        }
    }
    if copies.is_empty() {
        return Ok((overrides, String::new()));
    }
    let mut wrap = toml::Table::new();
    wrap.insert("mcp_servers".into(), copies.into());
    let t = toml::to_string(&wrap).map_err(|e| format!("Kişisel MCP sunucularının kapatma tablosu yazılamadı: {e}; ajan başlatılmadı."))?;
    Ok((overrides, format!("\n{t}")))
}

/// Writes the agent's profile into `home`; returns its `-p` name and the `-c` overrides that
/// switch off the user's MCP servers (for [`argv`]).
pub fn write_profile(home: &Path, agent: &str, p: &Profile) -> Result<(String, Vec<String>), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
    let name = profile_name(agent);
    let path = profile_path(home, &name);
    let config = home.join("config.toml");
    let user = match std::fs::read_to_string(&config) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        r => r.map_err(|e| format!("{}: {e}", config.display()))?,
    };
    let own: &[&str] = if p.browser.is_some() { &["dex", "browser"] } else { &["dex"] };
    let (overrides, off) = disable_user_mcp(&user, own)?;
    std::fs::write(&path, profile_toml(p) + &off).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((name, overrides))
}

/// `codex [resume] <options> [-- [<uuid>] [<prompt>]]`.
///
/// Match Claude's bypass mode for leaders and workers, including resume.
/// `-a never -s danger-full-access` removes the OS sandbox and approval dialogs. Dex's
/// PreToolUse still applies hard Deny decisions; worker hook transport failures also deny.
/// This is an accident guard, not an OS security boundary (GRAY_AT_PRE_TOOL stays false).
/// Passed explicitly so a user-level `approval_policy` cannot silently change an agent's powers.
/// `--dangerously-bypass-hook-trust`: the hooks are Dex's own, rewritten on every spawn (the port
/// changes per process), so a persisted trust hash could never match (CDX-W §4.3).
pub fn argv(profile: &str, overrides: &[String], launch: &Launch, extra_dirs: &[&Path]) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let resume = launch.resume.as_deref().filter(|r| !r.trim().is_empty());
    if resume.is_some() {
        a.push("resume".into());
    }
    for d in launch.add_dirs.iter().map(String::as_str).chain(extra_dirs.iter().filter_map(|p| p.to_str())) {
        a.extend(["--add-dir".into(), d.into()]);
    }
    a.extend(["-a", "never", "-s", "danger-full-access", "--dangerously-bypass-hook-trust", "-p"].map(String::from));
    a.push(profile.into());
    a.extend(overrides.iter().cloned());
    if let Some(e) = launch.effort {
        a.extend(["-c".into(), format!("model_reasoning_effort={}", e.as_str())]);
    }
    if let Some(m) = launch.model.as_deref().filter(|m| !m.trim().is_empty()) {
        a.extend(["-m".into(), m.into()]);
    }
    let prompt = launch.prompt.as_deref().filter(|p| !p.trim().is_empty());
    if resume.is_some() || prompt.is_some() {
        a.push("--".into()); // a prompt starting with "-" must stay positional
    }
    a.extend(resume.map(String::from));
    a.extend(prompt.map(String::from));
    a
}

// ── Sürüm kapısı ──────────────────────────────────────────────────────────────

fn parse_version(out: &str) -> Option<(u32, u32, u32)> {
    let v = out.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut it = v.split(['.', '-']).map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// `codex features list` row `hooks  stable  true`.
fn hooks_stable(features: &str) -> bool {
    features.lines().any(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        w.len() >= 3 && w[0] == "hooks" && w[1] == "stable" && w[2] == "true"
    })
}

fn run(bin: &str, args: &[&str]) -> Result<String, String> {
    let mut c = std::process::Command::new(bin);
    c.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from the GUI app
    }
    let out = c.output().map_err(|_| "codex bulunamadı — Codex CLI kurulu ve PATH'te olmalı".to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Minimum-version gate, checked before every codex spawn; only a pass is cached (an upgrade
/// while Dex runs is picked up on the next start attempt).
pub fn check_ready(bin: &str) -> Result<(), String> {
    static OK: OnceLock<()> = OnceLock::new();
    if OK.get().is_some() {
        return Ok(());
    }
    let (maj, min, pat) = MIN_VERSION;
    let need = format!("{maj}.{min}.{pat}");
    let v = parse_version(&run(bin, &["--version"])?).ok_or("codex sürümü okunamadı")?;
    if v < MIN_VERSION {
        return Err(format!(
            "Codex CLI {}.{}.{} eski; Dex en az {need} ister (hook desteği). `codex update` ile güncelle.",
            v.0, v.1, v.2
        ));
    }
    if !hooks_stable(&run(bin, &["features", "list"])?) {
        return Err("Bu Codex CLI sürümünde hook'lar kararlı (stable) ve açık değil; Dex ajan durumunu izleyemez. Codex'i güncelle ya da `[features] hooks = true` yap.".into());
    }
    let _ = OK.set(());
    Ok(())
}

// ── Model kataloğu ────────────────────────────────────────────────────────────

#[derive(Serialize, Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    /// The catalog's suggested successor; codex's TUI opens on a "switch model?" dialog for it.
    #[serde(skip)]
    pub upgrade: Option<String>,
}

/// `(model, successor)` when the catalog nudges away from `model`.
pub fn migration_for(model: &str) -> Option<(String, String)> {
    let up = models().ok()?.into_iter().find(|m| m.id == model)?.upgrade?;
    Some((model.to_string(), up))
}

/// Models the logged-in account can pick (`codex debug models`, `visibility: list`), in codex's
/// own priority order. Live, not a static list: the catalog follows the account and the CLI.
/// The server filters the catalog by the CLI's `client_version` (gpt-6.1 only reaches ≥ 0.160),
/// so the cache is keyed by the binary's path + mtime: a `codex update` while Dex runs shows the
/// new models on the next call instead of after a restart.
pub fn models() -> Result<Vec<ModelInfo>, String> {
    type Key = (String, Option<std::time::SystemTime>);
    static CACHE: Mutex<Option<(Key, Vec<ModelInfo>)>> = Mutex::new(None);
    let bin = resolve("codex");
    let key: Key = (bin.clone(), std::fs::metadata(&bin).and_then(|m| m.modified()).ok());
    if let Some((k, m)) = CACHE.lock().unwrap().as_ref() {
        if *k == key {
            return Ok(m.clone());
        }
    }
    let list = parse_models(&run(&bin, &["debug", "models"])?)?;
    *CACHE.lock().unwrap() = Some((key, list.clone()));
    Ok(list)
}

pub(crate) fn parse_models(json: &str) -> Result<Vec<ModelInfo>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| "codex model listesi okunamadı".to_string())?;
    let arr = v.get("models").unwrap_or(&v).as_array().ok_or("codex model listesi okunamadı")?;
    let mut rows: Vec<(i64, ModelInfo)> = arr
        .iter()
        .filter(|m| m.get("visibility").and_then(|x| x.as_str()) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug").or_else(|| m.get("id")).and_then(|x| x.as_str())?.to_string();
            let label = m.get("display_name").and_then(|x| x.as_str()).unwrap_or(&id).to_string();
            let upgrade = m.pointer("/upgrade/model").and_then(|x| x.as_str()).map(String::from);
            Some((m.get("priority").and_then(|x| x.as_i64()).unwrap_or(i64::MAX), ModelInfo { id, label, upgrade }))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    Ok(rows.into_iter().map(|r| r.1).collect())
}

/// The catalog is the account's and can change; the DB only needs a sane argv value.
pub fn check_model(model: &str) -> Result<(), String> {
    let ok = !model.is_empty() && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ok.then_some(()).ok_or_else(|| format!("geçersiz codex modeli: {model}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_protected_config_denies_shell_and_patch_for_worker_and_leader() {
        use crate::permit::{decide, Ctx, Verdict};
        let root = std::env::current_dir().unwrap();
        for leader in [false, true] {
            let ctx = Ctx { leader, write: vec![root.clone()], read: vec![root.clone()], cwd: Some(root.clone()), ..Default::default() };
            for file in [".codex/config.toml", ".git/config", "CLAUDE.md", "AGENTS.md", "~/.codex/config.toml"] {
                assert!(matches!(decide("exec_command", &serde_json::json!({"cmd":format!("echo x > {file}")}), &ctx), Verdict::Deny(_)), "shell: {file}");
                assert!(matches!(decide("apply_patch", &serde_json::json!({"command":format!("*** Begin Patch\n*** Add File: {file}\n+x\n*** End Patch")}), &ctx), Verdict::Deny(_)), "patch: {file}");
            }
            assert_eq!(decide("exec_command", &serde_json::json!({"cmd":"git add src/a.rs && git commit -m test"}), &ctx), Verdict::Allow);
        }
    }

    #[test]
    fn worker_pre_tool_hook_fails_closed() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::process::{Command, Stdio};
        let expected = serde_json::json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": "Dex izin denetimi yanıt vermedi; araç engellendi. Aynı adımı daha sonra yeniden dene."
        } });
        assert!(worker_hook_command(0).is_ascii());
        let invoke = |port| {
            let mut cmd = if cfg!(windows) {
                let mut c = Command::new("powershell.exe");
                // Reproduce Turkish Windows stdout bytes independently of the host code page.
                let command = format!("[Console]::OutputEncoding = [Text.Encoding]::GetEncoding(1254); {}", worker_hook_command(port));
                c.args(["-NoProfile", "-Command", &command]);
                c
            } else {
                let mut c = Command::new("sh");
                c.args(["-c", &worker_hook_command(port)]);
                c
            };
            let mut child = cmd.env("DEX_HOOK_TOKEN", "smoke-token").env("DEX_AGENT_ID", "smoke-agent")
                .env("DEX_MCP_KEY", "smoke-key").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(b"{}").unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
        };
        for (status, body, delay) in [
            (200, r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"policy"}}"#, 0),
            (200, "{}", 0), (401, "", 0), (200, "{}", 9),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let n = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                assert!(request.contains("Authorization: Bearer smoke-token") && request.contains("X-Dex-Agent: smoke-agent") && request.contains("X-Dex-Key: smoke-key"));
                std::thread::sleep(std::time::Duration::from_secs(delay));
                let _ = write!(stream, "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
            let out = invoke(port);
            server.join().unwrap();
            if status == 200 && delay == 0 {
                assert_eq!(out, serde_json::from_str::<serde_json::Value>(body).unwrap());
            } else {
                assert_eq!(out, expected);
            }
        }
        assert_eq!(invoke(0), expected);
        let p = Profile { worker: true, instructions: "", port: 1, browser: None, trust: vec![], migration: None };
        let t = profile_toml(&p);
        assert!(t.contains(" -sS -f -m 8 ") && t.contains("timeout = 15"));
        let t = profile_toml(&Profile { worker: false, ..p });
        assert!(!t.contains("timeout = 15") && !t.contains(" -sS -f -m 8 "));
    }

    #[test]
    fn argv_fresh_resume_and_dirs() {
        let mem = PathBuf::from("M");
        let fresh = Launch { model: Some("gpt-5.5".into()), add_dirs: vec!["P".into()], resume: None, prompt: Some("-iş".into()), effort: None };
        assert_eq!(
            argv("dex-a-1", &["-c".into(), "mcp_servers.x.enabled=false".into()], &fresh, &[&mem]),
            ["--add-dir", "P", "--add-dir", "M", "-a", "never", "-s", "danger-full-access", "--dangerously-bypass-hook-trust", "-p", "dex-a-1",
             "-c", "mcp_servers.x.enabled=false", "-m", "gpt-5.5", "--", "-iş"]
        );
        let resume = Launch { resume: Some("01a0-uuid".into()), ..Default::default() };
        let a = argv("p", &[], &resume, &[]);
        assert_eq!(a.first().map(String::as_str), Some("resume"));
        assert_eq!(&a[a.len() - 2..], ["--", "01a0-uuid"]);
        assert!(!a.contains(&"-m".to_string()));
        assert!(a.windows(4).any(|w| w == ["-a", "never", "-s", "danger-full-access"]));
        let bare = argv("p", &[], &Launch::default(), &[]);
        assert!(bare.windows(4).any(|w| w == ["-a", "never", "-s", "danger-full-access"]));
        assert!(!bare.contains(&"--".to_string()) && !bare.contains(&"resume".to_string()));
        assert!(!bare.iter().chain(&a).any(|x| x.contains("model_reasoning_effort")), "effort yoksa bayrak yok");
    }

    /// #671: the job's effort goes as `-c`, after the profile and its overrides, fresh and resume.
    #[test]
    fn argv_passes_effort_fresh_and_resume() {
        use crate::effort::Effort;
        let fresh = Launch { model: Some("gpt-5.5".into()), effort: Some(Effort::Xhigh), prompt: Some("iş".into()), ..Default::default() };
        let a = argv("p", &["-c".into(), "mcp_servers.x.enabled=false".into()], &fresh, &[]);
        let at = a.iter().position(|x| x == "model_reasoning_effort=xhigh").expect("effort bayrağı");
        assert_eq!(a[at - 1], "-c");
        assert!(at > a.iter().position(|x| x == "mcp_servers.x.enabled=false").unwrap());
        assert!(at < a.iter().position(|x| x == "--").unwrap());
        let resume = Launch { resume: Some("u".into()), effort: Some(Effort::Medium), ..Default::default() };
        let r = argv("p", &[], &resume, &[]);
        assert_eq!(r[0], "resume");
        assert!(r.windows(2).any(|w| w == ["-c", "model_reasoning_effort=medium"]), "{r:?}");
    }

    #[test]
    fn profile_has_no_secret_and_parses_shape() {
        let t = profile_toml(&Profile { worker: false, instructions: "Rol \"x\"\nsatır\\2", port: 4242, browser: Some(("cmd", vec!["/c", "npx"])), trust: vec![r"C:\P", r"C:\P\wt"], migration: Some(("gpt-5.5".into(), "gpt-5.6-sol".into())) });
        assert!(t.contains("[notice.model_migrations]\n\"gpt-5.5\" = \"gpt-5.6-sol\""), "{t}");
        assert!(t.contains(r#"developer_instructions = "Rol \"x\"\nsatır\\2""#), "{t}");
        assert!(t.contains("url = \"http://127.0.0.1:4242/mcp\""));
        assert!(t.contains("bearer_token_env_var = \"DEX_HOOK_TOKEN\""));
        assert!(t.contains("[mcp_servers.browser]\ncommand = \"cmd\"\nargs = [\"/c\", \"npx\"]"));
        for ev in EVENTS {
            assert!(t.contains(&format!("[[hooks.{ev}.hooks]]")), "{ev}");
        }
        assert!(!t.contains("Notification") && !t.contains("StopFailure"));
        assert!(t.contains("'Authorization: Bearer {{DEX_HOOK_TOKEN}}'"), "{t}");
        assert!(t.contains("'X-Dex-Key: {{DEX_MCP_KEY}}'"), "{t}");
        assert!(t.contains("'@-' http://127.0.0.1:4242/hook"));
        // TOML-escaped key: one backslash in the path, two in the file.
        let key = if cfg!(windows) { r"c:\\p" } else { r"C:\\P" };
        assert!(t.contains(&format!("[projects.\"{key}\"]\ntrust_level = \"trusted\"")), "{t}");
        let key = if cfg!(windows) { r"c:\\p\\wt" } else { r"C:\\P\\wt" };
        assert!(t.contains(&format!("[projects.\"{key}\"]\ntrust_level = \"trusted\"")), "{t}");
    }

    /// #395: a worker's profile trusts its job worktree also when the pane starts in the team folder
    /// (codex resumes the session there); leaders and job-less workers trust only the workdir.
    #[test]
    fn worker_trusts_its_job_worktree() {
        use crate::worktree::tests::{db_for, job, repo};
        let root = repo("codex-trust");
        let db = db_for(&root);
        let d = job(&db, "kaan");
        let wt = crate::worktree::prepare(&db, &d).unwrap().unwrap().path.to_string_lossy().into_owned();
        let team = root.to_string_lossy().into_owned();
        crate::db::with(&db, |c| {
            assert_eq!(trust_dirs(c, "kaan", true, Some(&team), None)?, [team.clone(), wt.clone()]);
            assert_eq!(trust_dirs(c, "kaan", true, Some(&wt), None)?, [wt.clone()], "job start: cwd is the worktree");
            assert_eq!(trust_dirs(c, "kaan", false, Some(&team), None)?, [team.clone()]);
            assert_eq!(trust_dirs(c, "ayla", true, Some(&team), None)?, [team.clone()]);
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #838 D: a session bound to job 2 trusts its own worktree — the folder it starts in, and the
    /// one a resumed session returns to — so codex never asks there; the latest started job's
    /// worktree (job 1, another project, while job 2 is still queued) is not trusted. Unbound: as
    /// before (#395).
    #[test]
    fn a_bound_session_trusts_its_own_worktree_only() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let (w1, w2, p2) = (r"C:\P1\.dex\wt\kaan-1", r"C:\P2\.dex\wt\kaan-2", r"C:\P2");
        c.execute_batch(&format!(
            "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES
                 (1,'timur','kaan','default','x','running',1,1),(2,'timur','kaan','default','y','queued',1,1);
             INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES
                 (1,'kaan','C:\\P1','{w1}','dex/kaan/1','b',1),(2,'kaan','C:\\P2','{w2}','dex/kaan/2','b',1);"
        ))
        .unwrap();
        assert_eq!(trust_dirs(&c, "kaan", true, Some(w2), Some(2)).unwrap(), [w2], "job start in its worktree");
        assert_eq!(trust_dirs(&c, "kaan", true, Some(p2), Some(2)).unwrap(), [p2, w2], "a resume returns to it");
        assert_eq!(trust_dirs(&c, "kaan", true, Some(w2), None).unwrap(), [w2, w1], "unbound: the latest started job's, as before");
        assert_eq!(trust_dirs(&c, "ayla", true, Some(p2), Some(2)).unwrap(), [p2], "never another agent's job");
        let p = Profile { worker: true, instructions: "", port: 1, browser: None, trust: vec![w2], migration: None };
        let key = if cfg!(windows) { w2.to_lowercase() } else { w2.to_string() };
        assert!(profile_toml(&p).contains(&format!("[projects.{}]\ntrust_level = \"trusted\"", q(&key))));
        c.execute("UPDATE delegation_worktrees SET removed_at = 1 WHERE delegation_id = 2", []).unwrap();
        assert_eq!(trust_dirs(&c, "kaan", true, Some(p2), Some(2)).unwrap(), [p2], "a removed worktree is not trusted");
    }

    #[test]
    fn user_mcp_servers_are_disabled_dex_stays() {
        let user = "[features]
x = 1
[mcp_servers.happier]
command = \"node\"
[mcp_servers.node_repl]
command = \"r\"
            [mcp_servers.node_repl.env]
A = \"1\"
[mcp_servers.dex]
url = \"x\"
";
        let (overrides, off) = disable_user_mcp(user, &["dex"]).unwrap();
        assert_eq!(overrides, ["-c", "mcp_servers.happier.enabled=false", "-c", "mcp_servers.node_repl.enabled=false"]);
        assert!(off.is_empty());
        assert_eq!(disable_user_mcp("[features]\n", &["dex"]).unwrap(), (vec![], String::new()));
        // Unparseable config: no spawn, and the error shows no config text (could be a token).
        let err = disable_user_mcp("[mcp_servers.a]\ncommand = \"sk-gizli\n", &["dex"]).unwrap_err();
        assert!(err.contains("satır 2") && !err.contains("sk-gizli"), "{err}");

        let home = std::env::temp_dir().join(format!("dex-codex-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let p = Profile { worker: false, instructions: "", port: 1, browser: None, trust: vec![], migration: None };
        // No config.toml at all: nothing to disable.
        let (_, overrides) = write_profile(&home, "ada", &p).unwrap();
        assert!(overrides.is_empty());
        std::fs::write(home.join("config.toml"), "[mcp_servers.one]
command = \"a\"
[mcp_servers.\"a.b\"]
command = \"npx\"
args = [\"-y\", \"x\"]
enabled = true
[mcp_servers.\"a.b\".env]
K = \"v\"
[mcp_servers.\"my server\"]
url = \"http://h/mcp\"
").unwrap();
        let (name, overrides) = write_profile(&home, "ada", &p).unwrap();
        assert_eq!(overrides, ["-c", "mcp_servers.one.enabled=false"]);
        let t = std::fs::read_to_string(profile_path(&home, &name)).unwrap();
        // #425: names `-c` cannot address are copied whole and switched off; the profile still
        // parses, and #395: every [mcp_servers.*] in it carries its own transport.
        let prof: toml::Table = t.parse().unwrap_or_else(|e| panic!("{e}\n{t}"));
        let servers = prof["mcp_servers"].as_table().unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["a.b", "dex", "my server"], "{t}");
        let ab = servers["a.b"].as_table().unwrap();
        assert_eq!(ab["enabled"].as_bool(), Some(false));
        assert_eq!(ab["command"].as_str(), Some("npx"));
        assert_eq!(ab["args"].as_array().unwrap().len(), 2);
        assert_eq!(ab["env"]["K"].as_str(), Some("v"));
        let my = servers["my server"].as_table().unwrap();
        assert_eq!((my["enabled"].as_bool(), my["url"].as_str()), (Some(false), Some("http://h/mcp")));
        assert!(servers.values().all(|s| s.get("url").or(s.get("command")).is_some()), "{t}");
        assert!(servers["dex"].get("enabled").is_none(), "Dex MCP açık kalır: {t}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn slot_binary_from_routes_conf() {
        let conf = "real = C:\\x\\claude.exe\n[slot-binaries]\n# c\nopencode = a.cmd\ncodex = C:\\Codex\\bin\\codex.exe\n[other]\ncodex = nope\n";
        assert_eq!(slot_binary(conf).as_deref(), Some("C:\\Codex\\bin\\codex.exe"));
        assert_eq!(slot_binary("[other]\ncodex = nope\n"), None);
        assert_eq!(resolve("claude"), "claude");
    }

    #[test]
    fn version_gate_parsing() {
        assert_eq!(parse_version("codex-cli 0.154.0\n"), Some((0, 154, 0)));
        assert_eq!(parse_version("codex-cli 1.2.3-alpha.1"), Some((1, 2, 3)));
        assert!(parse_version("codex-cli 0.147.0").unwrap() < MIN_VERSION);
        assert!(hooks_stable("hooks                                    stable             true\nmulti_agent stable true"));
        assert!(!hooks_stable("hooks  experimental  true"));
        assert!(!hooks_stable("hooks  stable  false\nplugin_hooks removed false"));
    }

    #[test]
    fn models_from_catalog() {
        let j = r#"{"models":[{"slug":"b","display_name":"B","visibility":"list","priority":12,"upgrade":{"model":"a"}},
            {"slug":"h","display_name":"H","visibility":"hide","priority":3},{"slug":"a","display_name":"A","visibility":"list","priority":1}]}"#;
        let m = parse_models(j).unwrap();
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((m[0].upgrade.as_deref(), m[1].upgrade.as_deref()), (None, Some("a")));
        assert!(check_model("gpt-5.6-sol").is_ok());
        assert!(check_model("").is_err() && check_model("a b").is_err() && check_model("x;rm").is_err());
    }

    #[test]
    fn sweep_removes_only_dead_and_own() {
        let home = std::env::temp_dir().join(format!("dex-codex-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let dead = home.join("dex-ada-1f2e-4294967294.config.toml");
        let mine = profile_path(&home, &profile_name("ada-1f2e"));
        let user = home.join("dex-notes.config.toml");
        for f in [&dead, &mine, &user] {
            std::fs::write(f, "").unwrap();
        }
        sweep_stale(&home);
        assert!(!dead.exists() && mine.exists() && user.exists());
        remove_own(&home);
        assert!(!mine.exists() && user.exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
