//! Bounded, metadata-only diagnostics. Raw prompts, commands and panic payloads never enter logs.
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

struct Rolling {
    dir: PathBuf,
    limit: u64,
}
impl Rolling {
    fn write(&self, day: u64, line: &[u8]) -> io::Result<()> {
        if line.len() as u64 > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log record too large",
            ));
        }
        crate::private_fs::log_dir(&self.dir)?;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some((date, slot)) = name
                .to_str()
                .and_then(|s| s.strip_prefix("dex-"))
                .and_then(|s| s.strip_suffix(".jsonl"))
                .and_then(|s| s.split_once('-'))
            else {
                continue;
            };
            if let (Ok(date), Ok(slot)) = (date.parse::<u64>(), slot.parse::<u8>()) {
                if slot < 4 && date < day.saturating_sub(6) && entry.file_type()?.is_file() {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        let path = |slot| self.dir.join(format!("dex-{day}-{slot}.jsonl"));
        if std::fs::metadata(path(0)).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > self.limit
        {
            if path(3).exists() {
                std::fs::remove_file(path(3))?;
            }
            for slot in (0..3).rev() {
                if path(slot).exists() {
                    std::fs::rename(path(slot), path(slot + 1))?;
                }
            }
        }
        crate::private_fs::append(&path(0))?.write_all(line)
    }
}

struct Logger {
    rolling: Mutex<Rolling>,
    level: log::LevelFilter,
}
static LOGGER: OnceLock<Logger> = OnceLock::new();

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= self.level
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Never persist free-form library errors (URLs, paths, headers, etc.).
        let payload = if record.target() == "dex::audit" {
            serde_json::from_str::<Value>(&record.args().to_string()).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let fields = sanitize(payload.get("fields").cloned().unwrap_or(Value::Null));
        let event = payload["event"]
            .as_str()
            .filter(|s| {
                matches!(
                    *s,
                    "hook_received"
                        | "hook_processed"
                        | "hook_timing"
                        | "cost_ingest_slow"
                        | "delegation"
                        | "pty_spawn"
                        | "pty_exit"
                        | "pty_kill"
                        | "pty_teardown_timeout"
                        | "pty_teardown_complete"
                        | "question"
                        | "leader_delivery"
                        | "permit"
                        | "panic"
                        | "startup"
                        | "shutdown"
                        | "maintenance"
                )
            })
            .unwrap_or("redacted");
        let now = now_ms();
        let mut line = json!({"timeMs": now, "level": record.level().as_str(), "event": event, "fields": fields}).to_string();
        line.push('\n');
        if self
            .rolling
            .lock()
            .map_err(|_| ())
            .and_then(|r| r.write(now / 86_400_000, line.as_bytes()).map_err(|_| ()))
            .is_err()
        {
            eprintln!("dex: diagnostic log write failed");
        }
    }
    fn flush(&self) {}
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Seven UTC days, four 2 MiB segments/day (56 MiB total). DEX_LOG=off/error/warn/info/debug/trace.
pub fn init(dir: &Path) -> Result<(), String> {
    crate::private_fs::log_dir(dir).map_err(|e| e.to_string())?;
    let level = std::env::var("DEX_LOG")
        .unwrap_or_else(|_| "info".into())
        .parse::<log::LevelFilter>()
        .map_err(|_| "DEX_LOG: off/error/warn/info/debug/trace bekleniyor")?;
    let rolling = Rolling {
        dir: dir.into(),
        limit: 2 * 1024 * 1024,
    };
    // Retention also runs when DEX_LOG=off and on the first start after a long absence.
    rolling
        .write(now_ms() / 86_400_000, b"")
        .map_err(|e| e.to_string())?;
    LOGGER
        .set(Logger {
            rolling: Mutex::new(rolling),
            level,
        })
        .map_err(|_| "log already initialized")?;
    log::set_logger(LOGGER.get().unwrap()).map_err(|e| e.to_string())?;
    log::set_max_level(level);
    std::panic::set_hook(Box::new(|info| {
        log::error!(target: "dex::audit", "{}", json!({"event": "panic", "fields": {"line": info.location().map(|l| l.line())}}));
        eprintln!("dex: panic (payload redacted; see diagnostic log)");
    }));
    event("startup", json!({}));
    Ok(())
}

pub fn event(name: &'static str, fields: Value) {
    log::info!(target: "dex::audit", "{}", json!({"event": name, "fields": fields}));
}

