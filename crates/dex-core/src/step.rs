// `agent.step`: what an agent is doing right now, as a short Turkish label for the Core mascot (#242).
//
// Built from the hook body's `tool_name` / `tool_input` and nothing else. PRIVACY: a label never
// carries arguments, file contents, URL paths/queries or env — a command shows its first word (a
// second one only for known subcommand CLIs), a file only its base name, a URL only its host.
// Everything goes through `permit::redact` last. Emitting is best-effort and independent of the
// permit decision (see hooks.rs).
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const EVENT: &str = "agent.step";

/// Longest label, in characters.
pub const LABEL_MAX: usize = 60;

/// Per agent: at most this many `start` events per [`WINDOW`].
const MAX_PER_WINDOW: usize = 4;
const WINDOW: Duration = Duration::from_secs(1);

/// Most open (started, not yet ended) tools tracked per agent; parallel tool calls are few.
const OPEN_MAX: usize = 32;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub agent_id: String,
    /// `"start"` | `"end"`.
    pub phase: &'static str,
    pub tool: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// Unix ms.
    pub ts: u64,
}

/// Per-agent rate limit. `end` is never counted: it is emitted only for a `start` that went out,
/// so the UI never holds a step that cannot end, nor sees an end without a start.
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<String, Window>>);

#[derive(Default)]
struct Window {
    starts: VecDeque<Instant>,
    open: HashSet<String>,
}

impl Limiter {
    /// True if a `start` for `key` (the tool_use_id) may go out now.
    pub fn start(&self, agent: &str, key: &str, now: Instant) -> bool {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let w = all.entry(agent.to_string()).or_default();
        while w.starts.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            w.starts.pop_front();
        }
        if w.starts.len() >= MAX_PER_WINDOW {
            return false;
        }
        w.starts.push_back(now);
        if w.open.len() >= OPEN_MAX {
            w.open.clear();
        }
        w.open.insert(key.to_string());
        true
    }

    /// True if `key` was started (and so may end); consumes it.
    pub fn end(&self, agent: &str, key: &str) -> bool {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        all.get_mut(agent).is_some_and(|w| w.open.remove(key))
    }

    /// The agent's turn is over (Stop, interrupt, session end): no step stays open. The rate
    /// window is kept — a turn boundary does not lift the limit.
    pub fn forget(&self, agent: &str) {
        if let Some(w) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get_mut(agent) {
            w.open.clear();
        }
    }
}

/// The step a hook body stands for, if any: (phase, ok). `None` for every other hook.
pub fn phase_of(event: &str) -> Option<(&'static str, Option<bool>)> {
    match event {
        "PreToolUse" => Some(("start", None)),
        "PostToolUse" => Some(("end", Some(true))),
        "PostToolUseFailure" => Some(("end", Some(false))),
        _ => None,
    }
}

/// The key that pairs a start with its end: `tool_use_id`, else the tool name.
pub fn key_of(body: &Value) -> String {
    let s = |k: &str| body.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    s("tool_use_id").or_else(|| s("tool_name")).unwrap_or("").to_string()
}

/// Short Turkish label for a tool call (≤ [`LABEL_MAX`] chars, redacted).
pub fn label(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let text = match tool {
        "Bash" | "PowerShell" => with("komut", command_head(s("command"))),
        "Write" => with("dosya yazıyor", base_name(s("file_path"))),
        "Edit" | "MultiEdit" => with("dosya düzenliyor", base_name(s("file_path"))),
        "NotebookEdit" => with("not defteri düzenliyor", base_name(s("notebook_path"))),
        "Read" => with("dosya okuyor", base_name(s("file_path"))),
        "Grep" => with("arıyor", term(s("pattern"))),
        "Glob" | "LS" => "dosya arıyor".to_string(),
        "WebFetch" => with("web", host(s("url"))),
        "WebSearch" => "web araması".to_string(),
        "Task" | "Agent" => "alt ajan çalıştırıyor".to_string(),
        "TodoWrite" => "görev listesi".to_string(),
        "ExitPlanMode" | "EnterPlanMode" => "plan".to_string(),
        "AskUserQuestion" => "soru soruyor".to_string(),
        t if t.starts_with("mcp__") => mcp(t, input),
        // Anything new: only an identifier-shaped tool name is shown.
        t => with("araç", ident(t)),
    };
    finish(&text)
}

fn with(verb: &str, what: String) -> String {
    if what.is_empty() { verb.to_string() } else { format!("{verb} · {what}") }
}

