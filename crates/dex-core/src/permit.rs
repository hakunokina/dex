//! Görev #23 — a worker's permission requests (PermissionRequest hook) answered by
//! policy instead of the patron: safe work inside the worker's folders is allowed, dangerous work
//! is denied with a reason, everything else goes to its leader (a denial naming a permit request
//! the leader may grant). Leaders are never asked here.
//!
//! Shell commands are judged as a whole chain: `&&`, `||`, `;`, `|`, `&`, newlines and `( )` split
//! it into segments, and every segment must pass. Command substitution (`$(…)`, backticks,
//! `<(…)`), variables (`$X`), loops, assignments and heredocs are never allowed automatically: what
//! they run or touch is not known before they run (#495: in no role). Paths are resolved lexically (no symlinks) against the cwd,
//! which a literal `cd` in the chain moves.

use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Görev #26: what PreToolUse does with an out-of-policy (leader) call. `false`: nothing — the
/// session's own permission mode decides, and a dialog it raises still comes back as a
/// PermissionRequest (numbered request for the leader). `true`: deny it right away with that
/// numbered request, in every mode.
pub const GRAY_AT_PRE_TOOL: bool = false;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Hard no: no permit opens it.
    Deny(String),
    /// Outside the policy: the leader decides (the text names what was asked).
    Leader(String),
}

/// What the policy knows about the worker.
#[derive(Default)]
pub struct Ctx {
    /// Personal assistant: web and scoped file reads only; no shell or mutation.
    pub assistant: bool,
    /// Trusted agent kind and team root, supplied by the database, not the tool input.
    pub leader: bool,
    pub team_root: Option<PathBuf>,
    /// Additional push roots supplied by trusted host metadata, never tool input.
    pub extra_push_roots: Vec<PathBuf>,
    /// Literal-URL push targets the patron allows (`settings` tablosundaki `push_allow` satırı:
    /// JSON string listesi, desenlerde yalnız `*` joker). Boş = eski davranış (tüm push yasak).
    /// Trusted host metadata (DB satırı), never tool input.
    pub push_allow: Vec<String>,
    /// Where it may write: its team folder (+ a service job's `docs/research`).
    pub write: Vec<PathBuf>,
    /// Where it may read: `write` + a service job's requester project.
    pub read: Vec<PathBuf>,
    /// Other teams' folders (never written to).
    pub others: Vec<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// QA may run the whole e2e suite (test budget).
    pub qa: bool,
    /// #288: a code-review worker only reads — whatever the policy would not plainly allow as a
    /// read is denied, never handed to the leader or the session.
    pub read_only: bool,
    /// #293: the reviewer's running job (DB): its `<team root>/.dex/results/<job>.md` is the one
    /// file it may write.
    pub job: Option<i64>,
    /// The reviewer's own Claude session output directory, from trusted session metadata.
    pub tool_results: Option<PathBuf>,
}

/// #288: why a code-review worker's call was denied.
const READ_ONLY: &str = "salt okuma: inceleyici dosya ya da repo değiştirmez";
const READ_ONLY_TESTS: &str = "salt okuma: inceleyici test koşmaz, worker raporundaki sonuçlara bak";
/// #471/#495: the parser simulates no variable, substitution, assignment or loop.
const READ_ONLY_DYNAMIC: &str = "salt okuma: değişken/ikame/döngü/atama yok — komutları literal argümanlarla ayrı yaz";

pub fn decide(tool: &str, input: &Value, ctx: &Ctx) -> Verdict {
    if ctx.assistant { return assistant_decide(tool, input, ctx); }
    let v = judge(tool, input, ctx);
    if !ctx.read_only {
        return v;
    }
    match tool {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" if !writes_own_report(tool, input, ctx) => {
            Verdict::Deny(format!("{READ_ONLY} ({tool})"))
        }
        // Session plumbing that touches no file: loading the dex tools, a skill, the todo list.
        "ToolSearch" | "Skill" | "TodoWrite" => Verdict::Allow,
        _ => {
            let command = shell_command(tool, input).map(|c| bash_text(&c, dialect(tool)));
            match (v, command.as_deref().map(|c| parsed(c, dialect(tool), ctx))) {
                (v @ Verdict::Deny(_), _) => v,
                // #402: a shell command passes only the read-only parser — `lex` hands what it does
                // not know to the leader (the reviewer has none) and reads `& (…)` as a plain group.
                (_, Some(Ok(()))) => Verdict::Allow,
                (_, Some(Err(Unparsed::Deny(why)))) => Verdict::Deny(format!("{why} — `{}`", short(command.as_deref().unwrap_or_default()))),
                (_, Some(Err(Unparsed::Dynamic))) => Verdict::Deny(format!("{READ_ONLY_DYNAMIC} — `{}`", short(command.as_deref().unwrap_or_default()))),
                (Verdict::Allow, None) => Verdict::Allow,
                (Verdict::Allow, Some(_)) => Verdict::Deny(format!("{READ_ONLY} — `{}`", short(command.as_deref().unwrap_or_default()))),
                (Verdict::Leader(what), _) => Verdict::Deny(format!("{READ_ONLY} — `{}`", if what.is_empty() { tool } else { &what })),
            }
        }
    }
}

fn assistant_decide(tool: &str, input: &Value, ctx: &Ctx) -> Verdict {
    match tool {
        "WebFetch" | "WebSearch" | "AskUserQuestion" | "ToolSearch" | "mcp__dex__memory_search" => Verdict::Allow,
        "Read" | "Glob" | "Grep" | "LS" => {
            // Glob's absolute/parent pattern can override its otherwise scoped search path.
            if tool == "Glob" && input.get("pattern").and_then(Value::as_str).is_some_and(|p| {
                Path::new(p).has_root() || expand_home(p).is_some()
                    || Path::new(p).components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
            }) {
                return Verdict::Deny("asistan Glob deseni izinli path altında göreli olmalı".into());
            }
            let raw = input.get(if tool == "Read" { "file_path" } else { "path" }).and_then(Value::as_str);
            if raw.is_some_and(|p| Path::new(p).components().any(|c| c == Component::ParentDir)) {
                return Verdict::Deny("asistan okuma yolunda üst dizin kullanamaz".into());
            }
            let path = raw.map(|p| resolve(ctx.cwd.as_deref(), p)).or_else(|| ctx.cwd.clone());
            let real = path.as_ref().and_then(|p| p.canonicalize().ok());
            // Judge the physical path and roots in the same spelling (Windows verbatim/UNC
            // prefixes and casing included). Do not run the lexical worker judge first.
            let physical = |p: &Path| PathBuf::from(crate::projects::path_key(&p.to_string_lossy()));
            // A replaced inbox/memory root must not turn an outside directory into an allowed
            // root. Trusted context paths already use their normalized absolute spelling.
            let roots = ctx.read.iter().filter_map(|p| {
                let real = p.canonicalize().ok()?;
                (physical(&real) == physical(p)).then(|| physical(&real))
            }).collect::<Vec<_>>();
            if path.is_some_and(|p| !secret(&p)) && real.is_some_and(|p| !secret(&p) && within(&physical(&p), &roots)) {
                Verdict::Allow
            } else { Verdict::Deny("asistan yalnız kendi takımının izinli dosyalarını okur".into()) }
        },
        _ => Verdict::Deny("asistan salt okur: yazma, kabuk/kod/betik, git ve diğer MCP araçları kapalı".into()),
    }
}

/// #293: every file the call writes is the reviewer's own report — no delete, no move.
fn writes_own_report(tool: &str, input: &Value, ctx: &Ctx) -> bool {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let files = match tool {
        "Write" | "Edit" | "MultiEdit" => s("file_path").into_iter().collect(),
        _ => Vec::new(),
    };
    // #298: no `..` — `resolve` drops it lexically, the OS walks it after a link (`link/../.dex`).
    !files.is_empty()
        && files.iter().all(|f| {
            !Path::new(f).components().any(|c| c == Component::ParentDir) && own_report(&resolve(ctx.cwd.as_deref(), f), ctx)
        })
}

/// `<team root>/.dex/results/<job>.md`, checked on the real path too: the path's own parent must
/// really be the team root's `.dex/results` (no link out), and the file itself is no link.
fn own_report(p: &Path, ctx: &Ctx) -> bool {
    let (Some(root), Some(job)) = (&ctx.team_root, ctx.job) else { return false };
    if key(p) != key(&root.join(".dex").join("results").join(format!("{job}.md"))) {
        return false;
    }
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let inside = matches!((real(root), p.parent().and_then(real)), (Some(r), Some(d)) if d == r.join(".dex").join("results"));
    inside
        && match std::fs::symlink_metadata(p) {
            Ok(m) => m.is_file(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
}

fn judge(tool: &str, input: &Value, ctx: &Ctx) -> Verdict {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    match tool {
        t if t.starts_with("mcp__dex__") => Verdict::Allow,
        "Read" | "NotebookRead" | "Glob" | "Grep" | "LS" => {
            let p = s("file_path").or_else(|| s("notebook_path")).or_else(|| s("path"));
            if tool == "Read" && ctx.read_only && p.is_some_and(|p| {
                !Path::new(p).components().any(|c| c == Component::ParentDir)
                    && session_result(&resolve(ctx.cwd.as_deref(), p), ctx)
            }) {
                return Verdict::Allow;
            }
            let base = p.map(|p| resolve(ctx.cwd.as_deref(), p)).or_else(|| ctx.cwd.clone());
            let mut v = read_path(base.clone(), ctx, tool);
            // #2361: a Glob `pattern` is a path too — `~/.ssh/*`, `C:/x/.aws/**` search elsewhere than
            // `path`, and `**/.ssh/*` only names a secret behind a wildcard.
            if tool == "Glob" {
                if let Some(pat) = s("pattern") {
                    let literal = pat.split(['*', '?', '[', '{']).next().unwrap_or("").trim_end_matches(['\\', '/']);
                    let mut judged = Vec::new();
                    if !literal.is_empty() {
                        judged.push(read_path(Some(resolve(base.as_deref(), literal)), ctx, tool));
                    }
                    match glob_probes(pat) {
                        Some(probes) if probes.iter().any(|p| secret(&resolve(base.as_deref(), p))) => {
                            judged.push(Verdict::Deny(format!("sır/kimlik dosyası: {pat}")));
                        }
                        Some(_) => {}
                        None => judged.push(Verdict::Leader(tool.into())),
                    }
                    v = worst(std::iter::once(v).chain(judged));
                }
            }
            v
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => match s("file_path").or_else(|| s("notebook_path")) {
            Some(p) => write_path(&resolve(ctx.cwd.as_deref(), p), ctx),
            None => Verdict::Leader(tool.into()),
        },
        "Bash" | "PowerShell" => match s("command") {
            Some(c) => shell(c, dialect(tool), ctx),
            None => Verdict::Leader(tool.into()),
        },
        _ => Verdict::Leader(tool.into()),
    }
}

/// #2384: the names a Glob pattern stands for, as probes for `secret()` — brace alternatives and
/// bracket members and `?` expanded (`{src,.ssh}`, `.ss[h]`, `id_rs?`), `*` as nothing and as a
/// letter (`**/.env*` → `.env`). `None`: too many to try, the leader decides.
fn glob_probes(pat: &str) -> Option<Vec<String>> {
    const MAX: usize = 256;
    let any = || ('a'..='z').chain('0'..='9').chain(['.', '_', '-']);
    let mut done = Vec::new();
    let mut todo = vec![pat.to_string()];
    while let Some(p) = todo.pop() {
        if done.len() + todo.len() > MAX {
            return None;
        }
        let group = |open: char, close: char| p.find(open).and_then(|o| p[o + 1..].find(close).map(|c| (o, o + 1 + c)));
        if let Some((o, c)) = group('{', '}') {
            todo.extend(p[o + 1..c].split(',').map(|alt| format!("{}{alt}{}", &p[..o], &p[c + 1..])));
            continue;
        }
        if let Some((o, c)) = group('[', ']') {
            let set: Vec<char> = p[o + 1..c].chars().collect();
            let mut members = Vec::new();
            if set.is_empty() || matches!(set[0], '!' | '^') {
                // A negated class matches nearly any name character.
                members.extend(any());
            } else {
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        members.extend(set[i]..=set[i + 2]);
                        i += 3;
                    } else {
                        members.push(set[i]);
                        i += 1;
                    }
                }
            }
            todo.extend(members.into_iter().map(|m| format!("{}{m}{}", &p[..o], &p[c + 1..])));
            continue;
        }
        if let Some(q) = p.find('?') {
            todo.extend(any().map(|m| format!("{}{m}{}", &p[..q], &p[q + 1..])));
            continue;
        }
        done.push(p.replace("**/", "").replace('*', ""));
        done.push(p.replace('*', "x"));
    }
    Some(done)
}

// ---- permit requests (in memory: a restart just makes the worker ask again) ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The next identical request of that worker goes through, once.
    Once,
    /// Identical requests of that worker go through until its session ends or the job it was
    /// granted during is over, whichever comes first.
    Session,
}

#[derive(Clone, Debug)]
pub struct Ask {
    pub id: i64,
    pub agent: String,
    /// `tool: exact request` — what a grant matches, byte for byte (whitespace collapsed).
    pub what: String,
}

struct Grant {
    id: i64,
    agent: String,
    what: String,
    scope: Scope,
    /// The worker's running job when granted: a session grant ends with it.
    job: Option<i64>,
}

#[derive(Default)]
struct State {
    last: i64,
    asks: Vec<Ask>,
    grants: Vec<Grant>,
}

#[derive(Default)]
pub struct Permits(std::sync::Mutex<State>);

impl Permits {
    /// The pending request #P for this exact ask (same one while it is pending).
    pub fn ask(&self, agent: &str, what: &str) -> i64 {
        let mut s = self.0.lock().unwrap();
        if let Some(a) = s.asks.iter().find(|a| a.agent == agent && a.what == what) {
            return a.id;
        }
        s.last += 1;
        let id = s.last;
        s.asks.push(Ask { id, agent: agent.into(), what: what.into() });
        id
    }

    pub fn get(&self, id: i64) -> Option<Ask> {
        self.0.lock().unwrap().asks.iter().find(|a| a.id == id).cloned()
    }

    /// The leader approved #P: the ask becomes a grant (`job`: the worker's running job now).
    pub fn grant(&self, id: i64, scope: Scope, job: Option<i64>) -> Option<Ask> {
        let mut s = self.0.lock().unwrap();
        let at = s.asks.iter().position(|a| a.id == id)?;
        let a = s.asks.remove(at);
        s.grants.push(Grant { id, agent: a.agent.clone(), what: a.what.clone(), scope, job });
        Some(a)
    }

    /// A granted identical request → its #P. `Once` grants are used up; a `Session` grant whose
    /// job is no longer the running one is dropped instead.
    pub fn take(&self, agent: &str, what: &str, running: Option<i64>) -> Option<i64> {
        let mut s = self.0.lock().unwrap();
        s.grants.retain(|g| !(g.agent == agent && g.scope == Scope::Session && g.job.is_some() && g.job != running));
        let at = s.grants.iter().position(|g| g.agent == agent && g.what == what)?;
        let id = s.grants[at].id;
        if s.grants[at].scope == Scope::Once {
            s.grants.remove(at);
        }
        Some(id)
    }

    /// The worker's session ended: its grants and asks go with it.
    pub fn forget(&self, agent: &str) {
        let mut s = self.0.lock().unwrap();
        s.asks.retain(|a| a.agent != agent);
        s.grants.retain(|g| g.agent != agent);
    }
}