/// Metadata only: an opaque shell body is deliberately unknown, never guessed or logged.
pub(crate) fn permit_fields(agent: &str, decision: &str, rule: &str, final_rule: &str, tool: &str, input: &Value,
    ctx: Option<&crate::permit::Ctx>, id: Option<i64>) -> Value {
    let mut fields = json!({"agent":agent,"decision":decision,"rule":rule,"final_rule":final_rule,
        "path_class":permit_path_class(tool, input, ctx),"bytes":input.to_string().len()});
    if let Some(id) = id { fields["id"] = json!(id); }
    fields
}

pub(crate) fn denial_rule(reason: &str) -> &'static str {
    let reason = reason.strip_prefix("Dex politikası izin vermiyor: ").unwrap_or(reason);
    for (prefix, rule) in [
        ("çalışma dizini dışına yazma", "outside_write"),
        ("başka takımın klasörüne yazma", "other_team_write"),
        ("salt okuma", "read_only"), ("sır/kimlik dosyası", "secret"),
        ("ajan/git yapılandırması", "protected"), ("hook atlatma yasak", "hook_bypass"),
        ("git push", "git_push"), ("git reset --hard", "destructive_git"), ("git clean", "destructive_git"),
        ("Tüm e2e paketini koşma", "test_budget"), ("Tüm GUT paketini koşma", "test_budget"),
        ("Alt takım kabuk çalıştıramaz", "service_shell"), ("Alt takım yalnız", "service_write"),
    ] {
        if reason.starts_with(prefix) { return rule; }
    }
    if reason.starts_with("Dex politikası dışı") { "permit_required" } else { "policy_deny" }
}

/// Later Dex gates can veto a call that the permit policy deferred (service jobs, questions).
pub(crate) fn permit_hook_reply(agent: &str, body: &Value, reply: Option<&str>) {
    if let Some(fields) = permit_hook_reply_fields(agent, body, reply) { event("permit", fields); }
}

/// #1813: PreToolUse karar süresi (kilit beklemeleri dahil). Her çağrıda bir satır: p50/p99
/// buradan. Kişisel veri yok — yalnız ajan adı, süre ve izin sonucu (allow/deny; yanıtsız 204'te
/// karar alanı yazılmaz).
pub(crate) fn hook_timing(agent: &str, decision_ms: u64, outcome: Option<&str>) {
    let mut fields = json!({"hook": "PreToolUse", "agent": agent, "duration_ms": decision_ms});
    if let Some(o) = outcome.filter(|o| matches!(*o, "allow" | "deny")) {
        fields["decision"] = o.into();
    }
    event("hook_timing", fields);
}

fn permit_hook_reply_fields(agent: &str, body: &Value, reply: Option<&str>) -> Option<Value> {
    let reply: Value = serde_json::from_str(reply?).ok()?;
    let decision = &reply["hookSpecificOutput"];
    if decision["permissionDecision"] != "deny" { return None; }
    let reason = decision["permissionDecisionReason"].as_str().unwrap_or("");
    let tool = body["tool_name"].as_str().unwrap_or("");
    let rule = if tool == "AskUserQuestion" { "ask_user" } else { denial_rule(reason) };
    Some(permit_fields(agent, "deny", "deny", rule, tool, &body["tool_input"], None, None))
}

fn permit_path_class(tool: &str, input: &Value, ctx: Option<&crate::permit::Ctx>) -> &'static str {
    use std::path::Component;
    if matches!(tool, "Bash" | "PowerShell") {
        return "unknown"; // The authoritative shell parser is private to permit, and is unchanged.
    }
    let Some(path) = ["file_path", "notebook_path", "path"].iter().find_map(|k| input.get(k).and_then(Value::as_str)) else {
        return "no_path";
    };
    let Some(ctx) = ctx else { return "unknown" };
    if path.starts_with('~') || (cfg!(windows) && path.starts_with('/')) { return "unknown"; }
    let path = Path::new(path);
    let path = if path.is_absolute() { path.to_path_buf() } else {
        let Some(cwd) = &ctx.cwd else { return "unknown" };
        cwd.join(path)
    };
    let normalize = |p: &Path| {
        let mut normalized = PathBuf::new();
        for part in p.components() {
            match part { Component::ParentDir => { normalized.pop(); }, Component::CurDir => {}, _ => normalized.push(part.as_os_str()) }
        }
        let key = normalized.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_string();
        if cfg!(windows) { key.to_lowercase() } else { key }
    };
    let path = normalize(&path);
    let inside = |root: &Path| {
        let root = normalize(root);
        !root.is_empty() && (path == root || path.starts_with(&format!("{root}/")))
    };
    if let Some(project) = &ctx.team_root {
        if inside(&project.join(".dex/results")) { return "results"; }
        if inside(&project.join(".dex/tmp")) { return "tmp"; }
        // permit_ctx puts the job worktree first; no-worktree and service sessions put P/home first.
        if ctx.write.first().is_some_and(|w| normalize(w) != normalize(project) && inside(w)) { return "worktree"; }
        if inside(project) { return "project"; }
    }
    if ctx.write.iter().any(|p| inside(p)) { "authorized_root" } else { "outside" }
}