/// Dex's own MCP tools read better than "mcp · dex/delegate"; the rest show server/tool.
fn mcp(tool: &str, input: &Value) -> String {
    let mut parts = tool.trim_start_matches("mcp__").splitn(2, "__");
    let (server, name) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let field = |k: &str| ident(input.get(k).and_then(Value::as_str).unwrap_or(""));
    if server == "dex" {
        return match name {
            "delegate" => with("devrediyor", [field("agentId"), field("role")].into_iter().find(|v| !v.is_empty()).unwrap_or_default()),
            "delegate_to_service" => with("devrediyor", field("team")),
            "ask_leader" => "liderine soruyor".to_string(),
            "report" => "rapor veriyor".to_string(),
            "task_list" | "task_create" | "task_update" => "görev panosu".to_string(),
            "" => "dex".to_string(),
            n => with("dex", ident(n)),
        };
    }
    with("mcp", [ident(server), ident(name)].into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>().join("/"))
}

/// Commands whose second word is a subcommand, not data (`cargo test`, `git status`).
const SUBCOMMAND_CLIS: &[&str] = &[
    "cargo", "git", "npm", "pnpm", "yarn", "npx", "bun", "docker", "kubectl", "gh", "go", "dotnet", "pip", "uv", "make", "just", "rustup", "systemctl", "pytest",
];

/// The command's first word (path and `.exe` stripped), plus a subcommand for [`SUBCOMMAND_CLIS`].
/// Leading `VAR=value` assignments are skipped — they are env and never shown.
fn command_head(cmd: &str) -> String {
    let mut words = cmd.split_whitespace().skip_while(|w| w.contains('='));
    let first = base_name(words.next().unwrap_or(""));
    let first = first.strip_suffix(".exe").unwrap_or(&first).to_string();
    if first.is_empty() || !first.chars().all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c)) {
        return String::new();
    }
    let sub = words.next().filter(|w| SUBCOMMAND_CLIS.contains(&first.as_str()) && is_subcommand(w));
    match sub {
        Some(sub) => format!("{first} {sub}"),
        None => first,
    }
}

fn is_subcommand(w: &str) -> bool {
    w.starts_with(|c: char| c.is_ascii_lowercase()) && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_:".contains(c))
}

/// Last path component only (either separator), stripped of control characters.
fn base_name(path: &str) -> String {
    let name = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    name.chars().filter(|c| !c.is_control()).collect()
}

/// A search term, only when it is a short plain word: patterns are arguments, regexes can hold data.
fn term(pattern: &str) -> String {
    let ok = !pattern.is_empty() && pattern.chars().count() <= 24 && pattern.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c));
    if ok { pattern.to_string() } else { String::new() }
}

/// URL host only: no scheme, userinfo, port, path, query or fragment.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or("", |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("");
    if host.chars().all(|c| c.is_ascii_alphanumeric() || "-.".contains(c)) { host.to_string() } else { String::new() }
}

/// An identifier (agent id, tool or server name), else nothing.
fn ident(s: &str) -> String {
    if !s.is_empty() && s.chars().count() <= 40 && s.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c)) { s.to_string() } else { String::new() }
}

/// Redact credential-looking words, drop control chars, cap at [`LABEL_MAX`] chars.
fn finish(text: &str) -> String {
    let clean: String = crate::permit::redact(text).chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= LABEL_MAX {
        return clean;
    }
    let mut cut: String = clean.chars().take(LABEL_MAX - 1).collect();
    cut.push('…');
    cut
}