/// For the log and the leader's note: values that look like credentials are masked.
pub fn redact(text: &str) -> String {
    let mut out = Vec::new();
    let mut header = false;
    for w in text.split_whitespace() {
        let lower = w.to_ascii_lowercase();
        let keyed = w.split_once('=').is_some_and(|(k, _)| {
            let k = k.to_ascii_lowercase();
            ["token", "key", "secret", "pass", "auth", "cred"].iter().any(|n| k.contains(n))
        });
        let alnum = w.chars().filter(char::is_ascii_alphanumeric).count();
        let opaque = alnum >= 32 && w.chars().any(|c| c.is_ascii_digit()) && !w.contains(['/', '\\']);
        let prefixed = ["sk-", "ghp_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "eyj"].iter().any(|p| lower.starts_with(p));
        if header || keyed || opaque || prefixed {
            out.push(match w.split_once('=') {
                Some((k, _)) if keyed => format!("{k}=[GİZLİ]"),
                _ => "[GİZLİ]".into(),
            });
        } else {
            out.push(w.to_string());
        }
        header = lower.contains("authorization") || lower == "bearer";
    }
    out.join(" ")
}

/// #35: the session's scratchpad (`…\claude\<project>\<session>\scratchpad`) from the hook's
/// `scratchpad_dir`. Only a dir named `scratchpad` with a session and a project above it counts —
/// never `%TEMP%` or the `claude` dir itself.
pub fn scratchpad(dir: &str) -> Option<PathBuf> {
    let p = resolve(None, dir);
    let named = p.file_name().is_some_and(|n| n == "scratchpad");
    let claude = p.parent().and_then(Path::parent).and_then(Path::parent).and_then(Path::file_name);
    (named && claude.is_some_and(|n| n == "claude") && p.is_absolute()).then_some(p)
}

// ---- paths ----

/// Git Bash (MSYS) spells Windows paths `/c/Users/x` (drive `c`) and `/tmp` (the user's temp dir).
/// Read as a plain Windows path, `/c/x` is rooted on the cwd's drive (`C:\c\x`). Other MSYS roots
/// (`/usr`, `/etc`, …) live under the Git install, which is unknown here: left as they are, they
/// resolve outside every worker folder.
fn msys(p: &str) -> Option<String> {
    let rest = p.strip_prefix('/')?;
    let (first, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let tail = tail.replace('/', "\\");
    let mut drive = first.chars();
    match (drive.next(), drive.next()) {
        (Some(d), None) if d.is_ascii_alphabetic() => Some(format!("{}:\\{tail}", d.to_ascii_uppercase())),
        _ if first == "tmp" => {
            let temp = std::env::var("TEMP").or_else(|_| std::env::var("TMP")).ok()?;
            Some(if tail.is_empty() { temp } else { format!("{}\\{tail}", temp.trim_end_matches('\\')) })
        }
        _ => None,
    }
}

/// #164 P4a: `~`, `~user`, `$HOME`, `${HOME}`, `$USERPROFILE`, `$env:USERPROFILE`, `%USERPROFILE%`
/// lead to the user's home, not to a folder of that name inside the cwd. With no home in the
/// environment they still lead outside every worker folder.
fn expand_home(p: &str) -> Option<String> {
    let home = || std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| "/dex-no-home".into());
    if let Some(rest) = p.strip_prefix('~') {
        let (user, tail) = rest.split_at(rest.find(['/', '\\']).unwrap_or(rest.len()));
        let base = if user.is_empty() {
            home()
        } else {
            let h = home();
            let parent = Path::new(&h).parent().map_or(h.clone(), |x| x.to_string_lossy().into_owned());
            format!("{parent}/{user}")
        };
        return Some(format!("{base}{tail}"));
    }
    let lower = p.to_ascii_lowercase();
    ["${home}", "${userprofile}", "$env:userprofile", "$env:home", "%userprofile%", "$userprofile", "$home"]
        .iter()
        .find(|v| lower.starts_with(*v) && !p[v.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
        .map(|v| format!("{}{}", home(), &p[v.len()..]))
}

fn resolve(cwd: Option<&Path>, p: &str) -> PathBuf {
    let home = expand_home(p);
    let p = home.as_deref().unwrap_or(p);
    let converted = if cfg!(windows) { msys(p) } else { None };
    let p = Path::new(converted.as_deref().unwrap_or(p));
    let joined = match cwd {
        Some(c) if !p.is_absolute() => c.join(p),
        _ => p.to_path_buf(),
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

fn within(p: &Path, roots: &[PathBuf]) -> bool {
    let p = key(p);
    roots.iter().map(|r| key(r)).any(|r| !r.is_empty() && (p == r || p.starts_with(&format!("{r}/"))))
}

/// Credentials and keys: no worker reads or writes them, whatever the folder. Judged on the path
/// as Windows reads it (same per-component spelling as `protected`) and, for an 8.3 short alias
/// (`~`), on the long path too — only a deny comes from that second look.
fn secret(p: &Path) -> bool {
    if secret_rules(p) {
        return true;
    }
    p.to_string_lossy().contains('~') && std::fs::canonicalize(p).is_ok_and(|real| secret_rules(&real))
}

fn secret_rules(p: &Path) -> bool {
    // Windows drops trailing dots/spaces and reads `name:stream` as `name`.
    let k = key(p);
    let parts: Vec<&str> = k.split('/').map(|c| c.split(':').next().unwrap_or("").trim_end_matches(['.', ' '])).collect();
    let name = parts.last().copied().unwrap_or("");
    let benign = [".example", ".sample", ".template", ".dist"].iter().any(|s| name.ends_with(s));
    let by_name = (name == ".env" || name.starts_with(".env."))
        || [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"].iter().any(|s| name.ends_with(s))
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || matches!(name, ".netrc" | "_netrc" | ".git-credentials" | ".npmrc" | ".pypirc" | ".credentials.json" | "credentials" | "credentials.json")
        || name.starts_with("dex-provider.")
        // #150: `dexd --local`'s endpoint token (crates/dexd/src/local.rs) — the core's own door.
        || name == "dexd.local.token";
    let by_dir = parts.iter().any(|c| matches!(*c, ".ssh" | ".aws" | ".gnupg" | ".azure" | ".kube" | "secrets"));
    (by_name && !benign) || by_dir
}

fn read_path(p: Option<PathBuf>, ctx: &Ctx, what: &str) -> Verdict {
    match p {
        Some(p) if secret(&p) => Verdict::Deny(format!("sır/kimlik dosyası: {}", p.display())),
        Some(p) if within(&p, &ctx.read) || (ctx.read_only && user_skill(&p)) => Verdict::Allow,
        Some(p) => Verdict::Leader(format!("{what} {}", p.display())),
        None => Verdict::Leader(what.into()),
    }
}

/// Only direct text outputs of this session; no symlink escape, directory or credential read.
fn session_result(p: &Path, ctx: &Ctx) -> bool {
    let Some(root) = &ctx.tool_results else { return false };
    if secret(p) || p.extension().is_none_or(|e| e != "txt") || p.parent().is_none_or(|d| key(d) != key(root)) {
        return false;
    }
    let Some(projects) = root.parent().and_then(Path::parent).and_then(Path::parent) else { return false };
    let (Ok(base), Ok(file), Ok(rel)) = (std::fs::canonicalize(projects), std::fs::canonicalize(p), p.strip_prefix(projects)) else { return false };
    file == base.join(rel) && file.is_file()
}

/// Validate the stored session transcript against Claude's project layout, never tool input.
pub fn claude_tool_results(transcript: &str, session: &str) -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    tool_results_root(Path::new(transcript), session, &PathBuf::from(home).join(".claude/projects"))
}

fn tool_results_root(transcript: &Path, session: &str, projects: &Path) -> Option<PathBuf> {
    if session.is_empty() || session.contains(['/', '\\', ':']) || matches!(session, "." | "..")
        || transcript.components().any(|c| c == Component::ParentDir)
        || transcript.file_name()? != std::ffi::OsStr::new(&format!("{session}.jsonl")) {
        return None;
    }
    let project = transcript.parent()?;
    let parent = project.parent()?;
    // #2019: an extra Claude account's `projects` is a link to the main one — the same folder.
    let same = key(parent) == key(projects)
        || std::fs::canonicalize(parent).ok().zip(std::fs::canonicalize(projects).ok()).is_some_and(|(a, b)| a == b);
    (project.is_absolute() && same).then(|| project.join(session).join("tool-results"))
}

/// #402: the user's own codex/claude skills (`~/.codex/skills`, `~/.claude/skills`) — a reviewer
/// reads them; nothing else in those folders.
fn user_skill(p: &Path) -> bool {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else { return false };
    let home = PathBuf::from(home);
    within(p, &[home.join(".codex").join("skills"), home.join(".claude").join("skills")])
}

fn write_path(p: &Path, ctx: &Ctx) -> Verdict {
    // Grafts rewrite history even if the worker was given .git/info as a write root.
    let parts: Vec<String> = key(p).split('/')
        .map(|c| c.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']).to_ascii_lowercase())
        .collect();
    if !ctx.leader && parts.windows(3).any(|p| p == [".git", "info", "grafts"]) {
        return Verdict::Deny("hook atlatma yasak: .git/info/grafts".into());
    }
    if !ctx.leader && parts.windows(3).any(|p| p == [".git", "refs", "replace"]) {
        return Verdict::Deny("hook atlatma yasak: .git/refs/replace".into());
    }
    if secret(p) {
        Verdict::Deny(format!("sır/kimlik dosyası: {}", p.display()))
    } else if within(p, &ctx.others) && !within(p, &ctx.write) {
        Verdict::Deny(format!("başka takımın klasörüne yazma: {}", p.display()))
    } else if !within(p, &ctx.write) {
        Verdict::Deny(format!("çalışma dizini dışına yazma: {}", p.display()))
    } else if protected(p, &ctx.write) {
        Verdict::Deny(format!("ajan/git yapılandırması (korumalı): {}", p.display()))
    } else {
        Verdict::Allow
    }
}

/// #164 P2: hooks, settings, MCP servers, instructions, git hooks/config. Written by an agent they
/// switch the policy off or plant code/instructions for the next session of any agent. Judged
/// below the write root it is in (a root may itself sit under such a folder, e.g. a worktree).
fn protected(p: &Path, roots: &[PathBuf]) -> bool {
    let k = key(p);
    // Core merge policy/source must remain protected even under a broad write root,
    // or when a job's write root is the reserved scratch directory itself.
    if k.split('/').any(|c| c.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']).eq_ignore_ascii_case(crate::merge_gate::SCRATCH_DIR)) {
        return true;
    }
    let Some(rel) = roots.iter().map(|r| key(r)).filter(|r| !r.is_empty()).find_map(|r| {
        if k == r { Some(String::new()) } else { k.strip_prefix(&format!("{r}/")).map(String::from) }
    }) else {
        return false;
    };
    // Windows drops trailing dots/spaces and reads `name:stream` as `name`.
    let parts: Vec<String> = rel
        .split('/')
        .map(|c| c.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']).to_ascii_lowercase())
        .collect();
    let name = parts.last().map(String::as_str).unwrap_or("");
    let parent = parts.len().checked_sub(2).map(|i| parts[i].as_str());
    parts.iter().any(|c| matches!(c.as_str(), ".claude" | ".git" | ".codex"))
        || matches!(name, ".mcp.json" | "claude.md" | "claude.local.md" | "agents.md")
        || (name == "tasks.json" && parent == Some(".vscode"))
}

/// Deny beats leader beats allow.
fn worst(vs: impl IntoIterator<Item = Verdict>) -> Verdict {
    let mut out = Verdict::Allow;
    for v in vs {
        match (&out, v) {
            (Verdict::Deny(_), _) => {}
            (_, v @ Verdict::Deny(_)) => out = v,
            (Verdict::Allow, v @ Verdict::Leader(_)) => out = v,
            _ => {}
        }
    }
    out
}

// ---- shell ----

#[derive(Default, Debug)]
struct Seg {
    words: Vec<String>,
    /// (`>`/`>>`/`<`, target)
    redirs: Vec<(String, String)>,
}

struct Lexed {
    segs: Vec<Seg>,
    /// `$(…)`, backticks, `<(…)`, `$VAR`, heredoc: not judged statically.
    dynamic: bool,
    /// Shell sözdizimi kullanıldı: ayraç (`;`, `&`, `|`, `(`, `)`) ya da `segs`'e yansımayan
    /// atık sözdizimi (fd çoğaltma `2>&1` gibi). Sonda tek `;`/`&` boş segment bırakmaz;
    /// `allowed_push`'un bütün-komut şartı bu bayrağa bakar.
    syntax: bool,
}

pub(crate) fn command_words(command: &str, d: Option<Dialect>) -> Vec<Vec<String>> {
    let Some(d) = d else { return Vec::new() };
    let mut rd = Rd { c: bash_text(command, Some(d)).chars().collect(), i: 0, d, depth: 0, dynamic: false, unknown: false, hidden: Vec::new(), heredocs: Vec::new() };
    rd.script(None).unwrap_or_default().split(|t| matches!(t, Tok::Op)).map(|toks| {
        toks.iter().filter_map(|t| match t {
            Tok::Word(w, _) => Some(w.clone()),
            _ => None,
        }).collect()
    }).collect()
}

fn lex(command: &str, d: Option<Dialect>) -> Lexed {
    let mut segs = vec![Seg::default()];
    let mut dynamic = false;
    let mut syntax = false;
    let mut word = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut redir: Option<String> = None;
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut chars = command.chars().peekable();
    fn flush(segs: &mut [Seg], word: &mut String, has: &mut bool, redir: &mut Option<String>, syntax: &mut bool) {
        if *has {
            let w = std::mem::take(word);
            let seg = segs.last_mut().unwrap();
            match redir.take() {
                // fd çoğaltma (`2>&1`): dosyaya değil tanıta yazılır, `segs`'e düşmez —
                // atıldı diye görünmez olmasın, sözdizimi sayılsın.
                Some(op) if op.ends_with('&') && w.chars().all(|c| c.is_ascii_digit() || c == '-') => {
                    *syntax = true;
                }
                Some(op) => seg.redirs.push((op.trim_end_matches('&').to_string(), w)),
                None => seg.words.push(w),
            }
            *has = false;
        }
    }
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') => quote = None,
            (Some('\''), c) => word.push(c),
            (Some('"'), '\\') if d == Some(Dialect::Bash) && matches!(chars.peek(), Some('$' | '`' | '"' | '\\')) => {
                word.push(chars.next().unwrap());
            }
            (Some('"'), '"') => quote = None,
            (Some('"'), '$' | '`') => {
                dynamic = true;
                word.push(c);
            }
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, '$' | '`') => {
                dynamic = true;
                word.push(c);
                has = true;
            }
            // #517: no `\` + newline is left here in bash (`join_lines`); PowerShell continues none.
            (None, '\\') => match chars.peek() {
                Some(&n) if n != '\n' && (n.is_whitespace() || "\"'\\&|;()$`<>".contains(n)) => {
                    word.push(n);
                    chars.next();
                    has = true;
                }
                _ => {
                    word.push('\\');
                    has = true;
                }
            },
            (None, '>' | '<') => {
                // `2>` / `1>`: the digits are the fd, not a word.
                let fd = has && !word.is_empty() && word.chars().all(|d| d.is_ascii_digit());
                if fd {
                    word.clear();
                    has = false;
                } else {
                    flush(&mut segs, &mut word, &mut has, &mut redir, &mut syntax);
                }
                if c == '<' && matches!(chars.peek(), Some('<' | '(')) {
                    dynamic = true; // heredoc / process substitution
                }
                if c == '<' && chars.peek() == Some(&'<') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    if ahead.peek() != Some(&'<') {
                        // Heredoc: the body is text for the command, not shell; skip it at the next newline.
                        chars.next();
                        let strip_tabs = chars.peek() == Some(&'-');
                        if strip_tabs {
                            chars.next();
                        }
                        while chars.peek().is_some_and(|w| *w == ' ' || *w == '\t') {
                            chars.next();
                        }
                        match heredoc_word(&mut chars) {
                            Some((delim, quoted, _, _)) if quoted || !delim.is_empty() => heredocs.push((delim, strip_tabs)),
                            _ => dynamic = true, // `<<` with no word: not what it seems
                        }
                        continue;
                    }
                }
                if c == '>' && chars.peek() == Some(&'(') {
                    dynamic = true;
                }
                let mut op = c.to_string();
                while let Some(&n) = chars.peek() {
                    if n == '>' || n == '|' {
                        op.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                // `>&2` / `2>&1` / `>&-`: fd duplication, no file — but `>&word` writes the file
                // `word` (#164 P4b); `flush` tells them apart by the word.
                let dup = chars.peek() == Some(&'&');
                if dup {
                    chars.next();
                }
                redir = Some(match (c, dup) {
                    ('<', _) if op.contains('>') => ">".into(), // `<>`: opened read-write (#164 P4c)
                    ('<', true) => "<&".into(),
                    ('<', false) => "<".into(),
                    (_, true) => ">&".into(),
                    _ if op.contains(">>") => ">>".into(),
                    _ => ">".into(),
                });
            }
            (None, '&') if chars.peek() == Some(&'>') => {
                // `&>file`: stdout + stderr to a file.
                flush(&mut segs, &mut word, &mut has, &mut redir, &mut syntax);
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                redir = Some(">".into());
            }
            (None, c) if c.is_whitespace() && c != '\n' => flush(&mut segs, &mut word, &mut has, &mut redir, &mut syntax),
            (None, '&' | '|' | ';' | '\n' | '(' | ')') => {
                flush(&mut segs, &mut word, &mut has, &mut redir, &mut syntax);
                // Sonda tek `;`/`&` boş segment bırakmaz (`retain` siler); yine de sözdizimidir.
                // `\n` hariç: heredoc gövdesi zaten newline ile ayrılır, tek başına zincir değildir.
                if c != '\n' {
                    syntax = true;
                }
                redir = None;
                let last = segs.last().unwrap();
                if !last.words.is_empty() || !last.redirs.is_empty() {
                    segs.push(Seg::default());
                }
                if c == '\n' {
                    for (delim, strip_tabs) in heredocs.drain(..) {
                        if !skip_heredoc_body(&mut chars, &delim, strip_tabs) {
                            dynamic = true; // never closed
                        }
                    }
                }
            }
            (None, c) => {
                word.push(c);
                has = true;
            }
        }
    }
    flush(&mut segs, &mut word, &mut has, &mut redir, &mut syntax);
    if quote.is_some() || redir.is_some() || !heredocs.is_empty() {
        dynamic = true; // unbalanced (or a heredoc with no body): not what it seems
    }
    segs.retain(|s| !s.words.is_empty() || !s.redirs.is_empty());
    Lexed { segs, dynamic, syntax }
}

/// Consumes heredoc body lines up to and including the closing `delim` line; false if it never closes.
fn skip_heredoc_body(chars: &mut std::iter::Peekable<std::str::Chars>, delim: &str, strip_tabs: bool) -> bool {
    loop {
        let (line, n) = body_line(chars);
        if n == 0 {
            return false;
        }
        if (if strip_tabs { line.trim_start_matches('\t') } else { &line }) == delim {
            return true;
        }
    }
}

/// #508: a here-document's delimiter word, as bash reads it: (delimiter, quoted, ambiguous, chars
/// read). Ambiguous (quoted and bare parts mixed): its body is judged as if unquoted. None: an
/// unclosed quote. #517: line continuations are already gone (`join_lines`).
fn heredoc_word<I: Iterator<Item = char>>(it: &mut std::iter::Peekable<I>) -> Option<(String, bool, bool, usize)> {
    let (mut delim, mut n) = (String::new(), 0);
    let (mut quoted, mut quotes, mut bare) = (false, false, false);
    while let Some(c) = it.next_if(|c| !c.is_whitespace() && !"&|;()<>".contains(*c)) {
        n += 1;
        match c {
            '\\' => match it.next() {
                Some(e) => {
                    n += 1;
                    quoted = true;
                    delim.push(e);
                }
                None => delim.push('\\'),
            },
            '\'' | '"' => {
                (quoted, quotes) = (true, true);
                loop {
                    let q = it.next()?;
                    n += 1;
                    match q {
                        _ if q == c => break,
                        '\\' if c == '"' => {
                            let e = it.next()?;
                            n += 1;
                            if !"$`\"\\".contains(e) {
                                delim.push('\\');
                            }
                            delim.push(e);
                        }
                        q => delim.push(q),
                    }
                }
            }
            c => {
                bare = true;
                delim.push(c);
            }
        }
    }
    Some((delim, quoted, quotes && bare, n))
}

/// #508: one here-document body line and the chars it took. #517: an unquoted delimiter's body
/// lines are already joined (`join_lines`).
fn body_line(it: &mut impl Iterator<Item = char>) -> (String, usize) {
    let (mut line, mut n) = (String::new(), 0);
    for c in it {
        n += 1;
        if c == '\n' {
            break;
        }
        line.push(c);
    }
    if line.ends_with('\r') {
        line.pop();
    }
    (line, n)
}

/// #517: bash removes every `\` + newline (a line continuation) before it reads anything — but in
/// single quotes (`$'…'` too), a comment or a quoted here-document's body; an escaped `\` (`\\`)
/// continues nothing; in backticks even single quotes do not keep one. Done once, before any
/// reader (allow or deny) sees the command: `$\` + newline + `(rm -rf src)` is `$(rm -rf src)`.
fn join_lines(command: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Cx {
        Plain,
        /// `$( )`, with its open plain parentheses.
        Sub(usize),
        Double,
        Tick,
    }
    let c: Vec<char> = command.chars().collect();
    let mut out = String::with_capacity(command.len());
    let mut stack = vec![Cx::Plain];
    // The here-documents of the line: where the delimiter word starts in `out`, `<<-`.
    let mut heredocs: Vec<(usize, bool)> = Vec::new();
    // `out`'s length after the last escaped character: a `$` there opens nothing.
    let mut escaped = 0;
    let mut i = 0;
    while let Some(&ch) = c.get(i) {
        let cx = *stack.last().unwrap_or(&Cx::Plain);
        let plain = matches!(cx, Cx::Plain | Cx::Sub(_));
        if ch == '\\' {
            match c.get(i + 1) {
                Some('\n') => {}
                Some(&n) => {
                    out.push(ch);
                    out.push(n);
                    escaped = out.len();
                }
                None => out.push(ch),
            }
            i += 2;
            continue;
        }
        let dollar = out.ends_with('$') && escaped != out.len();
        out.push(ch);
        i += 1;
        match (ch, cx) {
            // Single quotes, `$'…'` (its `\'` does not close it): copied as written.
            ('\'', _) if plain => {
                while let Some(&q) = c.get(i) {
                    out.push(q);
                    i += 1;
                    if q == '\'' {
                        break;
                    }
                    if dollar && q == '\\' {
                        if let Some(&e) = c.get(i) {
                            out.push(e);
                            i += 1;
                        }
                    }
                }
            }
            // A comment ends at the newline.
            ('#', _) if plain && out[..out.len() - 1].chars().next_back().map_or(true, |p| p.is_whitespace() || ";&|()<>`".contains(p)) => {
                while let Some(&q) = c.get(i).filter(|q| **q != '\n') {
                    out.push(q);
                    i += 1;
                }
            }
            ('"', _) if plain => stack.push(Cx::Double),
            ('"', Cx::Double) | ('`', Cx::Tick) => _ = stack.pop(),
            ('`', _) => stack.push(Cx::Tick),
            ('(', Cx::Plain | Cx::Double | Cx::Sub(_)) if dollar => stack.push(Cx::Sub(0)),
            ('(', Cx::Sub(n)) => *stack.last_mut().unwrap() = Cx::Sub(n + 1),
            (')', Cx::Sub(0)) => _ = stack.pop(),
            (')', Cx::Sub(n)) => *stack.last_mut().unwrap() = Cx::Sub(n - 1),
            ('<', _) if plain && c.get(i) == Some(&'<') => {
                out.push('<');
                i += 1;
                if c.get(i) == Some(&'<') {
                    out.push('<');
                    i += 1;
                    continue;
                }
                let strip_tabs = c.get(i) == Some(&'-');
                if strip_tabs {
                    out.push('-');
                    i += 1;
                }
                while let Some(&q) = c.get(i).filter(|q| matches!(q, ' ' | '\t')) {
                    out.push(q);
                    i += 1;
                }
                heredocs.push((out.len(), strip_tabs));
            }
            ('\n', _) if plain => {
                for (at, strip_tabs) in std::mem::take(&mut heredocs) {
                    let Some((delim, quoted, _, _)) = heredoc_word(&mut out[at..].chars().peekable()) else { continue };
                    if !quoted && delim.is_empty() {
                        continue;
                    }
                    // The body, line by line: an unquoted one joins its continuations.
                    while i < c.len() {
                        let start = out.len();
                        while let Some(&q) = c.get(i) {
                            i += 1;
                            if q == '\\' && !quoted && c.get(i) == Some(&'\n') {
                                i += 1;
                                continue;
                            }
                            out.push(q);
                            if q == '\n' {
                                break;
                            }
                            if q == '\\' && !quoted {
                                if let Some(&e) = c.get(i) {
                                    out.push(e);
                                    i += 1;
                                }
                            }
                        }
                        let line = out[start..].strip_suffix('\n').unwrap_or(&out[start..]);
                        let line = line.strip_suffix('\r').unwrap_or(line);
                        if (if strip_tabs { line.trim_start_matches('\t') } else { line }) == delim {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn name(w: &str) -> String {
    let base = w.rsplit(['/', '\\']).next().unwrap_or(w).to_ascii_lowercase();
    [".exe", ".cmd", ".bat", ".ps1"].iter().fold(base, |b, s| b.strip_suffix(s).map(String::from).unwrap_or(b))
}

fn strip_env(words: &[String]) -> &[String] {
    let n = words
        .iter()
        .take_while(|w| w.as_str() == "env" || w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')))
        .count();
    &words[n..]
}

fn looks_like_path(w: &str) -> bool {
    w.contains('/') || w.contains('\\') || w.starts_with('.') || w.starts_with('~') || Path::new(w).is_absolute()
}

const NULL_DEVICES: &[&str] = &["/dev/null", "nul", "$null", "/dev/stdout", "/dev/stderr"];

/// #517: the command as bash reads it (`join_lines`) — for every reader, allow and deny alike.
/// PowerShell continues no line with `\`; an unknown shell is read as bash.
fn bash_text(command: &str, d: Option<Dialect>) -> String {
    if d == Some(Dialect::Pwsh) { command.to_string() } else { join_lines(command) }
}

fn shell(command: &str, d: Option<Dialect>, ctx: &Ctx) -> Verdict {
    let joined = bash_text(command, d);
    let command = joined.as_str();
    let lexed = lex(command, d);
    if leader_server_push(command, ctx) {
        return Verdict::Allow;
    }
    if !ctx.read_only && allowed_push(command, &lexed, ctx) {
        return Verdict::Allow;
    }
    let short = short(command);
    // Hard noes first: whatever else the chain does.
    let mut cwd = ctx.cwd.clone();
    let mut verdicts = Vec::new();
    if !ctx.leader && lexed.segs.iter().any(|seg| {
        let wrapper = strip_env(&seg.words).first().is_some_and(|w| matches!(name(w).as_str(), "bash" | "sh" | "pwsh" | "powershell" | "eval"));
        wrapper && seg.words.iter().any(|body| lex(body, d).segs.iter().any(|inner| {
            matches!(segment(inner, &mut ctx.cwd.clone(), ctx), Verdict::Deny(why) if why.starts_with("hook atlatma yasak"))
        }))
    }) {
        verdicts.push(Verdict::Deny("hook atlatma yasak: shell sarmalı".into()));
    }
    // Narrow wrapper guard: these bodies are not ordinary outer command segments.
    let push_body = |text: &str| lex(text, d).segs.iter().any(|body| {
        let words = strip_env(&body.words);
        words.first().is_some_and(|w| name(w) == "git")
            && matches!(git(&words[1..], &mut ctx.cwd.clone(), ctx), Verdict::Deny(ref why) if why.starts_with("git push"))
    });
    let wrapped_push = lexed.segs.iter().any(|seg| {
        let wrapper = strip_env(&seg.words).first().is_some_and(|w| matches!(name(w).as_str(), "bash" | "sh" | "pwsh" | "powershell" | "eval"));
        let substitution = lexed.dynamic && seg.words.iter().any(|w| w.contains('`') || w.contains("$("));
        let body_push = wrapper && seg.words.iter().any(|word| push_body(word));
        body_push || ((wrapper || substitution) && seg.words.join(" ").split_whitespace().collect::<Vec<_>>().join(" ").contains("git push"))
    });
    if wrapped_push || (lexed.dynamic && command.split('`').skip(1).step_by(2).any(push_body)) {
        verdicts.push(Verdict::Deny("git push (patronun işi)".into()));
    }
    // #2286: hard noes survive any wrapper. A segment the policy hands to the leader runs an
    // unknown program: every argument of it is a possible body (`cmd /c "…"`, `xargs …`,
    // `node -e '…'`, `timeout …`, a git config alias) and is judged only for a deny — the
    // allow and gray path of the outer command is unchanged.
    if let Some(why) = bodies_denial(&lexed, d, ctx) {
        verdicts.push(Verdict::Deny(why));
    }
    for seg in &lexed.segs {
        verdicts.push(segment(seg, &mut cwd, ctx));
    }
    // #1876: a leader runs no e2e test at all; nobody but QA the whole suite (test_budget::on_pre_tool).
    let budget = crate::test_budget::leader_e2e_denial(command, ctx.cwd.as_deref(), d)
        .filter(|_| ctx.leader)
        .or_else(|| crate::test_budget::full_suite_denial_in_shell(command, ctx.cwd.as_deref(), d).filter(|_| !ctx.qa));
    if let Some(reason) = budget {
        verdicts.push(Verdict::Deny(reason.into()));
    }
    // #495: a worker's allow needs `parsed` below, which reads no `$` (but PowerShell's `>$null`),
    // backtick, substitution or here-document — what `lex` calls dynamic is its to judge.
    if lexed.dynamic && (ctx.leader || ctx.read_only) {
        verdicts.push(Verdict::Leader(String::new()));
    }
    match worst(verdicts) {
        Verdict::Deny(why) => Verdict::Deny(format!("{why} — `{short}`")),
        // #459: `lex` reads `& (…)` as a plain group and misses Unicode quotes: a worker's allow
        // also needs the real parser (the reviewer's is in `decide`), what it does not know goes
        // to the leader. A leader's allow and Leader answers are the session's anyway (lib.rs).
        // #480: what the parser finds denied stays denied (a hidden push or bulk delete), also
        // behind what `lex` already hands to the leader.
        v if !ctx.leader && !ctx.read_only => match (v, parsed(command, d, ctx)) {
            (_, Err(Unparsed::Deny(why))) => Verdict::Deny(format!("{why} — `{short}`")),
            (Verdict::Allow, Ok(())) => Verdict::Allow,
            _ => Verdict::Leader(short),
        },
        Verdict::Leader(_) => Verdict::Leader(short),
        Verdict::Allow => Verdict::Allow,
    }
}

/// Patron-approved exception (#222): a push whose target is a literal URL matching the
/// `push_allow` patterns (`settings` tablosu, boş = kapalı). Worker dahil herkes kullanabilir —
/// hedef URL olduğu için git config'teki bir remote adının arkasına saklanamaz. Bütün komut tek
/// push olmalı (zincir/sarmal/ikame yok: `lexed.dynamic`, `lexed.syntax` ve tek segment şartı);
/// globalde yalnız tek `-C` var (#225). Redler mevcut `git push` deny'ine düşer, sarmal guard
/// (`wrapped_push`) aynen çalışır.
fn allowed_push(command: &str, lexed: &Lexed, ctx: &Ctx) -> bool {
    // `syntax`: sonda `;`/`&` ve `2>&1` gibi `segs`'e yansımayan sözdizimi de red.
    if ctx.push_allow.is_empty() || lexed.dynamic || lexed.syntax || lexed.segs.len() != 1 {
        return false;
    }
    // Lexer `( )`'yi ayraç sayar: `(git push …)` tek segmente iner. Ham komut `git` ile
    // başlamıyorsa (subshell, yol, öneki…) shell sözdizimi var demektir.
    if !command.trim_start().starts_with("git") {
        return false;
    }
    let seg = &lexed.segs[0];
    if !seg.redirs.is_empty() {
        return false;
    }
    // #164 P4e: `FOO=1 git push …` allowlisted komut değildir.
    let words = strip_env(&seg.words);
    if words.len() != seg.words.len() {
        return false;
    }
    let Some(first) = words.first() else { return false };
    if first != "git" {
        return false;
    }
    let args = &words[1..];
    // Mevcut allowlisted globaller: yalnız tek `-C` (yazı kökü içinde) ve sayfalama/kilit bayrakları.
    // `-c`/`--config-env`/`--git-dir`/`--work-tree` ve diğerleri → red (ne çalışacağı bilinmez).
    // Ardışık `-C` red: Git görelileri bir öncekine göre çözer (#225 orta); tek `-C` zaten
    // `ctx.cwd`'den çözüldüğü için sıralı çözümle aynıdır. `-C ""` dizini korur (`resolve` no-op).
    let mut i = 0;
    let mut at = ctx.cwd.clone();
    let mut seen_c = false;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-C" => {
                if seen_c {
                    return false;
                }
                seen_c = true;
                at = args.get(i + 1).map(|d| resolve(at.as_deref(), d));
                i += 2;
            }
            "--no-pager" | "-P" | "--no-optional-locks" | "--paginate" | "-p" => i += 1,
            _ => return false,
        }
    }
    if !at.as_deref().is_some_and(|p| within(p, &ctx.write)) {
        return false;
    }
    let Some(sub) = args.get(i).map(String::as_str) else { return false };
    if sub != "push" {
        return false;
    }
    // Yalnız `--tags` bayrağına izin var (etiket iter, dal geçmişini ezmez; force zaten kapalı).
    // Reddedilenler ve gerekçeleri: `--mirror`/`--all` (tüm ref'leri iter, silmeleri de iter),
    // `--delete` (silme), `-f`/`--force*` (ezme), `--receive-pack`/`--exec` (uzak komut),
    // `-o`/`--push-option` (sunucu-tarafı bilinmez), `--repo` (hedefi bayrağa saklar),
    // `--recurse-submodules`/`--dry-run`/`-u` dahil geri kalan her bayrak (en küçük izin yüzeyi).
    let mut tags = false;
    let mut pos = Vec::new();
    for a in &args[i + 1..] {
        if a == "--tags" {
            if tags {
                return false;
            }
            tags = true;
        } else if a.starts_with('-') {
            return false;
        } else {
            pos.push(a.as_str());
        }
    }
    let [url, refspec] = pos.as_slice() else { return false };
    if !is_literal_push_url(url) || !is_safe_push_refspec(refspec) {
        return false;
    }
    ctx.push_allow.iter().any(|p| push_glob_match(p, url))
}

/// Literal URL mi: remote adı (`yayin`, `origin`, `server`) ve yerel yol değil; karakter
/// allowlist'i dışında (`..`, `$`, backtick, boşluk, joker, URL-kodlama…) hiçbir şey yok.
fn is_literal_push_url(url: &str) -> bool {
    if !remote(url) || url.contains("..") {
        return false;
    }
    url.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | ':' | '/' | '.' | '-' | '_'))
}

/// Tek refspec, ezme yok: `+` öneki yasak (force kapalı), `src:dst` yalnız `X:X` aynı-ad olabilir
/// (başka dala yazma yok), glob/`..` yok. Boş refspec yasak (ne iteceği `push.default`'a kalır).
fn is_safe_push_refspec(r: &str) -> bool {
    if r.is_empty() || r.starts_with('+') || r.contains("..") {
        return false;
    }
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'));
    match r.split_once(':') {
        Some((src, dst)) => src == dst && ok(src),
        None => ok(r),
    }
}

/// Desen eşleşmesi: desende yalnız `*` jokerdir ve `/` geçmez (alt dizine taşamaz); desenin ve
/// URL'nin geri kalanı birebir literaldir. Geçersiz desen eşleşmez (fail-closed).
fn push_glob_match(pattern: &str, url: &str) -> bool {
    if pattern.is_empty() || pattern.contains("..") {
        return false;
    }
    if pattern.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '@' | ':' | '/' | '.' | '-' | '_' | '*'))) {
        return false;
    }
    fn go(p: &[u8], u: &[u8]) -> bool {
        if p.is_empty() {
            return u.is_empty();
        }
        if p[0] == b'*' {
            let mut i = 0;
            loop {
                if go(&p[1..], &u[i..]) {
                    return true;
                }
                if i >= u.len() || u[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        if u.is_empty() || p[0] != u[0] {
            return false;
        }
        go(&p[1..], &u[1..])
    }
    go(pattern.as_bytes(), url.as_bytes())
}

/// Patron-approved exception: the entire command must be this one push, without shell syntax.
fn leader_server_push(command: &str, ctx: &Ctx) -> bool {
    if !ctx.leader {
        return false;
    }
    for refspec in ["master", "master:master"] {
        if command == format!("git push server {refspec}") {
            return true;
        }
        let Some(path) = command.strip_prefix("git -C ").and_then(|s| s.strip_suffix(&format!(" push server {refspec}"))) else { continue };
        let path = if let Some(q) = path.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            let Some(p) = path.strip_prefix(q).and_then(|p| p.strip_suffix(q)) else { continue };
            p
        } else {
            if path.chars().any(char::is_whitespace) { continue; }
            path
        };
        // No expansions, substitutions, globs, redirects, chains, or embedded quotes in paths.
        if path.is_empty() || !path.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '/' | '\\' | ':' | '.' | '-' | '_')) {
            continue;
        }
        let at = resolve(ctx.cwd.as_deref(), path);
        if ctx.team_root.as_deref().is_some_and(|root| key(&at) == key(&resolve(None, &root.to_string_lossy())))
            || ctx.extra_push_roots.iter().any(|root| key(&at) == key(&resolve(None, &root.to_string_lossy())))
        {
            return true;
        }
    }
    false
}

fn short(command: &str) -> String {
    let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 300 { flat } else { format!("{}…", flat.chars().take(299).collect::<String>()) }
}

fn segment(seg: &Seg, cwd: &mut Option<PathBuf>, ctx: &Ctx) -> Verdict {
    let mut vs = Vec::new();
    let words = strip_env(&seg.words);
    let env_words = if seg.words.first().is_some_and(|w| w.to_ascii_lowercase().starts_with("$env:") || matches!(name(w).as_str(), "export" | "set-item" | "set")) {
        &seg.words[..]
    } else {
        &seg.words[..seg.words.len() - words.len()]
    };
    if !ctx.leader && env_words.iter().any(|w| {
        let w = w.to_ascii_lowercase();
        w.trim_start_matches('$').trim_start_matches("env:").starts_with("git_config_")
    }) && env_words.iter().any(|w| w.to_ascii_lowercase().contains("core.hookspath")) {
        vs.push(Verdict::Deny("hook atlatma yasak: GIT_CONFIG_* hooksPath".into()));
    }
    for (op, target) in &seg.redirs {
        if NULL_DEVICES.contains(&target.to_ascii_lowercase().as_str()) {
            continue;
        }
        if ctx.read_only && op != "<" {
            vs.push(Verdict::Deny(format!("{READ_ONLY} (yönlendirme {op} {target})")));
            continue;
        }
        let p = resolve(cwd.as_deref(), target);
        vs.push(if op == "<" { read_path(Some(p), ctx, "okuma") } else { write_path(&p, ctx) });
    }
    // #164 P4e: `GIT_EXTERNAL_DIFF=… git diff`, `NODE_OPTIONS=--require … pnpm test`, `PATH=. ` —
    // an env prefix (or a bare assignment) changes what the command runs.
    if words.len() < seg.words.len() {
        vs.push(Verdict::Leader(String::new()));
    }
    let Some(first) = words.first() else { return worst(vs) };
    // #164 P4d: `./ls.cmd`, `tools/cat`: a program by path is not the allowlisted one of that name
    // (a lone leading `\`, bash's alias escape, is no path). Its hard noes still apply below.
    if first.trim_start_matches('\\').contains(['/', '\\']) {
        vs.push(Verdict::Leader(String::new()));
    }
    let cmd = name(first);
    let args = &words[1..];
    // Secrets are off limits in any command.
    for a in file_operands(&cmd, args) {
        if secret(&resolve(cwd.as_deref(), a)) {
            vs.push(Verdict::Deny(format!("sır/kimlik dosyası: {a}")));
        }
    }
    // #61: where a copy/download/extract lands is a write, like a redirect target.
    vs.extend(dest_operands(&cmd, args).iter().map(|d| dest_verdict(cwd.as_deref(), d, ctx)));
    vs.push(command_verdict(&cmd, args, cwd, ctx));
    worst(vs)
}

fn dest_verdict(cwd: Option<&Path>, d: &str, ctx: &Ctx) -> Verdict {
    if NULL_DEVICES.contains(&d.to_ascii_lowercase().as_str()) || d == "-" {
        Verdict::Allow
    } else {
        write_path(&resolve(cwd, d), ctx)
    }
}

/// `host:path`, `scheme://…`: not a file on this machine. A one-letter prefix is a drive (`C:`).
fn remote(w: &str) -> bool {
    w.contains("://") || w.find(':').is_some_and(|i| i > 1 && !w[..i].contains(['/', '\\']))
}

/// Values of `-o X`, `--output X`, `--output=X` and glued `-oX`.
fn flag_values<'a>(args: &'a [String], names: &[&str], ci: bool) -> Vec<&'a str> {
    let eq = |a: &str, n: &str| if ci { a.eq_ignore_ascii_case(n) } else { a == n };
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        for n in names {
            if eq(a, n) {
                if let Some(v) = args.get(i) {
                    out.push(v.as_str());
                }
                i += 1;
            } else if n.starts_with("--") && a.len() > n.len() && a.starts_with(&format!("{n}=")) {
                out.push(&a[n.len() + 1..]);
            } else if n.len() == 2 && !n.starts_with("--") && a.len() > 2 && !a.starts_with("--") && a.starts_with(n) {
                out.push(&a[2..]);
            } else {
                continue;
            }
            break;
        }
    }
    out
}

/// Operands that are not flags, skipping the values of `valued` flags.
fn positional<'a>(args: &'a [String], valued: &[&str], ci: bool) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.starts_with('-') && a.len() > 1 {
            if valued.iter().any(|v| if ci { a.eq_ignore_ascii_case(v) } else { a == v }) {
                it.next();
            }
        } else {
            out.push(a.as_str());
        }
    }
    out
}