fn sanitize(fields: Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(fields) = fields.as_object() {
        for (key, value) in fields {
            let safe = match key.as_str() {
                "seq" | "pid" | "id" | "bytes" | "line" | "generation" | "deleted" | "skipped" => {
                    value.is_u64() || value.is_null()
                }
                "duration_ms" | "pending" => value.is_u64(),
                "agent" => value.as_str().is_some_and(|s| {
                    s.len() <= 80
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
                }),
                "status" | "before" | "after" => value.as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "offline"
                            | "idle"
                            | "working"
                            | "waiting"
                            | "queued"
                            | "delivered"
                            | "running"
                            | "done"
                            | "failed"
                            | "done_no_report"
                            | "open"
                            | "answered"
                            | "escalated"
                    )
                }),
                "decision" => value
                    .as_str()
                    .is_some_and(|s| matches!(s, "allow" | "deny" | "leader")),
                "rule" => value
                    .as_str()
                    .is_some_and(|s| matches!(s, "policy" | "leader_bypass" | "grant" | "deny")),
                "final_rule" => value.as_str().is_some_and(|s| matches!(s,
                    "policy_unavailable" | "outside_write" | "other_team_write" | "read_only" | "secret" | "protected"
                    | "hook_bypass" | "git_push" | "destructive_git" | "test_budget" | "policy_deny"
                    | "deferred" | "leader_bypass" | "permit_required" | "permit_grant" | "allow_policy"
                    | "service_shell" | "service_write" | "ask_user")),
                "path_class" => value.as_str().is_some_and(|s| matches!(s,
                    "project" | "worktree" | "results" | "tmp" | "outside" | "no_path" | "unknown" | "authorized_root")),
                "reason" => value.as_str().is_some_and(|s| {
                    matches!(s, "idle_close" | "reset" | "user" | "replacement" | "exit"
                        | "stop_prepare" | "stop_accept" | "stop_retry" | "stale_stop" | "turn_end"
                        | "db_error" | "hook_reply" | "write_error"
                        | "offline" | "chat_queue" | "busy" | "starting" | "draft" | "draining"
                        | "readiness" | "reserved" | "no_pending" | "cancelled" | "accept_wait"
                        | "paste" | "enter" | "paste_accept" | "prompt_mismatch" | "patron_input"
                        | "recent" | "active_job" | "active_session" | "unsafe_path" | "io_error")
                }),
                "hook" => value.as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "SessionStart"
                            | "SessionEnd"
                            | "UserPromptSubmit"
                            | "PreToolUse"
                            | "PostToolUse"
                            | "PostToolUseFailure"
                            | "PermissionRequest"
                            | "Notification"
                            | "Stop"
                            | "StopFailure"
                            | "SubagentStart"
                            | "SubagentStop"
                    )
                }),
                _ => false,
            };
            // Field names can themselves contain a pasted secret; only preserve known names.
            let key = if matches!(
                key.as_str(),
                "seq"
                    | "pid"
                    | "id"
                    | "bytes"
                    | "deleted"
                    | "skipped"
                    | "line"
                    | "generation"
                    | "duration_ms"
                    | "pending"
                    | "agent"
                    | "status"
                    | "before"
                    | "after"
                    | "decision"
                    | "rule"
                    | "final_rule"
                    | "path_class"
                    | "reason"
                    | "hook"
                    | "command"
                    | "prompt"
                    | "env"
            ) {
                key.as_str()
            } else {
                "redacted"
            };
            out.insert(
                key.to_owned(),
                if safe {
                    value.clone()
                } else {
                    json!("[REDACTED]")
                },
            );
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn report_content_permit_metadata_is_closed_and_redacted() {
        let safe = sanitize(json!({"final_rule":"deferred","path_class":"results","command":"secret command","path":"/private/person/file"}));
        assert_eq!(safe["final_rule"], "deferred");
        assert_eq!(safe["path_class"], "results");
        assert!(!safe.to_string().contains("secret command"));
        assert!(!safe.to_string().contains("/private/person"));
        let unsafe_fields = sanitize(json!({"final_rule":"deny /private/person","path_class":"/private/person"}));
        assert_eq!(unsafe_fields["final_rule"], "[REDACTED]");
        assert_eq!(unsafe_fields["path_class"], "[REDACTED]");
    }

    #[test]
    fn permit_metadata_classifies_literal_paths_and_never_copies_inputs() {
        let base = if cfg!(windows) { PathBuf::from(r"C:\private-person\project") } else { PathBuf::from("/private-person/project") };
        let worktree = base.join(".dex/wt/worker");
        let ctx = crate::permit::Ctx { team_root: Some(base.clone()), cwd: Some(worktree.clone()),
            write: vec![worktree.clone(), base.join(".dex/results"), base.join(".dex/tmp")], ..Default::default() };
        for (path, class) in [
            (base.join("source.rs"), "project"), (worktree.join("source.rs"), "worktree"),
            (base.join(".dex/results/1.md"), "results"), (base.join(".dex/tmp/log.txt"), "tmp"),
            (base.parent().unwrap().join("outside/file"), "outside"),
        ] {
            let fields = sanitize(permit_fields("worker", "deny", "deny", "outside_write", "Write", &json!({"file_path":path,"content":"secret body"}), Some(&ctx), None));
            assert_eq!(fields["path_class"], class);
            assert!(!fields.to_string().contains("private-person"));
            assert!(!fields.to_string().contains("secret body"));
        }
        let fields = sanitize(permit_fields("worker", "leader", "policy", "deferred", "Bash",
            &json!({"command":"secret body /private-person/path"}), Some(&ctx), None));
        assert_eq!(fields["final_rule"], "deferred");
        assert_eq!(fields["path_class"], "unknown");
        assert!(!fields.to_string().contains("secret body"));
        assert_eq!(permit_path_class("Skill", &json!({}), Some(&ctx)), "no_path");
        assert_eq!(denial_rule("Dex politikası izin vermiyor: çalışma dizini dışına yazma: /private-person"), "outside_write");
    }

    #[test]
    fn later_dex_gate_denial_is_distinct_from_a_deferred_permit() {
        let body = json!({"tool_name":"Bash","tool_input":{"command":"private command"}});
        let reply = json!({"hookSpecificOutput":{"permissionDecision":"deny",
            "permissionDecisionReason":"Alt takım kabuk çalıştıramaz; private path"}}).to_string();
        let fields = sanitize(permit_hook_reply_fields("worker", &body, Some(&reply)).unwrap());
        assert_eq!(fields["decision"], "deny");
        assert_eq!(fields["final_rule"], "service_shell");
        assert_eq!(fields["path_class"], "unknown");
        assert!(!fields.to_string().contains("private"));
        assert!(permit_hook_reply_fields("worker", &body, None).is_none());
    }

    #[test]
    fn teardown_fields_only_accept_numeric_metadata() {
        assert_eq!(super::sanitize(serde_json::json!({"duration_ms": 5000, "pending": 2})),
            serde_json::json!({"duration_ms": 5000, "pending": 2}));
        assert_eq!(super::sanitize(serde_json::json!({"duration_ms": "private", "pending": "private"})),
            serde_json::json!({"duration_ms": "[REDACTED]", "pending": "[REDACTED]"}));
    }
    #[test]
    fn teardown_event_names_survive_persistence() {
        use log::Log;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("teardown-log-{}-{}", std::process::id(), crate::agents::next_seq()));
        let logger = Logger { rolling: Mutex::new(Rolling { dir: dir.clone(), limit: 2048 }), level: log::LevelFilter::Info };
        for event in ["pty_teardown_timeout", "pty_teardown_complete"] {
            logger.log(&log::Record::builder().target("dex::audit").level(log::Level::Error)
                .args(format_args!("{}", json!({"event": event, "fields": {"agent": "kaan", "generation": 1, "duration_ms": 5000, "pending": 2}}))).build());
        }
        let content = std::fs::read_to_string(dir.join(format!("dex-{}-0.jsonl", now_ms() / 86_400_000))).unwrap();
        let records: Vec<Value> = content.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(records.len(), 2);
        for (record, event) in records.iter().zip(["pty_teardown_timeout", "pty_teardown_complete"]) {
            assert_eq!(record["event"], event);
            assert_eq!(record["fields"], json!({"agent": "kaan", "generation": 1, "duration_ms": 5000, "pending": 2}));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn existing_log_modes_are_repaired_before_append() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("private-log-{}-{}", std::process::id(), crate::agents::next_seq()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["dex-100-0.jsonl", "dexd-stderr.1.log"] {
            let path = dir.join(name);
            std::fs::write(&path, "before\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let rolling = Rolling { dir: dir.clone(), limit: 1024 };
        for _ in 0..2 { rolling.write(100, b"after\n").unwrap(); }
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        for name in ["dex-100-0.jsonl", "dexd-stderr.1.log"] {
            assert_eq!(std::fs::metadata(dir.join(name)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_to_string(dir.join("dex-100-0.jsonl")).unwrap(), "before\nafter\nafter\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn maintenance_counts_and_reasons_are_metadata_only() {
        let clean = super::sanitize(serde_json::json!({"deleted":3,"skipped":2,"bytes":123,"reason":"active_job","path":"private-folder"}));
        // Unknown field names retain the existing redaction marker, never the supplied path.
        assert_eq!(clean,serde_json::json!({"deleted":3,"skipped":2,"bytes":123,"reason":"active_job","redacted":"[REDACTED]"}));
    }
    use super::*;

    #[test]
    fn daily_rotation_has_seven_day_and_size_bounds() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "logs-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), "unrelated").unwrap();
        let log = Rolling {
            dir: dir.clone(),
            limit: 20,
        };
        for day in 10..=18 {
            for _ in 0..12 {
                log.write(day, b"123456789\n").unwrap();
            }
        }
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name() != "keep.txt")
            .collect();
        assert_eq!(files.len(), 7 * 4);
        for file in files {
            assert!(file.metadata().unwrap().len() <= 20);
            assert!(!file.file_name().to_string_lossy().starts_with("dex-10-"));
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "unrelated"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn secrets_are_redacted_before_serialization() {
        let clean = sanitize(
            json!({"seq": 42, "command": "TOKEN=sk-secret curl -H 'Authorization: Bearer abc'", "prompt": "private", "env": {"KEY": "secret"}}),
        );
        assert_eq!(clean["seq"], 42);
        assert_eq!(clean["command"], "[REDACTED]");
        let line = clean.to_string();
        for secret in ["sk-secret", "Bearer", "private", "KEY"] {
            assert!(!line.contains(secret));
        }
    }

    #[test]
    fn persisted_logs_mask_untrusted_messages_and_field_names() {
        use log::Log;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "redacted-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        let logger = Logger {
            rolling: Mutex::new(Rolling {
                dir: dir.clone(),
                limit: 2048,
            }),
            level: log::LevelFilter::Info,
        };
        logger.log(
            &log::Record::builder()
                .target("dependency")
                .level(log::Level::Error)
                .args(format_args!("Authorization: Bearer private-token"))
                .build(),
        );
        logger.log(&log::Record::builder().target("dex::audit").level(log::Level::Info).args(format_args!("{}", json!({"event": "permit", "fields": {"command": "KEY=private-token", "private-token": "value", "decision": "deny", "final_rule":"outside_write", "path_class":"outside", "path":"/private/person"}}))).build());
        logger.log(
            &log::Record::builder()
                .target("dependency")
                .level(log::Level::Debug)
                .args(format_args!("debug-secret"))
                .build(),
        );
        let content =
            std::fs::read_to_string(dir.join(format!("dex-{}-0.jsonl", now_ms() / 86_400_000)))
                .unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(!content.contains("private-token"), "{content}");
        assert!(!content.contains("Authorization"));
        assert!(!content.contains("/private/person"));
        assert!(content.contains("outside_write"));
        assert!(content.contains("\"path_class\":\"outside\""));
        assert!(content.contains("[REDACTED]"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