/// The `agent.step` payload for a hook, if it stands for one and the limiter lets it through.
/// `denied`: the answer Dex gave a PreToolUse was a deny — the tool never runs, so no start goes out.
pub fn on_hook(limiter: &Limiter, agent: &str, event: &str, body: &Value, denied: bool, now: Instant, ts: u64) -> Option<Step> {
    let (phase, ok) = phase_of(event)?;
    let tool = body.get("tool_name").and_then(Value::as_str).filter(|t| !t.is_empty())?;
    let key = key_of(body);
    let allowed = if phase == "start" { !denied && limiter.start(agent, &key, now) } else { limiter.end(agent, &key) };
    allowed.then(|| Step {
        agent_id: agent.to_string(),
        phase,
        tool: finish(tool),
        label: label(tool, body.get("tool_input").unwrap_or(&Value::Null)),
        ok,
        ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn l(tool: &str, input: Value) -> String {
        label(tool, &input)
    }

    #[test]
    fn labels_for_the_common_tools() {
        assert_eq!(l("Bash", json!({ "command": "cargo test -p dex-core --lib" })), "komut · cargo test");
        assert_eq!(l("Bash", json!({ "command": "git push origin main" })), "komut · git push");
        assert_eq!(l("Bash", json!({ "command": "ls -la" })), "komut · ls");
        assert_eq!(l("Write", json!({ "file_path": "C:\\proj\\src\\world.ts", "content": "x" })), "dosya yazıyor · world.ts");
        assert_eq!(l("Edit", json!({ "file_path": "/a/b/hooks.rs", "old_string": "a", "new_string": "b" })), "dosya düzenliyor · hooks.rs");
        assert_eq!(l("Read", json!({ "file_path": "/a/b/lib.rs" })), "dosya okuyor · lib.rs");
        assert_eq!(l("Grep", json!({ "pattern": "permit", "path": "/secret/dir" })), "arıyor · permit");
        assert_eq!(l("Glob", json!({ "pattern": "**/*.rs" })), "dosya arıyor");
        assert_eq!(l("WebFetch", json!({ "url": "https://user:pw@example.com:8443/a/b?token=abc#x" })), "web · example.com");
        assert_eq!(l("WebSearch", json!({ "query": "my password hunter2" })), "web araması");
        assert_eq!(l("Task", json!({ "prompt": "x" })), "alt ajan çalıştırıyor");
    }

    #[test]
    fn labels_for_mcp_tools() {
        assert_eq!(l("mcp__dex__delegate", json!({ "agentId": "kernel-1", "prompt": "gizli iş" })), "devrediyor · kernel-1");
        assert_eq!(l("mcp__dex__delegate", json!({ "role": "backend", "prompt": "x" })), "devrediyor · backend");
        assert_eq!(l("mcp__dex__delegate", json!({ "agentId": "Ali Veli; rm -rf", "prompt": "x" })), "devrediyor");
        assert_eq!(l("mcp__dex__ask_leader", json!({ "question": "x" })), "liderine soruyor");
        assert_eq!(l("mcp__dex__report", json!({})), "rapor veriyor");
        assert_eq!(l("mcp__github__create_issue", json!({ "title": "gizli" })), "mcp · github/create_issue");
        assert_eq!(l("SomeNewTool", json!({})), "araç · SomeNewTool");
        assert_eq!(l("Weird Tool!", json!({})), "araç");
    }

    #[test]
    fn privacy_arguments_content_and_env_never_leak() {
        let secret = "sk-ABCDEF0123456789";
        // Env assignments are skipped; non-subcommand CLIs show only the program.
        assert_eq!(l("Bash", json!({ "command": format!("API_TOKEN={secret} curl -H 'Authorization: Bearer {secret}' https://x.io/p?k=v") })), "komut · curl");
        assert_eq!(l("Bash", json!({ "command": "cat /etc/passwd" })), "komut · cat");
        assert_eq!(l("Bash", json!({ "command": "C:\\tools\\node.exe server.js --key=abc" })), "komut · node");
        // Second word only when it is a plain subcommand, never a path/flag/value.
        assert_eq!(l("Bash", json!({ "command": "git -C /secret/repo status" })), "komut · git");
        assert_eq!(l("Bash", json!({ "command": "docker run --env X=1 img" })), "komut · docker run");
        assert_eq!(l("Bash", json!({ "command": "cargo ../secret" })), "komut · cargo");
        // Not even a program-shaped first word: no detail.
        assert_eq!(l("Bash", json!({ "command": "\"$(cat key)\" x" })), "komut");
        // A credential-looking file or term name is masked / dropped.
        assert_eq!(l("Read", json!({ "file_path": format!("/a/{secret}") })), "dosya okuyor · [GİZLİ]");
        assert_eq!(l("Grep", json!({ "pattern": "password=hunter2.*" })), "arıyor");
        assert_eq!(l("Grep", json!({ "pattern": "a-very-long-search-term-over-the-cap" })), "arıyor");
        // The raw input never shows up in any label.
        for (tool, input) in [
            ("Write", json!({ "file_path": "/x/y.txt", "content": "TOPSECRETCONTENT" })),
            ("Edit", json!({ "file_path": "/x/y.txt", "old_string": "TOPSECRETCONTENT", "new_string": "z" })),
            ("WebFetch", json!({ "url": "https://h.io/TOPSECRETCONTENT?q=TOPSECRETCONTENT" })),
            ("mcp__dex__delegate", json!({ "agentId": "a-1", "prompt": "TOPSECRETCONTENT" })),
        ] {
            assert!(!l(tool, input).contains("TOPSECRET"), "{tool}");
        }
    }

    #[test]
    fn label_is_capped_at_60_chars_and_one_line() {
        let long = "a".repeat(200);
        for (tool, input) in [
            ("Write", json!({ "file_path": format!("/x/{long}.ts") })),
            ("Bash", json!({ "command": long.clone() })),
            ("Read", json!({ "file_path": format!("/x/ünï{long}") })),
            ("Edit", json!({ "file_path": "/x/a\nb\u{7}c.ts" })),
        ] {
            let out = l(tool, input);
            assert!(out.chars().count() <= LABEL_MAX, "{out}");
            assert!(!out.chars().any(char::is_control), "{out:?}");
        }
        let out = l("Write", json!({ "file_path": format!("/x/{long}.ts") }));
        assert_eq!(out.chars().count(), LABEL_MAX);
        assert!(out.ends_with('…'));
        // Multi-byte text is cut on a char boundary.
        assert!(l("Read", json!({ "file_path": "ş".repeat(100) })).chars().count() <= LABEL_MAX);
    }

    fn hook(tool: &str, id: &str) -> Value {
        json!({ "tool_name": tool, "tool_use_id": id, "tool_input": { "file_path": "/a/b.rs" } })
    }

    #[test]
    fn phases_and_ok_follow_the_hook() {
        let lim = Limiter::default();
        let t = Instant::now();
        let s = on_hook(&lim, "ece", "PreToolUse", &hook("Read", "t1"), false, t, 1).unwrap();
        assert_eq!((s.phase, s.ok, s.tool.as_str(), s.label.as_str(), s.agent_id.as_str(), s.ts), ("start", None, "Read", "dosya okuyor · b.rs", "ece", 1));
        let e = on_hook(&lim, "ece", "PostToolUse", &hook("Read", "t1"), false, t, 2).unwrap();
        assert_eq!((e.phase, e.ok), ("end", Some(true)));
        on_hook(&lim, "ece", "PreToolUse", &hook("Edit", "t2"), false, t, 3).unwrap();
        let f = on_hook(&lim, "ece", "PostToolUseFailure", &hook("Edit", "t2"), false, t, 4).unwrap();
        assert_eq!((f.phase, f.ok), ("end", Some(false)));
        // Other hooks and a body without a tool make no step.
        assert!(on_hook(&lim, "ece", "Stop", &hook("Read", "t3"), false, t, 5).is_none());
        assert!(on_hook(&lim, "ece", "PreToolUse", &json!({}), false, t, 6).is_none());
        // Wire shape: camelCase, `ok` absent on start.
        let v = serde_json::to_value(on_hook(&lim, "ece", "PreToolUse", &hook("Read", "t4"), false, t, 7).unwrap()).unwrap();
        assert_eq!(v, json!({ "agentId": "ece", "phase": "start", "tool": "Read", "label": "dosya okuyor · b.rs", "ts": 7 }));
    }

    #[test]
    fn rate_limit_is_per_agent_and_ends_follow_their_start() {
        let lim = Limiter::default();
        let t = Instant::now();
        let start = |agent: &str, id: &str, at: Duration| on_hook(&lim, agent, "PreToolUse", &hook("Read", id), false, t + at, 0).is_some();
        let ms = Duration::from_millis;
        // 4 in a second go out, the 5th is dropped; another agent is unaffected.
        assert!([0, 100, 200, 300].iter().all(|m| start("ece", &format!("a{m}"), ms(*m))));
        assert!(!start("ece", "a400", ms(400)));
        assert!(start("kaan", "b0", ms(400)));
        // The window slides: a second after the first, one more fits.
        assert!(start("ece", "a1000", ms(1000)));
        assert!(!start("ece", "a1001", ms(1001)));
        // A dropped start has no end; an emitted one does, once.
        let end = |agent: &str, id: &str| on_hook(&lim, agent, "PostToolUse", &hook("Read", id), false, t, 0).is_some();
        assert!(!end("ece", "a400"));
        assert!(end("ece", "a0"));
        assert!(!end("ece", "a0"));
        // Ends do not eat the start budget.
        assert!(end("ece", "a100") && end("ece", "a200") && end("ece", "a300") && end("ece", "a1000"));
        assert!(!start("ece", "a1002", ms(1002)));
        // forget: nothing stays open after the turn.
        assert!(start("kaan", "b1", ms(500)));
        lim.forget("kaan");
        assert!(!end("kaan", "b1"));
    }

    #[test]
    fn denied_pre_tool_use_emits_no_start() {
        let lim = Limiter::default();
        let t = Instant::now();
        assert!(on_hook(&lim, "ece", "PreToolUse", &hook("Bash", "d1"), true, t, 0).is_none());
        // …and so no end can follow either.
        assert!(on_hook(&lim, "ece", "PostToolUse", &hook("Bash", "d1"), false, t, 0).is_none());
    }
}