/// Local destinations of copy/download/extract commands. Remote ones (`host:path`) are left out.
fn dest_operands(cmd: &str, args: &[String]) -> Vec<String> {
    let last_local = |ops: Vec<&str>| -> Vec<String> {
        match ops.last() {
            Some(l) if ops.len() >= 2 && !remote(l) => vec![l.to_string()],
            _ => Vec::new(),
        }
    };
    let owned = |v: Vec<&str>| v.into_iter().map(String::from).collect::<Vec<_>>();
    match cmd {
        "scp" => last_local(positional(args, &["-P", "-i", "-o", "-F", "-l", "-c", "-J", "-S", "-D"], false)),
        "cp" | "mv" | "rsync" => {
            let t = flag_values(args, &["-t", "--target-directory"], false);
            if !t.is_empty() {
                return owned(t);
            }
            let valued: &[&str] = if cmd == "rsync" {
                &["-e", "--rsh", "--exclude", "--include", "--exclude-from", "--include-from", "--filter", "-f", "--port", "--log-file", "--rsync-path", "--bwlimit"]
            } else {
                &["-S", "--suffix"]
            };
            last_local(positional(args, valued, false))
        }
        "copy-item" | "move-item" | "cpi" | "mi" | "copy" | "move" => {
            let d = flag_values(args, &["-destination"], true);
            if !d.is_empty() {
                return owned(d);
            }
            last_local(positional(args, &["-path", "-literalpath", "-filter", "-include", "-exclude", "-credential", "-stream"], true))
        }
        "set-content" | "add-content" | "out-file" | "sc" | "ac" => {
            let paths = ["-path", "-literalpath", "-filepath", "-pspath"];
            let valued = [
                "-inputobject", "-value", "-encoding", "-filter", "-include", "-exclude", "-stream", "-width", "-credential",
                "-erroraction", "-ea", "-warningaction", "-wa", "-informationaction", "-infa", "-progressaction",
                "-errorvariable", "-ev", "-warningvariable", "-wv", "-informationvariable", "-iv",
                "-outvariable", "-ov", "-outbuffer", "-ob", "-pipelinevariable", "-pv",
            ];
            let (mut named, mut first) = (Vec::new(), None);
            let mut it = args.iter();
            // Consume values in the same pass: InputObject/Value may themselves look like path flags.
            while let Some(a) = it.next() {
                let (param, inline) = a.split_once(':').map_or((a.as_str(), None), |(p, v)| (p, Some(v)));
                if paths.iter().any(|p| param.eq_ignore_ascii_case(p)) {
                    if let Some(v) = inline.or_else(|| it.next().map(String::as_str)) { named.push(v); }
                } else if valued.iter().any(|p| param.eq_ignore_ascii_case(p)) {
                    if inline.is_none() { it.next(); }
                } else if !a.starts_with('-') && first.is_none() {
                    first = Some(a.as_str());
                }
            }
            owned(if named.is_empty() { first.into_iter().collect() } else { named })
        }
        "adb" => {
            let Some(at) = args.iter().position(|a| a == "pull") else { return Vec::new() };
            last_local(positional(&args[at + 1..], &["-z"], false))
        }
        "curl" => owned(flag_values(args, &["-o", "--output", "--output-dir"], false)),
        "wget" => owned(flag_values(args, &["-O", "--output-document", "-P", "--directory-prefix"], false)),
        "tar" | "bsdtar" => tar_dests(args),
        _ => Vec::new(),
    }
}

/// `tar`: the extraction dir (`-C`) when extracting, the archive (`-f`) when creating.
fn tar_dests(args: &[String]) -> Vec<String> {
    let (mut extract, mut create) = (false, false);
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        let bundle = if a.starts_with("--") {
            None
        } else if a.starts_with('-') && a.len() > 1 {
            Some(&a[1..])
        } else if i == 1 {
            Some(a) // old style: `tar xzf a.tgz`
        } else {
            None
        };
        if let Some(b) = bundle {
            for c in b.chars() {
                match c {
                    'x' => extract = true,
                    'c' => create = true,
                    'f' | 'C' => {
                        if let Some(v) = args.get(i) {
                            if c == 'f' { files.push(v.clone()) } else { dirs.push(v.clone()) }
                        }
                        i += 1;
                    }
                    _ => {}
                }
            }
        } else if let Some(long) = a.strip_prefix("--") {
            let (n, v) = long.split_once('=').map_or((long, None), |(n, v)| (n, Some(v.to_string())));
            match n {
                "extract" | "get" => extract = true,
                "create" => create = true,
                "file" | "directory" => {
                    let v = v.or_else(|| {
                        i += 1;
                        args.get(i - 1).cloned()
                    });
                    if let Some(v) = v {
                        if n == "file" { files.push(v) } else { dirs.push(v) }
                    }
                }
                _ => {}
            }
        }
    }
    if extract { dirs } else if create { files } else { Vec::new() }
}

/// `git clone [opts] <repo> [dir]` (+ `--separate-git-dir`): where the clone lands.
fn clone_dests(args: &[String]) -> Vec<String> {
    let valued = ["-b", "--branch", "--depth", "-o", "--origin", "--reference", "--reference-if-able", "-c", "--config", "--template", "-j", "--jobs", "--filter", "-u", "--upload-pack", "--server-option", "--shallow-since", "--shallow-exclude", "--separate-git-dir", "--revision"];
    let mut out = flag_values(args, &["--separate-git-dir"], false).into_iter().map(String::from).collect::<Vec<_>>();
    if let Some(d) = positional(args, &valued, false).get(1) {
        out.push(d.to_string());
    }
    out
}

/// Arguments that name files. Search patterns, sed/jq scripts, `-e`/`-g`/`-name` values and
/// echoed text are not paths, so the secret rule must not read them as such.
fn file_operands<'a>(cmd: &str, args: &'a [String]) -> Vec<&'a String> {
    if matches!(cmd, "echo" | "write-output" | "write-host" | "tr") {
        return Vec::new();
    }
    let searching = matches!(cmd, "grep" | "rg" | "findstr" | "select-string" | "sls" | "sed" | "jq");
    let valued: &[&str] = match cmd {
        "find" => &["-name", "-iname", "-path", "-ipath", "-wholename", "-regex", "-iregex", "-type"],
        _ if searching => &["-e", "--regexp", "-g", "--glob", "--iglob", "-t", "--type", "-T", "--type-not", "--include", "--exclude", "--exclude-dir"],
        _ => &[],
    };
    // Without -e/-f the first operand is the pattern (or script), not a file.
    let mut pattern_next = searching && !has_short(args, 'e') && !has_short(args, 'f') && !has_flag(args, &["--regexp", "--file", "--expression", "--from-file"]);
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.starts_with('-') && a.len() > 1 {
            let bundle_e = searching && !a.starts_with("--") && a.ends_with('e');
            if valued.contains(&a.as_str()) || bundle_e {
                it.next();
            }
        } else if pattern_next {
            pattern_next = false;
        } else {
            out.push(a);
        }
    }
    out
}

fn has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|a| flags.iter().any(|f| a.eq_ignore_ascii_case(f)))
}

/// Short-flag bundles: `-rf` has `r`.
fn has_short(args: &[String], c: char) -> bool {
    args.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains(c))
}

fn command_verdict(cmd: &str, args: &[String], cwd: &mut Option<PathBuf>, ctx: &Ctx) -> Verdict {
    let leader = || Verdict::Leader(String::new());
    // Path-like arguments of a reading command must stay readable.
    let reads = |cwd: &Option<PathBuf>| {
        worst(
            file_operands(cmd, args)
                .into_iter()
                .filter(|a| looks_like_path(a))
                .map(|a| read_path(Some(resolve(cwd.as_deref(), a)), ctx, "okuma")),
        )
    };
    match cmd {
        // #288: builds and tests run arbitrary code (build.rs, test bodies) and write outputs.
        "cargo" | "npx" | "bunx" | "pnpm" | "npm" | "yarn" | "node" | "deno" | "bun" | "python" | "python3" | "py"
        | "vitest" | "playwright" | "tsc" | "make"
            if ctx.read_only =>
        {
            Verdict::Deny(READ_ONLY_TESTS.into())
        }
        "git" => git(args, cwd, ctx),
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri" => {
            let bulk = has_short(args, 'r') || has_short(args, 'R') || has_flag(args, &["--recursive", "/s", "-recurse"]) || args.iter().any(|a| a.contains(['*', '?']));
            if bulk { Verdict::Deny("toplu/özyinelemeli silme".into()) } else { leader() }
        }
        "cd" | "pushd" | "set-location" | "sl" | "chdir" => match args.iter().find(|a| !a.starts_with('-')) {
            Some(d) if !d.starts_with('~') => {
                let to = resolve(cwd.as_deref(), d);
                let ok = within(&to, &ctx.read);
                *cwd = Some(to);
                if ok { Verdict::Allow } else { leader() }
            }
            _ => leader(),
        },
        "cargo" => {
            let sub = args.iter().find(|a| !a.starts_with('-') && !a.starts_with('+')).map(String::as_str);
            match sub {
                Some("build" | "b" | "test" | "t" | "clippy" | "fmt" | "check" | "c" | "run" | "r" | "doc" | "tree" | "metadata" | "bench" | "nextest") => Verdict::Allow,
                _ => leader(),
            }
        }
        "npx" | "bunx" => js_tool(args),
        "pnpm" | "npm" | "yarn" => match args.first().map(String::as_str) {
            Some("test" | "t" | "build" | "lint" | "typecheck" | "tsc") => Verdict::Allow,
            Some("run") => match args.get(1).map(String::as_str) {
                Some("test" | "build" | "lint" | "typecheck" | "check" | "format" | "tsc") => Verdict::Allow,
                _ => leader(),
            },
            Some("exec") => js_tool(&args[1..]),
            Some(_) if cmd == "pnpm" => js_tool(args),
            _ => leader(),
        },
        "find" => {
            if has_flag(args, &["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls"]) { leader() } else { reads(cwd) }
        }
        "sed" => {
            if args.iter().any(|a| a.starts_with("-i") || a.starts_with("--in-place")) || sed_unsafe(args) { leader() } else { reads(cwd) }
        }
        // #164 P4f: allowlisted readers with a write/run side door.
        "sort" if has_short(args, 'o') || args.iter().any(|a| a.starts_with("--output") || a.starts_with("--compress-program")) => leader(),
        "rg" if args.iter().any(|a| a == "--pre" || a.starts_with("--pre=")) => leader(),
        "tree" if has_short(args, 'o') => leader(),
        "uniq" if positional(args, &["-f", "-s", "-w", "--skip-fields", "--skip-chars", "--check-chars"], false).len() > 1 => leader(),
        // `-Pattern x`: a pattern (`has_short` reads the flag as `-e`), the rest is read as usual.
        "select-string" | "sls" if ctx.read_only && args.iter().any(|a| a.eq_ignore_ascii_case("-pattern")) => {
            let mut rest = args.to_vec();
            let at = rest.iter().position(|a| a.eq_ignore_ascii_case("-pattern")).unwrap_or(0);
            rest.drain(at..(at + 2).min(rest.len()));
            worst(rest.iter().filter(|a| !a.starts_with('-') && looks_like_path(a)).map(|a| read_path(Some(resolve(cwd.as_deref(), a)), ctx, "okuma")))
        }
        "nl" | "convertfrom-json" | "convertto-json" | "where-object" | "format-table" | "format-list" | "out-string" | "group-object"
            if ctx.read_only =>
        {
            reads(cwd)
        }
        "ls" | "dir" | "cat" | "type" | "head" | "tail" | "wc" | "rg" | "grep" | "findstr" | "echo" | "pwd" | "which"
        | "where" | "sort" | "uniq" | "diff" | "file" | "stat" | "tree" | "du" | "df" | "date" | "whoami" | "true"
        | "false" | "jq" | "cut" | "tr" | "basename" | "dirname" | "realpath" | "popd" | "get-content" | "gc"
        | "get-childitem" | "gci" | "select-string" | "sls" | "test-path" | "get-location" | "gl" | "resolve-path"
        | "measure-object" | "select-object" | "sort-object" | "write-output" | "write-host" => reads(cwd),
        _ => leader(),
    }
}

/// `npx`/`pnpm exec`: the project's own dev tools. `playwright test` also meets the test budget
/// (checked on the whole command in [`shell`]).
fn js_tool(args: &[String]) -> Verdict {
    let tool = args.iter().find(|a| !a.starts_with('-')).map(|a| name(a));
    match tool.as_deref() {
        Some("tsc" | "vitest" | "eslint" | "prettier" | "vite") => Verdict::Allow,
        Some("playwright") if args.iter().any(|a| a == "test") => Verdict::Allow,
        _ => Verdict::Leader(String::new()),
    }
}

/// #164 P4f: a sed script that runs a command (`e`, `s///e`), writes a file (`w`, `W`, `s///w`) or
/// reads one (`r`, `R`); a script from a file (`-f`) is not known. `--sandbox` makes sed refuse them.
fn sed_unsafe(args: &[String]) -> bool {
    if has_flag(args, &["--sandbox"]) {
        return false;
    }
    let mut scripts = Vec::new();
    let mut first = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-l" {
            it.next();
        } else if a == "--expression" {
            scripts.extend(it.next().cloned());
        } else if let Some(v) = a.strip_prefix("--expression=") {
            scripts.push(v.to_string());
        } else if a == "--file" || a.starts_with("--file=") {
            return true;
        } else if a.starts_with('-') && !a.starts_with("--") && a.len() > 1 {
            // `-ne 'p'`, `-e'p'`, `-f x`: the first `e`/`f` in the bundle takes the rest or the next word.
            if let Some(at) = a[1..].find(['e', 'f']).map(|i| i + 1) {
                if a[at..].starts_with('f') {
                    return true;
                }
                let glued = &a[at + 1..];
                scripts.extend(if glued.is_empty() { it.next().cloned() } else { Some(glued.to_string()) });
            }
        } else if !a.starts_with("--") && first.is_none() {
            first = Some(a.clone());
        }
    }
    if scripts.is_empty() {
        scripts.extend(first);
    }
    scripts.iter().any(|s| sed_script_unsafe(s))
}

fn sed_script_unsafe(script: &str) -> bool {
    let c: Vec<char> = script.chars().collect();
    let n = c.len();
    // Index just past the closing `d` of a delimited part starting at `i`; None if never closed.
    let part = |mut i: usize, d: char| -> Option<usize> {
        while i < n {
            match c[i] {
                '\\' => i += 2,
                x if x == d => return Some(i + 1),
                _ => i += 1,
            }
        }
        None
    };
    let to = |mut i: usize, stop: &[char]| {
        while i < n && !stop.contains(&c[i]) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < n {
        // Addresses: `12`, `$`, `1,5`, `0~4`, `+3`, `!`, `/re/I`, `\%re%`.
        loop {
            match c.get(i) {
                Some(x) if x.is_whitespace() || x.is_ascii_digit() || matches!(x, ';' | '{' | '}' | '$' | ',' | '~' | '+' | '!') => i += 1,
                Some('/') => match part(i + 1, '/') {
                    Some(j) => i = j,
                    None => return true,
                },
                Some('\\') => match c.get(i + 1).and_then(|d| part(i + 2, *d)) {
                    Some(j) => i = j,
                    None => return true,
                },
                Some('I' | 'M') if i > 0 && matches!(c[i - 1], '/' | 'I' | 'M') => i += 1,
                _ => break,
            }
        }
        let Some(&cmd) = c.get(i) else { break };
        i += 1;
        match cmd {
            'e' | 'w' | 'W' | 'r' | 'R' => return true,
            's' | 'y' => {
                let Some(&d) = c.get(i) else { return true };
                let Some(j) = part(i + 1, d).and_then(|j| part(j, d)) else { return true };
                let end = to(j, &[';', '\n', '}']);
                if cmd == 's' && c[j..end].iter().any(|f| matches!(f, 'e' | 'w' | 'W')) {
                    return true;
                }
                i = end;
            }
            'a' | 'i' | 'c' | '#' => i = to(i, &['\n']),
            'b' | 't' | 'T' | ':' | 'v' | 'q' | 'Q' | 'l' | 'L' => i = to(i, &[';', '\n', '}']),
            '=' | 'd' | 'D' | 'g' | 'G' | 'h' | 'H' | 'x' | 'n' | 'N' | 'p' | 'P' | 'z' | 'F' => {}
            _ => return true, // not a command sed knows: not what it seems
        }
    }
    false
}

/// Inspect options, not commit messages or paths following `--`.
fn git_skips_hooks(sub: &str, args: &[String]) -> bool {
    if !matches!(sub, "commit" | "merge" | "rebase" | "am") {
        return false;
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" { break; }
        if a.starts_with("--no-v") && "--no-verify".starts_with(a.as_str()) { return true; }
        let valued = matches!(a.as_str(), "--message" | "--file")
            || (sub == "commit" && ["--author", "--date", "--cleanup", "--reuse-message", "--reedit-message", "--fixup", "--squash", "--trailer", "--template", "--pathspec-from-file"].contains(&a.as_str()));
        if valued {
            it.next();
        } else if a.starts_with('-') && !a.starts_with("--") {
            for (i, flag) in a.char_indices().skip(1) {
                if flag == 'n' && sub == "commit" { return true; }
                if sub == "commit" && "uS".contains(flag) { break; } // Optional attached value.
                if "mFcCt".contains(flag) {
                    if i + 1 == a.len() { it.next(); }
                    break;
                }
            }
        }
    }
    false
}

fn git(args: &[String], cwd: &mut Option<PathBuf>, ctx: &Ctx) -> Verdict {
    // Global options before the subcommand; `-C dir` moves where it acts.
    let mut i = 0;
    let mut at = cwd.clone();
    // #164 P4f: `-c core.fsmonitor=…`, `--config-env`, `--exec-path`, `--git-dir`…: what git runs
    // or acts on changes; only these globals keep an allowlisted subcommand allowed.
    let mut gray = false;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-C" => {
                // Git semantiği: her göreli `-C` bir öncekine göre çözülür; `-C ""` no-op.
                at = args.get(i + 1).map(|d| resolve(at.as_deref(), d));
                i += 2;
            }
            "--no-pager" | "-P" | "--no-optional-locks" | "--paginate" | "-p" => i += 1,
            a => {
                gray = true;
                let valued = ["-c", "--config-env", "--git-dir", "--work-tree", "--namespace", "--super-prefix"];
                i += if valued.contains(&a) { 2 } else { 1 };
            }
        }
    }
    let Some(sub) = args.get(i).map(String::as_str) else { return Verdict::Leader(String::new()) };
    let rest = &args[i + 1..];
    if !ctx.leader && args[..i].iter().enumerate().any(|(j, a)| {
        let setting = match a.as_str() {
            "-c" | "--config-env" => args.get(j + 1).map(String::as_str),
            _ => a.strip_prefix("-c").or_else(|| a.strip_prefix("--config-env=")),
        };
        setting.is_some_and(|s| s.split('=').next().is_some_and(|k| k.eq_ignore_ascii_case("core.hooksPath")))
    }) {
        return Verdict::Deny("hook atlatma yasak: core.hooksPath".into());
    }
    // `git config core.hooksPath <dir>` points every later commit at other hooks (#2361).
    if !ctx.leader
        && sub == "config"
        && positional(rest, &["-f", "--file", "--value"], false).first().is_some_and(|k| k.eq_ignore_ascii_case("core.hooksPath"))
    {
        return Verdict::Deny("hook atlatma yasak: core.hooksPath".into());
    }
    if !ctx.leader && git_skips_hooks(sub, rest) {
        return Verdict::Deny("hook atlatma yasak: git --no-verify/-n".into());
    }
    if !ctx.leader && (sub == "replace" || (sub == "update-ref" && rest.iter().any(|a| a.to_ascii_lowercase().starts_with("refs/replace/") || a == "--stdin"))) {
        // --stdin can hide replacement refs in a file or a pipe.
        return Verdict::Deny("hook atlatma yasak: git replace/refs/replace".into());
    }
    // `diff/log/show --output=…` writes anywhere; `grep -O<cmd>` runs a pager command.
    gray |= rest.iter().any(|a| a.starts_with("--output") || a.starts_with("--open-files-in-pager") || (sub == "grep" && a.starts_with("-O")));
    match sub {
        "push" => return Verdict::Deny("git push (patronun işi)".into()),
        "reset" if has_flag(rest, &["--hard"]) => return Verdict::Deny("git reset --hard".into()),
        "clean" if !has_flag(rest, &["-n", "--dry-run"]) => return Verdict::Deny("git clean".into()),
        _ => {}
    }
    if sub == "clone" {
        let v = worst(clone_dests(rest).iter().map(|d| dest_verdict(at.as_deref(), d, ctx)));
        if matches!(v, Verdict::Deny(_)) {
            return v;
        }
    }
    let roots = if ctx.read_only { &ctx.read } else { &ctx.write };
    if gray || !at.as_deref().is_some_and(|p| within(p, roots)) {
        return Verdict::Leader(String::new());
    }
    // #402: `diff --no-index` reads the files it is given, wherever they are (`-` is stdin, a `<( )`).
    if ctx.read_only && sub == "diff" && has_flag(rest, &["--no-index"]) {
        let v = worst(positional(rest, &[], false).into_iter().filter(|a| *a != "-").map(|a| read_path(Some(resolve(at.as_deref(), a)), ctx, "okuma")));
        if v != Verdict::Allow {
            return Verdict::Leader(String::new());
        }
    }
    let ok = match sub {
        "add" | "commit" => !ctx.read_only,
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "blame" | "describe" | "shortlog" | "grep" => true,
        // #402: more object/ref readers for the reviewer.
        "ls-tree" | "merge-base" | "cat-file" | "rev-list" | "check-attr" | "diff-tree" | "show-ref" | "name-rev" if ctx.read_only => true,
        // #288: a code-review worker only lists branches (a bare name would create one).
        "branch" if ctx.read_only => rest.iter().all(|a| matches!(a.as_str(), "-a" | "--all" | "-r" | "--remotes" | "-v" | "-vv" | "--verbose" | "--list" | "--show-current")),
        "branch" => !has_flag(rest, &["-d", "-D", "--delete", "-m", "-M", "--move", "-c", "-C", "--copy", "-f", "--force"]),
        "stash" => matches!(rest.first().map(String::as_str), Some("list" | "show")),
        "worktree" => rest.first().map(String::as_str) == Some("list"),
        "remote" => rest.iter().all(|a| a == "-v" || a == "--verbose"),
        _ => false,
    };
    if ok { Verdict::Allow } else { Verdict::Leader(String::new()) }
}

// ---- #2286: hard noes behind any wrapper ----

/// How deep a body's own bodies are read (`cmd /c sh -c '…'`), and how many texts and bytes are
/// lexed at most (tails make the bytes quadratic in the words: the hook's time bound). #2384: a body left unread for either is a worker's hard no ([`BODY_SHORT`]) — padding or
/// nesting must not turn a denial back into a gray the leader may approve; the leader's own
/// command stays gray as before.
const BODY_DEPTH: usize = 4;
const BODY_BUDGET: usize = 4000;
const BODY_BYTES: usize = 1 << 21;
const BODY_SHORT: &str = "gövde taranamadı: çok büyük/derin";

/// #2286: what a body scan made of one word list.
#[derive(Clone, PartialEq)]
enum Scanned {
    /// An allowlisted command: what follows it in the same list is its data.
    Allowed,
    /// A hard no found inside the body.
    Denied(String),
    /// Nothing plain: the leader decides, as before.
    Gray,
}

/// #2384: one body scan — its lex budget, whether a text was left unread (depth or budget), and
/// what each text already made at which depth: a tail's own words and tails are the outer
/// list's, so each is lexed once and the scan stays linear in the words (it was exponential).
struct Scan<'a> {
    d: Option<Dialect>,
    ctx: &'a Ctx,
    budget: usize,
    bytes: usize,
    short: bool,
    seen: std::collections::HashMap<String, (usize, Scanned)>,
}

impl<'a> Scan<'a> {
    fn new(d: Option<Dialect>, ctx: &'a Ctx) -> Self {
        Scan { d, ctx, budget: BODY_BUDGET, bytes: BODY_BYTES, short: false, seen: Default::default() }
    }

    /// One candidate body text of a word list scanned `depth` deep; `tail` is the list's words it
    /// was joined from, if any.
    fn text(&mut self, text: &str, depth: usize, tail: Option<&[String]>) -> Scanned {
        if let Some((seen, out)) = self.seen.get(text) {
            if *seen >= depth {
                return out.clone();
            }
        }
        if depth == 0 || self.budget == 0 || self.bytes < text.len() {
            self.short = true;
            return Scanned::Gray;
        }
        self.budget -= 1;
        self.bytes -= text.len();
        let out = scan_text(text, self, depth - 1, tail);
        self.seen.insert(text.to_string(), (depth, out.clone()));
        out
    }

    /// A worker's denial for a body left unread.
    fn short_denial(&self) -> Option<String> {
        (self.short && !self.ctx.leader).then(|| BODY_SHORT.to_string())
    }
}

/// Every gray segment's arguments are possible bodies — judged only for a deny (#480): an allow
/// never comes from here, and the allow/gray path of the outer command is unchanged. A denied or
/// allowlisted segment is skipped: the chain already denies, and an allowlisted command's
/// arguments are its data (`git commit -m 'git push'`).
fn bodies_denial(lexed: &Lexed, d: Option<Dialect>, ctx: &Ctx) -> Option<String> {
    let mut scan = Scan::new(d, ctx);
    for seg in &lexed.segs {
        let words = strip_env(&seg.words);
        let Some(first) = words.first() else { continue };
        // A flag is not a command: a glued value (`--expression='r .netrc'` re-split) is data.
        if first.starts_with('-') {
            continue;
        }
        if !matches!(
            segment(&Seg { words: words.to_vec(), redirs: Vec::new() }, &mut ctx.cwd.clone(), ctx),
            Verdict::Leader(_)
        ) {
            continue;
        }
        if let Scanned::Denied(why) = scan_word_bodies(words, &mut scan, BODY_DEPTH) {
            return Some(why);
        }
    }
    scan.short_denial()
}

/// The bodies one gray command may run: for `git` only its config bodies (`-c alias.x=!cmd` →
/// cmd, `alias.x=sub` → `git sub`, `config alias.x v`) — `git rm -r src` operands are data. Any
/// other command: each word on its own (a quoted body re-splits when lexed alone; a script
/// command hidden in punctuation `['git','push']` is flattened once), then every tail of the
/// arguments, stopping at the first plainly allowed command (`cmd /c echo git push` stays gray).
fn scan_word_bodies(words: &[String], scan: &mut Scan, depth: usize) -> Scanned {
    let args = &words[1..];
    // #2384: cmd reads `^x` as `x` (`cmd /c g^it push`): its words are read caret-free too.
    if name(&words[0]) == "cmd" && args.iter().any(|a| a.contains('^')) {
        let plain: Vec<String> = words.iter().map(|w| uncaret(w)).collect();
        if let Scanned::Denied(why) = scan_word_bodies(&plain, scan, depth) {
            return Scanned::Denied(why);
        }
    }
    let mut one = |text: &str, tail: Option<&[String]>| scan.text(text, depth, tail);
    if name(&words[0]) == "git" {
        for body in git_config_bodies(args) {
            if let Scanned::Denied(why) = one(&body, None) {
                return Scanned::Denied(why);
            }
        }
        return Scanned::Gray;
    }
    // A quoted script as the command itself (`execSync('git push …')` re-lexed): it holds spaces.
    let mut texts: Vec<&str> = Vec::new();
    if words[0].contains(char::is_whitespace) {
        texts.push(&words[0]);
    }
    texts.extend(args.iter().map(String::as_str));
    for t in &texts {
        if let Scanned::Denied(why) = one(t, None) {
            return Scanned::Denied(why);
        }
        let flat: String = t.chars().map(|c| if "[]{},".contains(c) { ' ' } else { c }).collect();
        if flat != *t {
            if let Scanned::Denied(why) = one(&flat, None) {
                return Scanned::Denied(why);
            }
        }
    }
    for i in 0..args.len() {
        match one(&args[i..].join(" "), Some(&args[i..])) {
            Scanned::Denied(why) => return Scanned::Denied(why),
            Scanned::Allowed => break,
            Scanned::Gray => {}
        }
    }
    Scanned::Gray
}

/// cmd's escape: `^x` is `x`, `^^` is `^`.
fn uncaret(w: &str) -> String {
    let mut out = String::with_capacity(w.len());
    let mut chars = w.chars();
    while let Some(c) = chars.next() {
        out.extend(if c == '^' { chars.next() } else { Some(c) });
    }
    out
}

/// One candidate body text as a whole (small) command chain: a denied segment is the finding, a
/// gray one recurses into its own bodies, an allowlisted one says the text is plainly allowed.
/// #2384: a tail that lexes back to the words it was joined from has no bodies of its own but
/// git's config bodies — its words and tails are the outer list's, read there (else cubic).
fn scan_text(text: &str, scan: &mut Scan, depth: usize, tail: Option<&[String]>) -> Scanned {
    let ctx = scan.ctx;
    let mut out = Scanned::Allowed;
    let lexed = lex(text, scan.d);
    if lexed.segs.is_empty() {
        return Scanned::Gray;
    }
    for seg in &lexed.segs {
        let words = strip_env(&seg.words);
        let Some(first) = words.first() else {
            out = Scanned::Gray;
            continue;
        };
        if first.starts_with('-') {
            out = Scanned::Gray;
            continue;
        }
        match segment(&Seg { words: words.to_vec(), redirs: seg.redirs.clone() }, &mut ctx.cwd.clone(), ctx) {
            Verdict::Deny(why) => return Scanned::Denied(why),
            Verdict::Allow => {}
            Verdict::Leader(_) => {
                out = Scanned::Gray;
                if tail == Some(words) && name(&words[0]) != "git" {
                    continue;
                }
                if let Scanned::Denied(why) = scan_word_bodies(words, scan, depth) {
                    return Scanned::Denied(why);
                }
            }
        }
    }
    out
}

/// `git`'s config bodies (values a worker can turn into a command): `-c alias.x=!cmd` → cmd,
/// `alias.x=sub` → `git sub`, and the `config alias.x v` subcommand. Everything else a git
/// invocation carries is data.
fn git_config_bodies(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let body_of = |v: &str| match v.strip_prefix('!') {
        Some(rest) => rest.to_string(),
        None if v == "git" || v.starts_with("git ") => v.to_string(),
        None => format!("git {v}"),
    };
    let alias = |v: &str| {
        v.split_once('=').and_then(|(k, val)| {
            k.to_ascii_lowercase().starts_with("alias.").then(|| body_of(val))
        })
    };
    let valued = ["-C", "-c", "--config-env", "--git-dir", "--work-tree", "--namespace", "--super-prefix"];
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        let a = args[i].as_str();
        if let Some(glued) = a.strip_prefix("-c").filter(|s| !s.is_empty()) {
            out.extend(alias(glued));
            i += 1;
        } else if valued.contains(&a) {
            if a == "-c" {
                out.extend(args.get(i + 1).map(String::as_str).and_then(alias));
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    // `git config [flags] alias.x v` — the key and the value are the first two operands.
    if args.get(i).is_some_and(|a| a == "config") {
        let ops = positional(&args[i + 1..], &["-f", "--file", "--value"], false);
        if let [k, v, ..] = ops.as_slice() {
            if k.to_ascii_lowercase().starts_with("alias.") {
                out.push(body_of(v));
            }
        }
    }
    out
}

// ---- #402/#495: the shell, read for real ----
//
// `lex` is blind to `& (…)`, braces and Unicode quotes, so an allow also needs this reader. It
// reads the command in the shell that runs it (Bash tool: bash; PowerShell tool: PowerShell)
// and allows only simple
// commands with literal arguments, joined by `|`, `;`, newlines, `&&` and `||`. #495: nothing
// else is ever allowed — no `$` outside single quotes (but PowerShell's `>$null`), no backtick,
// `$( )`, `<( )`, group, block, loop or assignment — and none of it ends the reading: the commands
// around and inside it are still judged, so a hidden denied one stays denied (#484).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    Bash,
    Pwsh,
}

#[derive(Debug)]
enum Tok {
    /// A word (`false`: not literal — what it expands to is not known).
    Word(String, bool),
    /// `;` (a newline, and syntax this reader does not follow, too), `|`, `||`, `&&`: all just
    /// join commands, each judged on its own.
    Op,
    /// `>`/`>>` to a literal file name: a write `segment` judges (a reviewer's is denied).
    Redir(String),
}

struct Rd {
    c: Vec<char>,
    i: usize,
    d: Dialect,
    depth: usize,
    /// A variable, a substitution or a backtick was read.
    dynamic: bool,
    /// Syntax this reader does not follow (group, block, background, here-document…).
    unknown: bool,
    /// The scripts of `$( )`, `<( )` and backticks: judged on their own, for a deny.
    hidden: Vec<Vec<Tok>>,
    /// bash here-documents waiting for the next newline: (delimiter, `<<-`,
    /// body judged for its substitutions).
    heredocs: Vec<(String, bool, bool)>,
}

impl Rd {
    fn at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    /// Tokens up to `close` (consumed); None: unterminated — the shell runs none of it. #508: what
    /// was read before is still judged, for a deny.
    fn script(&mut self, close: Option<char>) -> Option<Vec<Tok>> {
        let mut out = Vec::new();
        match self.nested(|rd| rd.tokens(close, &mut out)) {
            Some(()) => Some(out),
            None => {
                self.hidden.push(out);
                None
            }
        }
    }

    /// #508: every nesting (`$( )`, backticks, `${ }`, …) shares one bounded depth; past it None —
    /// no allow, and no stack overflow.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        self.depth += 1;
        let r = if self.depth > 16 { None } else { f(self) };
        self.depth -= 1;
        r
    }

    fn tokens(&mut self, close: Option<char>, out: &mut Vec<Tok>) -> Option<()> {
        let pw = self.d == Dialect::Pwsh;
        let mut word = String::new();
        let mut lit = true;
        let mut started = false;
        fn flush(out: &mut Vec<Tok>, word: &mut String, lit: &mut bool, started: &mut bool) {
            if *started {
                out.push(Tok::Word(std::mem::take(word), *lit));
                *started = false;
                *lit = true;
            }
        }
        loop {
            let Some(ch) = self.at(0) else {
                flush(out, &mut word, &mut lit, &mut started);
                return (close.is_none() && self.heredocs.is_empty()).then_some(());
            };
            match ch {
                _ if Some(ch) == close => {
                    self.i += 1;
                    flush(out, &mut word, &mut lit, &mut started);
                    return Some(());
                }
                ' ' | '\t' | '\r' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    self.i += 1;
                }
                '\n' | ';' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    out.push(Tok::Op);
                    self.i += 1;
                    if ch == '\n' {
                        self.bodies()?;
                    }
                }
                '|' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    let two = self.at(1) == Some('|');
                    out.push(Tok::Op);
                    self.i += if two { 2 } else { 1 };
                }
                '&' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    match self.at(1) {
                        Some('&') => {
                            out.push(Tok::Op);
                            self.i += 2;
                        }
                        Some('>') if !pw => {
                            self.i += 1;
                            out.extend(self.redirect());
                        }
                        // The call operator: only a command by name may follow (`& $x`, `& (…)`,
                        // `& { }`, `& '.\x.exe'` run what is not known here).
                        _ if pw && !matches!(out.last(), Some(Tok::Word(..) | Tok::Redir(_))) => {
                            self.i += 1;
                            while matches!(self.at(0), Some(' ' | '\t')) {
                                self.i += 1;
                            }
                            self.unknown |= !self.at(0).is_some_and(|c| c.is_ascii_alphabetic());
                        }
                        // In the background: not followed.
                        _ => {
                            self.unknown = true;
                            out.push(Tok::Op);
                            self.i += 1;
                        }
                    }
                }
                // Groups, blocks, conditions, brace expansion: not followed; what is in them is
                // still read as commands of their own.
                '(' | ')' | '{' | '}' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    self.unknown = true;
                    out.push(Tok::Op);
                    self.i += 1;
                }
                // bash `<( )`, `>( )`: a pipe the command reads or writes.
                '<' | '>' if !pw && self.at(1) == Some('(') => {
                    self.i += 2;
                    self.hide(&mut word, &mut lit, Some(')'))?;
                    started = true;
                }
                // bash here-document / here-string.
                '<' if !pw && self.at(1) == Some('<') => {
                    flush(out, &mut word, &mut lit, &mut started);
                    self.unknown = true;
                    self.i += 2;
                    if self.at(0) == Some('<') {
                        self.i += 1;
                    } else {
                        self.heredoc()?;
                    }
                }
                // bash `<>`: opened for writing too.
                '>' | '<' if ch == '>' || (!pw && self.at(1) == Some('>')) => {
                    // `2>` (PowerShell `*>`): the digits are the stream, not a word.
                    let fd = lit && started && (word.chars().all(|c| c.is_ascii_digit()) || (pw && word == "*"));
                    if fd {
                        word.clear();
                        started = false;
                    } else {
                        flush(out, &mut word, &mut lit, &mut started);
                    }
                    if ch == '<' {
                        self.i += 1;
                    }
                    out.extend(self.redirect());
                }
                // An input redirection: its file is read as an argument.
                '<' => {
                    flush(out, &mut word, &mut lit, &mut started);
                    self.unknown = true;
                    self.i += 1;
                }
                '#' if !started => {
                    while self.at(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\'' => {
                    self.i += 1;
                    started = true;
                    self.single(&mut word)?;
                }
                '"' => {
                    self.i += 1;
                    started = true;
                    self.double(&mut word, &mut lit)?;
                }
                '$' => {
                    started = true;
                    self.dollar(&mut word, &mut lit)?;
                }
                '`' => {
                    started = true;
                    self.backtick(&mut word, &mut lit)?;
                }
                '\\' if !pw => {
                    match self.at(1) {
                        Some(n) => {
                            word.push(n);
                            started = true;
                        }
                        None => return None,
                    }
                    self.i += 2;
                }
                // PowerShell here-string: its text is not read — #503: but `@" "@` runs its `$( )`.
                '@' if pw && !started && matches!(self.at(1), Some('\'' | '"')) => {
                    let q = self.at(1)?;
                    self.dynamic |= q == '"';
                    self.unknown = true;
                    let end: Vec<char> = ['\n', q, '@'].into();
                    let rest = self.c.get(self.i + 2..)?;
                    let n = rest.windows(3).position(|w| w == end.as_slice())?;
                    let close = self.i + 2 + n;
                    if q == '"' {
                        self.i += 2;
                        while self.i < close {
                            match self.c[self.i] {
                                '$' | '`' => self.substitution(&mut String::new())?,
                                _ => self.i += 1,
                            }
                        }
                    }
                    if self.i > close {
                        return None;
                    }
                    self.i = close + 3;
                    word.push('x');
                    lit = false;
                    started = true;
                }
                // Splat/array/hashtable, type literal, stop-parsing.
                '@' | '[' if pw && !started => {
                    self.unknown = true;
                    word.push(ch);
                    started = true;
                    self.i += 1;
                }
                '-' if pw && !started && self.c[self.i..].starts_with(&['-', '-', '%']) => {
                    self.unknown = true;
                    word.push(ch);
                    started = true;
                    self.i += 1;
                }
                c => {
                    // #503: a bash glob outside quotes expands to file names (`git diff *` with a
                    // `--output=x` file in the folder) — not literal.
                    if !pw && (c == '*' || c == '?' || (c == ']' && word.contains('['))) {
                        self.dynamic = true;
                        lit = false;
                    }
                    word.push(c);
                    started = true;
                    self.i += 1;
                }
            }
        }
    }

    /// At `<<`: a here-document's delimiter; its body is skipped at the next newline.
    fn heredoc(&mut self) -> Option<()> {
        let strip_tabs = self.at(0) == Some('-');
        if strip_tabs {
            self.i += 1;
        }
        while matches!(self.at(0), Some(' ' | '\t')) {
            self.i += 1;
        }
        let (delim, quoted, ambiguous, n) = heredoc_word(&mut self.c[self.i..].iter().copied().peekable())?;
        self.i += n;
        (quoted || !delim.is_empty()).then(|| self.heredocs.push((delim, strip_tabs, !quoted || ambiguous)))
    }

    /// After a newline: the bodies of the here-documents opened on that line. #503: an unquoted
    /// delimiter's body runs its substitutions — judged on their own, for a deny. #508: the body
    /// ends where bash ends it (line by line; #517: continuations already joined); one never
    /// closed is the rest of the command.
    fn bodies(&mut self) -> Option<()> {
        for (delim, strip_tabs, scan) in std::mem::take(&mut self.heredocs) {
            let start = self.i;
            let mut end = None;
            while self.i < self.c.len() {
                let (line, n) = body_line(&mut self.c[self.i..].iter().copied());
                if (if strip_tabs { line.trim_start_matches('\t') } else { &line }) == delim {
                    end = Some(self.i);
                    self.i += n;
                    break;
                }
                self.i += n;
            }
            let end = end.unwrap_or_else(|| {
                self.unknown = true;
                self.c.len()
            });
            if scan {
                self.body(start, end);
            }
        }
        Some(())
    }

    /// The substitutions of a here-document body, read on their own: none runs past the body.
    fn body(&mut self, start: usize, end: usize) {
        let mut rd = Rd { c: self.c[start..end].to_vec(), i: 0, d: self.d, depth: self.depth, dynamic: false, unknown: false, hidden: Vec::new(), heredocs: Vec::new() };
        while let Some(c) = rd.at(0) {
            match c {
                '\\' => rd.i += 2,
                '$' | '`' => {
                    let at = rd.i;
                    if rd.substitution(&mut String::new()).is_none() {
                        rd.unknown = true;
                        rd.i = rd.i.max(at + 1);
                    }
                }
                _ => rd.i += 1,
            }
        }
        self.hidden.append(&mut rd.hidden);
        self.dynamic |= rd.dynamic;
        self.unknown |= rd.unknown;
    }

    /// At `$` or a backtick inside quoted text (a double-quoted word or redirection target, an
    /// unquoted here-document, a PowerShell `@" "@`): #503 — what it runs is judged on its own.
    fn substitution(&mut self, word: &mut String) -> Option<()> {
        let mut lit = false;
        match self.at(0)? {
            '`' if self.d == Dialect::Pwsh => {
                self.dynamic = true;
                self.i += 2;
                Some(())
            }
            '`' => self.backtick(word, &mut lit),
            _ => self.dollar(word, &mut lit),
        }
    }

    /// The script up to `close` runs before the command: judged on its own, the word holds `x`.
    fn hide(&mut self, word: &mut String, lit: &mut bool, close: Option<char>) -> Option<()> {
        self.dynamic = true;
        let inner = self.script(close)?;
        self.hidden.push(inner);
        word.push('x');
        *lit = false;
        Some(())
    }

    /// At `>`: to a null device or a stream (`2>&1`) nothing; to a literal file name a write;
    /// anything else is not known.
    fn redirect(&mut self) -> Option<Tok> {
        self.i += 1;
        while matches!(self.at(0), Some('>' | '|')) {
            self.i += 1;
        }
        let pw = self.d == Dialect::Pwsh;
        if self.at(0) == Some('&') {
            self.i += 1;
            let s = self.i;
            while self.at(0).is_some_and(|c| c.is_ascii_digit() || c == '-') {
                self.i += 1;
            }
            if self.i > s {
                self.unknown |= !self.at(0).is_none_or(|c| c.is_whitespace() || "|;&)>".contains(c));
                return None;
            }
            // bash `>&file`: stdout and stderr to that file.
            self.unknown |= pw;
        }
        while matches!(self.at(0), Some(' ' | '\t')) {
            self.i += 1;
        }
        // #480: quotes hold literal text (`> "build logs/x.log"`); no expansion, no escape in them.
        let (mut t, mut quoted, mut bare_escape, mut expand, mut var) = (String::new(), false, false, false, false);
        while let Some(c) = self.at(0).filter(|c| !c.is_whitespace() && !"|;&()<>{}".contains(*c)) {
            self.i += 1;
            match c {
                '\'' => {
                    quoted = true;
                    loop {
                        let Some(q) = self.at(0) else {
                            self.unknown = true;
                            return None;
                        };
                        self.i += 1;
                        if q == '\'' {
                            if pw && self.at(0) == Some('\'') {
                                t.push('\'');
                                self.i += 1;
                                continue;
                            }
                            break;
                        }
                        t.push(q);
                    }
                }
                '"' => {
                    quoted = true;
                    loop {
                        let Some(q) = self.at(0) else {
                            self.unknown = true;
                            return None;
                        };
                        self.i += 1;
                        match q {
                            '"' if pw && self.at(0) == Some('"') => {
                                expand = true;
                                self.i += 1;
                            }
                            '"' => break,
                            // #503: `> "$(rm -rf src)"` runs what is in it.
                            '$' | '`' => {
                                var = true;
                                if (q == '`' && !pw) || (q == '$' && matches!(self.at(0), Some('(' | '{'))) {
                                    self.i -= 1;
                                    if self.substitution(&mut String::new()).is_none() {
                                        self.unknown = true;
                                        return None;
                                    }
                                }
                            }
                            '\\' if !pw && matches!(self.at(0), Some('$' | '`' | '"' | '\\')) => expand = true,
                            q => t.push(q),
                        }
                    }
                }
                '$' | '`' if (c == '`' && !pw) || (c == '$' && matches!(self.at(0), Some('(' | '{'))) => {
                    var = true;
                    self.i -= 1;
                    if self.substitution(&mut t).is_none() {
                        self.unknown = true;
                        return None;
                    }
                }
                c => {
                    bare_escape |= c == '\\';
                    // #503: a bash glob picks the file.
                    expand |= "#~".contains(c) || (!pw && "*?[".contains(c));
                    var |= "$`".contains(c);
                    t.push(c);
                }
            }
        }
        // #495: the one `$` read — PowerShell's constant `$null` (bash's is a variable).
        if pw && !quoted && t.eq_ignore_ascii_case("$null") {
            return None;
        }
        // #503: any other null device only as written — `> "$nul"` is whatever `$nul` holds.
        if !(expand || var || bare_escape || t.starts_with('$')) && NULL_DEVICES.contains(&t.to_ascii_lowercase().as_str()) {
            return None;
        }
        // bash drops an unquoted `\` (`C:\w` → `C:w`); read as written, like `lex`, unless that
        // could join dots into a `..` (`a/.\./x`), start a path (`\/x`) or meet a quoted part.
        let escape = self.d == Dialect::Bash && bare_escape && (quoted || t.starts_with('\\') || t.contains(".\\") || t.contains("\\."));
        if t.is_empty() || expand || var || escape || t.starts_with('$') {
            self.dynamic |= var;
            self.unknown = true;
            return None;
        }
        Some(Tok::Redir(t))
    }

    fn single(&mut self, word: &mut String) -> Option<()> {
        loop {
            let c = self.at(0)?;
            self.i += 1;
            if c == '\'' {
                if self.d == Dialect::Pwsh && self.at(0) == Some('\'') {
                    word.push('\'');
                    self.i += 1;
                    continue;
                }
                return Some(());
            }
            word.push(c);
        }
    }

    fn double(&mut self, word: &mut String, lit: &mut bool) -> Option<()> {
        let pw = self.d == Dialect::Pwsh;
        loop {
            match self.at(0)? {
                '"' => {
                    self.i += 1;
                    return Some(());
                }
                '$' => self.dollar(word, lit)?,
                '`' => self.backtick(word, lit)?,
                '\\' if !pw && matches!(self.at(1), Some('$' | '`' | '"' | '\\')) => {
                    word.push(self.at(1)?);
                    self.i += 2;
                }
                c => {
                    word.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// At `$`: #495 — never literal. `$( )` is judged on its own; a variable's name stays in the
    /// word as written (`git push $R` is still a push).
    fn dollar(&mut self, word: &mut String, lit: &mut bool) -> Option<()> {
        self.i += 1;
        self.dynamic = true;
        *lit = false;
        match self.at(0) {
            Some('(') => {
                self.i += 1;
                self.hide(word, lit, Some(')'))
            }
            // #503: `${x:-$(…)}` runs what is in it.
            Some('{') => {
                self.i += 1;
                self.nested(|rd| loop {
                    match rd.at(0)? {
                        '}' => break Some(()),
                        '$' | '`' => rd.substitution(&mut String::new())?,
                        '\\' => rd.i += 2,
                        _ => rd.i += 1,
                    }
                })?;
                self.i += 1;
                word.push('x');
                Some(())
            }
            _ => {
                word.push('$');
                Some(())
            }
        }
    }

    /// At a backtick: #495 — never literal. bash runs the script up to the next one; PowerShell
    /// escapes the next character (a line continuation at the end of a line).
    fn backtick(&mut self, word: &mut String, lit: &mut bool) -> Option<()> {
        self.i += 1;
        if self.d == Dialect::Bash {
            return self.hide(word, lit, Some('`'));
        }
        self.dynamic = true;
        *lit = false;
        let n = self.at(0)?;
        self.i += 1;
        if n != '\n' {
            word.push(n);
        }
        Some(())
    }
}

/// A shell tool's command, as `judge` reads it.
fn shell_command(tool: &str, input: &Value) -> Option<String> {
    if !matches!(tool, "Bash" | "PowerShell") {
        return None;
    }
    Some(input.get("command").and_then(Value::as_str).unwrap_or_default().to_string())
}

/// The shell a tool's command runs in: the Bash tool's is bash, the PowerShell tool's
/// PowerShell; anything else the platform's (PowerShell on Windows).
pub(crate) fn dialect(tool: &str) -> Option<Dialect> {
    Some(match tool {
        "Bash" => Dialect::Bash,
        "PowerShell" => Dialect::Pwsh,
        _ if cfg!(windows) => Dialect::Pwsh,
        _ => Dialect::Bash,
    })
}

/// Why `parsed` said no.
#[derive(Debug, PartialEq)]
enum Unparsed {
    /// #480: a command the policy denies (the reason).
    Deny(String),
    /// Syntax, a shell or a command this reader does not know.
    Unknown,
    /// #471/#495: a variable, a substitution, a backtick, an assignment or a loop — what they hold
    /// or run is not simulated.
    Dynamic,
}

/// Every simple command of `command` passes `segment` (#402; #459: a worker's too) and is plain:
/// literal words joined by `|`, `;`, `&&`, `||` (#495). A deny anywhere wins (#484).
fn parsed(command: &str, d: Option<Dialect>, ctx: &Ctx) -> Result<(), Unparsed> {
    let Some(d) = d else { return Err(Unparsed::Unknown) };
    if command.trim().is_empty() {
        return Err(Unparsed::Unknown);
    }
    // PowerShell reads these as ASCII quotes, dashes and blanks: so does this reader. A reviewer
    // gets none; a worker's commit message keeps its `—`. bash takes them as plain text.
    let pwsh_ascii = |c: char| match c {
        '‘' | '’' | '‚' | '‛' => Some('\''),
        '“' | '”' | '„' => Some('"'),
        '–' | '—' | '―' => Some('-'),
        c if c.is_whitespace() && !c.is_ascii() => Some('\n'),
        _ => None,
    };
    let unusual = command.chars().any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\r' | '\n'))
        || (ctx.read_only && command.chars().any(|c| pwsh_ascii(c).is_some()));
    let chars = command.chars().map(|c| if d == Dialect::Pwsh { pwsh_ascii(c).unwrap_or(c) } else { c }).collect();
    let mut rd = Rd { c: chars, i: 0, d, depth: 0, dynamic: false, unknown: false, hidden: Vec::new(), heredocs: Vec::new() };
    let toks = rd.script(None);
    let mut ro = Ro { ctx, d, dynamic: false, deny: None, gray: false };
    for t in toks.iter().chain(&rd.hidden) {
        ro.list(t, &mut ctx.cwd.clone());
    }
    match ro.deny {
        Some(why) => Err(Unparsed::Deny(why)),
        None if rd.dynamic || ro.dynamic => Err(Unparsed::Dynamic),
        None if toks.is_none() || unusual || rd.unknown || ro.gray => Err(Unparsed::Unknown),
        None => Ok(()),
    }
}

/// #471: loops — bash, PowerShell keywords and `ForEach-Object`.
const LOOPS: &[&str] = &["for", "while", "until", "select", "do", "foreach", "foreach-object", "%"];
/// #495: keywords in front of a command: not followed, the command after one is still judged.
const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "done", "case", "esac", "in", "function", "time", "coproc", "!", "elseif", "switch",
    "try", "catch", "finally", "trap", "filter", "return", "throw", "exit", "break", "continue",
];

struct Ro<'a> {
    ctx: &'a Ctx,
    d: Dialect,
    /// #471: a loop or an assignment was read.
    dynamic: bool,
    /// #480: the first command the policy denies — it stays a deny, hidden or not.
    deny: Option<String>,
    /// #480: a command the policy hands to the leader; the rest is still read for a deny.
    gray: bool,
}

impl Ro<'_> {
    /// Every simple command, one after another (the cwd follows a literal `cd`).
    fn list(&mut self, t: &[Tok], cwd: &mut Option<PathBuf>) {
        for cmd in t.split(|x| matches!(x, Tok::Op)) {
            self.simple(cmd, cwd);
        }
    }

    fn simple(&mut self, toks: &[Tok], cwd: &mut Option<PathBuf>) {
        let mut toks = toks;
        let key = |t: &Tok| match t {
            Tok::Word(w, true) => Some(if self.d == Dialect::Pwsh { w.to_ascii_lowercase() } else { w.clone() }),
            _ => None,
        };
        while let Some(k) = toks.first().and_then(key) {
            if LOOPS.contains(&k.as_str()) {
                self.dynamic = true;
            } else if !KEYWORDS.contains(&k.as_str()) {
                break;
            }
            self.gray = true;
            toks = &toks[1..];
            // `%`/`ForEach-Object` take a block, not a command.
            if matches!(k.as_str(), "%" | "foreach-object") {
                return;
            }
        }
        // #471: bash `NAME=value`, alone or in front of a command (`segment` hands it to the leader).
        if self.d == Dialect::Bash && matches!(toks.first(), Some(Tok::Word(w, _)) if w.split_once('=').is_some_and(|(n, _)| ident(n))) {
            self.dynamic = true;
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        for t in toks {
            match t {
                Tok::Word(w, lit) => {
                    self.gray |= !lit;
                    words.push(w.clone());
                }
                Tok::Redir(target) => redirs.push((">".to_string(), target.clone())),
                Tok::Op => {}
            }
        }
        if words.is_empty() && redirs.is_empty() {
            return;
        }
        match segment(&Seg { words: words.clone(), redirs }, cwd, self.ctx) {
            Verdict::Allow => {}
            Verdict::Deny(why) => {
                self.deny.get_or_insert(why);
            }
            // #2286: the same body scan in this reader too — a Unicode-quoted PowerShell body
            // (`cmd /c '…'`) is a plain quoted word only here.
            Verdict::Leader(_) => {
                self.gray = true;
                let mut scan = Scan::new(Some(self.d), self.ctx);
                if let Scanned::Denied(why) = scan_word_bodies(&words, &mut scan, BODY_DEPTH) {
                    self.deny.get_or_insert(why);
                } else if let Some(why) = scan.short_denial() {
                    self.deny.get_or_insert(why);
                }
            }
        }
    }
}

fn ident(n: &str) -> bool {
    n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reviewer_744_git_reads_from_read_only_root() {
        let root = PathBuf::from(abs("/Users/user/ExampleProject"));
        let reviewer = Ctx { read_only: true, read: vec![root.clone()], ..ctx() };
        let root = key(&root);
        for command in [format!("git -C {root} merge-base --all main 8f3c25b"), format!("git -C {root} ls-tree -r --name-only 06a58b09 tests")] {
            assert_eq!(decide("Bash", &json!({"command": command}), &reviewer), Verdict::Allow, "{command}");
        }
        for sub in ["add .", "commit -m x", "merge-tree main HEAD", "diff --output=x", "status > file", "push origin main"] {
            assert!(matches!(decide("Bash", &json!({"command": format!("git -C {root} {sub}")}), &reviewer), Verdict::Deny(_)), "{sub}");
        }
        assert!(matches!(decide("Bash", &json!({"command": "git -C /unapproved merge-base --all main HEAD"}), &reviewer), Verdict::Deny(_)));
        let worker = Ctx { read_only: false, ..reviewer };
        assert_ne!(decide("Bash", &json!({"command": format!("git -C {root} status")}), &worker), Verdict::Allow);
    }

    #[test]
    fn reviewer_1756_diff_log_show_status_reads_and_writes_stay_denied() {
        let root = PathBuf::from(abs("/w/proj"));
        let reviewer = Ctx { read_only: true, read: vec![root.clone()], write: vec![root.clone()], cwd: Some(root.clone()), ..ctx() };
        // The review prompt's form: `-C` quoted, forward slashes (also bare, run from the root).
        let slash = root.to_string_lossy().replace('\\', "/");
        let sha = "8f3c25b09d4e6f7a8b9c0d1e2f3a4b5c6d7e8f9";
        for command in [
            format!("git diff team...{sha}"),
            format!("git diff team...{sha} -- crates/dex-core/src/permit.rs"),
            format!("git show {sha}"),
            format!("git show {sha}:crates/dex-core/src/permit.rs"),
            format!("git log --oneline team..{sha}"),
            "git status".to_string(),
            format!("git -C \"{slash}\" diff team...{sha}"),
            format!("git -C \"{slash}\" diff team...{sha} -- crates/dex-core/src/permit.rs"),
            format!("git -C \"{slash}\" show {sha}"),
            format!("git -C \"{slash}\" show {sha}:crates/dex-core/src/permit.rs"),
            format!("git -C \"{slash}\" log --oneline team..{sha}"),
            format!("git -C \"{slash}\" status"),
        ] {
            assert_eq!(decide("Bash", &json!({"command": command}), &reviewer), Verdict::Allow, "{command}");
        }
        // #1756: an unquoted backslash path is what bash runs — `C:\w` → `C:w`, outside the roots.
        if cfg!(windows) {
            assert!(matches!(decide("Bash", &json!({"command": format!("git -C {} status", root.display())}), &reviewer), Verdict::Deny(_)));
        }
        for sub in [
            "add .", "commit -m x", "checkout topic", "reset --hard HEAD", "merge topic", "stash push",
            "worktree add ../other -b other", "push origin main", "clean -fd", "branch -D topic", "remote add o url",
        ] {
            assert!(matches!(decide("Bash", &json!({"command": format!("git {sub}")}), &reviewer), Verdict::Deny(_)), "{sub}");
            assert!(matches!(decide("Bash", &json!({"command": format!("git -C \"{slash}\" {sub}")}), &reviewer), Verdict::Deny(_)), "-C {sub}");
        }
    }

    #[test]
    fn reviewer_744_quoted_redirect_pattern_is_text() {
        let reviewer = Ctx { read_only: true, ..ctx() };
        for command in [
            r#"grep -n "2>&1\|merge-tree\|> \.dex\|cd {root}" crates/dex-core/src/permit.rs | head -60"#,
            r#"grep -n "a\" > \.dex\" b" crates/dex-core/src/permit.rs | head -60"#,
            r#"ls .; grep -rn "a\|b" '--include=*.rs' . | grep -v x | head; grep -n "2>&1\|merge-tree\|> \.dex\|cd {root}" crates/dex-core/src/permit.rs | head -60"#,
        ] {
            assert_eq!(decide("Bash", &json!({"command": command}), &reviewer), Verdict::Allow, "{command}");
        }
        for command in [r#"grep -n "> \.dex" crates/dex-core/src/permit.rs > file"#, r#"grep -n "$X > \.dex" crates/dex-core/src/permit.rs"#] {
            assert!(matches!(decide("Bash", &json!({"command": command}), &reviewer), Verdict::Deny(_)), "{command}");
        }
        // The same backslash is not a quote escape in PowerShell.
        assert!(matches!(decide("PowerShell", &json!({"command": r#"grep -n "a\" > file"#}), &reviewer), Verdict::Deny(_)));
        assert!(matches!(decide("Bash", &json!({"command": "grep -rn a --include=*.rs ."}), &reviewer), Verdict::Deny(_)), "unquoted glob remains dynamic");
    }

    #[test]
    fn reviewer_744_reads_only_own_session_output() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().join(".dex/tmp/permit-744-session");
        let results = root.join("project/s1/tool-results");
        std::fs::create_dir_all(&results).unwrap();
        let file = results.join("x.txt");
        std::fs::write(&file, "output").unwrap();
        std::fs::write(results.join("x.json"), "{}").unwrap();
        std::fs::write(results.join("dex-provider.txt"), "credential fixture").unwrap();
        let reviewer = Ctx { read_only: true, ..ctx() };
        // Reproduce the live refusal before supplying the session capability.
        assert!(matches!(decide("Read", &json!({"file_path": file}), &reviewer), Verdict::Deny(_)));
        let transcript = root.join("project/s1.jsonl");
        let capability = tool_results_root(&transcript, "s1", &root).unwrap();
        assert_eq!(capability, results);
        for (path, session) in [(root.join("project/s2.jsonl"), "s1"), (root.join("project/../project/s1.jsonl"), "s1"), (transcript.clone(), "../s1"), (root.join("elsewhere/project/s1.jsonl"), "s1")] {
            assert!(tool_results_root(&path, session, &root).is_none());
        }
        // #2019: a Claude 2 session's transcript path runs through its account's link to `projects`.
        let alias = root.with_file_name("permit-744-alias");
        let _ = std::fs::remove_dir(&alias);
        if crate::worktree::link_dir(&root, &alias) {
            let via = tool_results_root(&alias.join("project/s1.jsonl"), "s1", &root).unwrap();
            assert_eq!(via, alias.join("project/s1/tool-results"));
            let r = Ctx { read_only: true, tool_results: Some(via), ..ctx() };
            assert_eq!(decide("Read", &json!({"file_path": alias.join("project/s1/tool-results/x.txt")}), &r), Verdict::Allow);
            assert!(tool_results_root(&alias.join("project/s1.jsonl"), "s1", &root.join("project")).is_none());
            #[cfg(windows)]
            std::fs::remove_dir(&alias).unwrap();
            #[cfg(unix)]
            std::fs::remove_file(&alias).unwrap();
        } else {
            eprintln!("projects link test unavailable on this host");
        }
        let reviewer = Ctx { tool_results: Some(capability), ..reviewer };
        assert_eq!(decide("Read", &json!({"file_path": file}), &reviewer), Verdict::Allow);
        for bad in [root.join("project/s2/tool-results/x.txt"), results.join("../secret.txt"), results.join("x.json"), results.join("dex-provider.txt"), results.join("../tool-results/x.txt"), results.clone()] {
            assert!(matches!(decide("Read", &json!({"file_path": bad}), &reviewer), Verdict::Deny(_)), "{}", bad.display());
        }
        for tool in ["Write", "Edit", "MultiEdit", "Glob", "Grep", "NotebookRead"] {
            assert!(matches!(decide(tool, &json!({"file_path": file}), &reviewer), Verdict::Deny(_)), "{tool}");
        }
        assert!(matches!(decide("Bash", &json!({"command": format!("cat '{}'", file.display())}), &reviewer), Verdict::Deny(_)));
        // Even an output-looking link cannot read a sibling file.
        let link = results.join("linked.txt");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&file, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&file, &link);
        if linked.is_ok() {
            assert!(matches!(decide("Read", &json!({"file_path": link}), &reviewer), Verdict::Deny(_)));
        } else {
            eprintln!("session output symlink test unavailable on this host");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn ctx() -> Ctx {
        let root = if cfg!(windows) { PathBuf::from(r"C:\w\proj") } else { PathBuf::from("/w/proj") };
        let other = if cfg!(windows) { PathBuf::from(r"C:\w\other") } else { PathBuf::from("/w/other") };
        Ctx { write: vec![root.clone()], read: vec![root.clone()], others: vec![other], cwd: Some(root), qa: false, ..Default::default() }
    }

    fn abs(p: &str) -> String {
        if cfg!(windows) { format!(r"C:{}", p.replace('/', "\\")) } else { p.into() }
    }

    fn bash(c: &str) -> Verdict {
        decide("Bash", &json!({ "command": c }), &ctx())
    }

    fn allow(c: &str) {
        assert_eq!(bash(c), Verdict::Allow, "{c}");
    }

    fn deny(c: &str) {
        assert!(matches!(bash(c), Verdict::Deny(_)), "{c}: {:?}", bash(c));
    }

    fn leader(c: &str) {
        assert!(matches!(bash(c), Verdict::Leader(_)), "{c}: {:?}", bash(c));
    }

    /// #2361: `secret()` reads the name the way Windows does (stream suffix, trailing dot/space,
    /// 8.3 alias), like `protected()`.
    #[test]
    fn secret_name_spellings_name_the_same_file() {
        let c = ctx();
        for f in [".env", ".env ", ".env.", ".env::$DATA", "certs/server.pem.", "id_rsa.", "id_rsa::$DATA", ".ssh /config", ".aws./credentials"] {
            for tool in ["Read", "Write", "Edit"] {
                let v = decide(tool, &json!({ "file_path": f }), &c);
                assert!(matches!(v, Verdict::Deny(_)), "{tool} {f}: {v:?}");
            }
        }
        for cmd in [r#"cat ".env ""#, "cat .env::$DATA", "cp .env. out.txt", "cat certs/server.pem."] {
            deny(cmd);
        }
        assert!(matches!(decide("Read", &json!({ "file_path": ".env.example" }), &c), Verdict::Allow));
        assert!(matches!(decide("Read", &json!({ "file_path": "src/envelope.rs" }), &c), Verdict::Allow));
        // An 8.3 alias of a secret folder: the long path decides.
        let root = std::env::temp_dir().join(format!("dex-secret-83-{}", std::process::id()));
        let real = root.join(".ssh");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("config"), "x").unwrap();
        let alias = root.join("sshdir~1");
        if crate::worktree::link_dir(&real, &alias) {
            assert!(secret(&alias.join("config")), "8.3 alias of .ssh");
        } else {
            eprintln!("8.3 alias test unavailable on this host");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// #2361: the Glob `pattern` is judged as a path, not only its `path` argument.
    #[test]
    fn glob_pattern_is_judged_as_a_path() {
        let c = ctx();
        let glob = |pat: &str| decide("Glob", &json!({ "pattern": pat }), &c);
        for pat in [abs("/dex-fixture/.ssh/*"), "~/.ssh/*".into(), "../../.aws/**".into(), "**/.ssh/*".into(), "**/*.pem".into(), "secrets/**".into()] {
            assert!(matches!(glob(&pat), Verdict::Deny(_)), "{pat}: {:?}", glob(&pat));
        }
        for pat in [abs("/w/other/**"), "../other/*.txt".into()] {
            assert!(matches!(glob(&pat), Verdict::Leader(_)), "{pat}: {:?}", glob(&pat));
        }
        for pat in ["src/**/*.rs", "**/*.rs", "*.md", "crates/*/Cargo.toml"] {
            assert_eq!(glob(pat), Verdict::Allow, "{pat}");
        }
        // #2384: karakter sınıfı, brace ve boş eşleşen joker sır adını gizlemez.
        for pat in ["**/.ss[h]/*", "**/.ss[a-z]/*", "**/.ss[!x]/*", "{src,.ssh}/*", "**/.env*", "**/id_rs?"] {
            assert!(matches!(glob(pat), Verdict::Deny(_)), "{pat}: {:?}", glob(pat));
        }
        for pat in ["src/{a,b}/*.rs", "src/[ab].rs", "**/*.{ts,tsx}"] {
            assert_eq!(glob(pat), Verdict::Allow, "{pat}");
        }
        assert!(matches!(glob("[!a][!a][!a][!a]/x"), Verdict::Leader(_)), "{:?}", glob("[!a][!a][!a][!a]/x"));
    }

    /// #2384 (patron kararı): `scripts/hooks` ve `core.hooksPath` hedefi worker'a yazımda korumalı
    /// değil — birleştirmede `scripts/hooks/` yine patron onayı ister (worktree POLICY_FILES);
    /// worker'ın `git config core.hooksPath …` yazımı Deny kalır.
    #[test]
    fn repo_hooks_are_merge_policy_not_write_protected() {
        let c = ctx();
        assert_eq!(decide("Write", &json!({ "file_path": "scripts/hooks/pre-commit" }), &c), Verdict::Allow);
        deny("git config core.hooksPath x");
    }

    #[test]
    fn workers_cannot_skip_git_hooks() {
        for cmd in [
            "git commit --no-verify -m x",
            "git commit -n -m x",
            "git commit -an -m x",
            "git merge --no-verify topic",
            "git merge --squash --no-verify topic",
            "git rebase --no-verify topic",
            "git -C . commit --no-verify -m x",
            "git status; git commit --no-verify -m x",
            "echo $(git commit --no-verify -m x)",
        ] {
            deny(cmd);
        }
        allow("git commit -m 'mention --no-verify and -n'");
        allow("git commit -m '--no-verify'");
        allow("git commit -m x -- src/a.rs");
        allow("git commit -unormal -m x");
        leader("git merge topic");
        let c = Ctx { leader: true, ..ctx() };
        assert_eq!(decide("Bash", &json!({"command": "git commit --no-verify -m x"}), &c), Verdict::Allow);
    }

    #[test]
    fn workers_cannot_rewrite_git_objects() {
        for cmd in [
            "git replace HEAD other",
            "git replace --list",
            "git -C . replace --graft HEAD",
            "git --git-dir=.git replace HEAD other",
            "git update-ref refs/replace/fixture HEAD",
            "git update-ref -d refs/replace/fixture",
            "git update-ref --stdin",
            "git -c fixture.note=x update-ref refs/replace/fixture HEAD",
            "bash -c 'git replace HEAD other'",
            "sh -c 'git update-ref refs/replace/fixture HEAD'",
            "echo $(git replace HEAD other)",
        ] { deny(cmd); }
        for cmd in [
            "pwsh -Command 'git replace HEAD other'",
            "powershell -Command 'git update-ref refs/replace/fixture HEAD'",
        ] {
            assert!(matches!(decide("PowerShell", &json!({"command": cmd}), &ctx()), Verdict::Deny(_)), "{cmd}");
        }
        allow("git commit -m 'git replace refs/replace/fixture'");
        allow("git add -- refs/replace/fixture");
        leader("git update-ref refs/heads/topic HEAD");
        let c = Ctx { leader: true, ..ctx() };
        assert!(matches!(decide("Bash", &json!({"command": "git replace HEAD other"}), &c), Verdict::Leader(_)));
    }

    #[test]
    fn workers_cannot_write_git_grafts_even_with_a_broad_root() {
        let mut c = ctx();
        c.write.push(c.cwd.as_ref().unwrap().join(".git/info"));
        // Put the nested root first so ordinary relative-root protection cannot help.
        c.write.reverse();
        for path in [".git/info/grafts", ".git/info/grafts.", ".git/info/grafts::$DATA"] {
            for tool in ["Write", "Edit"] {
                assert!(matches!(decide(tool, &json!({"file_path": path}), &c), Verdict::Deny(_)), "{tool}: {path}");
            }
        }
        for cmd in [
            "echo fixture > .git/info/grafts",
            "cp fixture .git/info/grafts",
            "bash -c 'echo fixture > .git/info/grafts'",
        ] {
            assert!(matches!(decide("Bash", &json!({"command": cmd}), &c), Verdict::Deny(_)), "{cmd}");
        }
        assert!(matches!(decide("Write", &json!({"file_path": ".git/info/grafts", "content": "fixture"}), &c), Verdict::Deny(_)));
        for cmd in [
            "Set-Content -LiteralPath .git/info/grafts -Value fixture",
            "Add-Content .git/info/grafts fixture",
            "Out-File -FilePath .git/info/grafts",
            "pwsh -Command 'Set-Content -Path .git/info/grafts -Value fixture'",
        ] {
            assert!(matches!(decide("PowerShell", &json!({"command": cmd}), &c), Verdict::Deny(_)), "{cmd}");
        }
    }

    #[test]
    fn powershell_content_writes_check_the_target_after_input_values() {
        let mut c = ctx();
        c.write.insert(0, c.cwd.as_ref().unwrap().join(".git/info"));
        c.write.insert(0, c.cwd.as_ref().unwrap().join(".git/refs/replace"));
        for path in [".git/info/grafts", ".git/refs/replace/fixture"] {
            for cmd in [
                format!("Out-File -InputObject x {path}"),
                format!("Out-File -InputObject x -FilePath {path}"),
                format!("Out-File -InputObject x -Encoding utf8 -Width 80 {path}"),
                format!("Out-File -inPUTobject x -literalPATH {path}"),
                format!("pwsh -Command 'Out-File -InputObject x {path}'"),
                format!("powershell -Command 'Out-File -InputObject x -FilePath {path}'"),
                format!("Set-Content -Value x -Encoding utf8 {path}"),
                format!("Add-Content -Value x -Encoding utf8 {path}"),
                format!("sc -Value x {path}"),
                format!("ac -Value x {path}"),
            ] {
                let verdict = decide("PowerShell", &json!({"command": cmd}), &c);
                assert!(matches!(verdict, Verdict::Deny(_)), "{cmd}: {verdict:?}");
            }
        }
    }

    #[test]
    fn powershell_content_writes_do_not_bind_path_flags_inside_values() {
        for cmd in ["Out-File -InputObject", "Set-Content -Value", "Add-Content -Value", "sc -Value", "ac -Value"] {
            for value in ["-FilePath", "-Path", "-LiteralPath"] {
                let command = format!("{cmd} '{value}' -LiteralPath .git/info/grafts");
                let verdict = decide("PowerShell", &json!({"command": command}), &ctx());
                assert!(matches!(verdict, Verdict::Deny(_)), "{command}: {verdict:?}");
            }
        }
    }

    #[test]
    fn powershell_content_writes_skip_common_parameter_values() {
        for cmd in ["Out-File", "Set-Content", "Add-Content", "sc", "ac"] {
            for (param, value) in [
                ("-ErrorAction", "SilentlyContinue"), ("-ea", "SilentlyContinue"),
                ("-WarningAction", "Continue"), ("-wa", "Continue"),
                ("-InformationAction", "Continue"), ("-infa", "Continue"),
                ("-ProgressAction", "Continue"),
                ("-ErrorVariable", "fixture"), ("-ev", "fixture"),
                ("-WarningVariable", "fixture"), ("-wv", "fixture"),
                ("-InformationVariable", "fixture"), ("-iv", "fixture"),
                ("-OutVariable", "fixture"), ("-ov", "fixture"),
                ("-OutBuffer", "1"), ("-ob", "1"),
                ("-PipelineVariable", "fixture"), ("-pv", "fixture"),
            ] {
                let command = format!("{cmd} {param} {value} .git/info/grafts");
                let verdict = decide("PowerShell", &json!({"command": command}), &ctx());
                assert!(matches!(verdict, Verdict::Deny(_)), "{command}: {verdict:?}");
            }
        }
        for cmd in ["Set-Content", "Add-Content", "sc", "ac"] {
            let command = format!("{cmd} -Credential fixture .git/info/grafts -Value x");
            let verdict = decide("PowerShell", &json!({"command": command}), &ctx());
            assert!(matches!(verdict, Verdict::Deny(_)), "{command}: {verdict:?}");
        }
    }

    #[test]
    fn powershell_content_writes_bind_named_path_forms() {
        for path in [".git/info/grafts", ".git/refs/replace/fixture"] {
            for command in [
                format!("Out-File -InputObject:x -FilePath:{path}"),
                format!("Out-File -InputObject x -PSPath {path}"),
                format!("Out-File -InputObject x -LiteralPath:{path}"),
                format!("Set-Content -Value:x -Path:{path}"),
                format!("Add-Content -Value x -PSPath:{path}"),
            ] {
                let verdict = decide("PowerShell", &json!({"command": command}), &ctx());
                assert!(matches!(verdict, Verdict::Deny(_)), "{command}: {verdict:?}");
            }
        }
    }

    #[test]
    fn powershell_content_writes_keep_input_values_out_of_targets() {
        for command in [
            "Out-File -InputObject '.git/info/grafts' .dex/tmp/result.txt",
            "Out-File -InputObject '-FilePath' -LiteralPath .dex/tmp/result.txt",
            "Out-File -InputObject '-FilePath:.git/info/grafts' -FilePath .dex/tmp/result.txt",
            "Out-File -InputObject:x -Encoding:utf8 .dex/tmp/result.txt",
            "Set-Content -Value '.git/refs/replace/fixture' -Path .dex/tmp/result.txt",
            "Add-Content -Value '-LiteralPath' -LiteralPath .dex/tmp/result.txt",
            "Set-Content -Value:x -Encoding:utf8 .dex/tmp/result.txt",
            "Out-File -InputObject x -Append -Force -NoClobber .dex/tmp/result.txt",
        ] {
            let verdict = decide("PowerShell", &json!({"command": command}), &ctx());
            assert!(matches!(verdict, Verdict::Leader(_)), "{command}: {verdict:?}");
        }
    }

    #[test]
    fn workers_cannot_override_git_hooks() {
        for cmd in [
            "git -c core.hooksPath=disabled commit -m x",
            "git -ccore.hooksPath=disabled commit -m x",
            "git -c CoRe.HoOkSpAtH=disabled merge topic",
            "git --config-env core.hooksPath=HOOKS commit -m x",
            "git --config-env=core.hooksPath=HOOKS commit -m x",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=disabled git commit -m x",
            "GIT_CONFIG_PARAMETERS='core.hooksPath=disabled' git commit -m x",
            "export GIT_CONFIG_PARAMETERS='core.hooksPath=disabled'; git commit -m x",
            "bash -c 'git commit --no-verify -m x'",
            "sh -c 'git -c core.hooksPath=disabled commit -m x'",
        ] { deny(cmd); }
        for cmd in [
            "$env:GIT_CONFIG_KEY_0='core.hooksPath'; git commit -m x",
            "$env:GIT_CONFIG_PARAMETERS='core.hooksPath=disabled'; git commit -m x",
            "$env:GIT_CONFIG_KEY_0 = 'core.hooksPath'; git commit -m x",
            "Set-Item Env:GIT_CONFIG_PARAMETERS 'core.hooksPath=disabled'; git commit -m x",
        ] {
            assert!(matches!(decide("PowerShell", &json!({"command": cmd}), &ctx()), Verdict::Deny(_)), "{cmd}");
        }
        leader("git -c core.autocrlf=false commit -m x");
        leader("git -c fixture.note=core.hooksPath commit -m x");
        leader("git -c core.hooksPathExtra=x commit -m x");
        // #2361: writing the setting redirects every later commit's hooks.
        deny("git config core.hooksPath disabled");
        deny("git config --local core.hooksPath ./x");
        deny("git config core.hookspath x");
        leader("git config user.name x");
        leader("GIT_EXTERNAL_DIFF=./x git diff");
        leader("GIT_AUTHOR_EMAIL=alias@users.noreply.github.com git commit -m x");
        leader("GIT_CONFIG_GLOBAL=./config git status");
        allow("git commit -m 'core.hooksPath=disabled GIT_CONFIG_PARAMETERS=x'");
        allow("git commit -m 'GIT_CONFIG_PARAMETERS=core.hooksPath=disabled'");
    }

    #[test]
    fn only_leaders_may_push_server_master_as_a_whole_command() {
        let mut c = ctx();
        c.leader = true;
        c.team_root = c.cwd.clone();
        c.extra_push_roots.push(PathBuf::from(abs("/w/dex")));
        let mut commands = vec!["git push server master".to_string(), "git push server master:master".to_string()];
        for path in [".".to_string(), abs("/w/proj"), abs("/w/proj/sub/.."), format!("'{}'", abs("/w/proj")), format!("\"{}\"", abs("/w/proj"))] {
            for refspec in ["master", "master:master"] {
                commands.push(format!("git -C {path} push server {refspec}"));
            }
        }
        commands.push(format!("git -C {} push server master", abs("/w/dex")));
        if cfg!(windows) {
            commands.push("git -C /c/w/dex push server master".into());
            commands.push("git -C c:/W/DEX push server master".into());
        }
        for command in commands {
            assert_eq!(decide("Bash", &json!({ "command": command }), &c), Verdict::Allow, "{command}");
            c.leader = false;
            assert!(matches!(decide("Bash", &json!({ "command": command }), &c), Verdict::Deny(_)), "worker: {command}");
            c.leader = true;
        }
        // Write roots such as memory and scratchpads are not the team's repository root.
        c.write.push(PathBuf::from(abs("/w/memory")));
        for command in [
            "git push origin master", "git push https://example.com/repo master", "git push server main",
            "git push server HEAD", "git push server master:main", "git push server +master", "git push server +master:master",
            "git push server master main", "git push server", "git push",
            "git push --force server master", "git push -f server master", "git push --force-with-lease server master",
            "git push server master --force", "git push --delete server master", "git push --mirror server master",
            "git push --all server master", "git push --tags server master", "git push -o x server master",
            "git push --push-option=x server master", "git push --receive-pack=x server master", "git push --exec=x server master",
            "git -c x=y push server master", "git -c alias.p=push push server master",
            "git --config-env x=Y push server master", "git --no-pager push server master",
            "git -C . -C . push server master", "git -C . -c x=y push server master",
            "git push server master && git status", "git status && git push server master",
            "git push server master; git status", "git push server master | cat", "git push server master &",
            "(git push server master)", "echo $(git push server master)", "echo `git push server master`",
            "echo `git -C . push server master`", "echo $(git -C . push server master)",
            "bash -c 'git push server master'", "sh -c 'git push server master'", "pwsh -c 'git push server master'",
            "powershell -Command \"git push server master\"", "eval 'git push server master'",
            "git push server master\ngit status", "git push server master > out.txt", "git push server master 2>&1",
            "FOO=1 git push server master", "./git push server master", "git.exe push server master",
            "git -C '$HOME' push server master", "git -C .dex/tmp push server master",
        ] {
            for leader in [true, false] {
                c.leader = leader;
                let verdict = decide("Bash", &json!({ "command": command }), &c);
                assert!(matches!(verdict, Verdict::Deny(_)), "{command}, leader={leader}: {verdict:?}");
            }
        }
        c.leader = true;
        for path in [abs("/w/proj/sub"), abs("/w/proj/.."), abs("/w/other"), abs("/w/memory"), abs("/w/proj-evil")] {
            let command = format!("git -C {path} push server master");
            assert!(matches!(decide("Bash", &json!({ "command": command }), &c), Verdict::Deny(_)), "{command}");
        }
        c.extra_push_roots.clear();
        let command = format!("git -C {} push server master", abs("/w/dex"));
        assert!(matches!(decide("Bash", &json!({ "command": command }), &c), Verdict::Deny(_)), "unknown executable root");
        for wrapper in ["bash -c", "sh -c", "pwsh -c", "eval"] {
            for body in ["git -C . push server master", "git -c x=y push server master", "git  push server master"] {
                let command = format!("{wrapper} '{body}'");
                for leader in [true, false] {
                    c.leader = leader;
                    assert!(matches!(decide("Bash", &json!({ "command": command }), &c), Verdict::Deny(_)), "{command}, leader={leader}");
                }
            }
        }
    }

    /// #222: izinli push hedefleri — yalnız listedeki desene uyan literal URL, tek refspec,
    /// force ve riskli bayraklar kapalı. Liste boşken eski davranış (deny).
    #[test]
    fn allowed_push_targets_match_a_literal_url_only() {
        let url = "deploy@192.0.2.10:/srv/git/dex.git";
        let surl = "ssh://deploy@192.0.2.10/srv/git/dex.git";
        let mut c = ctx();
        c.push_allow = vec![
            "deploy@192.0.2.10:/srv/git/*.git".into(),
            "ssh://deploy@192.0.2.10/srv/git/*.git".into(),
        ];
        let verdict = |cmd: &str, c: &Ctx| decide("Bash", &json!({ "command": cmd }), c);
        for cmd in [
            format!("git push {url} master"),
            format!("git push {url} master:master"),
            format!("git push {surl} main"),
            format!("git push {url} team/dev"),
            format!("git push {url} HEAD"),
            format!("git push {url} master --tags"),
            format!("git push --tags {url} master"),
            format!("git push '{url}' master"),
            format!("git --no-pager push {url} master"),
            format!("git -C . push {url} master"),
        ] {
            assert_eq!(verdict(&cmd, &c), Verdict::Allow, "{cmd}");
        }
        // #225 orta: `-C ""` dizini korur, izinli kökte kalır.
        assert_eq!(verdict(&format!("git -C \"\" push {url} master"), &c), Verdict::Allow, "-C bos");
        for cmd in [
            // remote adı (git config'ten çözülür) ve server: hep red
            "git push yayin master".into(),
            "git push origin master".into(),
            "git push server master".into(),
            // listedede olmayan URL
            "git push https://github.com/x/y.git master".into(),
            "git push git@github.com:x/y.git master".into(),
            format!("git push deploy@192.0.2.11:/srv/git/dex.git master"),
            format!("git push deploy@192.0.2.10:/srv/other/dex.git master"),
            // `*` `/` geçmez: alt dizin eşleşmez
            format!("git push deploy@192.0.2.10:/srv/git/sub/dex.git master"),
            format!("git push ssh://deploy@192.0.2.10/srv/git/sub/dex.git master"),
            // URL'de `..`; `--tags` yalnız bir kez
            format!("git push deploy@192.0.2.10:/srv/git/../etc/x.git master"),
            format!("git push {url} master --tags --tags"),
            // sarmal ve zincir: bütün komut tek push değil
            format!("bash -c 'git push {url} master'"),
            format!("sh -c 'git push {url} master'"),
            format!("eval 'git push {url} master'"),
            format!("echo $(git push {url} master)"),
            format!("echo `git push {url} master`"),
            format!("(git push {url} master)"),
            format!("git push {url} master && git status"),
            format!("git push {url} master; git status"),
            format!("git push {url} master > out.txt"),
            // gri git bayrakları
            format!("git -c x=y push {url} master"),
            format!("git --config-env x=y push {url} master"),
            format!("git --git-dir=. push {url} master"),
            format!("git --work-tree=. push {url} master"),
            // riskli push bayrakları
            format!("git push --receive-pack=x {url} master"),
            format!("git push --exec=x {url} master"),
            format!("git push --mirror {url} master"),
            format!("git push --all {url} master"),
            format!("git push --delete {url} master"),
            format!("git push -o x {url} master"),
            format!("git push --push-option=x {url} master"),
            format!("git push --repo={url} master"),
            format!("git push --dry-run {url} master"),
            // force kapalı
            format!("git push -f {url} master"),
            format!("git push --force {url} master"),
            format!("git push --force-with-lease {url} master"),
            format!("git push {url} +master"),
            format!("git push {url} +master:master"),
            // refspec kuralları
            format!("git push {url}"),
            format!("git push {url} master main"),
            format!("git push {url} master:main"),
            // env öneki, git.exe, -C dışarısı
            format!("FOO=1 git push {url} master"),
            format!("git.exe push {url} master"),
            format!("git -C {} push {url} master", abs("/w/other")),
            // #225 orta: ardışık `-C` — Git bir öncekine göre çözer, tek `-C` şartı red
            format!("git -C .. -C . push {url} master"),
            format!("git -C {} -C . push {url} master", abs("/w/other")),
            // #225 düşük: `segs`'e yansımayan kuyruk sözdizimi (`syntax` bayrağı red)
            format!("git push {url} master;"),
            format!("git push {url} master &"),
            format!("git push {url} master 2>&1"),
        ] {
            assert!(matches!(verdict(&cmd, &c), Verdict::Deny(_)), "{cmd}: {:?}", verdict(&cmd, &c));
        }
        // Liste boşken izinli URL bile eski davranışa döner.
        c.push_allow.clear();
        for cmd in [format!("git push {url} master"), format!("git push {surl} main")] {
            assert!(matches!(verdict(&cmd, &c), Verdict::Deny(_)), "boş liste: {cmd}");
        }
    }

    #[test]
    fn git_dash_c_resolves_sequentially_like_git() {
        // #225 orta: eski `git()` ayrıştırıcısı her `-C`'yi `ctx.cwd`'den çözüyordu; Git
        // görelileri bir öncekine göre çözer, `-C ""` dizini korur.
        leader("git -C .. -C . status");
        leader(&format!("git -C {} -C . status", abs("/w/other")));
        allow("git -C \"\" status");
        allow("git -C . status");
    }

    #[test]
    fn push_glob_star_does_not_cross_slash_and_bad_patterns_never_match() {
        assert!(push_glob_match("deploy@192.0.2.10:/srv/git/*.git", "deploy@192.0.2.10:/srv/git/dex.git"));
        assert!(!push_glob_match("deploy@192.0.2.10:/srv/git/*.git", "deploy@192.0.2.10:/srv/git/a/b.git"));
        assert!(push_glob_match("deploy@192.0.2.10:/srv/git/*.git", "deploy@192.0.2.10:/srv/git/.git"), "`*` boş da tutar");
        assert!(push_glob_match("ssh://h/*.git", "ssh://h/x.git"));
        for bad in ["", "a/../b", "a?b", "a[b", "a b", "a$b"] {
            assert!(!push_glob_match(bad, "a"), "{bad}");
        }
    }

    /// #459: a worker's allow passes the #402 parser too — what `lex` misreads goes to the leader,
    /// its everyday commands (bash and PowerShell) stay allowed.
    #[test]
    fn worker_allow_passes_the_shell_parser() {
        let c = ctx();
        let run = |tool: &str, cmd: &str| decide(tool, &json!({ "command": cmd }), &c);
        // The two holes of #402: `& (…)` runs x.exe, Unicode quotes hide `Remove-Item`.
        for (tool, cmd) in [
            ("PowerShell", "git show HEAD:a | & (Get-ChildItem x.exe)"),
            ("PowerShell", "git status; & (Get-ChildItem x.exe)"),
            ("PowerShell", "Write-Output \"a” ; Remove-Item x ; ”b\""),
            ("PowerShell", "Write-Output ‘a’ ; Remove-Item x ; ‘b’"),
            ("PowerShell", "Write-Output 'a’ ; Start-Process x.exe ; ‘b'"),
            ("Bash", "git show HEAD:a | & (Get-ChildItem x.exe)"),
            // Not parsed: unterminated quote, Unicode blank, a `\.` that bash would join into `..`.
            ("PowerShell", "Write-Output \"a"),
            ("Bash", "echo a\u{2028}rm x"),
            ("Bash", r"echo x > a/b/.\./.\./.\./out.txt"),
        ] {
            let v = run(tool, cmd);
            assert!(matches!(v, Verdict::Leader(_)), "{tool}: {cmd}: {v:?}");
        }
        // Hard noes stay hard.
        assert!(matches!(run("PowerShell", "Write-Output “a” ; git push"), Verdict::Deny(_)));
        assert!(matches!(run("Bash", "echo x > /tmp/out.txt"), Verdict::Deny(_)));
        // Everyday work.
        for (tool, cmd) in [
            ("Bash", "cargo test -p dex-core --lib permit"),
            ("Bash", "cargo check"),
            ("Bash", "cargo clippy -p dex-core -- -D warnings"),
            ("Bash", "cd crates/dex-core && cargo test permit 2>&1 | tail -20"),
            ("Bash", "npm test"),
            ("Bash", "npm run build"),
            ("Bash", "npx tsc --noEmit && npm test"),
            ("Bash", "npx playwright test e2e/gpu-settings.spec.ts --workers=1"),
            ("Bash", "git status --short"),
            ("Bash", "git diff --stat"),
            ("Bash", "git log --oneline -5"),
            ("Bash", "git add -A && git commit -m \"feat(permit): worker kabuğu — ayrıştırıcıdan geçer\""),
            ("Bash", "git add crates/x.rs; git commit -m 'fix: x “y”'"),
            ("Bash", "cargo build > .dex/tmp/build.log 2>&1"),
            ("Bash", "echo x >> notes.txt"),
            ("Bash", "rg -n 'fn main' crates | head -5"),
            ("Bash", "cat Cargo.toml | grep version"),
            ("Bash", "ls -la src"),
            ("PowerShell", "cargo test -p dex-core --lib permit 2>&1 | Select-Object -Last 20"),
            ("PowerShell", "npm run build"),
            ("PowerShell", "npx tsc --noEmit; npm test"),
            ("PowerShell", "git add -A; git commit -m \"fix: x — y\""),
            ("PowerShell", "git commit -m “fix: akıllı tırnak”"),
            ("PowerShell", "Get-Content src/a.rs | Select-Object -First 5"),
            ("PowerShell", "Get-ChildItem src"),
            ("PowerShell", "cargo build *> build.log"),
            ("PowerShell", "& cargo test permit"),
        ] {
            assert_eq!(run(tool, cmd), Verdict::Allow, "{tool}: {cmd}");
        }
    }

    /// #480: what the parser finds denied stays denied, also behind a part it hands the leader;
    /// a quoted redirection target is read as the shell reads it.
    #[test]
    fn worker_parser_keeps_denies_and_reads_quoted_targets() {
        let c = ctx();
        let run = |tool: &str, cmd: &str| decide(tool, &json!({ "command": cmd }), &c);
        for cmd in [
            "Write-Output \"a” ; git push ; ”b\"",
            "Write-Output \"a” ; Remove-Item -Recurse . ; ”b\"",
            "Write-Output \"a” ; Start-Process x.exe ; git push ; ”b\"",
        ] {
            let v = run("PowerShell", cmd);
            assert!(matches!(v, Verdict::Deny(_)), "{cmd}: {v:?}");
        }
        let inside = [
            ("Bash", "cargo build > \".dex/tmp/build.log\""),
            ("Bash", "cargo build > \"build logs/build.log\" 2>&1"),
            ("Bash", "cargo build >> 'build logs/build.log'"),
            ("PowerShell", "cargo build > \".dex/tmp/build.log\""),
            ("PowerShell", "cargo build *> \"build logs/build.log\""),
            ("PowerShell", "cargo build > 'build logs/build.log'"),
            ("PowerShell", "cargo build > “build logs/build.log”"),
        ];
        for (tool, cmd) in inside {
            assert_eq!(run(tool, cmd), Verdict::Allow, "{tool}: {cmd}");
        }
        for tool in ["Bash", "PowerShell"] {
            for target in [format!("\"{}\"", abs("/w/other/build logs/x.log")), format!("'{}'", abs("/tmp/x.log")), "\"../other/x.log\"".into()] {
                let cmd = format!("cargo build > {target}");
                let v = run(tool, &cmd);
                assert!(matches!(v, Verdict::Deny(_)), "{tool}: {cmd}: {v:?}");
            }
            // Expansion in the quotes: not known.
            for cmd in ["cargo build > \"$HOME/x.log\"", "cargo build > \"a`b.log\""] {
                assert_ne!(run(tool, cmd), Verdict::Allow, "{tool}: {cmd}");
            }
        }
    }

    #[test]
    fn safe_commands_are_allowed() {
        for c in [
            "cargo test -p dex-core --lib permit",
            "cargo clippy -p dex-core -- -D warnings",
            "cargo +nightly fmt",
            "git status --short",
            "git -C . log --oneline -5",
            "git add crates/x.rs && git commit -m \"fix: x\"",
            "git stash list",
            "git worktree list",
            "git branch",
            "ls -la src | grep ts",
            "rg -n 'fn main' crates",
            "cat Cargo.toml",
            "cd crates/dex-core && cargo test permit 2>&1 | tail -20",
            "cargo build > build.log",
            "echo hi > /dev/null",
            "npx playwright test tests/x.spec.ts --workers=1",
            "pnpm exec playwright test --grep \"login\"",
            "pnpm test",
            "npx tsc --noEmit",
            "find . -name '*.rs'",
            "sed -n 1,20p src/lib.rs",
        ] {
            allow(c);
        }
    }

    #[test]
    fn secret_rule_skips_patterns_and_text_but_not_file_operands() {
        // #503: its unquoted globs are the leader's — but no secret deny.
        leader(r#"cat e2e/layout.spec.ts; grep -n "palette\|Control+K" -i e2e/*.ts | head; grep -rn "Ctrl\|key ===\|e.key" src/palette/*.tsx | head -5"#);
        for c in [
            "grep -n key src/x.ts",
            "grep -rn e.key src",
            "rg -e '.pem' -g '*.key' src",
            "grep -ne .key src/x.ts",
            "sed -n '/.key/p' src/x.ts",
            "echo .env id_rsa",
            "find . -name '*.pem'",
        ] {
            allow(c);
        }
        for c in [
            "grep TOKEN .env",
            "grep -rn foo .env",
            "grep -e foo .env",
            "grep -f .env x.txt",
            "rg foo secrets/",
            "rg -e foo -- .env",
            "sed -n p .env",
            "cat .env",
            "cat '.env'",
            "grep foo < .env",
        ] {
            deny(c);
        }
    }

    #[test]
    fn recursive_flag_of_non_delete_commands_is_not_bulk_delete() {
        for c in [
            r#""$ADB" install -r "C:\\fixtures\\user\\apk\\x.apk"; "$ADB" shell dumpsys package x | grep versionName"#,
            r#"$ADB install -r C:\fixtures\user\apk\x.apk"#,
            "grep -r foo src",
            "adb install -r app.apk",
            "cp -r a b",
            "rsync -r a/ b/",
        ] {
            assert!(!matches!(bash(c), Verdict::Deny(_)), "{c}: {:?}", bash(c));
        }
        allow("grep -r foo src");
        for c in ["rm -rf x", "Remove-Item -Recurse x", "del /s x", "rmdir /s x"] {
            deny(c);
        }
        // `git rm -r` is not a bulk-delete deny: it only unstages/removes tracked files (recoverable
        // from history), but it is still a deletion, so the leader decides.
        leader("git rm -r src");
    }

    /// #61: the destination of a copy/download/extract is a write; outside the folder it is denied,
    /// inside it still goes through the usual verdict (leader), and remote targets are not files.
    #[test]
    fn copy_and_download_destinations_are_writes() {
        let out = abs("/Users/user/apk/");
        let outf = abs("/Users/user/apk/x.apk");
        for c in [
            format!("scp build-host.example.invalid:~/x.apk {out}"),
            "scp -P 22 -i key build-host.example.invalid:~/x.apk ../apk/".to_string(),
            "scp build-host.example.invalid:~/x.apk ~/apk/".to_string(),
            format!("cp a.apk {out}"),
            format!("mv -f a.apk {out}"),
            format!("cp -t {out} a.apk"),
            format!("rsync -av build-host.example.invalid:~/b/ {out}"),
            format!("adb pull /sdcard/x.apk {out}"),
            format!("adb -s emu pull -a /sdcard/x.apk {outf}"),
            format!("curl -o {outf} https://x/y"),
            format!("curl -sSL --output={outf} https://x/y"),
            format!("curl --output-dir {out} -O https://x/y"),
            format!("wget -O {outf} https://x/y"),
            format!("wget -P {out} https://x/y"),
            format!("tar -xf a.tgz -C {out}"),
            format!("tar xzf a.tgz -C {out}"),
            format!("tar -czf {outf} src"),
            format!("Copy-Item a.apk {out}"),
            format!("Copy-Item -Path a.apk -Destination {out}"),
            format!("Move-Item a.apk {out} -Force"),
            format!("git clone https://x/y.git {out}"),
            format!("git clone --depth 1 -b main https://x/y.git {out}"),
            "git clone https://x/y.git ../elsewhere".to_string(),
            "cd .. && cp a b".to_string(),
        ] {
            deny(&c);
        }
        assert!(matches!(bash("scp build-host.example.invalid:~/x.apk ../x.apk"), Verdict::Deny(_)));
        for c in [
            "scp build-host.example.invalid:~/x.apk .dex/release/android/".to_string(),
            "adb pull /sdcard/x.apk .dex/release/android/".to_string(),
            "adb pull /sdcard/x.apk".to_string(),
            "curl -o .dex/tmp/y.json https://x/y".to_string(),
            "curl -O https://x/y".to_string(),
            "curl -o /dev/null -s https://x/y".to_string(),
            "wget -qO- https://x/y".to_string(),
            "Copy-Item a.apk .dex/release/android/".to_string(),
            "cp -r a .dex/tmp/b".to_string(),
            "tar -xf a.tgz -C .dex/tmp/x".to_string(),
            "tar -xf C:/elsewhere/a.tgz".to_string(),
            "git clone https://x/y.git .dex/tmp/y".to_string(),
            "git clone https://x/y.git".to_string(),
            // remote targets are not files here
            "scp a.apk build-host.example.invalid:~/".to_string(),
            "rsync -av a/ build-host.example.invalid:~/a/".to_string(),
            format!("scp {outf} user@host:/srv/"),
        ] {
            assert!(!matches!(bash(&c), Verdict::Deny(_)), "{c}: {:?}", bash(&c));
        }
    }

    #[test]
    fn dangerous_commands_are_denied_even_inside_a_chain() {
        for c in [
            "git push",
            "git push origin team/dev",
            "cargo test && git push",
            "git status; git push --force",
            "ls | xargs echo; git reset --hard HEAD~1",
            "git clean -fdx",
            "rm -rf target",
            "rm -r -f docs",
            "rm *.log",
            "Remove-Item -Recurse build",
            "cat .env",
            "cat ~/.ssh/id_rsa",
            "echo x > ../outside.txt",
            "cargo build > ../../elsewhere/log.txt",
            "npx playwright test",
            "pnpm exec playwright test --workers=2",
        ] {
            deny(c);
        }
        assert!(matches!(bash(&format!("echo x >> {}", abs("/w/other/a.txt"))), Verdict::Deny(_)));
    }

    #[test]
    fn everything_else_goes_to_the_leader() {
        for c in [
            "curl https://example.com",
            "npm install left-pad",
            "cargo install ripgrep",
            "rm notes.txt",
            "git checkout main",
            "git branch -D old",
            "git stash",
            "cargo test $(echo --lib)",
            "cargo test `echo --lib`",
            "cat $HOME/x",
            "diff <(ls a) <(ls b)",
            "cat <<EOF\nhi\nEOF",
            "cd .. && ls",
            "python script.py",
            "find . -name x -delete",
            "sed -i s/a/b/ f.txt",
            "echo \"unclosed",
        ] {
            leader(c);
        }
        leader(&format!("cat {}", abs("/etc/hosts")));
    }

    /// #53: a heredoc body is the command's text, not shell; only the command line is judged.
    #[test]
    fn heredoc_body_is_not_parsed_as_shell() {
        // The live refusal: `)` / `>` / `;` in the body read as redirects and paths.
        let body = "import re\ns = s.replace(\"\"\"struct Reset { a: Vec<u8> }\"\"\", \"x\")\nprint(f(a) > b); x = C:\\)));\nassert a > ../outside.txt\ngit push\nrm -rf target\n";
        for tag in ["<<'EOF'", "<<EOF", "<<\"EOF\"", "<<-EOF", "<< EOF"] {
            let v = bash(&format!("python - {tag}\n{body}EOF"));
            assert!(matches!(&v, Verdict::Leader(_)), "{tag}: {v:?}");
        }
        // Commands before and after the heredoc are still judged.
        deny("git push\ncat <<EOF\nhi\nEOF");
        deny("cat <<EOF\nhi\nEOF\ngit push");
        deny("cat <<EOF; git push\nhi\nEOF");
        // The command line's own redirect is judged.
        leader(&format!("cat <<EOF > {}\nhi\nEOF", abs("/w/proj/x")));
        deny(&format!("cat <<EOF > {}\nhi\nEOF", abs("/w/other/x")));
        deny("cat <<EOF > ../out\nhi\nEOF");
        // `<<-` strips tabs before the closing word; a body line after the close is shell again.
        deny("cat <<-EOF\n\thi\n\tEOF\ngit push");
        deny("cat <<EOF\nEOF\ngit push");
        // Not closed (or closing word only as a prefix): the leader decides, as before.
        leader("cat <<EOF\nhi");
        leader("cat <<EOF\nhi\nEOFX");
        leader("cat <<EOF");
    }

    #[test]
    fn grants_match_exactly_once_or_for_the_job_and_session() {
        let p = Permits::default();
        let a = p.ask("kaan", "Bash: curl x");
        assert_eq!(p.ask("kaan", "Bash: curl x"), a, "same pending ask, same number");
        assert_eq!(p.take("kaan", "Bash: curl x", Some(7)), None, "not granted yet");
        p.grant(a, Scope::Once, Some(7)).unwrap();
        assert!(p.grant(a, Scope::Once, Some(7)).is_none(), "granted once");
        assert_eq!(p.take("kaan", "Bash: curl x && git push", Some(7)), None, "exact only");
        assert_eq!(p.take("ayla", "Bash: curl x", Some(7)), None, "that worker only");
        assert_eq!(p.take("kaan", "Bash: curl x", Some(7)), Some(a));
        assert_eq!(p.take("kaan", "Bash: curl x", Some(7)), None, "used up");
        // Session: repeats within the job; ends with the job, or with the session.
        let b = p.ask("kaan", "Bash: make");
        p.grant(b, Scope::Session, Some(7)).unwrap();
        assert_eq!(p.take("kaan", "Bash: make", Some(7)), Some(b));
        assert_eq!(p.take("kaan", "Bash: make", Some(7)), Some(b));
        assert_eq!(p.take("kaan", "Bash: make", None), None, "job over");
        assert_eq!(p.take("kaan", "Bash: make", Some(7)), None, "gone for good");
        let c = p.ask("kaan", "Bash: make");
        p.grant(c, Scope::Session, Some(8)).unwrap();
        p.forget("kaan");
        assert_eq!(p.take("kaan", "Bash: make", Some(8)), None, "session over");
    }

    #[test]
    fn audit_text_masks_credentials() {
        let r = redact("curl -H Authorization: Bearer abc123 https://x?token=zzz GITHUB_TOKEN=ghp_123 sk-abcdef ok");
        assert!(!r.contains("abc123") && !r.contains("zzz") && !r.contains("ghp_123") && !r.contains("sk-abcdef"), "{r}");
        assert!(r.contains("curl") && r.contains("ok"), "{r}");
        assert_eq!(redact("cargo test -p dex-core"), "cargo test -p dex-core");
    }

    /// #35: the session's own scratchpad is writable; a sibling session's and %TEMP% are not.
    #[test]
    fn only_this_sessions_scratchpad_is_writable() {
        let temp = abs("/tmp/claude");
        let sp = scratchpad(&format!("{temp}/C--w-proj/s1/scratchpad")).unwrap();
        let mut c = ctx();
        c.write.push(sp.clone());
        c.read.push(sp);
        let write = |p: String| decide("Write", &json!({ "file_path": p }), &c);
        assert_eq!(write(format!("{temp}/C--w-proj/s1/scratchpad/notes.py")), Verdict::Allow);
        assert!(matches!(write(format!("{temp}/C--w-proj/s2/scratchpad/notes.py")), Verdict::Deny(_)), "sibling session");
        assert!(matches!(write(format!("{temp}/C--w-proj/s1/x.py")), Verdict::Deny(_)), "the session dir itself");
        assert!(matches!(write(abs("/tmp/x.py")), Verdict::Deny(_)), "%TEMP% root");
        assert!(matches!(write(format!("{temp}/C--w-proj/s1/scratchpad/../../x.py")), Verdict::Deny(_)), "climbing out");
        // Only a real scratchpad path is taken as a root.
        for bad in [abs("/tmp"), abs("/tmp/scratchpad"), format!("{temp}/scratchpad"), "scratchpad".into(), format!("{temp}/C--w-proj/s1")] {
            assert!(scratchpad(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn qa_may_run_the_whole_suite() {
        let mut c = ctx();
        c.qa = true;
        assert_eq!(decide("Bash", &json!({ "command": "npx playwright test" }), &c), Verdict::Allow);
    }

    #[test]
    fn leader_runs_no_e2e_test() {
        let spec = "npx playwright test e2e/x.spec.ts --workers=1";
        assert_eq!(decide("Bash", &json!({ "command": spec }), &ctx()), Verdict::Allow, "worker: single spec");
        let mut c = ctx();
        c.leader = true;
        for command in [spec, "npx playwright test --grep \"ad\"", "npx playwright test"] {
            match decide("Bash", &json!({ "command": command }), &c) {
                Verdict::Deny(why) => assert!(why.starts_with(crate::test_budget::DENY_LEADER), "{command}: {why}"),
                v => panic!("{command}: {v:?}"),
            }
        }
        for command in ["npx tsc --noEmit", "cargo test -p dex-core --lib x"] {
            assert_eq!(decide("Bash", &json!({ "command": command }), &c), Verdict::Allow, "{command}");
        }
        // #1879: the e2e rule adds to the leader's full-suite budget, it does not replace it.
        for (command, reason) in [
            ("cargo test", crate::test_budget::DENY_RUST),
            ("godot -s addons/gut/gut_cmdln.gd -gdir=res://tests -ginclude_subdirs", crate::test_budget::DENY_GUT),
        ] {
            match decide("Bash", &json!({ "command": command }), &c) {
                Verdict::Deny(why) => assert!(why.starts_with(reason), "{command}: {why}"),
                v => panic!("{command}: {v:?}"),
            }
        }
    }

    #[test]
    fn file_tools_stay_in_the_folder() {
        let c = ctx();
        let t = |tool: &str, p: &str| decide(tool, &json!({ "file_path": p }), &c);
        assert_eq!(t("Edit", "src/a.rs"), Verdict::Allow);
        assert_eq!(t("Write", &abs("/w/proj/docs/n.md")), Verdict::Allow);
        assert!(matches!(t("Write", &abs("/w/other/x.rs")), Verdict::Deny(m) if m.contains("başka takım")));
        assert!(matches!(t("Write", &abs("/w/proj/../x.rs")), Verdict::Deny(m) if m.contains("dışına")));
        assert!(matches!(t("Edit", ".env"), Verdict::Deny(_)));
        assert_eq!(t("Edit", ".env.example"), Verdict::Allow);
        assert_eq!(t("Read", "src/a.rs"), Verdict::Allow);
        assert!(matches!(t("Read", &abs("/etc/hosts")), Verdict::Leader(_)));
        assert!(matches!(t("Read", "config/secrets/db.txt"), Verdict::Deny(_)));
        assert!(matches!(t("Read", &abs(&format!("/u/AppData/Roaming/{}/dexd.local.token", crate::desktop_storage::LEGACY_IDENTIFIER))), Verdict::Deny(_)));
        assert_eq!(decide("mcp__dex__report", &json!({}), &c), Verdict::Allow);
        assert!(matches!(decide("WebFetch", &json!({ "url": "https://x" }), &c), Verdict::Leader(_)));
    }

    #[test]
    fn msys_paths_translate_to_windows_drives() {
        assert_eq!(msys("/c/w/proj/a.rs").as_deref(), Some(r"C:\w\proj\a.rs"));
        assert_eq!(msys("/D/x").as_deref(), Some(r"D:\x"));
        assert_eq!(msys("/c").as_deref(), Some(r"C:\"));
        assert_eq!(msys("/c/").as_deref(), Some(r"C:\"));
        for no in ["/usr/bin/x", "/etc/hosts", "/cc/x", "/", "c/x", "src/a.rs", "C:/w/x", "./c/x"] {
            assert_eq!(msys(no), None, "{no}");
        }
    }

    /// Windows' Git Bash: `/c/…` is `C:\…`, not `C:\c\…` (#39).
    #[cfg(windows)]
    #[test]
    fn git_bash_paths_are_judged_where_they_point() {
        // The reported case: cd + heredoc write inside the folder. The path is no longer the
        // reason; the heredoc still goes to the leader, as before.
        let v = bash("cd /c/w/proj; cat > src/Snip.tsx <<'EOF'\nx\nEOF");
        assert!(matches!(&v, Verdict::Leader(_)), "{v:?}");
        let v = bash("cat > /c/w/proj/src/Snip.tsx <<'EOF'\nx\nEOF");
        assert!(matches!(&v, Verdict::Leader(_)), "{v:?}");
        for c in [
            "cd /c/w/proj && cargo test permit",
            "cd /c/w/proj/crates; git status",
            "echo x > /c/w/proj/out.txt",
            "echo x > /C/w/proj/out.txt",
            "cat /c/w/proj/Cargo.toml",
            "echo x > C:/w/proj/out.txt",
            r"echo x > C:\w\proj\out.txt",
            "echo x > ./out.txt",
            "echo x > src/out.txt",
            "echo x > /c/w/proj/a/../out.txt",
        ] {
            allow(c);
        }
        for c in [
            "echo x > /d/w/proj/out.txt",
            "echo x > /c/w/out.txt",
            "echo x > /c/w/proj/../out.txt",
            "echo x > /c/w/other/out.txt",
            "echo x > /c/out.txt",
            "echo x > /tmp/out.txt",
            "echo x > /usr/out.txt",
            "echo x > C:/w/out.txt",
            "cat /c/w/proj/.env",
            "cat /c/w/proj/.ssh/id_rsa",
        ] {
            deny(c);
        }
        leader("cd /d/w/proj && ls");
        leader("cat /d/w/proj/a.txt");
        let c = ctx();
        let t = |tool: &str, p: &str| decide(tool, &json!({ "file_path": p }), &c);
        assert_eq!(t("Write", "/c/w/proj/src/a.rs"), Verdict::Allow);
        assert!(matches!(t("Write", "/d/w/proj/src/a.rs"), Verdict::Deny(m) if m.contains("D:\\w\\proj")));
    }

    /// #164 P2: agent/git config inside the folder is never written — file tools, shell.
    #[test]
    fn agent_and_git_config_is_protected() {
        let c = ctx();
        let t = |tool: &str, p: &str| decide(tool, &json!({ "file_path": p }), &c);
        for p in [
            ".claude/settings.local.json",
            ".claude/settings.json",
            ".Claude/hooks/x.sh",
            ".mcp.json",
            "CLAUDE.md",
            "CLAUDE.local.md",
            "crates/x/claude.md",
            "AGENTS.md",
            "sub/AGENTS.md",
            ".git/hooks/pre-commit",
            ".git/config",
            "sub/.git",
            ".codex/config.toml",
            ".vscode/tasks.json",
            "CLAUDE.md.",
            "CLAUDE.md::$DATA",
        ] {
            for tool in ["Write", "Edit"] {
                assert!(matches!(t(tool, p), Verdict::Deny(ref m) if m.contains("korumalı")), "{tool} {p}: {:?}", t(tool, p));
            }
        }
        for p in [".vscode/settings.json", "docs/claude.mdx", "agents.md.bak/x", ".github/workflows/ci.yml", "src/git.rs", "claude/x.md"] {
            assert_eq!(t("Write", p), Verdict::Allow, "{p}");
        }
        for cmd in [
            "echo x > .claude/settings.json",
            "echo x >> CLAUDE.md",
            "cargo test > .git/hooks/post-checkout",
            "cp hook.sh .git/hooks/pre-commit",
            "curl -o .mcp.json https://x/y",
            "echo x >&AGENTS.md",
        ] {
            deny(cmd);
        }
        // git's own writes to .git stay allowed.
        allow("git add a.rs && git commit -m x");
        // Judged below the root: a worktree under `.claude/worktrees` is a normal folder.
        let wt = if cfg!(windows) { PathBuf::from(r"C:\w\proj\.claude\worktrees\a") } else { PathBuf::from("/w/proj/.claude/worktrees/a") };
        let c2 = Ctx { write: vec![wt.clone()], read: vec![wt.clone()], others: vec![], cwd: Some(wt), qa: false, ..Default::default() };
        assert_eq!(decide("Write", &json!({ "file_path": "src/a.rs" }), &c2), Verdict::Allow);
        assert!(matches!(decide("Write", &json!({ "file_path": ".claude/settings.json" }), &c2), Verdict::Deny(_)));
    }

    /// #164 P4a: `~` and the home variables point at the home, not at a folder in the cwd.
    #[test]
    fn home_paths_point_at_the_home() {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap();
        assert_eq!(expand_home("~").as_deref(), Some(home.as_str()));
        assert_eq!(expand_home("~/a").as_deref(), Some(format!("{home}/a").as_str()));
        assert_eq!(expand_home("$HOME/a").as_deref(), Some(format!("{home}/a").as_str()));
        assert_eq!(expand_home("${USERPROFILE}\\a").as_deref(), Some(format!("{home}\\a").as_str()));
        assert!(expand_home("~bob/a").is_some_and(|p| !p.starts_with('~') && p.ends_with("bob/a")));
        for no in ["$HOMEX/a", "a/~", "src/x", "$HOME_DIR"] {
            assert_eq!(expand_home(no), None, "{no}");
        }
        for c in [
            "echo x >> ~/.bashrc",
            "echo x > ~/.claude/settings.json",
            "echo x > $HOME/.bashrc",
            "echo x > ${HOME}/x",
            "echo x > %USERPROFILE%\\x",
            "echo x > $env:USERPROFILE\\x",
            "cp a ~bob/x",
            "cat ~/.ssh/config",
        ] {
            deny(c);
        }
        // Reading the home is not the folder: the leader decides.
        leader("cat ~/.config/gh/hosts.yml");
        leader("ls ~");
        let c = ctx();
        assert!(matches!(decide("Write", &json!({ "file_path": "~/.bashrc" }), &c), Verdict::Deny(_)));
        assert!(matches!(decide("Read", &json!({ "file_path": "~/notes.txt" }), &c), Verdict::Leader(_)));
    }

    /// #164 P4b/c: `>&word` and `<>` write the file; `>&2`, `2>&1`, `>&-` stay fd duplication.
    #[test]
    fn fd_duplication_is_not_a_file_but_word_targets_are() {
        for c in ["echo x >&../out.txt", "echo x >& ../out.txt", "echo x 1<>../out.txt", "cat <> ../out", "ls 2>&1 >&../o"] {
            deny(c);
        }
        for c in ["cargo test permit 2>&1 | tail -20", "echo x >&2", "echo x 1>&2", "ls >&-", "echo x >&out.txt", "cat <>out.txt", "ls 2>&1>out.txt"] {
            allow(c);
        }
    }

    /// #164 P4d/e: a program by path or with an env prefix is not the allowlisted command.
    #[test]
    fn path_commands_and_env_prefixes_go_to_the_leader() {
        for c in [
            "./ls.cmd",
            "./cat x",
            "tools/cat x",
            r"tools\git.exe status",
            "C:/x/git.exe status",
            "GIT_EXTERNAL_DIFF=./x.sh git diff",
            "NODE_OPTIONS='--require ./x.js' pnpm test",
            "RUSTC_WRAPPER=./w cargo build",
            "env cargo test permit",
            "CDPATH=/c cd x",
            "PATH=.",
            "PATH=. ; ls",
        ] {
            leader(c);
        }
        // Hard noes stay hard.
        for c in ["./git push", "FOO=1 git push", "env rm -rf x", "/usr/bin/git reset --hard"] {
            deny(c);
        }
        allow(r"\git status");
    }

    /// #164 P4f: allowlisted readers with a write/run side door.
    #[test]
    fn side_doors_of_allowlisted_commands_go_to_the_leader() {
        for c in [
            "git -c core.fsmonitor=./x status",
            "git --config-env core.pager=X log",
            "git --exec-path=./x status",
            "git --git-dir=../other/.git log",
            "git diff --output=../x.patch",
            "git log --output x.txt",
            "git grep -Ovim foo",
            "git grep --open-files-in-pager=vim foo",
            "rg --pre ./x foo",
            "rg --pre=./x foo src",
            "sort -o out.txt a.txt",
            "sort -ro out.txt a.txt",
            "sort --output=out.txt a.txt",
            "sort --compress-program=./x a.txt",
            "sed 's/a/b/w out' f.txt",
            "sed -n '1e id' f.txt",
            "sed -e 'w x' f.txt",
            "sed '/x/W y' f.txt",
            "sed -ne 's/a/b/e' f.txt",
            "sed -f s.sed f.txt",
            "sed --expression='r .netrc' f.txt",
            "sed 'r x.txt' f.txt",
            "sed '1!G;h;$!d;w o' f.txt",
            "sed 's/a/b' f.txt",
            "tree -o out.txt",
            "uniq a.txt b.txt",
        ] {
            leader(c);
        }
        deny("git -c x=y push");
        // #2286: an alias body is a command the worker runs — a push behind it is a push.
        deny("git -c alias.p=push p");
        for c in [
            "git --no-pager log -3",
            "git -C . diff --stat",
            "git grep -n foo",
            "rg --pretty foo src",
            "sort -u a.txt",
            "sort -k2 -t, a.txt",
            "sed -n 1,20p src/lib.rs",
            "sed -n '/.key/p' src/x.ts",
            "sed 's/a/b/g' f.txt",
            "sed -E 's/(a|b)/\\1/' f.txt",
            "sed -n '$=' f.txt",
            "sed 's|/|-|g' f.txt",
            "sed '/^#/d;s/x/y/2' f.txt",
            "sed -n '/start/,/end/{p}' f.txt",
            "sed '1!G;h;$!d' f.txt",
            "sed --sandbox 'w x' f.txt",
            "tree -L 2 src",
            "uniq -c a.txt",
        ] {
            allow(c);
        }
    }

    /// #2286: sert yasaklar sarmalayıcı adından bağımsız. Gri segmentin argümanları gövde olarak
    /// yalnız Deny için okunur; izinli komutun argümanı veridir, git'te yalnız config gövdesi
    /// okunur, kuyruk taraması ilk izinli komutta durur.
    #[test]
    fn hard_noes_survive_any_wrapper() {
        for cmd in [
            r#"cmd /c "git push origin HEAD""#,
            r#"cmd.exe /c "git commit -n -m x""#,
            r#"cmd /k "git reset --hard""#,
            r#"cmd.exe /c "git clean -fd""#,
            "git -c alias.p='!git push origin HEAD' p",
            "git -c alias.p=push p",
            "git -cnote.x=1 -c alias.p='!git push' p",
            "git config alias.p '!git push origin HEAD'",
            "git -C . -c alias.p=push p",
            "echo origin HEAD | xargs git push",
            "xargs -t git push origin HEAD",
            r#"node -e "require('child_process').execSync('git push origin HEAD')""#,
            r#"python -c "import os; os.system('git push origin HEAD')""#,
            r#"node -e "['git','push']""#,
            "timeout 5 git push origin HEAD",
            "cmd /c \"git replace HEAD other\"",
            // #2384: cmd'nin `^` kaçışı.
            "cmd /c g^it push origin HEAD",
            r#"cmd /c "g^i^t pu^sh origin HEAD""#,
            r#"node -e "require('child_process').execSync('cmd /c g^it push')""#,
        ] {
            deny(cmd);
        }
        // Gövde değil veri: izinli komutun argümanları, kuyrukta ilk izinli komuttan sonrası,
        // git'in config dışı operandları (`git rm -r src` toplu silme sayılmaz).
        for cmd in [
            "cmd /c echo git push",
            "node scripts/build.mjs",
            "git rm -r src",
            "git -c user.email=x@y commit -m x",
        ] {
            leader(cmd);
        }
        allow("git commit -m 'git push'");
        allow("echo rm -rf x");
        leader("cmd /c cargo test");
        // PowerShell'un Unicode tırnağı: gövde sözcüğünü yalnız gerçek okuyucu (parsed) görür.
        for cmd in ["cmd /c 'git push origin HEAD'", "cmd /c ‘git push origin HEAD’", "cmd /c ‘git commit -n -m x’"] {
            assert!(matches!(decide("PowerShell", &json!({ "command": cmd }), &ctx()), Verdict::Deny(_)), "{cmd}");
        }
    }

    /// #2384: gövde taraması bütçe ya da derinlik tükenince worker için fail-closed — dolgu ya da
    /// iç içe sarmalama sert yasağı onaylanabilir griye geri düşürmez; lider eskisi gibi gri görür.
    #[test]
    fn an_unread_body_is_a_workers_hard_no() {
        let lead = Ctx { leader: true, ..ctx() };
        let run = |c: &str, x: &Ctx| decide("Bash", &json!({ "command": c }), x);
        let short = |v: Verdict| matches!(v, Verdict::Deny(ref w) if w.starts_with(BODY_SHORT));
        let push = r#""git push origin HEAD""#;
        // Aynı sözcükle dolgu: bir kez okunur, push bulunur.
        let same = format!("cmd /c {} {push}", vec!["a"; 4000].join(" "));
        assert!(matches!(run(&same, &ctx()), Verdict::Deny(ref w) if w.starts_with("git push")), "{:?}", run(&same, &ctx()));
        // Farklı sözcüklerle dolgu bütçeyi bitirir: push okunmadan kalır, worker için yine Deny.
        let distinct = format!("cmd /c {} {push}", (0..BODY_BUDGET).map(|i| format!("a{i}")).collect::<Vec<_>>().join(" "));
        assert!(short(run(&distinct, &ctx())));
        assert!(matches!(decide("PowerShell", &json!({ "command": distinct }), &ctx()), Verdict::Deny(_)));
        assert!(matches!(run(&distinct, &lead), Verdict::Leader(_)), "{:?}", run(&distinct, &lead));
        // İç içe `cmd /c`: dört kat okunur (push bulunur), beşinci kat okunmaz (Deny yine).
        let nest = |n: usize| (0..n).fold("git push origin HEAD".to_string(), |body, _| {
            format!("cmd /c \"{}\"", body.replace('\\', "\\\\").replace('"', "\\\""))
        });
        assert!(matches!(run(&nest(4), &ctx()), Verdict::Deny(ref w) if w.starts_with("git push")), "{:?}", run(&nest(4), &ctx()));
        assert!(short(run(&nest(5), &ctx())), "{}", nest(5));
        assert!(matches!(run(&nest(5), &lead), Verdict::Leader(_)), "{:?}", run(&nest(5), &lead));
        // Meşru uzun gri komut: her metin bir kez okunur — yüzlerce argüman gri kalır (eskiden
        // tarama üsteldi, ~20 argüman bütçeyi bitiriyordu).
        let files = |n: usize| (0..n).map(|i| format!("src/f{i}.ts")).collect::<Vec<_>>().join(" ");
        leader(&format!("node scripts/fmt.mjs {}", files(300)));
        // Bilinen yanlış pozitif (bilerek): bütçeyi aşan zararsız gri komut worker'da Deny (kısa
        // dosya adlarıyla ~500+ argüman; kuyruklar baytı karesel büyütür). Bu boyda bir gri komut
        // gerçekte nadir; okunmamış gövdeyi geçirmek atlatmayı geri açar. Lider için gri kalır;
        // worker argümanları bölerek ya da bir dosya listesiyle koşar.
        let long = format!("node scripts/fmt.mjs {}", files(1000));
        assert!(short(run(&long, &ctx())));
        assert!(matches!(run(&long, &lead), Verdict::Leader(_)));
    }

    #[test]
    fn code_review_worker_is_read_only_and_others_are_unchanged() {
        let reviewer = Ctx { read_only: true, push_allow: vec!["https://example.com/*".into()], ..ctx() };
        let worker = Ctx { push_allow: vec!["https://example.com/*".into()], ..ctx() };
        let run = |c: &str, x: &Ctx| decide("Bash", &json!({ "command": c }), x);
        let file = abs("/w/proj/src/a.rs");
        for (tool, input) in [
            ("Write", json!({ "file_path": file, "content": "x" })),
            ("Edit", json!({ "file_path": file, "old_string": "a", "new_string": "b" })),
            ("MultiEdit", json!({ "file_path": file, "edits": [] })),
            ("NotebookEdit", json!({ "notebook_path": abs("/w/proj/n.ipynb") })),
        ] {
            assert!(matches!(decide(tool, &input, &reviewer), Verdict::Deny(ref w) if w.starts_with(READ_ONLY)), "{tool}");
            assert_eq!(decide(tool, &input, &worker), Verdict::Allow, "{tool}");
        }
        // Reads, read-only git and the dex tools stay open.
        for c in ["git diff main...HEAD", "git log --oneline -5", "git show HEAD:src/a.rs", "git status", "git blame src/a.rs",
            "git grep -n foo", "git branch -a", "git stash list", "cat src/a.rs | head -20", "rg -n 'fn main' crates",
            "git diff 2>/dev/null", "cd crates && git log -1"] {
            assert_eq!(run(c, &reviewer), Verdict::Allow, "{c}");
            assert_eq!(run(c, &worker), Verdict::Allow, "{c}");
        }
        assert_eq!(decide("mcp__dex__report", &json!({ "delegationId": 1 }), &reviewer), Verdict::Allow);
        assert_eq!(decide("mcp__dex__ask_leader", &json!({ "question": "?" }), &reviewer), Verdict::Allow);
        assert_eq!(decide("Read", &json!({ "file_path": file }), &reviewer), Verdict::Allow);
        assert_eq!(decide("ToolSearch", &json!({ "query": "select:mcp__dex__report" }), &reviewer), Verdict::Allow);
        // What the policy allows a worker but changes the repo or the disk: a hard no for the reviewer.
        for c in ["git commit -m x", "git add src/a.rs", "git branch topic", "echo x > f", "echo x >> f", "cat a.rs > b.rs",
            "cargo test -p dex-core permit", "pnpm test", "npx tsc --noEmit", "npx playwright test e2e/a.spec.ts",
            "(git commit -m x)", "(echo x > f)", "git diff && git commit -am x", "git push https://example.com/r.git HEAD:dev"] {
            assert!(matches!(run(c, &reviewer), Verdict::Deny(_)), "{c}: {:?}", run(c, &reviewer));
            assert!(!matches!(run(c, &worker), Verdict::Deny(_)) || c.starts_with("git push"), "{c}: {:?}", run(c, &worker));
        }
        assert!(matches!(run("cargo test", &reviewer), Verdict::Deny(ref w) if w.contains("test koşmaz")));
        // Out of policy for anyone: the leader decides for a worker, the reviewer is denied (never the session).
        for c in ["rm src/a.rs", "mv a b", "cp a b", "git checkout -b x", "git reset HEAD~1", "git merge dev", "git stash",
            "git rebase main", "touch f", "mkdir d", "tee f", "sed -i s/a/b/ f", "pip install x", "curl -o f https://x",
            "echo $(git commit -m x)", "FOO=1 git diff", "./tools/cat a", "powershell -c 'x'", "git diff --output=f"] {
            assert!(matches!(run(c, &reviewer), Verdict::Deny(ref w) if w.starts_with("salt okuma")), "{c}: {:?}", run(c, &reviewer));
            assert!(!matches!(run(c, &worker), Verdict::Allow), "{c}: {:?}", run(c, &worker));
        }
        assert!(matches!(decide("WebFetch", &json!({ "url": "https://x" }), &reviewer), Verdict::Deny(_)));
        assert!(matches!(decide("Read", &json!({ "file_path": abs("/elsewhere/a") }), &reviewer), Verdict::Deny(_)));
        // Hard noes keep their own reason.
        assert!(matches!(run("rm -rf src", &reviewer), Verdict::Deny(ref w) if w.starts_with("toplu")));
    }

    #[test]
    fn assistant_allows_web_and_inbox_reads_but_no_shell_mutation_or_mcp_write() {
        let f = crate::projects::tests::Fixture::new();
        let root=f.0.canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".dex/inbox")).unwrap();
        std::fs::write(root.join(".dex/inbox/image.png"),b"fixture").unwrap();
        let assistant = Ctx { assistant: true, read_only: true, read:vec![root.clone()],cwd:Some(root), ..Default::default() };
        for tool in ["WebFetch", "WebSearch", "AskUserQuestion", "ToolSearch", "mcp__dex__memory_search"] {
            assert_eq!(decide(tool, &serde_json::json!({}), &assistant), Verdict::Allow, "{tool}");
        }
        let inbox = assistant.cwd.as_ref().unwrap().join(".dex/inbox/image.png");
        assert_eq!(decide("Read", &serde_json::json!({"file_path":inbox}), &assistant), Verdict::Allow);
        for tool in ["Write", "Edit", "Bash", "PowerShell", "mcp__dex__task_create", "mcp__dex__report", "Skill", "Agent"] {
            assert!(matches!(decide(tool, &serde_json::json!({"command":"git status","file_path":inbox}), &assistant), Verdict::Deny(_)), "{tool}");
        }
    }

    #[test]
    fn assistant_search_path_cannot_be_shadowed_by_unused_file_path() {
        let f = crate::projects::tests::Fixture::new();
        let inbox = f.0.join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        let ctx = Ctx { assistant: true, read: vec![inbox.clone()], cwd: Some(f.0.clone()), ..Default::default() };
        for tool in ["Glob", "Grep", "LS"] {
            assert!(matches!(decide(tool, &json!({"file_path":inbox,"path":f.0,"pattern":"*"}), &ctx), Verdict::Deny(_)), "{tool}");
        }
    }

    #[test]
    fn assistant_glob_cannot_override_its_scoped_path() {
        let f = crate::projects::tests::Fixture::new();
        let inbox = f.0.join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        let ctx = Ctx { assistant: true, read: vec![inbox.clone()], cwd: Some(f.0.clone()), ..Default::default() };
        for pattern in [f.0.join("outside/*.txt").to_string_lossy().into_owned(), "../outside/*.txt".into()] {
            assert!(matches!(decide("Glob", &json!({"path":inbox,"pattern":pattern}), &ctx), Verdict::Deny(_)));
        }
        assert_eq!(decide("Glob", &json!({"path":inbox,"pattern":"**/*.png"}), &ctx), Verdict::Allow);
    }

    #[test]
    fn assistant_denies_a_linked_allowed_root() {
        let f = crate::projects::tests::Fixture::new();
        let outside = f.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("private.txt"), "fixture").unwrap();
        let root = f.0.join("inbox");
        #[cfg(windows)]
        let link = std::os::windows::fs::symlink_dir(&outside, &root);
        #[cfg(unix)]
        let link = std::os::unix::fs::symlink(&outside, &root);
        if link.is_err() { return; }
        let ctx = Ctx { assistant: true, read: vec![root.clone()], cwd: Some(f.0.clone()), ..Default::default() };
        assert!(matches!(decide("Read", &json!({"file_path":root.join("private.txt")}), &ctx), Verdict::Deny(_)));
    }

    #[test]
    fn assistant_denies_symlink_escape_reads() {
        let f=crate::projects::tests::Fixture::new();
        let own=f.0.join("own"); let outside=f.0.join("outside");
        std::fs::create_dir_all(&own).unwrap(); std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("private.txt"),"private").unwrap();
        #[cfg(windows)]
        let link=std::os::windows::fs::symlink_dir(&outside,own.join("link"));
        #[cfg(unix)]
        let link=std::os::unix::fs::symlink(&outside,own.join("link"));
        if link.is_err() { return; }
        let ctx=Ctx {assistant:true,read_only:true,read:vec![own.clone()],cwd:Some(own.clone()),..Default::default()};
        assert!(matches!(decide("Read",&serde_json::json!({"file_path":own.join("link/private.txt")}),&ctx),Verdict::Deny(_)));
    }

    #[test]
    fn reviewer_writes_only_its_own_report_and_reads_dex_skills() {
        let base = std::env::temp_dir().join(format!("dex-permit-293-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("team");
        let results = root.join(".dex").join("results");
        let skills = base.join("dex-skills-1");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::create_dir_all(skills.join("dex-code-review").join("skills").join("x")).unwrap();
        let reviewer = Ctx {
            team_root: Some(root.clone()),
            write: vec![root.clone()],
            read: vec![root.clone(), skills.clone()],
            cwd: Some(root.clone()),
            read_only: true,
            job: Some(291),
            ..Default::default()
        };
        let worker = Ctx { read_only: false, job: None, read: vec![root.clone()], ..reviewer_clone(&reviewer) };
        let own = results.join("291.md");
        let own_s = own.to_string_lossy().to_string();
        let write = |p: &str, x: &Ctx| decide("Write", &json!({ "file_path": p, "content": "x" }), x);
        let denied = |v: Verdict| matches!(v, Verdict::Deny(ref w) if w.starts_with(READ_ONLY));

        // Its own report: Write (absolute and relative), Edit, MultiEdit.
        assert_eq!(write(&own_s, &reviewer), Verdict::Allow);
        assert_eq!(write(".dex/results/291.md", &reviewer), Verdict::Allow);
        assert_eq!(decide("Edit", &json!({ "file_path": own_s, "old_string": "a", "new_string": "b" }), &reviewer), Verdict::Allow);
        assert_eq!(decide("MultiEdit", &json!({ "file_path": own_s, "edits": [] }), &reviewer), Verdict::Allow);

        // Another job's report, another file, a sibling trick, outside the dir: no.
        for p in [".dex/results/290.md", ".dex/results/2910.md", ".dex/results/291.md.bak", ".dex/results/291.txt",
            ".dex/results/../results2/291.md", ".dex/tmp/291.md", "src/a.rs", "291.md"] {
            assert!(denied(write(p, &reviewer)), "{p}");
        }
        assert!(denied(write(&base.join("other").join(".dex").join("results").join("291.md").to_string_lossy(), &reviewer)));
        assert!(denied(decide("NotebookEdit", &json!({ "notebook_path": own_s }), &reviewer)));
        // No running job, no report to write.
        assert!(denied(write(&own_s, &Ctx { job: None, ..reviewer_clone(&reviewer) })));
        // Shell writes stay closed, the report too.
        assert!(denied(decide("Bash", &json!({ "command": "echo x > .dex/results/291.md" }), &reviewer)));

        // Dex's role skills are readable — Read and shell readers, PowerShell too.
        let skill = skills.join("dex-code-review").join("skills").join("x").join("SKILL.md").to_string_lossy().to_string();
        assert_eq!(decide("Read", &json!({ "file_path": skill }), &reviewer), Verdict::Allow);
        for c in ["cat", "type", "Get-Content", "gc", "head -50"] {
            assert_eq!(decide("Bash", &json!({ "command": format!("{c} '{skill}'") }), &reviewer), Verdict::Allow, "{c}");
            assert_eq!(decide("PowerShell", &json!({ "command": format!("{c} '{skill}'") }), &reviewer), Verdict::Allow, "{c}");
        }
        // Other outside reads stay denied.
        assert!(denied(decide("Read", &json!({ "file_path": base.join("elsewhere.md").to_string_lossy() }), &reviewer)));
        let outside = if cfg!(windows) { r"cat \\server\share\a.md" } else { "cat /server/share/a.md" };
        assert!(denied(decide("Bash", &json!({ "command": outside }), &reviewer)));

        // Other roles: unchanged — the worker writes its folder as before, the skills are not in its read set.
        assert_eq!(write("src/a.rs", &worker), Verdict::Allow);
        assert_eq!(write(&own_s, &worker), Verdict::Allow);
        assert!(matches!(decide("Read", &json!({ "file_path": skill }), &worker), Verdict::Leader(_)));
        // #2442: with the skills root in its read set (lib.rs gives it to every agent), a worker reads a
        // folder skill's nested file — Read and shell — and still cannot write there.
        let worker = Ctx { read: vec![root.clone(), skills.clone()], ..reviewer_clone(&worker) };
        let nested = skills.join("dex-gameplay-programmer").join("gamedev").join("skills").join("godot").join("SKILL.md");
        let nested_s = nested.to_string_lossy().to_string();
        assert_eq!(decide("Read", &json!({ "file_path": nested_s }), &worker), Verdict::Allow);
        assert_eq!(decide("Bash", &json!({ "command": format!("cat '{nested_s}'") }), &worker), Verdict::Allow);
        assert!(matches!(write(&nested_s, &worker), Verdict::Deny(ref w) if w.starts_with("çalışma dizini dışına yazma")));
        assert!(matches!(decide("Edit", &json!({ "file_path": nested_s, "old_string": "a", "new_string": "b" }), &worker), Verdict::Deny(_)));

        // A link out of the team root: `.dex/results` must really be under it.
        #[cfg(unix)]
        {
            let out = base.join("out");
            std::fs::create_dir_all(&out).unwrap();
            let linked = base.join("linked");
            std::fs::create_dir_all(linked.join(".dex")).unwrap();
            std::os::unix::fs::symlink(&out, linked.join(".dex").join("results")).unwrap();
            let r = Ctx { team_root: Some(linked.clone()), write: vec![linked.clone()], cwd: Some(linked.clone()), ..reviewer_clone(&reviewer) };
            assert!(denied(write(".dex/results/291.md", &r)));
            std::os::unix::fs::symlink(base.join("elsewhere.md"), results.join("292.md")).unwrap();
            let r = Ctx { job: Some(292), ..reviewer_clone(&reviewer) };
            assert!(denied(write(".dex/results/292.md", &r)));
            // #298: `link/..` is the link target's parent to the OS, the team root to `resolve`.
            let sub = base.join("outside").join("sub");
            std::fs::create_dir_all(base.join("outside").join(".dex").join("results")).unwrap();
            std::fs::create_dir_all(&sub).unwrap();
            std::os::unix::fs::symlink(&sub, root.join("link")).unwrap();
            assert!(denied(write("link/../.dex/results/291.md", &reviewer)));
            assert!(denied(write(&root.join("link").join("..").join(".dex").join("results").join("291.md").to_string_lossy(), &reviewer)));
        }
        #[cfg(windows)]
        {
            // A junction needs no privilege on Windows.
            let out = base.join("out");
            std::fs::create_dir_all(&out).unwrap();
            let linked = base.join("linked");
            std::fs::create_dir_all(linked.join(".dex")).unwrap();
            let ok = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(linked.join(".dex").join("results"))
                .arg(&out)
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "mklink /J");
            let r = Ctx { team_root: Some(linked.clone()), write: vec![linked.clone()], cwd: Some(linked.clone()), ..reviewer_clone(&reviewer) };
            assert!(denied(write(".dex/results/291.md", &r)));
            // #298: `link\..` is the junction target's parent to the OS, the team root to `resolve`.
            let sub = base.join("outside").join("sub");
            std::fs::create_dir_all(base.join("outside").join(".dex").join("results")).unwrap();
            std::fs::create_dir_all(&sub).unwrap();
            let ok = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(root.join("link"))
                .arg(&sub)
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "mklink /J link");
            for p in ["link/../.dex/results/291.md", r"link\..\.dex\results\291.md"] {
                assert!(denied(write(p, &reviewer)), "{p}");
            }
            assert!(denied(write(&root.join("link").join("..").join(".dex").join("results").join("291.md").to_string_lossy(), &reviewer)));
            // The plain path still passes beside the junction.
            assert_eq!(write(".dex/results/291.md", &reviewer), Verdict::Allow);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reviewer_reads_plain_commands_only() {
        let reviewer = Ctx { read_only: true, team_root: ctx().cwd, ..ctx() };
        let worker = ctx();
        let sh = |tool: &str, c: &str, x: &Ctx| decide(tool, &json!({ "command": c }), x);
        let root = abs("/w/proj").replace('\\', "/");
        // The reviewer's reads, written out with literal arguments: pipes, `;`, `&&`, `||`.
        let bash = [
            "git ls-tree -r -l 8f26394c -- .dex/design/office-assets".to_string(),
            "git ls-tree -r --name-only f4ce75f src | grep -v uid".into(),
            "git merge-base main 5f59c949".into(),
            format!("git -C {root} merge-base main 5f59c949"),
            format!("git -C {root} show HEAD:tools/ci/.gitattributes; git -C {root} ls-tree -r --name-only HEAD tests"),
            format!("cd {root}; git grep -n \"palette_atlas\" 0d0202d -- . ':!*.import' | head; git ls-files --eol a.sh; git check-attr -a a.sh"),
            "git show 3bd34f51:crates/dex-core/src/gpu.rs | nl -ba | sed -n '175,420p'".into(),
            "git show HEAD:a.rs | head -20 | tail -5 | grep -n fn | cat -n | sort | uniq -c | wc -l".into(),
            "git show db76f700:handoff.md; git diff --name-only db76f700^ db76f700; git cat-file -s db76f700:core-idle.png".into(),
            "git ls-tree -r --name-only f4ce75f | grep -E '^src/(core|autoload)|player.gd$'; echo '=== src/a.gd'; git show f4ce75f:src/a.gd".into(),
            "git show a6ff3ae:docs/style-guide.md | cat -n && echo '== mat_palette' && git show a6ff3ae:assets/materials/mat_palette.tres".into(),
            format!("cd {root} 2>/dev/null; git merge-base main 5f59c949; git diff --stat 5f59c949 main | tail -20"),
            "git diff --name-only HEAD~1; git log -1 --format=%h -- src/a.rs".into(),
            "git show HEAD:a.rs 2>&1 | head".into(),
            "git diff main...HEAD -- src || git status".into(),
            "rg -n 'cost: \\$5' src".into(),
            "git log --format='%H %s' -3\ngit status --short".into(),
        ];
        for c in &bash {
            assert_eq!(sh("Bash", c, &reviewer), Verdict::Allow, "{c}");
        }
        let pwsh = [
            "git show 1c8efa9:src/usage/store.ts | Select-Object -First 180; git show 1c8efa9:e2e/a.spec.ts | Select-Object -Skip 240 -First 230",
            "git show fd01aff9:crates/dex-core/src/gpu.rs | Select-String -Pattern '\\.json\\(' -Context 4,8",
            "git show '6991b01e:.dex/design/office-assets/jobs.json' | ConvertFrom-Json | ConvertTo-Json -Depth 12 -Compress",
            "rg -n -C 5 'rules::system_prompt' crates; git show ebb38a76:crates/dex-core/src/stack.rs | Select-Object -First 40; Get-Content -Encoding utf8 .dex/memory/a.md",
            "git show 'f4ce75f:src/a.rs' | Select-Object -First 5",
            // #495: `$null` is the one `$` — a redirection target only.
            "git show HEAD:a.rs 2>$null | Measure-Object",
            "git status *>$null; git diff >$NULL",
            "git log --format='%H $x' -1",
        ];
        for c in pwsh {
            assert_eq!(sh("PowerShell", c, &reviewer), Verdict::Allow, "{c}");
        }
        for c in &pwsh[5..] {
            assert_eq!(sh("PowerShell", c, &worker), Verdict::Allow, "{c}");
        }
        // #471/#495: no variable, substitution, backtick, assignment or loop — in any role. The
        // reviewer is told why, a worker's goes to the leader (never an allow).
        let dynamic = [
            ("Bash", "X=--output=review.txt; false && X=HEAD; git diff $X"),
            ("Bash", "X=HEAD; for f in a; do X=--output=review.txt; done; git diff $X"),
            ("Bash", "for PATH in .; do git status; done"),
            ("Bash", "for GIT_DIR in ../other/.git; do git log; done"),
            ("PowerShell", "$x = Get-ChildItem -LiteralPath a.txt; $x.IsReadOnly = $true"),
            ("PowerShell", "$(Get-ChildItem -LiteralPath a.txt).IsReadOnly = 1"),
            ("PowerShell", "$env:PATH='.'; git status"),
            ("PowerShell", "for ($i = 0; $i -lt 2; $i++) { git status }"),
            ("PowerShell", "foreach ($PATH in '.') { git status }"),
            ("PowerShell", "while (1) { git status }"),
            ("PowerShell", "do { git status } while (1)"),
            ("PowerShell", "git ls-files | ForEach-Object { git status }"),
            ("PowerShell", "git ls-files | %{ git status }"),
            ("PowerShell", "git show \"${C}:src/a.rs\""),
            ("PowerShell", "git show HEAD:a.rs | Where-Object { $_ -match 'fn' }"),
            ("Bash", "C=f4ce75f; git show $C:a.rs"),
            ("Bash", "git show HEAD:${F}"),
            ("Bash", "echo \"$HOME\""),
            ("Bash", "echo rc=$?"),
            ("Bash", "echo rc=$"),
            ("Bash", "while true; do git status; done"),
            ("Bash", "FOO=1 git diff"),
            ("PowerShell", "$C = 'f4ce75f'; git show $C"),
            // The substitutions #402 allowed and #479 broke: none is read any more.
            ("Bash", "git diff $(git merge-base main HEAD) -- src"),
            ("Bash", "git log --oneline \"$(git merge-base main HEAD)..HEAD\""),
            ("Bash", "git diff --no-index --stat <(git show 0d0202d:a.png.import) a.png.import"),
            ("Bash", "git show HEAD:a.rs | cat > >(cat)"),
            ("Bash", "echo `git status`"),
            ("Bash", "git status 2>$null"),
            ("PowerShell", "git diff $(git merge-base main HEAD)"),
            ("PowerShell", "git show \"HEAD:a`.rs\""),
            ("PowerShell", "Get-Content src/a.rs `\n| Select-Object -First 5"),
        ];
        let reason = |v: &Verdict| matches!(v, Verdict::Deny(w) if w.starts_with(READ_ONLY_DYNAMIC));
        for (tool, c) in dynamic {
            let v = sh(tool, c, &reviewer);
            assert!(reason(&v), "{tool}: {c}: {v:?}");
            assert_ne!(sh(tool, c, &worker), Verdict::Allow, "{tool}: {c}");
            assert_eq!(parsed(c, Some(if tool == "Bash" { Dialect::Bash } else { Dialect::Pwsh }), &worker), Err(Unparsed::Dynamic), "{tool}: {c}");
        }
        // #495: groups, blocks, conditions and expressions are not followed either.
        for (tool, c) in [
            ("PowerShell", "if (Test-Path src/a.rs) { Get-Content src/a.rs } else { git status }"),
            ("PowerShell", "'{0}: {1}' -f 'a', 'b'"),
            ("PowerShell", "(Get-ChildItem -LiteralPath a.txt).IsReadOnly = 1"),
            ("PowerShell", "(git status)"),
            ("PowerShell", "git show HEAD:a --% > f"),
            ("PowerShell", "& 'C:\\x\\git.exe' status"),
            ("Bash", "(git status)"),
            ("Bash", "{ git status; }"),
            ("Bash", "git log & git status"),
            ("Bash", "cat < src/a.rs"),
            ("Bash", "git show HEAD@{1}"),
        ] {
            let v = sh(tool, c, &reviewer);
            assert!(matches!(v, Verdict::Deny(ref w) if w.starts_with(READ_ONLY)), "{tool}: {c}: {v:?}");
            assert!(matches!(sh(tool, c, &worker), Verdict::Leader(_)), "{tool}: {c}: {:?}", sh(tool, c, &worker));
        }

        // #479: the holes of #471's reader.
        let other = abs("/w/other/a.txt").replace('\\', "/");
        let holes = [
            // The tool's shell, not the one of the platform: bash runs `rm f`.
            sh("Bash", "echo `rm f`x", &reviewer),
            sh("Bash", "echo `rm -rf src`x", &worker),
            // A non-ASCII variable is a variable too.
            sh("PowerShell", "git diff \"$é$(echo --output=.dex/tmp/review.txt)\"", &reviewer),
            sh("PowerShell", "git diff $é --output=.dex/tmp/review.txt", &reviewer),
            // A substitution's output is not a revision.
            sh("Bash", &format!("git diff --no-index $(git rev-parse {other}) a.txt"), &reviewer),
            sh("Bash", "git diff --no-index $(git rev-parse ../other/a.txt) a.txt", &reviewer),
            // No panic: an unknown value where git's options are.
            sh("Bash", "git -C \"$(echo .)\" status", &reviewer),
            sh("Bash", "git $(echo -C) . status", &reviewer),
            sh("PowerShell", "git -C \"$(echo .)\" status", &reviewer),
        ];
        for v in holes {
            assert!(matches!(v, Verdict::Deny(_)), "{v:?}");
        }
        for (tool, c) in [
            ("PowerShell", "git diff \"$é$(echo --output=.dex/tmp/review.txt)\""),
            ("Bash", "git diff --no-index $(git rev-parse ../other/a.txt) a.txt"),
            ("Bash", "git -C \"$(echo .)\" status"),
        ] {
            assert!(matches!(sh(tool, c, &worker), Verdict::Leader(_)), "{tool}: {c}: {:?}", sh(tool, c, &worker));
        }
        assert!(matches!(sh("Bash", "echo `rm f`x", &worker), Verdict::Leader(_)));

        // #484: a deny is found behind anything this reader does not follow, in any role.
        for (tool, c) in [
            ("PowerShell", "Write-Output \"a” ; git diff $(Write-Output HEAD) ; Remove-Item -Recurse . ; ”b\""),
            ("PowerShell", "(Set-Location .) ; git push"),
            ("PowerShell", "git ls-files | ForEach-Object { Remove-Item -Recurse . }"),
            ("PowerShell", "if ($true) { git push }"),
            ("PowerShell", "echo \"$(git push)\""),
            ("PowerShell", "$x = 1; Remove-Item -Recurse src"),
            ("PowerShell", "& (Get-ChildItem x.exe); git reset --hard"),
            ("Bash", "git diff $(git rev-parse HEAD); rm -rf src"),
            ("Bash", "echo $(rm -rf src)"),
            ("Bash", "echo \"$(git push)\""),
            ("Bash", "echo `git push`"),
            ("Bash", "git diff <(rm -rf src) a"),
            ("Bash", "if true; then rm -rf src; fi"),
            ("Bash", "for f in a; do git clean -fd; done"),
            ("Bash", "{ rm -rf src; }"),
            ("Bash", "(cd .. && git push)"),
            ("Bash", "X=1; git push"),
            ("Bash", "git log & rm -rf src"),
        ] {
            for x in [&reviewer, &worker] {
                let v = sh(tool, c, x);
                assert!(matches!(v, Verdict::Deny(ref w) if !w.starts_with(READ_ONLY)), "{tool}: {c}: {v:?}");
            }
        }
        // A here-document is text, not commands: a commit message stays the leader's.
        for c in ["git commit -F - <<'EOF'\nfix: x\n\ngit push sonra\nrm -rf eski notlar\nEOF",
            "git commit -m \"$(cat <<'EOF'\nfix: x\n\nrm -rf eski notlar\nEOF\n)\""] {
            assert!(matches!(sh("Bash", c, &worker), Verdict::Leader(_)), "{c}: {:?}", sh("Bash", c, &worker));
        }
        assert!(matches!(sh("PowerShell", "git commit -m @'\nfix: x\ngit push sonra\n'@", &worker), Verdict::Leader(_)));

        // Every write marker stays a no — in a pipe, a loop body, a `$( )`, a script block.
        let bash_writes = ["cat a > f", "cat a >> f", "tee f", "sed -i s/a/b/ f", "rm f", "mkdir d", "git add a", "./evil.exe", "python -c x"];
        for w in bash_writes {
            for c in [format!("git show HEAD:a | {w}"), format!("for f in a b; do git show HEAD:a | {w}; done"), format!("echo $({w})"),
                format!("git diff <({w}) a"), format!("C=1; {w}"), format!("git log & {w}")] {
                assert!(matches!(sh("Bash", &c, &reviewer), Verdict::Deny(ref r) if r.starts_with("salt okuma")), "{c}: {:?}", sh("Bash", &c, &reviewer));
            }
        }
        let pwsh_writes = ["Get-Content a > f", "Get-Content a >> f", "Tee-Object f", "Set-Content f x", "Out-File f", "Add-Content f x", "New-Item f",
            "Remove-Item f", "Remove-ItemProperty f x", "Start-Process notepad", "New-Object System.Diagnostics.Process", "Invoke-Expression x", "iex x",
            "& ./evil.exe", "& C:\\x\\evil.exe", "& $x", "& (Get-ChildItem x.exe)", "& { Remove-Item f }", ". ./x.ps1"];
        for w in pwsh_writes {
            for c in [format!("git show HEAD:a | {w}"), format!("for ($i = 0; $i -lt 2; $i++) {{ {w} }}"), format!("echo \"$({w})\""),
                format!("git show HEAD:a | ForEach-Object {{ $_ | {w} }}"), format!("$x = {w}"), format!("if ($true) {{ {w} }}")] {
                assert!(matches!(sh("PowerShell", &c, &reviewer), Verdict::Deny(ref r) if r.starts_with("salt okuma")), "{c}: {:?}", sh("PowerShell", &c, &reviewer));
            }
        }
        // Doors that only look like reads.
        for (tool, c) in [
            ("PowerShell", "$x = Get-ChildItem f; $x.Delete()"),
            ("PowerShell", "Get-ChildItem f | ForEach-Object Delete"),
            ("PowerShell", "Get-ChildItem f | % -MemberName Delete"),
            ("PowerShell", "Write-Output \"a” ; Remove-Item x ; ”b\""),
            ("PowerShell", "git show HEAD:a | Select-Object @{n='x';e={ Remove-Item f }}"),
            ("PowerShell", "git show HEAD:a | Sort-Object { Remove-Item f }"),
            ("PowerShell", "$x = 'a'; $x.\"Delete\"()"),
            ("PowerShell", "[IO.File]::WriteAllText('f', 'x')"),
            ("PowerShell", "$C:x"),
            ("PowerShell", "Get-Content ${C:\\x}"),
            ("PowerShell", "$x = git ls-files; git diff $x"),
            ("PowerShell", "git show HEAD:a | x{ $_ }"),
            ("PowerShell", "git status > '$null'"),
            ("PowerShell", "git status > \"$x.log\""),
            ("PowerShell", "Get-Content a.rs\u{a0}| Remove-Item"),
            ("Bash", "PATH=.; git status"),
            ("Bash", "GIT_EXTERNAL_DIFF=x; git diff"),
            ("Bash", "for f in $(ls); do cat $f; done"),
            ("Bash", "cat $(git merge-base main HEAD)/../../x/.env"),
            ("Bash", "f(){ rm x; }; f"),
            ("Bash", "x=$'\\x41'; git show $x"),
            ("Bash", "git -C ../other show HEAD"),
            ("Bash", "for f in a; do cd ..; done; cat x"),
            ("Bash", "cat ~/.codex/config.toml"),
            ("Bash", if cfg!(windows) { r"cat \\192.0.2.10\opt\API.md" } else { "cat /192.0.2.10/opt/API.md" }),
            ("Bash", "echo a\u{2028}rm x"),
        ] {
            assert!(matches!(sh(tool, c, &reviewer), Verdict::Deny(_)), "{tool}: {c}: {:?}", sh(tool, c, &reviewer));
            assert_ne!(sh(tool, c, &worker), Verdict::Allow, "{tool}: {c}");
        }
        assert!(matches!(sh("Bash", "git diff --no-index ../other/a a", &reviewer), Verdict::Deny(_)));
        // The user's skills are readable to the reviewer only.
        let home = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap());
        for dir in [".codex", ".claude"] {
            let skill = home.join(dir).join("skills").join("requesting-code-review").join("SKILL.md").to_string_lossy().replace('\\', "/");
            assert_eq!(decide("Read", &json!({ "file_path": skill }), &reviewer), Verdict::Allow);
            assert_eq!(sh("PowerShell", &format!("Get-Content -Encoding UTF8 {skill}"), &reviewer), Verdict::Allow);
            assert_eq!(sh("Bash", &format!("cat ~/{dir}/skills/x/SKILL.md | head"), &reviewer), Verdict::Allow);
            assert!(matches!(decide("Read", &json!({ "file_path": skill }), &worker), Verdict::Leader(_)));
        }
        // Other roles: unchanged — the leader still decides what the policy does not know.
        for c in [&bash[0], &bash[2], &bash[5]] {
            assert!(matches!(sh("Bash", c, &worker), Verdict::Leader(_)), "{c}");
        }
        assert!(matches!(sh("PowerShell", pwsh[2], &worker), Verdict::Leader(_)));
    }

    /// #503: a null device only as written, a bash glob is an expansion, and what a quoted
    /// redirection target, `${…}`, an unquoted here-document or `@" "@` runs is judged for a deny.
    #[test]
    fn expansions_never_allow_and_their_scripts_are_judged() {
        let reviewer = Ctx { read_only: true, team_root: ctx().cwd, ..ctx() };
        let worker = ctx();
        let sh = |tool: &str, c: &str, x: &Ctx| decide(tool, &json!({ "command": c }), x);
        let denied = |v: &Verdict| matches!(v, Verdict::Deny(_));
        // A variable that looks like a null device: not one.
        for tool in ["Bash", "PowerShell"] {
            for c in ["echo hi > \"$nul\"", "echo hi > $nul", "echo hi > \"${nul}\"", "git status 2> \"$null\""] {
                assert_ne!(sh(tool, c, &worker), Verdict::Allow, "{tool}: {c}");
                assert!(denied(&sh(tool, c, &reviewer)), "{tool}: {c}");
            }
            for c in ["git status > nul", "git status 2>/dev/null", "git status > \"nul\""] {
                assert_eq!(sh(tool, c, &worker), Verdict::Allow, "{tool}: {c}");
                assert_eq!(sh(tool, c, &reviewer), Verdict::Allow, "{tool}: {c}");
            }
        }
        assert_eq!(sh("PowerShell", "git status 2>$null", &worker), Verdict::Allow);
        // bash globs outside quotes: denied to the reviewer, the leader's for a worker.
        for c in ["git diff *", "git diff -- src/*.rs", "git log a?", "git show HEAD:[ab].rs", "cat src/*", "git status > *.log"] {
            assert!(denied(&sh("Bash", c, &reviewer)), "{c}");
            assert!(matches!(sh("Bash", c, &worker), Verdict::Leader(_)), "{c}");
        }
        for c in ["git log -- '*.rs'", "git diff -- \"src/*.rs\"", r"git log -- \*.rs", "rg -n 'a[0-9]?' src"] {
            assert_eq!(sh("Bash", c, &reviewer), Verdict::Allow, "{c}");
        }
        assert_eq!(sh("PowerShell", "git diff -- src/*.rs", &reviewer), Verdict::Allow);
        // What a substitution runs is judged, wherever it is.
        let hidden = [
            ("Bash", "echo hi > \"$(rm -rf src)\""),
            ("Bash", "echo hi > \"`rm -rf src`\""),
            ("Bash", "echo hi > `rm -rf src`"),
            ("Bash", "echo hi > \"a${x:-$(git push)}\""),
            ("Bash", "echo ${x:-$(git push)}"),
            ("Bash", "echo \"${x:-`rm -rf src`}\""),
            ("Bash", "cat <<EOF\n$(git push)\nEOF"),
            ("Bash", "cat <<-EOF\n\ta `rm -rf src` b\n\tEOF"),
            ("Bash", "cat <<EOF\nx $(\ngit push\n) y\nEOF\ngit status"),
            ("PowerShell", "echo hi > \"$(Remove-Item -Recurse src)\""),
            ("PowerShell", "Write-Output @\"\n$(git push)\n\"@"),
        ];
        for (tool, c) in hidden {
            assert!(denied(&sh(tool, c, &worker)), "{tool}: {c}: {:?}", sh(tool, c, &worker));
            assert!(denied(&sh(tool, c, &reviewer)), "{tool}: {c}");
        }
        // Quoted here-documents are text: not a deny, still not an allow.
        for (tool, c) in [
            ("Bash", "cat <<'EOF'\n$(git push)\nEOF"),
            ("Bash", "cat <<\"EOF\"\n`rm -rf src`\nEOF"),
            ("Bash", "cat <<\\EOF\n$(git push)\nEOF"),
            ("Bash", "cat <<EOF\n\\$(git push)\nEOF"),
            ("PowerShell", "Write-Output @'\n$(git push)\n'@"),
        ] {
            assert!(matches!(sh(tool, c, &worker), Verdict::Leader(_)), "{tool}: {c}: {:?}", sh(tool, c, &worker));
        }
    }

    /// #508: a here-document ends where bash ends it — a `\` + newline in its delimiter or an
    /// unquoted body line is a continuation; no here-document is an allow.
    #[test]
    fn heredocs_end_where_bash_ends_them_and_never_allow() {
        let reviewer = Ctx { read_only: true, team_root: ctx().cwd, ..ctx() };
        let worker = ctx();
        let sh = |c: &str, x: &Ctx| decide("Bash", &json!({ "command": c }), x);
        let denied = |v: &Verdict| matches!(v, Verdict::Deny(_));
        for c in [
            // The two of the review: the real delimiter is an unquoted `EOF`.
            "cat <<EO\\\nF\n$(rm -rf src)\nEOF",
            "cat <<EOF\nEO\\\nF\nrm -rf src\nEOF",
            // A quoted body joins nothing: its first `EOF` line ends it.
            "cat <<'EOF'\nx\\\nEOF\nrm -rf src\nEOF",
            "cat <<'A'\nx\\\nA\ncat <<EOF\nEO\\\nF\ngit push\nEOF",
            // Ambiguous delimiters: the body is judged as if unquoted.
            "cat <<E'O'F\n$(rm -rf src)\nEOF",
            "cat <<'E'O\\\nF\n`git push`\nEOF",
            // A substitution does not run past the body's end.
            "cat <<EOF\n$(\nEOF\nrm -rf src\n)\nEOF",
            // Never closed: the rest is the body, its substitutions still judged.
            "cat <<EOF\nhi\n$(git push)",
            "cat <<EOF\nEO\\\nFX\n`rm -rf src`",
        ] {
            assert!(denied(&sh(c, &worker)), "{c}: {:?}", sh(c, &worker));
            assert!(denied(&sh(c, &reviewer)), "{c}");
        }
        // Quoted, with a space or a continuation inside the quotes: text — not a deny, no allow.
        for c in ["cat <<'E O'\n$(git push)\nE O", "cat <<'EOF'\nx \\\n$(git push)\nEOF", "git status <<'EOF'\nhi\nEOF", "git status <<<hi"] {
            assert!(matches!(sh(c, &worker), Verdict::Leader(_)), "{c}: {:?}", sh(c, &worker));
            assert!(denied(&sh(c, &reviewer)), "{c}");
        }
    }

    /// #517: a `\` + newline is gone before anything is read — but in single quotes, a comment or a
    /// quoted here-document's body (as bash 5.2 does it).
    #[test]
    fn line_continuations_are_joined_once_before_any_reading() {
        let reviewer = Ctx { read_only: true, team_root: ctx().cwd, ..ctx() };
        let worker = ctx();
        let sh = |c: &str, x: &Ctx| decide("Bash", &json!({ "command": c }), x);
        let denied = |v: &Verdict| matches!(v, Verdict::Deny(_));
        for c in [
            // The review's input.
            "cat <<EOF\n$\\\n(rm -rf src)\nEOF",
            "cat <<-EOF\n\tx $\\\n(git push)\n\tEOF",
            "cat <<E\\\nOF\n$\\\n(rm -rf src)\nEOF",
            "cat <<EOF\n$\\\n{x:-$\\\n(git push)}\nEOF",
            // In the command itself.
            "echo $\\\n(rm -rf src)",
            "echo \"$\\\n(rm -rf src)\"",
            "echo ${x:-$\\\n(git push)}",
            "echo hi > $\\\n(rm -rf src)",
            "echo hi > \"$\\\n(git push)\"",
            "echo $(rm -r\\\nf src)",
            "r\\\nm -rf src",
            "git pu\\\nsh",
            // An escaped `\` continues nothing; a comment ends at the newline.
            "echo a\\\\\ngit push",
            "echo a # b \\\ngit push",
        ] {
            assert!(denied(&sh(c, &worker)), "{c}: {:?}", sh(c, &worker));
            assert!(denied(&sh(c, &reviewer)), "{c}");
        }
        // Kept as written: single quotes, `$'…'`, a comment, a quoted here-document's body.
        for c in [
            "echo 'a\\\nb'",
            "echo $'a\\\nb\\'c\\\nd'",
            "echo a # b \\\nc",
            "cat <<'EOF'\nx\\\ny\nEOF",
            "cat <<\"EOF\"\n$\\\n(rm -rf src)\nEOF",
            "echo \"$(echo 'a\\\nb')\"",
        ] {
            assert_eq!(join_lines(c), c);
        }
        assert!(!denied(&sh("echo '$\\\n(rm -rf src)'", &worker)));
        assert!(!denied(&sh("cat <<'EOF'\n$\\\n(rm -rf src)\nEOF", &worker)));
        // Joined everywhere else — in backticks even inside single quotes.
        for (c, j) in [
            ("echo a\\\nb \"c\\\nd\" e\\\\\nf", "echo ab \"cd\" e\\\\\nf"),
            ("echo `echo 'a\\\nb'`", "echo `echo 'ab'`"),
            ("cat <<E\\\nOF\na\\\nb\\\\\nEO\\\nF\nc\\\nd", "cat <<EOF\nab\\\\\nEOF\ncd"),
            ("cat <<\"E\\\nOF\"\na\\\nEOF\nb\\\nc", "cat <<\"EOF\"\na\\\nEOF\nbc"),
        ] {
            assert_eq!(join_lines(c), j, "{c}");
        }
    }

    /// #508: every nesting shares one bounded depth: past it no allow and no stack overflow; a
    /// deny read before it stays a deny.
    #[test]
    fn nesting_depth_is_bounded_and_keeps_denies() {
        let reviewer = Ctx { read_only: true, team_root: ctx().cwd, ..ctx() };
        let worker = ctx();
        let p = |c: &str| parsed(c, Some(Dialect::Bash), &worker);
        let braces = |n: usize, inner: &str| format!("echo {}{inner}{}", "${x:-".repeat(n), "}".repeat(n));
        let subs = |n: usize, inner: &str| format!("echo {}{inner}{}", "$(".repeat(n), ")".repeat(n));
        // The script is depth 1: 15 nestings fit, the 16th is past the bound — not read, no allow.
        assert!(matches!(p(&subs(15, "rm -rf src")), Err(Unparsed::Deny(_))));
        assert_eq!(p(&subs(16, "rm -rf src")), Err(Unparsed::Dynamic));
        assert!(matches!(p(&braces(14, "$(rm -rf src)")), Err(Unparsed::Deny(_))));
        assert_eq!(p(&braces(15, "$(rm -rf src)")), Err(Unparsed::Dynamic));
        assert_eq!(p(&braces(16, "a")), Err(Unparsed::Dynamic));
        // Read before the bound: still a deny.
        for c in [format!("echo ${{x:-$(rm -rf src)}}{}", braces(100_000, "a")), format!("echo $(rm -rf src; {})", subs(100_000, "a"))] {
            assert!(matches!(p(&c), Err(Unparsed::Deny(_))));
        }
        for c in [braces(100_000, "a"), subs(100_000, "a"), format!("echo \"{}\"", braces(100_000, "a")), format!("cat <<EOF\n{}\nEOF", braces(100_000, "a"))] {
            assert!(matches!(decide("Bash", &json!({ "command": c }), &worker), Verdict::Leader(_)));
            assert!(matches!(decide("Bash", &json!({ "command": c }), &reviewer), Verdict::Deny(_)));
        }
    }

    fn reviewer_clone(c: &Ctx) -> Ctx {
        Ctx { team_root: c.team_root.clone(), write: c.write.clone(), read: c.read.clone(), cwd: c.cwd.clone(), read_only: c.read_only, job: c.job, ..Default::default() }
    }
}
