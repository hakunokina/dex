//! Dex MCP server (DEX-4): streamable-HTTP endpoint `POST /mcp` on the hook receiver's port.
//! Hand-rolled JSON-RPC (single-response mode, no SSE stream): the receiver is plain std threads,
//! and rmcp would pull in a tokio/axum stack beside it for five tools.
//!
//! Identity: every claude pane gets `DEX_AGENT_ID` + a per-spawn random `DEX_MCP_KEY` in its env;
//! the config file only names them (`${VAR}`, expanded by Claude), so no secret touches disk.
//! Team and leader/worker are looked up here, and authorization is enforced here.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use crate::db::tasks;
use crate::delegate::{self, Caller};
use crate::Core;

pub struct Mcp {
    pub config: PathBuf,
    keys: Mutex<HashMap<String, SpawnIdentity>>,
}

struct SpawnIdentity {
    key: String,
    delegation_id: Option<i64>,
}

#[cfg(test)]
thread_local! {
    /// Deterministically interleave a second paste at the MCP CAS boundary.
    pub(crate) static BEFORE_REBIND: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod worker_note_tests {
    use super::*;

    #[test]
    fn running_note_arrives_at_next_hook_once() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("worker-note-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let d = crate::db::with(&core.db, |c| {
            let lead = delegate::caller(c, "timur")?;
            let d = delegate::insert(c, delegate::NewDelegation { from: &lead, to_agent: "kaan".into(),
                team_id: "default".into(), service_team_id: None, prompt: "work".into(), task_id: None,
                patron_words: None, effort: None })?;
            delegate::set_status(c, d.id, "running")?;
            Ok(d)
        }).unwrap();
        let reply = call_with_args_for_test(&core, "timur", "note", json!({"delegationId":d.id,"text":"şunu şöyle yap"}));
        assert_eq!(reply["structuredContent"]["status"], "running", "{reply}");
        let status = call_with_args_for_test(&core, "timur", "delegation_status", json!({"id":d.id}));
        assert_eq!(status["structuredContent"]["pendingNotes"], 1);
        let hook = || core.delegation_on_hook("kaan", "PostToolUse", &json!({}), false);
        let out: Value = serde_json::from_str(&hook().unwrap()).unwrap();
        assert!(out["hookSpecificOutput"]["additionalContext"].as_str().unwrap()
            .contains(&format!("Liderden not (#{}): şunu şöyle yap", d.id)));
        assert!(hook().is_some(), "no CLI receipt: the same note is retried");
        let status = call_with_args_for_test(&core, "timur", "delegation_status", json!({"id":d.id}));
        assert_eq!(status["structuredContent"]["pendingNotes"], 1);
        // Simulate the exact text acknowledged by the CLI; the transcript watcher
        // and session/process fences are exercised in worker_notes::tests.
        crate::db::with(&core.db, |c| crate::worker_notes::accept_text(c, "kaan",
            out["hookSpecificOutput"]["additionalContext"].as_str().unwrap(), crate::diagnostics::now_ms())).unwrap();
        assert!(hook().is_none(), "a note is delivered once");
        let status = call_with_args_for_test(&core, "timur", "delegation_status", json!({"id":d.id}));
        assert_eq!(status["structuredContent"]["pendingNotes"], 0);
        assert!(tools_list_for_test(&core,"timur").iter().any(|t| t["name"] == "note"));
        assert!(!tools_list_for_test(&core,"kaan").iter().any(|t| t["name"] == "note"));
        core.shutdown();
    }
}

#[cfg(all(test, windows))]
mod report_content_tests {
    use super::*;

    struct Fixture {
        core: std::sync::Arc<Core>,
        job: delegate::Delegation,
        root: PathBuf,
        dir: crate::projects::tests::Fixture,
    }

    impl Fixture {
        fn new() -> Self { Self::job(false, "report") }
        fn service(prompt: &str) -> Self { Self::job(true, prompt) }
        fn job(service: bool, prompt: &str) -> Self {
            let dir = crate::projects::tests::Fixture::new();
            let root = dir.dir("report-project");
            // Core background shutdown may still hold SQLite handles on Windows; keep its
            // storage outside the project fixture's synchronous directory cleanup.
            let storage = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
                .join(format!("report-core-{}-{}", std::process::id(), crate::agents::next_seq()));
            let core = Core::start(crate::Paths { data_dir: storage.join("data"), config_dir: storage.join("config") },
                crate::Events::new(crate::events::NullSink)).unwrap();
            let job = crate::db::with(&core.db, |c| {
                crate::teams::update_team(c, &dir.0, "default", crate::teams::TeamPatch {
                    // These jobs exercise delivery, not worktree orchestration. Disable the
                    // startup sweep so its Git cwd cannot race root-junction fixture cleanup.
                    cwd: Some(Some(root.to_string_lossy().into_owned())), worktrees: Some(false), ..Default::default()
                })?;
                let lead = delegate::caller(c, "timur")?;
                if service {
                    c.execute_batch("INSERT INTO teams(id,name,kind,created_at) VALUES('service','Service','service',1);
                        INSERT INTO agents(id,team_id,name,kind,model,shirt,hair,skin) VALUES('svc','service','S','worker','sonnet','a','b','c');").map_err(|e| e.to_string())?;
                }
                let job = delegate::insert(c, delegate::NewDelegation { from: &lead, to_agent: if service { "svc" } else { "kaan" }.into(),
                    team_id: "default".into(), service_team_id: service.then(|| "service".into()), prompt: prompt.into(), task_id: None,
                    patron_words: None, effort: None })?;
                delegate::set_status(c, job.id, "running")?;
                Ok(job)
            }).unwrap();
            Self { core, job, root, dir }
        }

        fn file(&self) -> PathBuf { crate::db::with(&self.core.db, |c| delegate::result_file(c, &self.job)).unwrap().unwrap() }
        fn call(&self, args: Value) -> Value { call_with_args_for_test(&self.core, &self.job.to_agent, "report", args) }
        fn state(&self) -> delegate::Delegation {
            crate::db::with(&self.core.db, |c| delegate::get(c, self.job.id)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { self.core.shutdown(); }
    }

    #[test]
    fn worker_cannot_write_file_but_content_delivers_to_snapshot_root() {
        let f = Fixture::new();
        let file = f.file();
        crate::db::with(&f.core.db, |c| c.execute("UPDATE agents SET role_slug='code-review' WHERE id='kaan'", [])
            .map_err(|e| e.to_string())).unwrap();
        let blocked = f.core.delegation_on_hook("kaan", "PreToolUse", &json!({"tool_name":"Write","tool_input":{"file_path":file}}), false).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&blocked).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
        let other = f.dir.dir("other-project");
        crate::db::with(&f.core.db, |c| c.execute("UPDATE teams SET cwd=?1 WHERE id='default'",
            [other.to_string_lossy()]).map_err(|e| e.to_string())).unwrap();
        let content = "engelli: derleme komutu reddedildi\nTam rapor.";
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti: engelli teslim","content":content}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
        assert_eq!(f.state().result_path.as_deref(), file.to_str());
        assert!(!other.join(".dex/results").join(format!("{}.md", f.job.id)).exists());
    }

    #[test]
    fn content_cannot_select_foreign_or_outside_path() {
        let f = Fixture::new();
        for path in [f.root.join(".dex/results/99999.md"), f.dir.0.join("outside.md")] {
            let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"body","resultPath":path}));
            assert_eq!(reply["isError"], true, "{reply}");
            assert!(reply.to_string().contains("birlikte"), "explicit conflict: {reply}");
            assert!(!path.exists());
            assert!(!f.file().exists());
            assert_eq!(f.state().status, "running");
        }
        let reply = call_with_args_for_test(&f.core, "ayla", "report", json!({"delegationId":f.job.id,"summary":"bitti","content":"body"}));
        assert_eq!(reply["isError"], true);
        assert!(!f.file().exists());
    }

    #[test]
    fn legacy_result_path_is_preserved_without_content() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.file().parent().unwrap()).unwrap();
        let file = f.file().parent().unwrap().join("legacy-name.md");
        std::fs::write(&file, "legacy report").unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","resultPath":file}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(f.state().result_path.as_deref(), file.to_str());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "legacy report");
    }

    #[test]
    fn legacy_reviewer_result_path_reads_file_before_summary() {
        let f = Fixture::new();
        crate::db::with(&f.core.db, |c| c.execute(
            "INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(?1,?1,'fixture-sha',1)", [f.job.id]
        ).map_err(|e| e.to_string())).unwrap();
        std::fs::create_dir_all(f.file().parent().unwrap()).unwrap();
        std::fs::write(f.file(), "karar: düzelt\n## önemli\n- legacy finding\n").unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"karar: onay","resultPath":f.file()}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        let review = crate::db::with(&f.core.db, |c| crate::review::get(c, f.job.id)).unwrap().unwrap();
        assert_eq!(review.verdict.as_deref(), Some("düzelt"));
        assert_eq!(review.findings.as_deref(), Some("- legacy finding"));
    }

    #[test]
    fn reviewer_content_is_parsed_from_dex_written_file() {
        let f = Fixture::new();
        crate::db::with(&f.core.db, |c| c.execute(
            "INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(?1,?1,'fixture-sha',1)", [f.job.id]
        ).map_err(|e| e.to_string())).unwrap();
        let rejected = f.call(json!({"delegationId":f.job.id,"summary":"karar: onay","content":"inceledim\nkarar: onay"}));
        assert_eq!(rejected["isError"], true, "{rejected}");
        assert_eq!(f.state().status, "running");
        let body = "karar: düzelt\n## önemli\n- file.rs:1 — hata\n## küçük\nyok\n";
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":body}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        let review = crate::db::with(&f.core.db, |c| crate::review::get(c, f.job.id)).unwrap().unwrap();
        assert_eq!(review.verdict.as_deref(), Some("düzelt"));
        assert!(review.findings.unwrap().contains("file.rs:1"));
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), body);
    }

    #[test]
    fn content_io_failure_keeps_job_open_and_reports_error() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.file()).unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"body"}));
        assert_eq!(reply["isError"], true, "{reply}");
        assert!(reply.to_string().contains("rapor dosyası"), "I/O reason: {reply}");
        assert_eq!(f.state().status, "running");
        assert!(f.file().is_dir());
    }

    #[test]
    fn content_rejects_linked_results_directory() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.root.join(".dex")).unwrap();
        let outside = f.dir.dir("outside");
        assert!(crate::memory::tests::link_dir(&outside, &f.root.join(".dex/results")));
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"body"}));
        assert_eq!(reply["isError"], true);
        assert!(reply.to_string().contains("güvenli"), "{reply}");
        assert!(!outside.join(format!("{}.md", f.job.id)).exists());
        assert_eq!(f.state().status, "running");
        std::fs::remove_dir(f.root.join(".dex/results")).unwrap();
    }

    fn redirected_snapshot(ancestor: bool) {
        let f = Fixture::new();
        let outside = crate::projects::tests::Fixture::new();
        let target = outside.dir("report-project");
        let results = target.join(".dex/results");
        std::fs::create_dir_all(&results).unwrap();
        let sentinel = results.join(format!("{}.md", f.job.id));
        std::fs::write(&sentinel, "outside original").unwrap();
        let path = if ancestor { &f.dir.0 } else { &f.root };
        let moved = path.with_extension("retained");
        std::fs::rename(path, &moved).unwrap();
        let linked = crate::memory::tests::link_dir(if ancestor { &outside.0 } else { &target }, path);
        if !linked {
            std::fs::rename(&moved, path).unwrap();
            panic!("önkoşul hatası: junction/symlink oluşturulamadı");
        }
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"host body"}));
        // Restore before assertions; fixture cleanup must never traverse the test junction.
        std::fs::remove_dir(path).unwrap();
        std::fs::rename(&moved, path).unwrap();
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside original", "host wrote through redirected snapshot");
        assert_eq!(std::fs::read_dir(results).unwrap().count(), 1, "no outside temp write");
        assert_eq!(reply["isError"], true, "{reply}");
        assert_eq!(f.state().status, "running");
        assert!(f.state().result_path.is_none());
    }

    #[test]
    fn content_rejects_snapshot_root_replaced_by_junction() { redirected_snapshot(false); }

    #[test]
    fn content_rejects_snapshot_ancestor_replaced_by_junction() { redirected_snapshot(true); }

    #[cfg(windows)]
    #[test]
    fn content_directory_swap_after_check_never_writes_outside() {
        let f = Fixture::new();
        let results = f.file().parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&results).unwrap();
        let moved = results.with_extension("retained");
        let outside = f.dir.dir("outside-race");
        let redirected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wrote = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (r, w, q, p, saved) = (redirected.clone(), wrote.clone(), outside.clone(), results.clone(), moved.clone());
        let reply = delegate::with_report_io_hook(move |stage, temp| {
            if stage == "directories" && std::fs::rename(&p, &saved).is_ok() {
                assert!(crate::memory::tests::link_dir(&q, &p), "önkoşul hatası: junction oluşturulamadı");
                r.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if stage == "written" {
                w.store(q.join(temp.file_name().unwrap()).exists(), std::sync::atomic::Ordering::SeqCst);
            }
        }, || f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"host body"})));
        if redirected.load(std::sync::atomic::Ordering::SeqCst) {
            std::fs::remove_dir(&results).unwrap();
            std::fs::rename(&moved, &results).unwrap();
        }
        assert!(!wrote.load(std::sync::atomic::Ordering::SeqCst), "host created/wrote temp in Q after check");
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 1, "only fixture .git in Q");
        if reply["isError"] == true { assert_eq!(f.state().status, "running"); }
        else { assert_eq!(std::fs::read_to_string(f.file()).unwrap(), "host body"); }
    }

    #[cfg(windows)]
    #[test]
    fn content_publish_cannot_redirect_to_prepared_outside_temp() {
        let f = Fixture::new();
        let outside = f.dir.dir("prepared-race");
        let sentinel = outside.join(format!("{}.md", f.job.id));
        std::fs::write(&sentinel, "outside original").unwrap();
        let results = f.file().parent().unwrap().to_path_buf();
        let moved = results.with_extension("retained");
        let attempted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let redirected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (a, r, q, p, saved) = (attempted.clone(), redirected.clone(), outside.clone(), results.clone(), moved.clone());
        let reply = delegate::with_report_io_hook(move |stage, temp| {
            if stage == "temp_name" { std::fs::write(q.join(temp.file_name().unwrap()), "prepared malicious body").unwrap(); }
            if stage == "publish" {
                a.store(true, std::sync::atomic::Ordering::SeqCst);
                if std::fs::rename(&p, &saved).is_ok() {
                    assert!(crate::memory::tests::link_dir(&q, &p), "önkoşul hatası: junction oluşturulamadı");
                    r.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }, || f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"host body"})));
        if redirected.load(std::sync::atomic::Ordering::SeqCst) {
            std::fs::remove_dir(&results).unwrap();
            std::fs::rename(&moved, &results).unwrap();
        }
        assert!(attempted.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside original");
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), "host body");
    }

    #[cfg(windows)]
    #[test]
    fn content_temp_is_not_mutable_before_publication() {
        let f = Fixture::new();
        let attacked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let a = attacked.clone();
        let body = "karar: düzelt\n## önemli\n- original finding\n";
        crate::db::with(&f.core.db, |c| c.execute(
            "INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(?1,?1,'fixture-sha',1)", [f.job.id]
        ).map_err(|e| e.to_string())).unwrap();
        let reply = delegate::with_report_io_hook(move |stage, temp| {
            if stage == "publish" {
                a.store(true, std::sync::atomic::Ordering::SeqCst);
                assert!(std::fs::write(temp, "karar: onay\n").is_err(), "temp can be overwritten");
                assert!(std::fs::remove_file(temp).is_err(), "temp can be replaced");
                assert!(std::fs::rename(temp, temp.with_extension("stolen")).is_err(), "temp can be renamed");
            }
        }, || f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":body})));
        assert!(attacked.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), body);
        let review = crate::db::with(&f.core.db, |c| crate::review::get(c, f.job.id)).unwrap().unwrap();
        assert_eq!(review.verdict.as_deref(), Some("düzelt"));
        assert_eq!(review.findings.as_deref(), Some("- original finding"));
    }

    #[cfg(windows)]
    #[test]
    fn reviewer_verdict_uses_content_even_if_archive_changes_before_record() {
        let f = Fixture::new();
        crate::db::with(&f.core.db, |c| c.execute(
            "INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(?1,?1,'fixture-sha',1)", [f.job.id]
        ).map_err(|e| e.to_string())).unwrap();
        let reply = delegate::with_report_io_hook(|stage, path| {
            if stage == "archived" { std::fs::write(path, "karar: onay\n## önemli\n- forged finding\n").unwrap(); }
        }, || f.call(json!({"delegationId":f.job.id,"summary":"karar: onay","content":"karar: düzelt\n## önemli\n- original finding\n"})));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        let review = crate::db::with(&f.core.db, |c| crate::review::get(c, f.job.id)).unwrap().unwrap();
        assert_eq!(review.verdict.as_deref(), Some("düzelt"));
        assert_eq!(review.findings.as_deref(), Some("- original finding"));
    }

    #[cfg(windows)]
    fn reparse_directory(path: &Path, target: Option<&Path>) -> std::io::Result<()> {
        reparse_directory_with_access(path, target, 0x40000000)
    }

    #[cfg(windows)]
    fn reparse_directory_with_access(path: &Path, target: Option<&Path>, access: u32) -> std::io::Result<()> {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
        #[link(name = "kernel32")]
        extern "system" {
            fn DeviceIoControl(handle: *mut std::ffi::c_void, code: u32, input: *const std::ffi::c_void, input_len: u32,
                output: *mut std::ffi::c_void, output_len: u32, returned: *mut u32, overlapped: *mut std::ffi::c_void) -> i32;
        }
        let file = std::fs::OpenOptions::new().access_mode(access).share_mode(7)
            .custom_flags(0x02000000 | 0x00200000).open(path)?;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&0xA0000003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
        if let Some(target) = target {
            let target = target.canonicalize()?.to_string_lossy().into_owned();
            let target = format!(r"\??\{}", target.strip_prefix(r"\\?\").unwrap_or(&target));
            let text: Vec<u16> = std::ffi::OsStr::new(&target).encode_wide().collect();
            buffer.extend_from_slice(&((8 + (text.len() + 2) * 2) as u16).to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            for value in [0, (text.len() * 2) as u16, ((text.len() + 1) * 2) as u16, 0] {
                buffer.extend_from_slice(&value.to_le_bytes());
            }
            for c in text { buffer.extend_from_slice(&c.to_le_bytes()); }
            buffer.extend_from_slice(&[0; 4]);
        } else { buffer.extend_from_slice(&[0; 4]); }
        let mut returned = 0;
        // SAFETY: live directory handle, initialized mount-point buffer, synchronous null OVERLAPPED.
        let ok = unsafe { DeviceIoControl(file.as_raw_handle(), if target.is_some() { 0x000900A4 } else { 0x000900AC },
            buffer.as_ptr().cast(), buffer.len() as u32, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut()) };
        if ok == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }

    #[cfg(windows)]
    #[test]
    fn content_in_place_reparse_after_check_cannot_escape_handles() {
        let f = Fixture::new();
        let results = f.file().parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&results).unwrap();
        let outside = f.dir.dir("in-place-target");
        let sentinel = outside.join(format!("{}.md", f.job.id));
        std::fs::write(&sentinel, "outside original").unwrap();
        // Positive control: lack of junction support is an explicit prerequisite failure.
        let control = f.dir.0.join("reparse-control");
        std::fs::create_dir(&control).unwrap();
        reparse_directory(&control, Some(&outside)).expect("önkoşul hatası: yerinde junction oluşturulamadı");
        reparse_directory(&control, None).unwrap();
        let linked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let escaped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (l, e, q, p) = (linked.clone(), escaped.clone(), outside.clone(), results.clone());
        let root = f.root.clone();
        let ancestor = f.dir.0.clone();
        let reply = delegate::with_report_io_hook(move |stage, temp| {
            if stage == "directories" {
                for directory in [&ancestor, &root, &root.join(".dex"), &p] {
                    assert!(std::fs::rename(directory, directory.with_extension("moved")).is_err(), "directory chain not pinned");
                }
                // Attribute-only opens are not covered by Windows data sharing checks either.
                let changed = reparse_directory(&p, Some(&q)).is_ok()
                    || reparse_directory_with_access(&p, Some(&q), 0x100).is_ok();
                l.store(changed, std::sync::atomic::Ordering::SeqCst);
            }
            if stage == "written" { e.store(q.join(temp.file_name().unwrap()).exists(), std::sync::atomic::Ordering::SeqCst); }
        }, || f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"host body"})));
        if linked.load(std::sync::atomic::Ordering::SeqCst) { reparse_directory(&results, None).unwrap(); }
        assert!(!escaped.load(std::sync::atomic::Ordering::SeqCst), "relative create followed parent reparse");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside original");
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 2, "only .git and sentinel in Q");
        if reply["isError"] == true { assert_eq!(f.state().status, "running"); }
        else { assert_eq!(std::fs::read_to_string(f.file()).unwrap(), "host body"); }
    }

    #[test]
    fn service_content_uses_snapshot_slug_and_does_not_touch_other_job() {
        let prompt = "Türkçe / ayraç: ../../ başka\\iş | araştırma";
        let f = Fixture::service(prompt);
        let other_job = crate::db::with(&f.core.db, |c| {
            let lead = delegate::caller(c, "timur")?;
            delegate::insert(c, delegate::NewDelegation { from: &lead, to_agent: "svc".into(), team_id: "default".into(),
                service_team_id: Some("service".into()), prompt: prompt.into(), task_id: None, patron_words: None, effort: None })
        }).unwrap();
        let expected = f.root.join("docs").join("research").join(format!("{}-turkce-ayrac-baska-is-arastirma.md", f.job.id));
        let foreign = f.root.join("docs").join("research").join(format!("{}-turkce-ayrac-baska-is-arastirma.md", other_job.id));
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&foreign, "other job original").unwrap();
        let later = f.dir.dir("later-team-cwd");
        crate::db::with(&f.core.db, |c| c.execute("UPDATE teams SET cwd=?1 WHERE id='default'", [later.to_string_lossy()])
            .map_err(|e| e.to_string())).unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"service body"}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(f.state().result_path.as_deref(), expected.to_str());
        assert_eq!(std::fs::read_to_string(expected).unwrap(), "service body");
        assert_eq!(std::fs::read_to_string(foreign).unwrap(), "other job original");
        assert_eq!(crate::db::with(&f.core.db, |c| delegate::get(c, other_job.id)).unwrap().status, "queued");
        assert!(!later.join("docs").exists());
        assert!(!f.root.join(".dex/results").exists());
    }

    #[test]
    fn service_content_rejects_linked_or_non_directory_research_chain() {
        for case in ["docs-link", "research-link", "docs-file", "research-file"] {
            let f = Fixture::service("../../ Türkçe / araştırma");
            let outside = f.dir.dir("outside-service");
            let path = if case.starts_with("docs-") { f.root.join("docs") }
                else { std::fs::create_dir(f.root.join("docs")).unwrap(); f.root.join("docs/research") };
            if case.ends_with("link") {
                assert!(crate::memory::tests::link_dir(&outside, &path), "önkoşul hatası: junction oluşturulamadı");
            } else { std::fs::write(&path, "original file").unwrap(); }
            let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"service body"}));
            if case.ends_with("link") { std::fs::remove_dir(path).unwrap(); }
            else { assert_eq!(std::fs::read_to_string(path).unwrap(), "original file"); }
            assert_eq!(reply["isError"], true, "{case}: {reply}");
            assert_eq!(f.state().status, "running");
            assert!(f.state().result_path.is_none());
            assert_eq!(std::fs::read_dir(outside).unwrap().count(), 1, "only .git; no outside write");
        }
    }

    #[test]
    fn content_has_a_byte_limit_and_requires_a_string() {
        let f = Fixture::new();
        for content in [json!("x".repeat(1024 * 1024 + 1)), json!(null), json!(42)] {
            let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":content}));
            assert_eq!(reply["isError"], true);
            assert!(reply.to_string().contains("content"));
            assert!(!f.file().exists());
            assert_eq!(f.state().status, "running");
        }
    }

    #[test]
    fn content_preserves_unanswered_question_and_closed_job_guards() {
        let f = Fixture::new();
        crate::db::with(&f.core.db, |c| {
            let worker = delegate::caller(c, "kaan")?;
            crate::questions::ask(c, &worker, &json!({"delegationId":f.job.id,"question":"decision?"}))
        }).unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"body"}));
        assert_eq!(reply["isError"], true);
        assert!(reply.to_string().contains("Açık sorun"));
        assert!(!f.file().exists());
        assert_eq!(f.state().status, "running");
        crate::db::with(&f.core.db, |c| delegate::set_status(c, f.job.id, "done")).unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"body"}));
        assert_eq!(reply["isError"], true);
        assert!(reply.to_string().contains("zaten kapandı"));
        assert!(!f.file().exists());
    }

    #[test]
    fn content_cannot_corrupt_snapshot_through_database() {
        let f = Fixture::new();
        let invalid = f.root.join("..").join("report-project");
        let error = crate::db::with(&f.core.db, |c| c.execute("UPDATE delegations SET project_root=?2 WHERE id=?1",
            rusqlite::params![f.job.id, invalid.to_string_lossy()]).map_err(|e| e.to_string())).unwrap_err();
        assert!(error.contains("project_mismatch"), "{error}");
        assert_eq!(f.state().project_root, f.job.project_root);
        assert!(!f.file().exists());
    }

    #[test]
    fn content_atomically_replaces_existing_report_without_temp_residue() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.file().parent().unwrap()).unwrap();
        std::fs::write(f.file(), "old report").unwrap();
        let body = "complete new report\n";
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":body}));
        assert_eq!(reply["structuredContent"]["status"], "done", "{reply}");
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), body);
        assert_eq!(std::fs::read_dir(f.file().parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn content_failed_replace_keeps_original_and_cleans_temp() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.file().parent().unwrap()).unwrap();
        std::fs::write(f.file(), "original").unwrap();
        let mut permissions = std::fs::metadata(f.file()).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(f.file(), permissions.clone()).unwrap();
        let reply = f.call(json!({"delegationId":f.job.id,"summary":"bitti","content":"new body"}));
        // Restore before assertions so a failed assertion can still clean up the fixture.
        permissions.set_readonly(false);
        std::fs::set_permissions(f.file(), permissions).unwrap();
        assert_eq!(reply["isError"], true, "{reply}");
        assert!(reply.to_string().contains("atomik taşınamadı"), "{reply}");
        assert_eq!(f.state().status, "running");
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), "original");
        assert_eq!(std::fs::read_dir(f.file().parent().unwrap()).unwrap().count(), 1);
    }
}

impl Mcp {
    /// Fresh key for this spawn of `agent`; the previous process of that id is gone (pty_spawn kills it).
    pub fn key_for(&self, agent: &str) -> String {
        self.key_for_job(agent, None)
    }

    /// G/D pass the trusted launch job before spawning, including while it is queued.
    /// A later queued job never changes the authority of this key. No env/IPC job id is read.
    pub fn key_for_job(&self, agent: &str, delegation_id: Option<i64>) -> String {
        let mut raw = [0u8; 16];
        let _ = getrandom::fill(&mut raw);
        let key: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        self.keys.lock().unwrap().insert(agent.to_string(), SpawnIdentity { key: key.clone(), delegation_id });
        key
    }

    /// #838 G (#906): the live session of `agent` took a job of its own project by paste — its spawn
    /// key now speaks for `job`. Only a job-bound key changes (an unbound one stays unbound) and the
    /// key itself stays. The previous job, when it matched `expected` and changed. The caller
    /// checks agent and project; stale paste rollback must not overwrite a newer binding.
    pub(crate) fn rebind_job(&self, agent: &str, expected: i64, job: i64) -> Option<i64> {
        #[cfg(test)]
        if let Some(before) = BEFORE_REBIND.with(|c| c.borrow_mut().take()) { before(); }
        let mut keys = self.keys.lock().unwrap();
        let known = keys.get_mut(agent)?;
        let prior = known.delegation_id?;
        if prior != expected { return None; }
        known.delegation_id = Some(job);
        Some(prior)
    }

    /// The per-spawn key binds every local HTTP call to the agent id in its header.
    pub(crate) fn verifies(&self, agent: &str, key: &str) -> bool {
        self.identity(agent, key).is_some()
    }

    fn identity(&self, agent: &str, key: &str) -> Option<Option<i64>> {
        self.keys
            .lock()
            .unwrap()
            .get(agent)
            .filter(|known|eq_ct(key.as_bytes(), known.key.as_bytes()))
            .map(|known|known.delegation_id)
    }
}

const PROTOCOL: &str = "2025-06-18";
const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Writes `<config_dir>/dex-mcp-<pid>.json` for the hook receiver's `port` (sweeping files of
/// dead processes first). The host removes it on exit with [`Mcp::remove_config`].
pub fn start(config_dir: &Path, port: u16) -> Result<Mcp, String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    sweep_stale(config_dir);
    let config = config_dir.join(format!("dex-mcp-{}.json", std::process::id()));
    let text = serde_json::to_vec_pretty(&config_json(port)).map_err(|e| e.to_string())?;
    std::fs::write(&config, text).map_err(|e| e.to_string())?;
    Ok(Mcp { config, keys: Mutex::default() })
}

impl Mcp {
    pub fn remove_config(&self) {
        let _ = std::fs::remove_file(&self.config);
    }
}

fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_string_lossy().strip_prefix("dex-mcp-").and_then(|r| r.strip_suffix(".json")).and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|p| !crate::proc::alive(p)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn config_json(port: u16) -> Value {
    json!({ "mcpServers": { "dex": {
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "headers": {
            "Authorization": "Bearer ${DEX_HOOK_TOKEN}",
            "X-Dex-Agent": "${DEX_AGENT_ID}",
            "X-Dex-Key": "${DEX_MCP_KEY}"
        }
    }}})
}

/// `hooks::serve` hands over any connection whose request line targets `/mcp`.
pub(crate) fn is_mcp(s: &TcpStream, deadline: crate::local_http::Deadline) -> Result<bool, &'static str> {
    let mut buf = [0u8; 16];
    loop {
        deadline.arm(s)?;
        match s.peek(&mut buf) {
            Ok(n) if n >= 12 || buf[..n].contains(&b'\n') => {
                let line = String::from_utf8_lossy(&buf[..n]);
                return Ok(line.split(' ').nth(1).is_some_and(|p| p == "/mcp" || p.starts_with("/mcp?") || p.starts_with("/mcp/") || (n == 16 && p.starts_with("/mcp"))));
            }
            Ok(0) => return Err("400 Bad Request"),
            Err(_) => return Err("408 Request Timeout"),
            Ok(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

struct Req {
    method: String,
    agent: String,
    key: String,
    body: Vec<u8>,
}

fn read(s: &mut TcpStream, mcp: &Mcp, token: &str, deadline: crate::local_http::Deadline) -> Result<Req, &'static str> {
    let (mut buf, head_end) = crate::local_http::read_head(s, deadline)?;
    let mut chunk = [0u8; 8192];
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let method = lines.next().and_then(|l| l.split(' ').next()).unwrap_or("").to_string();
    let (mut len, mut auth, mut agent, mut key) = (0usize, String::new(), String::new(), String::new());
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.parse().map_err(|_| "400 Bad Request")?,
                "authorization" => auth = v,
                "x-dex-agent" => agent = v,
                "x-dex-key" => key = v,
                _ => {}
            }
        }
    }
    // Reject the headers before waiting for or allocating the declared body.
    let identity = mcp.identity(&agent, &key);
    if !eq_ct(auth.strip_prefix("Bearer ").unwrap_or("").as_bytes(), token.as_bytes()) || identity.is_none() {
        return Err("401 Unauthorized");
    }
    if len > 4 * 1024 * 1024 {
        return Err("413 Payload Too Large");
    }
    let mut body = buf.split_off(head_end);
    while body.len() < len {
        let n = deadline.read(s, &mut chunk)?;
        if n == 0 {
            return Err("400 Bad Request");
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Req { method, agent, key, body })
}

fn eq_ct(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(s: &mut TcpStream, status: &str, body: Option<&str>) {
    let body = body.unwrap_or("");
    let ctype = if body.is_empty() { "" } else { "Content-Type: application/json\r\n" };
    let _ = write!(s, "HTTP/1.1 {status}\r\n{ctype}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

pub(crate) fn serve(mut s: TcpStream, core: &Core, mcp: &Mcp, token: &str, deadline: crate::local_http::Deadline) {
    let req = match read(&mut s, mcp, token, deadline) {
        Ok(r) => r,
        Err(code) => return respond(&mut s, code, None),
    };
    // A spawn may rotate its key while the authenticated body is arriving.
    let identity = mcp.identity(&req.agent, &req.key);
    if identity.is_none() { return respond(&mut s, "401 Unauthorized", None); }
    // POST only: no server-initiated stream (GET) and no server-side session to end (DELETE).
    if req.method != "POST" {
        return respond(&mut s, "405 Method Not Allowed", None);
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => {
            let err = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } });
            return respond(&mut s, "400 Bad Request", Some(&err.to_string()));
        }
    };
    // Notifications / responses carry no id → 202, no body.
    let Some(id) = msg.get("id").cloned() else { return respond(&mut s, "202 Accepted", None) };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            let version = if SUPPORTED.contains(&asked) { asked } else { PROTOCOL };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "dex", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Dex ofisinin görev panosu ve devretme araçları. Pano yalnız kendi takımın. \
                                 Devretme (delegate, delegate_to_service) yalnız liderler içindir; \
                                 sana devredilen işi bitirince report ile teslim et."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for(core, &req.agent, identity.flatten()) })),
        "tools/call" => Ok(call_in(core, &req.agent, identity.flatten(), &params)),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    let out = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    };
    respond(&mut s, "200 OK", Some(&out.to_string()));
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn catalog_delay_rechecks_leader_authority_before_plan_write() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("model-authority-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let lead = crate::db::with(&core.db, |c| delegate::caller(c, "timur")).unwrap();
        let catalogue_core = core.clone();
        crate::job_model::CATALOG_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(move || {
            crate::db::with(&catalogue_core.db, |c| {
                c.execute("UPDATE agents SET kind='worker' WHERE id='timur'", []).map_err(|e| e.to_string())?;
                Ok(())
            })
        })));
        let result = core.plan_create_validated(&lead, serde_json::from_value(json!({"tasks":[{
            "key":"A","title":"analysis","prompt":"analysis","agentId":"kaan","model":"opus","modelReason":"analysis"
        }]})).unwrap());
        crate::job_model::CATALOG_HOOK.with(|h| h.borrow_mut().take());
        assert!(result.is_err(), "a former leader must not create a plan after catalogue validation");
        crate::db::with(&core.db, |c| {
            assert_eq!(c.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            Ok(())
        }).unwrap();
        core.shutdown();
    }

    #[test]
    fn slow_model_catalog_releases_db_and_preserves_route_at_write() {
        use std::sync::mpsc;
        use std::time::Duration;
        for plan in [false, true] {
            for role in [false, true] {
                let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
                    .join(format!("slow-model-{}-{}", std::process::id(), crate::agents::next_seq()));
                let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
                    crate::Events::new(crate::events::NullSink)).unwrap();
                let (lead, slug) = crate::db::with(&core.db, |c| {
                    let slug = crate::teams::get_agent(c, "kaan")?.role_slug.unwrap();
                    c.execute("UPDATE agents SET role_slug=NULL WHERE kind='worker' AND id<>'kaan'", []).unwrap();
                    Ok((delegate::caller(c, "timur")?, slug))
                }).unwrap();
                let (entered_tx, entered_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                let request_core = core.clone();
                let request = std::thread::spawn(move || {
                    crate::job_model::CATALOG_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(move || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                        Ok(())
                    })));
                    let target = if role { json!({"role":slug}) } else { json!({"agentId":"kaan"}) };
                    let mut args = target;
                    args["prompt"] = json!("slow catalog");
                    args["model"] = json!("opus");
                    args["modelReason"] = json!("deep analysis");
                    let result = if plan {
                        args["key"] = json!("A");
                        args["title"] = json!("slow catalog");
                        request_core.plan_create_validated(&lead, serde_json::from_value(json!({"tasks":[args]})).unwrap()).map(|_| ())
                    } else {
                        delegate_call(&request_core, &lead, &args, false).map(|_| ())
                    };
                    crate::job_model::CATALOG_HOOK.with(|h| h.borrow_mut().take());
                    result
                });
                entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                let (db_tx, db_rx) = mpsc::channel();
                let db_core = core.clone();
                let writer = std::thread::spawn(move || {
                    let _gate = db_core.agent_start_gate.lock().unwrap();
                    let worker_read = run(&db_core, "ayla", None, "task_list", &json!({}));
                    let result = crate::db::with(&db_core.db, |c| {
                        // A different worker's hook/RPC progresses while the catalogue is paused.
                        worker_read?;
                        c.execute("UPDATE agents SET active_engine='codex',active_model='gpt-chain' WHERE id='kaan'", [])
                            .map_err(|e| e.to_string())?;
                        Ok(())
                    });
                    db_tx.send(result).unwrap();
                });
                let progressed = db_rx.recv_timeout(Duration::from_millis(750));
                release_tx.send(()).unwrap();
                let requested = request.join().unwrap();
                writer.join().unwrap();
                assert!(progressed.is_ok(), "DB must progress during blocked catalogue (plan={plan}, role={role})");
                progressed.unwrap().unwrap();
                requested.unwrap();
                crate::db::with(&core.db, |c| {
                    if plan {
                        let note = crate::plan::take_notes(c, "timur")?.unwrap();
                        assert!(note.contains("Plan kabul edildi") && note.contains("gpt-chain"), "{note}");
                        crate::plan::step(c)?;
                    }
                    let p = crate::proposals::list_pending(c)?.remove(0);
                    crate::proposals::decide(c, Path::new("."), p.id, true, None)?;
                    let mut a = crate::teams::get_agent(c, "kaan")?;
                    assert!(!crate::job_model::apply(c, &mut a, p.delegation_id)?);
                    assert_eq!(a.active_model.as_deref(), Some("gpt-chain"));
                    let note = crate::plan::take_notes(c, "timur")?.unwrap();
                    assert!(note.contains("opus") && note.contains("gpt-chain") && note.contains("Neden:"), "{note}");
                    Ok(())
                }).unwrap();
                core.shutdown();
            }
        }
    }

    #[test]
    fn role_model_approval_skips_pending_and_starts_on_available_route() {
        for different_route in [false, true] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
                .join(format!("role-model-{}-{}", std::process::id(), crate::agents::next_seq()));
            let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
                crate::Events::new(crate::events::NullSink)).unwrap();
            let (lead, role) = crate::db::with(&core.db, |c| {
                let role = crate::teams::get_agent(c, "kaan")?.role_slug.unwrap();
                c.execute("UPDATE agents SET role_slug=NULL WHERE kind='worker' AND id<>'kaan'", []).unwrap();
                c.execute("UPDATE agents SET sort=-10 WHERE id='kaan'", []).unwrap();
                if different_route {
                    c.execute("UPDATE agents SET role_slug=?1,provider='glm',model='GLM-5.3',sort=10 WHERE id='ayla'", [&role]).unwrap();
                }
                Ok((delegate::caller(c, "timur")?, role))
            }).unwrap();
            let pending = delegate_call(&core, &lead, &json!({"role":role,"prompt":"approval intersection",
                "model":"opus","modelReason":"deep analysis"}), false).unwrap();
            assert!(pending.to_agent.is_empty(), "pending approval must not reserve a worker");
            assert_eq!(core.delegation_dispatch(pending.clone()).unwrap().status, "queued");
            assert!(!core.ptys.is_running("kaan"));
            let proposal = core.proposal_list().unwrap().remove(0);
            assert_eq!(proposal.role_slug, role);
            assert!(proposal.name.is_none(), "approval card represents the role, not a provisional worker");
            assert!(!proposal.reason.contains("Kaan"));
            let ready = delegate_call(&core, &lead, &json!({"role":role,"prompt":"ready behind approval"}), false).unwrap();
            assert_eq!(ready.to_agent, "kaan", "pending head must not block ready FIFO jobs");
            crate::db::with(&core.db, |c| {
                delegate::start_request(c, &ready)?;
                delegate::set_status(c, ready.id, "running")?;
                assert_eq!(delegate::get(c, ready.id)?.status, "running");
                if !different_route { delegate::set_status(c, ready.id, "done")?; }
                Ok(())
            }).unwrap();
            crate::agents::CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
            let result = core.proposal_decide(proposal.id, true, None);
            let captured = crate::agents::CAPTURE.with(|c| c.borrow_mut().take().unwrap());
            result.unwrap();
            assert_eq!(captured.len(), 1, "approval must assign and spawn the available role worker");
            assert_eq!(captured[0].1, Some(pending.id));
            let expected_worker = if different_route { "ayla" } else { "kaan" };
            let expected_model = if different_route { "GLM-5.3" } else { "opus" };
            assert!(captured[0].0.args.windows(2).any(|a| a == ["--model", expected_model]));
            crate::db::with(&core.db, |c| {
                let actual = delegate::get(c, pending.id)?;
                assert_eq!(actual.to_agent, expected_worker);
                assert_eq!(actual.status, "running");
                assert_eq!(actual.model.as_deref(), Some("opus"));
                assert_eq!(actual.actual_model.as_deref(), Some(expected_model));
                if different_route {
                    let note = crate::plan::take_notes(c, "timur")?.unwrap();
                    assert!(note.contains("opus") && note.contains("GLM-5.3") && note.contains("glm")
                        && note.contains("Neden: rol havuzu") && !note.contains("opus ()"), "{note}");
                }
                delegate::set_status(c, pending.id, "done")
            }).unwrap();
            core.shutdown();
        }
    }

    #[test]
    fn model_job_requires_patron_approval_before_start() {
        let test_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("job-model-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: test_root.join("data"), config_dir: test_root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let lead = crate::db::with(&core.db, |c| delegate::caller(c, "timur")).unwrap();
        let d = delegate_call(&core, &lead, &json!({"agentId":"kaan","prompt":"model işi","model":"opus","modelReason":"karmaşık analiz"}), false).unwrap();
        let pending = core.proposal_list().unwrap();
        assert_eq!(pending.len(), 1, "model işi patron onayında beklemeli");
        assert_eq!(pending[0].model, "opus");
        assert_eq!(d.status, "queued");
        assert_eq!(core.delegation_dispatch(d.clone()).unwrap().status, "queued");
        assert!(!core.ptys.is_running("kaan"));
        crate::agents::CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        let decision = core.proposal_decide(pending[0].id, true, None);
        let captured = crate::agents::CAPTURE.with(|c| c.borrow_mut().take().unwrap());
        decision.unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].0.args.windows(2).any(|a| a == ["--model", "opus"]));
        assert_eq!(captured[0].1, Some(d.id));
        crate::db::with(&core.db, |c| {
            assert_eq!(delegate::get(c, d.id)?.model_approval.as_deref(), Some("approved"));
            assert_eq!(delegate::get(c, d.id)?.actual_model.as_deref(), Some("opus"));
            assert_eq!(delegate::get(c, d.id)?.actual_provider.as_deref(), Some("anthropic"));
            assert_ne!(crate::teams::get_agent(c, "kaan")?.model, "opus");
            delegate::set_status(c, d.id, "done")
        }).unwrap();
        core.ptys.kill("kaan");
        core.shutdown();
    }

    #[test]
    fn approved_model_job_continues_through_real_chain_spawn_and_informs_leader() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("job-model-chain-{}-{}",std::process::id(),crate::agents::next_seq()));
        let core = Core::start(crate::Paths {data_dir:root.join("data"),config_dir:root.join("config")},crate::Events::new(crate::events::NullSink)).unwrap();
        let lead = crate::db::with(&core.db,|c| {
            c.execute("UPDATE agents SET provider='glm',model='GLM-5.3' WHERE id='kaan'",[]).unwrap();
            crate::chain::set_order(c,&["glm".into(),"anthropic".into(),"codex".into(),"muse".into(),"local".into()])?;
            delegate::caller(c,"timur")
        }).unwrap();
        let d=delegate_call(&core,&lead,&json!({"agentId":"kaan","prompt":"Model zinciri","model":"GLM-5.3-Flash","modelReason":"Hızlı analiz"}),false).unwrap();
        let p=core.proposal_list().unwrap().remove(0);
        crate::agents::CAPTURE.with(|c|*c.borrow_mut()=Some(Vec::new()));
        let result=core.proposal_decide(p.id,true,None);
        let initial=crate::agents::CAPTURE.with(|c|c.borrow_mut().take().unwrap());
        result.unwrap();
        assert_eq!(initial.len(),1);
        assert!(initial[0].0.args.windows(2).any(|a|a==["--model","GLM-5.3-Flash"]));
        let body=json!({"error":"billing_error","last_assistant_message":"Credit balance is too low"});
        let plan=core.chain_plan("kaan",&body).expect("onaylı iş zincir geçişini korur");
        crate::agents::CAPTURE.with(|c|*c.borrow_mut()=Some(Vec::new()));
        core.chain_execute("kaan",plan);
        let continued=crate::agents::CAPTURE.with(|c|c.borrow_mut().take().unwrap());
        assert_eq!(continued.len(),1);
        assert_eq!(continued[0].0.provider.as_deref(),Some("anthropic"));
        assert_eq!(continued[0].1,Some(d.id));
        crate::db::with(&core.db,|c| {
            let actual=delegate::get(c,d.id)?;
            assert_eq!(actual.status,"running");
            assert_eq!(actual.model.as_deref(),Some("GLM-5.3-Flash"));
            assert_eq!(actual.actual_provider.as_deref(),Some("anthropic"));
            assert_ne!(actual.actual_model.as_deref(),actual.model.as_deref());
            assert_eq!(crate::proposals::get(c,p.id)?.status,"approved");
            assert_eq!(crate::teams::get_agent(c,"kaan")?.model,"GLM-5.3");
            let note=crate::plan::take_notes(c,"timur")?.unwrap();
            assert!(note.contains("GLM-5.3-Flash") && note.contains("anthropic") && note.contains("kota"),"{note}");
            delegate::set_status(c,d.id,"done")
        }).unwrap();
        core.shutdown();
    }

    #[test]
    fn unauthenticated_headers_are_rejected_without_waiting_for_body() {
        use std::io::{Read, Write};
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("mcp-header-auth-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let key = core.mcp.key_for("timur");
        let mut outcomes = Vec::new();
        for (token, key) in [("wrong-token", key.as_str()), (core.hooks.token.as_str(), "wrong-key")] {
            let mut client = TcpStream::connect(("127.0.0.1", core.hooks.port)).unwrap();
            client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            write!(client, "POST /mcp HTTP/1.1\r\nAuthorization: Bearer {token}\r\nX-Dex-Agent: timur\r\nX-Dex-Key: {key}\r\nContent-Length: 100000\r\n\r\n").unwrap();
            let mut response = Vec::new();
            outcomes.push(client.read_to_end(&mut response).map(|_| String::from_utf8_lossy(&response).into_owned()));
        }
        core.shutdown();
        for result in outcomes {
            assert!(result.as_ref().is_ok_and(|r| r.starts_with("HTTP/1.1 401 Unauthorized")), "{result:?}");
        }
    }

    #[test]
    fn memory_search_http_uses_spawn_scope_and_validates_limits() {
        let f = crate::projects::tests::Fixture::new();
        let core_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("memory-search-core-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = Core::start(crate::Paths { data_dir: core_root.join("data"), config_dir: core_root.join("config") },
            crate::Events::new(crate::events::NullSink)).unwrap();
        let team = f.0.join("memory");
        let put = |path: &Path, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for i in 0..10 {
            put(&team.join(format!("shared/n{i}.md")), "---\nname: needle\ndescription: sığmadı\n---\nneedle");
        }
        put(&team.join("agents/kaan/own.md"), "needle");
        put(&team.join("agents/ayla/secret.md"), "needle");
        put(&team.join("agents/timur/leader.md"), "needle");
        let one = f.dir("one");
        let two = f.dir("two");
        put(&one.join(".dex/memory/shared/project.md"), "needle");
        put(&two.join(".dex/memory/shared/foreign.md"), "needle");
        let job = crate::db::with(&core.db, |c| {
            c.execute("UPDATE teams SET team_memory_root=?1,agent_memory_root=?1 WHERE id='default'", [team.to_string_lossy()]).unwrap();
            let create = |root: &Path, name: &str| {
                let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
                crate::projects::create(c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready).unwrap()
            };
            let p = create(&one, "one");
            create(&two, "two");
            let lead = delegate::caller(c, "timur")?;
            let ctx = delegate::resolve_context(c, &f.0, &lead, Some(&p.id), None)?;
            delegate::insert_bound(c, delegate::NewDelegation {
                from: &lead, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None,
                prompt: "needle".into(), task_id: None, patron_words: None, effort: None,
            }, &ctx)
        }).unwrap();
        let invoke = |agent: &str, key: &str, method: &str, params: Value| -> Value {
            ureq::post(&format!("http://127.0.0.1:{}/mcp", core.hooks.port))
                .set("Authorization", &format!("Bearer {}", core.hooks.token))
                .set("X-Dex-Agent", agent).set("X-Dex-Key", key)
                .timeout(Duration::from_secs(5))
                .send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})).unwrap().into_json().unwrap()
        };
        let key = core.mcp.key_for_job("kaan", Some(job.id));
        let listed = invoke("kaan", &key, "tools/list", json!({}));
        let default = invoke("kaan", &key, "tools/call", json!({"name":"memory_search","arguments":{"query":"needle"}}));
        let all = invoke("kaan", &key, "tools/call", json!({"name":"memory_search","arguments":{"query":"needle","limit":100}}));
        let one = invoke("kaan", &key, "tools/call", json!({"name":"memory_search","arguments":{"query":"needle","limit":1}}));
        let invalid: Vec<_> = [json!({"query":" "}), json!({"query":"needle","limit":0}), json!({"query":"needle","limit":101}),
            json!({"query":"needle","limit":"8"}), json!({"query":"needle","project":"foreign"})].into_iter()
            .map(|args| invoke("kaan", &key, "tools/call", json!({"name":"memory_search","arguments":args}))).collect();
        let foreign_job = invoke("ayla", &core.mcp.key_for_job("ayla", Some(job.id)), "tools/call", json!({"name":"memory_search","arguments":{"query":"needle"}}));
        let lead = invoke("timur", &core.mcp.key_for("timur"), "tools/call", json!({"name":"memory_search","arguments":{"query":"needle","limit":100}}));
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(&core_root);
        assert_ne!(default["result"]["isError"], true, "{default}");
        let tools = listed["result"]["tools"].as_array().unwrap();
        let tool = tools.iter().find(|t| t["name"] == "memory_search").unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let results = |value: &Value| value["result"]["structuredContent"]["result"].as_array().unwrap().clone();
        assert_eq!(results(&default).len(), 8);
        assert_eq!(results(&one).len(), 1);
        let hits = results(&all);
        assert_eq!(hits.len(), 12);
        assert!(hits.iter().any(|h| h["slug"] == "project") && hits.iter().any(|h| h["slug"] == "own"));
        assert!(!hits.iter().any(|h| matches!(h["slug"].as_str(), Some("secret" | "foreign" | "leader"))));
        assert!(hits.iter().all(|h| h["path"].is_string() && h["snippet"].is_string() && h["description"].is_string()));
        assert!(invalid.iter().all(|v| v["result"]["isError"] == true));
        assert_eq!(foreign_job["result"]["isError"], true);
        assert_eq!(results(&lead).len(), 11);
        assert!(results(&lead).iter().any(|h| h["slug"] == "leader"));
    }

    #[test]
    fn project_tool_schemas_are_read_only_and_selection_is_an_id() {
        let all = tools();
        for name in ["delegate", "delegate_to_service", "plan"] {
            let t = all.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(t["inputSchema"]["properties"]["project"]["type"], "string", "{name}");
        }
        let plan = all.as_array().unwrap().iter().find(|t| t["name"] == "plan").unwrap();
        assert_eq!(plan["inputSchema"]["properties"]["tasks"]["items"]["properties"]["project"]["type"], "string");
        for name in ["project_list", "project_context"] {
            let t = all.as_array().unwrap().iter().find(|t| t["name"] == name).expect("project read tool");
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["inputSchema"]["additionalProperties"], false);
        }
        assert!(!all
            .as_array()
            .unwrap()
            .iter()
            .any(|t| matches!(t["name"].as_str(), Some("project_create" | "project_update" | "project_archive"))));
    }

    #[test]
    fn delegated_task_rolls_back_when_job_insert_fails() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let task = tasks::create(
            &c,
            tasks::NewTask {
                title: "x".into(),
                description: "before".into(),
                status: None,
                agent_id: None,
                team_id: Some("default".into()),
            },
        )
        .unwrap();
        c.execute_batch("CREATE TRIGGER reject_job BEFORE INSERT ON delegations BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let context = delegate::resolve_context(&c, Path::new(""), &lead, None, None).unwrap();
        let d = delegate::NewDelegation {
            from: &lead,
            to_agent: "kaan".into(),
            team_id: "default".into(),
            service_team_id: None,
            prompt: "x".into(),
            task_id: Some(task.id),
            patron_words: Some("patron".into()),
            effort: None,
        };
        assert!(create_job(&c, d, &context, None, None, None).is_err());
        let after = tasks::get(&c, task.id).unwrap();
        assert_eq!((after.status, after.agent_id, after.description, after.project_id), ("todo".into(), None, "before".into(), None));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn delegate_role_enters_pool_but_agent_id_stays_pinned() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "mcp-role-pool-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
        let core = Core::start(
            crate::Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let lead = crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET role_slug='backend' WHERE id='kaan'", [])
                .unwrap();
            let lead = delegate::caller(c, "timur")?;
            for to in ["ayla", "kaan"] {
                let d = delegate::insert(
                    c,
                    delegate::NewDelegation {
                        from: &lead,
                        to_agent: to.into(),
                        team_id: lead.team_id.clone(),
                        service_team_id: None,
                        prompt: "busy".into(),
                        task_id: None,
                        patron_words: None,
                        effort: None,
                    },
                )?;
                delegate::set_status(c, d.id, "running")?;
            }
            Ok(lead)
        })
        .unwrap();
        let pooled = call_with_args_for_test(
            &core,
            &lead.id,
            "delegate",
            json!({"role":"backend","prompt":"pool"}),
        );
        let pinned = call_with_args_for_test(
            &core,
            &lead.id,
            "delegate",
            json!({"agentId":"kaan","prompt":"pinned"}),
        );
        let status = call_with_args_for_test(
            &core,
            &lead.id,
            "delegation_status",
            json!({"id":pooled["structuredContent"]["delegationId"]}),
        );
        let hidden = call_with_args_for_test(
            &core,
            "bora",
            "delegation_status",
            json!({"id":pooled["structuredContent"]["delegationId"]}),
        );
        core.shutdown();
        assert_eq!(
            pooled["structuredContent"]["to"], "",
            "role jobs wait without a recipient"
        );
        assert_eq!(
            pooled["structuredContent"]["note"],
            "rol kuyruğunda: backend, sıra 1"
        );
        assert_eq!(pinned["structuredContent"]["to"], "kaan");
        assert_eq!(status["structuredContent"]["targetRole"], "backend");
        assert_eq!(status["structuredContent"]["queuePosition"], 1);
        assert_eq!(
            hidden["isError"], true,
            "pool rows are visible only to their leader"
        );
    }

    #[test]
    fn fix_and_gpu_targets_remain_with_the_original_worker() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        c.execute("UPDATE agents SET role_slug='backend' WHERE id='kaan'", [])
            .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let original = delegate::insert(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "ayla".into(),
                team_id: lead.team_id.clone(),
                service_team_id: None,
                prompt: "original".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        delegate::set_status(&c, original.id, "running").unwrap();
        c.execute("INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(?1,'ayla','','wt','dex/ayla/1','base',1)",[original.id]).unwrap();
        assert_eq!(
            select_target(
                &c,
                &lead,
                &json!({"role":"backend"}),
                None,
                Some(original.id)
            )
            .unwrap(),
            ("ayla".into(), None)
        );
        assert!(select_target(
            &c,
            &lead,
            &json!({"agentId":"kaan"}),
            None,
            Some(original.id)
        )
        .is_err());
        c.execute("INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,created_at) VALUES(1,'default','timur','ayla','[]','test',1,1)",[]).unwrap();
        c.execute("INSERT INTO gpu_grants(request_id,team_id,worker,types,budget,created_at) VALUES(1,'default','ayla','[\"agent\"]',1,1)",[]).unwrap();
        let grant = c.last_insert_rowid();
        assert_eq!(
            select_target(
                &c,
                &lead,
                &json!({"role":"backend","gpuGrant":grant}),
                None,
                None
            )
            .unwrap(),
            ("ayla".into(), None)
        );
        assert!(select_target(
            &c,
            &lead,
            &json!({"role":"frontend","gpuGrant":grant}),
            None,
            None
        )
        .is_err());
        c.execute(
            "INSERT INTO teams(id,name,kind,created_at) VALUES('foreign','Foreign','main',1)",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE gpu_grants SET team_id='foreign' WHERE id=?1",
            [grant],
        )
        .unwrap();
        assert!(select_target(
            &c,
            &lead,
            &json!({"role":"backend","gpuGrant":grant}),
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn per_spawn_key_rejects_spoofed_agent_identity() {
        let mcp = Mcp { config: PathBuf::new(), keys: Mutex::default() };
        let kaan = mcp.key_for("kaan");
        let ayla = mcp.key_for("ayla");
        assert!(mcp.verifies("kaan", &kaan));
        assert!(!mcp.verifies("ayla", &kaan), "another agent cannot reuse the shared hook token");
        assert!(!mcp.verifies("kaan", &ayla));
        assert!(!mcp.verifies("kaan", ""));
    }

    #[test]
    fn worker_project_read_uses_spawn_job_and_masks_foreign_ids() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let root = f.dir("one");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let one =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "One".into(), worktrees: None }, &ready).unwrap();
        let ctx = delegate::resolve_context(&c, &f.0, &lead, Some(&one.id), None).unwrap();
        let job = delegate::insert_bound(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "x".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
            &ctx,
        )
        .unwrap();
        let root = f.dir("two");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let two =
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "Two".into(), worktrees: None }, &ready).unwrap();
        let worker = delegate::caller(&c, "kaan").unwrap();
        let mcp = Mcp { config: PathBuf::new(), keys: Mutex::default() };
        let key = mcp.key_for_job("kaan", Some(job.id));
        let bound = mcp.identity("kaan", &key).unwrap();
        let out = project_read(&c, &f.0, &worker, bound, "project_context", &json!({"cwd":two.root_path,"delegationId":999})).unwrap();
        assert_eq!(out["projectId"], one.id, "queued job binding is visible before running");
        for args in [json!({"project":two.id}), json!({"project":"missing"}), json!({"teamId":"foreign"})] {
            assert_eq!(project_read(&c, &f.0, &worker, bound, "project_context", &args).unwrap_err(), "project_not_found");
        }
        assert_eq!(project_read(&c, &f.0, &worker, None, "project_context", &json!({})).unwrap_err(), "project_not_found");
        let list = project_read(&c, &f.0, &worker, bound, "project_list", &json!({})).unwrap();
        assert_eq!(list["projects"].as_array().unwrap().len(), 1);
        assert_eq!(list["projects"][0]["id"], one.id);
        let next = mcp.key_for_job("kaan", None);
        assert!(mcp.identity("kaan", &key).is_none());
        assert_eq!(mcp.identity("kaan", &next), Some(None));
    }

    #[test]
    fn fix_service_and_grant_keep_the_requesting_project_and_late_failure_rolls_back() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let project = |name: &str| {
            let root = f.dir(name);
            let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready).unwrap()
        };
        let one = project("one");
        let ctx = delegate::resolve_context(&c, &f.0, &lead, Some(&one.id), None).unwrap();
        let old = delegate::insert_bound(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "old".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
            &ctx,
        )
        .unwrap();
        c.execute("INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at) VALUES(?1,'kaan',?2,?2,'job-branch','base',1)",rusqlite::params![old.id,one.root_path]).unwrap();
        c.execute("INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,project_id,created_at) VALUES(1,'default','timur','kaan','[]','test',1,?1,1)",[&one.id]).unwrap();
        c.execute("INSERT INTO gpu_grants(id,request_id,team_id,worker,types,budget,delegation_id,project_id,created_at) VALUES(1,1,'default','kaan','[]',1,?1,?2,1)",rusqlite::params![old.id,one.id]).unwrap();
        let two = project("two");
        crate::projects::set_default(&c, "default", Some(&two.id)).unwrap();
        assert_eq!(delegate::resolve_context(&c, &f.0, &lead, Some(&two.id), Some(old.id)).unwrap_err(), "project_mismatch");
        let inherited = delegate::resolve_context(&c, &f.0, &lead, None, Some(old.id)).unwrap();
        assert_eq!(inherited.project_id, ctx.project_id);
        let task = tasks::create(
            &c,
            tasks::NewTask {
                title: "fix".into(),
                description: "unchanged".into(),
                status: None,
                agent_id: None,
                team_id: Some("default".into()),
            },
        )
        .unwrap();
        let new = || delegate::NewDelegation {
            from: &lead,
            to_agent: "kaan".into(),
            team_id: "default".into(),
            service_team_id: None,
            prompt: "fix".into(),
            task_id: Some(task.id),
            patron_words: None,
            effort: None,
        };
        c.execute_batch(
            "CREATE TRIGGER reject_bind BEFORE UPDATE OF delegation_id ON gpu_grants BEGIN SELECT RAISE(ABORT,'late-failure'); END;",
        )
        .unwrap();
        assert!(create_job(&c, new(), &inherited, None, Some(old.id), None).unwrap_err().contains("late-failure"));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM delegation_fixes", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(tasks::get(&c, task.id).unwrap().project_id, None);
        assert_eq!(tasks::get(&c, task.id).unwrap().status, "todo");
        assert_eq!(c.query_row("SELECT delegation_id FROM gpu_grants WHERE id=1", [], |r| r.get::<_, i64>(0)).unwrap(), old.id);
        c.execute_batch("DROP TRIGGER reject_bind").unwrap();
        let fixed = create_job(&c, new(), &inherited, None, Some(old.id), None).unwrap();
        assert_eq!(fixed.project_id, Some(one.id.clone()));
        assert_eq!(fixed.project_root, one.root_path);
        assert_eq!(crate::review::fix_of(&c, fixed.id).unwrap(), Some(old.id));
        assert_eq!(c.query_row("SELECT delegation_id FROM gpu_grants WHERE id=1", [], |r| r.get::<_, i64>(0)).unwrap(), fixed.id);
        let p2 = delegate::resolve_context(&c, &f.0, &lead, Some(&two.id), None).unwrap();
        assert_eq!(check_grant_project(&c, 1, &p2).unwrap_err(), "project_mismatch");
        c.execute_batch(
            "INSERT INTO teams(id,name,kind,created_at) VALUES('service','Service','service',1);
            INSERT INTO agents(id,team_id,name,kind,model,shirt,hair,skin) VALUES('svc','service','S','worker','sonnet','a','b','c');",
        )
        .unwrap();
        let service = create_job(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "svc".into(),
                team_id: "default".into(),
                service_team_id: Some("service".into()),
                prompt: "research".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
            &ctx,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(service.project_id, Some(one.id));
        let expected_report =
            PathBuf::from(service.project_root.as_ref().unwrap()).join("docs").join("research").join(format!("{}-research.md", service.id));
        assert_eq!(
            service.result_path.as_deref(),
            expected_report.to_str(),
            "the resolver must see the same service filename as the prompt"
        );
        assert_eq!(
            delegate::result_dir(&c, &service).unwrap(),
            Some(PathBuf::from(service.project_root.clone().unwrap()).join("docs/research"))
        );
        let worker = delegate::caller(&c, "svc").unwrap();
        assert_eq!(
            project_read(&c, &f.0, &worker, Some(service.id), "project_context", &json!({})).unwrap()["projectRoot"],
            json!(service.project_root)
        );
        assert_eq!(
            project_read(&c, &f.0, &worker, Some(service.id), "project_context", &json!({})).unwrap()["resultPath"],
            json!(service.result_path)
        );
        assert_eq!(
            project_read(&c, &f.0, &worker, Some(service.id), "project_list", &json!({})).unwrap()["projects"].as_array().unwrap().len(),
            1
        );
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES('svc','session','claude',1,?1,?2)",rusqlite::params![service.project_id,service.id]).unwrap();
        let allowed = delegate::worker_prompt(&c, &service).unwrap();
        assert!(allowed.contains(service.project_root.as_deref().unwrap()));
        assert!(!allowed.contains(two.root_path.as_deref().unwrap()));
    }

    #[test]
    fn gpu_selection_preserves_legacy_singleton_and_closes_explicit_multi_window() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let root = f.dir("legacy");
        crate::teams::update_team(
            &c,
            &f.0,
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(root.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let context = delegate::resolve_context(&c, &f.0, &lead, None, None).unwrap();
        assert!(crate::project_context::validate(&context).unwrap().is_some());
        c.execute_batch("INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,created_at) VALUES(1,'default','timur','kaan','[]','legacy',1,1);
            INSERT INTO gpu_grants(id,request_id,team_id,worker,types,budget,created_at) VALUES(1,1,'default','kaan','[]',1,1);").unwrap();
        check_grant_project(&c, 1, &context).unwrap();
        let root = f.dir("second");
        let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        crate::projects::create(&c, "default", crate::projects::NewProject { name: "Second".into(), worktrees: None }, &ready).unwrap();
        assert_eq!(delegate::resolve_context(&c, &f.0, &lead, None, None).unwrap_err(), "project_required");
        assert_eq!(check_grant_project(&c, 1, &context).unwrap_err(), "project_mismatch");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM gpu_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn mcp_project_error_envelopes_and_validated_delegate_do_not_trust_input_paths() {
        let f = crate::projects::tests::Fixture::new();
        // Core's event watcher can retain the DB until process exit. Keep its data separate
        // from the strict project fixture, under the same workspace scratch directory.
        let core_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/mcp-855").join(f.0.file_name().unwrap());
        let core = Core::start(
            crate::Paths { data_dir: core_root.join("data"), config_dir: core_root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let first = f.dir("first");
        core.team_update(
            "default",
            crate::teams::TeamPatch { cwd: Some(Some(first.to_string_lossy().into_owned())), ..Default::default() },
        )
        .unwrap();
        let second = f.dir("second");
        let ready = crate::projects::prepare_root(second.to_str().unwrap(), None).unwrap();
        let task = {
            let c = lock(&core).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: "Second".into(), worktrees: None }, &ready).unwrap();
            tasks::create(
                &c,
                tasks::NewTask {
                    title: "t".into(),
                    description: "before".into(),
                    status: None,
                    agent_id: None,
                    team_id: Some("default".into()),
                },
            )
            .unwrap()
        };
        for (project, code) in [
            (None, "project_required"),
            (Some(json!(second)), "project_not_found"),
            (Some(json!(42)), "project_not_found"),
            (Some(json!("foreign")), "project_not_found"),
        ] {
            let mut args = json!({"prompt":"x","agentId":"kaan","taskId":task.id});
            if let Some(project) = project {
                args["project"] = project;
            }
            let out = call_with_args_for_test(&core, "timur", "delegate", args);
            assert_eq!(out["isError"], true);
            assert_eq!(out["content"][0]["text"], code);
        }
        let all = tools_list_for_test(&core, "timur");
        assert_eq!(all.iter().find(|t| t["name"] == "gpu_request").unwrap()["inputSchema"]["properties"]["project"]["type"], "string");
        let lead = {
            let c = lock(&core).unwrap();
            delegate::caller(&c, "timur").unwrap()
        };
        let job = delegate_call(&core, &lead, &json!({"prompt":"x","agentId":"kaan","taskId":task.id,"project":"legacy-default"}), false)
            .unwrap();
        assert_eq!(job.project_root, Some(first.to_string_lossy().into_owned()));
        assert_eq!(job.merge_branch.as_deref(), Some("fixture-main"));
        {
            let c = lock(&core).unwrap();
            assert_eq!(tasks::get(&c, task.id).unwrap().project_id, job.project_id);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
        core.shutdown();
        drop(core);
        let _ = std::fs::remove_dir_all(core_root);
    }

    #[test]
    fn report_rejects_unknown_fields() {
        assert!(report_args(&json!({ "delegationId": 1, "summary": "bitti: x", "resultPath": "a.md" })).is_ok());
        assert!(report_args(&json!({ "delegationId": 1, "summary": "bitti: x", "content": "full report" })).is_ok());
        let err = report_args(&json!({ "delegationId": 1, "summary": "x", "body": "uzun rapor" })).unwrap_err();
        assert!(err.contains("body"), "{err}");
        let schema = tools().as_array().unwrap().iter().find(|t| t["name"] == "report").cloned().unwrap();
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
        assert_eq!(schema["inputSchema"]["properties"]["content"]["type"], "string");
        assert_eq!(schema["inputSchema"]["required"], json!(["delegationId", "summary"]));
    }

    /// Görev #20: `report` reminds of the memory once, and says to skip it when there is nothing.
    #[test]
    fn report_reply_reminds_of_memory_without_forcing_it() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let from = delegate::caller(&c, "timur").unwrap();
        let d = delegate::insert(&c, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "x".into(), task_id: None, patron_words: None, effort: None }).unwrap();
        let reply = report_reply(&d);
        assert_eq!((reply["ok"].as_bool(), reply["delegation"].as_i64()), (Some(true), Some(d.id)));
        let note = reply["note"].as_str().unwrap();
        assert!(note.contains("hafızana yaz") && note.contains("yoksa yazma"), "{note}");
        assert!(!note.contains('\n') && note.chars().count() <= 160, "one short line: {note}");
    }
}

fn tools() -> Value {
    let status = json!({ "type": "string", "enum": tasks::STATUSES });
    let mut all = json!([
        { "name": "dex_status", "description": "Takımının canlı çekirdek durumu; salt okunur: her ajan için id, ad, rol, unvan, motor/model ve sayı/durum. Prompt, komut ve sır içermez.",
          "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false } },
        { "name": "task_list", "description": "Takımının görev panosunu listeler.",
          "inputSchema": { "type": "object", "properties": { "status": status } } },
        { "name": "task_create", "description": "Takımının panosuna görev ekler. agentId verilirse takımından biri olmalı.",
          "inputSchema": { "type": "object", "required": ["title"], "properties": {
              "title": { "type": "string" }, "description": { "type": "string" }, "status": status,
              "agentId": { "type": "string" } } } },
        { "name": "task_update", "description": "Takımının panosundaki bir görevin durumunu/atamasını/başlığını değiştirir. agentId: null atamayı kaldırır.",
          "inputSchema": { "type": "object", "required": ["id"], "properties": {
              "id": { "type": "integer" }, "status": status, "agentId": { "type": ["string", "null"] },
              "title": { "type": "string" }, "description": { "type": "string" } } } },
        { "name": "delegate", "description": "YALNIZ LİDER. Takımındaki bir worker'a iş verir (agentId: id ya da ad; ya da role: rol/unvan). Worker kapalıysa başlar, meşgulse işi kuyruğa alınır. Bittiğinde sonraki mesajında bağlamına eklenir.",
          "inputSchema": { "type": "object", "required": ["prompt"], "properties": {
              "agentId": { "type": "string" }, "role": { "type": "string", "description": "Katalogdaki rol slug'ı ya da adı; liste lider kurallarında" }, "prompt": { "type": "string" },
              "taskId": { "type": "integer", "description": "Panodaki ilgili görev; worker'a atanır ve 'doing'e alınır." },
              "patronWords": { "type": "string", "description": "İş patronun bir isteğinden doğuyorsa patronun ilgili sözü, birebir alıntı; worker'a liderin açıklamasından (prompt) önce ve üstün gider" },
              "effort": crate::effort::schema(),
              "model": crate::job_model::fields()["model"],
              "modelReason": crate::job_model::fields()["modelReason"],
              "project": project_schema(),
              "fixOf": { "type": "integer", "description": "Kod incelemesi `düzelt` dediyse düzeltilen devretmenin id'si; yeni iş dalı onun dalının ucunda açılır, bitince yeniden incelenir" },
              "gpuGrant": { "type": "integer", "description": "Patronun onayladığı GPU izni (gpu_request sonucu); aynı worker'a, kullanılmamış. fixOf'ta düzeltilen işin izni kendiliğinden devralınır" } } } },
        { "name": "delegate_to_service", "description": "YALNIZ LİDER. Bir alt takıma (kind=service, ör. Araştırma) iş verir; sonuç projenin docs/research/ klasörüne dosya olarak düşer.",
          "inputSchema": { "type": "object", "required": ["team", "prompt"], "properties": {
              "team": { "type": "string", "description": "Alt takımın adı ya da id'si" }, "prompt": { "type": "string" }, "project": project_schema() } } },
        { "name": "project_list", "description": "Salt okuma: lider kendi takımının projelerini, worker yalnız bu spawn'a bağlı işin projesini görür.",
          "annotations": { "readOnlyHint": true },
          "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
              "limit": { "type": "integer", "minimum": 1, "maximum": 100 }, "offset": { "type": "integer", "minimum": 0 } } } },
        { "name": "project_context", "description": "Salt okuma: liderin proje/takım bağlamı veya worker'ın spawn'a bağlı kendi işinin kayıtlı bağlamı. project/cwd/env yetki kaynağı değildir.",
          "annotations": { "readOnlyHint": true },
          "inputSchema": { "type": "object", "additionalProperties": false, "properties": { "project": project_schema() } } },
        { "name": "memory_search", "description": "Salt okuma: işe başlarken ilgili kök neden, tuzak ve kararları hafızada ara. Yalnız kendi takım/proje ortak ve özel hafızan; Türkçe ve ASCII sorgular eşleşir.",
          "annotations": { "readOnlyHint": true },
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["query"], "properties": {
              "query": { "type": "string", "minLength": 1 },
              "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 8 } } } },
        { "name": "note", "description": "YALNIZ LİDER. Açık devretmeye düzeltme/ek bilgi gönderir. Çalışan işte ilk uygun hook sınırında, kuyruktaki işte başlarken bir kez iletilir. Yeni iş için delegate kullan.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["delegationId", "text"], "properties": {
              "delegationId": { "type": "integer" }, "text": { "type": "string", "minLength": 1 } } } },
        { "name": "delegation_status", "description": "Devretmelerin durumu ve pendingNotes bekleyen not sayısı (id verilmezse gönderdiğin/aldığın son devretmeler).",
          "inputSchema": { "type": "object", "properties": { "id": { "type": "integer" } } } },
        { "name": "report", "description": "Sana devredilen işi teslim et. Devretmenin cevaplanmamış (open/escalated) ask_leader sorusu varsa reddedilir; cevabı bekle ve bitince teslim et. Tam raporu content ile ver (en fazla 1 MiB UTF-8); dosyayı işin kayıtlı proje kökünde Dex yazar, worker dosya yazmaz. content ile resultPath birlikte verilmez. Eski dosya teslimi resultPath ile korunur; projesiz takım yalnız summary verir. summary tek satır (\"bitti: <1 cümle>\"). Sohbet mesajın lidere gitmez.",
          "inputSchema": { "type": "object", "additionalProperties": false, "required": ["delegationId", "summary"], "properties": {
              "delegationId": { "type": "integer" }, "summary": { "type": "string" }, "resultPath": { "type": "string" },
              "content": { "type": "string", "minLength": 1, "maxLength": delegate::REPORT_CONTENT_LIMIT } } } },
        { "name": "reset_ready", "description": "YALNIZ LİDER. Dex oturum sıfırlaması istediğinde devir notunu hafızana yazdıktan sonra çağır; Dex oturumunu kapatıp temiz açar. Dex istemediyse reddedilir.",
          "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} } }
    ]);
    // DEX-22: plan, plan_status, plan_cancel, propose_member (+ DEX-28 plan_retry).
    all.as_array_mut().unwrap().extend(crate::plan::tools());
    // DEX-24: ask_leader, answer, escalate.
    all.as_array_mut().unwrap().extend(crate::questions::tools());
    // Görev #144: delegation_merge.
    all.as_array_mut().unwrap().extend(crate::worktree::tools());
    all
}

pub(crate) fn project_schema() -> Value {
    json!({"type":"string", "minLength":1, "description":"Proje id'si (project_list); klasör yolu kabul edilmez. Birden çok bağlı aktif projede zorunlu; default/UI seçimi fallback değildir."})
}

#[cfg(test)]
fn call(core: &Core, agent: &str, params: &Value) -> Value {
    call_in(core, agent, None, params)
}

fn call_in(core: &Core, agent: &str, job: Option<i64>, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match run(core, agent, job, name, &args) {
        Ok(v) => json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": wrap(v) }),
        Err(msg) => json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
    }
}

#[cfg(test)]
pub(crate) fn call_for_test(core: &Core, agent: &str, tool: &str) -> Value {
    call(core, agent, &json!({ "name": tool, "arguments": {} }))
}

#[cfg(test)]
pub(crate) fn call_with_args_for_test(core: &Core, agent: &str, tool: &str, args: Value) -> Value {
    call(core, agent, &json!({ "name": tool, "arguments": args }))
}

#[cfg(test)]
pub(crate) fn tools_list_for_test(core: &Core, agent: &str) -> Vec<Value> {
    tools_for(core, agent, None).as_array().cloned().unwrap_or_default()
}

/// Görev #289: GPU tools only for whom they are — the leader's request tools, a worker's job tools
/// only while its granted job runs.
fn tools_for(core: &Core, agent: &str, job: Option<i64>) -> Value {
    let mut all = tools();
    if let Ok(conn) = lock(core) {
        if let Ok(c) = delegate::caller(&conn, agent) {
            if !c.is_leader() { all.as_array_mut().unwrap().retain(|t| t["name"] != "note"); }
            all.as_array_mut().unwrap().extend(crate::gpu::tools_for(&conn, &c, job));
        }
    }
    if let Some(tool) = all.as_array_mut().unwrap().iter_mut().find(|t| t["name"] == "gpu_request") {
        tool["inputSchema"]["properties"]["project"] = project_schema();
    }
    all
}

// structuredContent must be an object.
fn wrap(v: Value) -> Value {
    if v.is_object() { v } else { json!({ "result": v }) }
}

fn s<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn lock(core: &Core) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>, String> {
    core.db.0.lock().map_err(|_| "db kilidi zehirli".to_string())
}

fn own_task(conn: &rusqlite::Connection, c: &Caller, id: i64) -> Result<tasks::Task, String> {
    let t = tasks::get(conn, id)?;
    if t.team_id.as_deref() != Some(c.team_id.as_str()) {
        return Err(format!("görev #{id} senin takımında değil"));
    }
    Ok(t)
}

#[cfg(test)]
fn create_job(
    conn: &rusqlite::Connection,
    d: delegate::NewDelegation,
    context: &crate::project_context::ProjectContext,
    ready: Option<&crate::projects::PreparedRoot>,
    fix_of: Option<i64>,
    gpu_grant: Option<i64>,
) -> Result<delegate::Delegation, String> {
    create_targeted_job(conn, d, context, ready, (fix_of, gpu_grant), None, (None, None, None))
}

fn create_targeted_job(
    conn: &rusqlite::Connection,
    d: delegate::NewDelegation,
    context: &crate::project_context::ProjectContext,
    ready: Option<&crate::projects::PreparedRoot>,
    fix_and_grant: (Option<i64>, Option<i64>),
    role: Option<&str>,
    model: (Option<&str>, Option<&str>, Option<&crate::job_model::Snapshot>),
) -> Result<delegate::Delegation, String> {
    let (fix_of, gpu_grant) = fix_and_grant;
    let (model, model_reason, model_snapshot) = model;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let context = delegate::revalidate_context(&tx, d.from, context, ready)?;
    if let Some(snapshot) = model_snapshot { snapshot.revalidate(&tx)?; }
    if let Some(f) = fix_of {
        crate::review::check_fix_of(&tx, &d.team_id, f)?;
    }
    if let Some(g) = gpu_grant {
        crate::gpu::check_grant(&tx, &d.team_id, g, &d.to_agent)?;
        check_grant_project(&tx, g, &context)?;
    } else if let Some(f) = fix_of {
        let mut st = tx.prepare("SELECT id FROM gpu_grants WHERE delegation_id=?1 AND worker=?2").map_err(|e| e.to_string())?;
        let grants = st
            .query_map(rusqlite::params![f, d.to_agent], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for g in grants {
            check_grant_project(&tx, g, &context)?;
        }
    }
    if let Some(id) = d.task_id {
        let task = own_task(&tx, d.from, id)?;
        if task.project_id.is_some() && task.project_id != context.project_id {
            return Err(crate::projects::ProjectError::Mismatch.to_string());
        }
        let description = d
            .patron_words
            .as_deref()
            .filter(|w| !task.description.contains(w))
            .map(|w| format!("{}{}", delegate::patron_section(Some(w)), task.description));
        tx.execute("UPDATE tasks SET project_id=?2 WHERE id=?1", rusqlite::params![id, context.project_id]).map_err(|e| e.to_string())?;
        tasks::update(
            &tx,
            id,
            tasks::TaskPatch { status: Some("doing".into()), agent_id: Some(Some(d.to_agent.clone())), description, ..Default::default() },
        )?;
    }
    let mut d = delegate::insert_bound(&tx, d, &context)?;
    crate::job_model::request(&tx, d.id, model, model_reason, role, model_snapshot.map(|s| &s.route))?;
    d = delegate::get(&tx, d.id)?;
    if d.service_team_id.is_some() {
        // A's resolver reads this saved path, so its launch/permit context and the service
        // prompt agree on the existing id-prompt filename convention before the first hook.
        d.result_path = delegate::result_file(&tx, &d)?.map(|p| p.to_string_lossy().into_owned());
        tx.execute("UPDATE delegations SET result_path=?2 WHERE id=?1", rusqlite::params![d.id, d.result_path])
            .map_err(|e| e.to_string())?;
    }
    if let Some(f) = fix_of {
        crate::review::set_fix_of(&tx, d.id, f)?;
    }
    match (gpu_grant, fix_of) {
        (Some(g), _) => crate::gpu::bind(&tx, g, d.id)?,
        (None, Some(f)) => {
            crate::gpu::inherit(&tx, f, d.id, &d.to_agent)?;
        }
        _ => {}
    }
    if let Some(role) = role {
        delegate::queue_role(&tx, d.id, role, None)?;
        d = delegate::assign_role_job(&tx, d.id)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(d)
}

fn check_grant_project(conn: &rusqlite::Connection, grant: i64, context: &crate::project_context::ProjectContext) -> Result<(), String> {
    let project: Option<String> =
        conn.query_row("SELECT project_id FROM gpu_grants WHERE id=?1", [grant], |r| r.get(0)).map_err(|e| e.to_string())?;
    // #336: old implicit single-project GPU requests still produce NULL until F integrates.
    // Accept that window only for the same legacy singleton, using the sole resolver.
    let legacy = project.is_none()
        && context.project_id.as_deref() == Some(format!("legacy-{}", context.team_id).as_str())
        && context
            .agent_id
            .as_deref()
            .and_then(|id| delegate::caller(conn, id).ok())
            .and_then(|caller| delegate::resolve_context(conn, Path::new(""), &caller, None, None).ok())
            .is_some_and(|selected| selected.project_id == context.project_id && selected.project_root == context.project_root);
    if project != context.project_id && !(project.is_none() && context.project_root.is_none()) && !legacy {
        return Err(crate::projects::ProjectError::Mismatch.to_string());
    }
    Ok(())
}

fn project_arg(args: &Value) -> Result<Option<&str>, String> {
    args.get("project")
        .map(|v| v.as_str().filter(|s| !s.is_empty()).ok_or_else(|| crate::projects::ProjectError::NotFound.to_string()))
        .transpose()
}

fn project_read(
    conn: &rusqlite::Connection,
    data_dir: &Path,
    c: &Caller,
    job: Option<i64>,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    use crate::project_context::{self, ContextMode, ResolveRequest};
    let missing = || crate::projects::ProjectError::NotFound.to_string();
    let c = delegate::caller(conn, &c.id).map_err(|_| missing())?;
    if args.get("teamId").is_some_and(|v| v.as_str() != Some(c.team_id.as_str())) {
        return Err(missing());
    }
    let project = project_arg(args)?;
    let context = if c.is_leader() {
        if name == "project_list" && project.is_none() {
            None
        } else {
            Some(
                project_context::resolve(
                    conn,
                    data_dir,
                    ResolveRequest {
                        team_id: &c.team_id,
                        agent_id: Some(&c.id),
                        delegation_id: None,
                        project_id: project,
                        mode: ContextMode::Team,
                    },
                )
                .map_err(|e| e.to_string())?,
            )
        }
    } else {
        let job = job.ok_or_else(missing)?;
        // Always mask a foreign project/job/team as the same outcome; no existence oracle.
        let ctx = project_context::resolve(
            conn,
            data_dir,
            ResolveRequest {
                team_id: &c.team_id,
                agent_id: Some(&c.id),
                delegation_id: Some(job),
                project_id: project,
                mode: ContextMode::Selection,
            },
        )
        .map_err(|_| missing())?;
        Some(ctx)
    };
    if name == "project_context" {
        return Ok(json!(context.ok_or_else(missing)?));
    }
    let all = if c.is_leader() {
        crate::projects::list(conn, &c.team_id, false).map_err(|e| e.to_string())?
    } else if let Some(ctx) = &context {
        if let (Some(owner), Some(id)) = (&ctx.project_team_id, &ctx.project_id) {
            let mut p = crate::projects::get(conn, owner, id).map_err(|_| missing())?;
            p.root_path = ctx.project_root.as_ref().map(|p| p.to_string_lossy().into_owned());
            p.default_branch = ctx.merge_branch.clone();
            vec![p]
        } else {
            vec![]
        }
    } else {
        vec![]
    };
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(usize::MAX as u64) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100) as usize;
    let total = all.len();
    let next = offset.saturating_add(limit);
    Ok(json!({"projects":all.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"nextOffset":if next<total {Some(next)}else{None}}))
}

fn delegate_call(core: &Core, c: &Caller, args: &Value, service: bool) -> Result<delegate::Delegation, String> {
    let _gate = core.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
    let conn = lock(core)?;
    let c = delegate::caller(&conn, &c.id)?;
    let prompt = s(args, "prompt").map(str::trim).filter(|p| !p.is_empty()).ok_or("prompt gerekli")?;
    let fix_of = if service { None } else { args.get("fixOf").and_then(Value::as_i64) };
    let context = delegate::resolve_context(&conn, &core.paths.data_dir, &c, project_arg(args)?, fix_of)?;
    let service_team = if service {
        if context.project_root.is_none() {
            return Err(crate::projects::ProjectError::Unavailable.to_string());
        }
        let want = s(args, "team").ok_or("team gerekli")?.trim().to_lowercase();
        let (team, kind): (String, String) = conn
            .query_row(
                "SELECT id,kind FROM teams WHERE lower(id)=?1 OR lower(name)=?1 ORDER BY kind='service' DESC LIMIT 1",
                [&want],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| format!("takım yok: {want}"))?;
        if kind != "service" {
            return Err(format!("{want} bir alt takım (service) değil"));
        }
        Some(team)
    } else {
        None
    };
    let (to, role) = select_target(&conn, &c, args, service_team.as_deref(), fix_of)?;
    let effort = crate::effort::Effort::from_arg(args.get("effort"))?;
    let (model, model_reason) = crate::job_model::args(args)?;
    let model_snapshot = crate::job_model::snapshot(&conn, &to, model, model_reason, role.as_deref())?;
    drop(conn);
    drop(_gate);
    if let (Some(snapshot), Some(model)) = (&model_snapshot, model) {
        snapshot.route.validate_catalog(model, Some(core))?;
    }
    let ready = crate::project_context::validate(&context).map_err(|e| e.to_string())?;
    let _gate = core.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
    let conn = lock(core)?;
    create_targeted_job(
        &conn,
        delegate::NewDelegation {
            from: &c,
            to_agent: to,
            team_id: c.team_id.clone(),
            service_team_id: service_team,
            prompt: prompt.into(),
            task_id: if service { None } else { args.get("taskId").and_then(Value::as_i64) },
            patron_words: delegate::patron_words(s(args, "patronWords")),
            effort,
        },
        &context,
        ready.as_ref(),
        (fix_of, args.get("gpuGrant").and_then(Value::as_i64)),
        role.as_deref(),
        (model, model_reason, model_snapshot.as_ref()),
    )
}

fn select_target(
    conn: &rusqlite::Connection,
    c: &Caller,
    args: &Value,
    service: Option<&str>,
    fix_of: Option<i64>,
) -> Result<(String, Option<String>), String> {
    if let Some(team) = service {
        return Ok((pick(conn, team, None)?, None));
    }
    if let Some(fix) = fix_of {
        crate::review::check_fix_of(conn, &c.team_id, fix)?;
        let original = delegate::get(conn, fix)?.to_agent;
        if let Some(id) = s(args, "agentId") {
            if pick(conn, &c.team_id, Some(id))? != original {
                return Err("fixOf işi aynı worker'a verilmelidir".into());
            }
        }
        return Ok((original, None));
    }
    if let Some(id) = s(args, "agentId") {
        return Ok((pick(conn, &c.team_id, Some(id))?, None));
    }
    if let Some(key) = s(args, "role") {
        let (role, worker) = delegate::role_target(conn, &c.team_id, key)?;
        if let Some(grant) = args.get("gpuGrant").and_then(Value::as_i64) {
            let granted: String = conn
                .query_row(
                    "SELECT worker FROM gpu_grants WHERE id=?1 AND team_id=?2",
                    rusqlite::params![grant, c.team_id],
                    |r| r.get(0),
                )
                .map_err(|_| format!("GPU izni yok: #{grant}"))?;
            let actual: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agents WHERE id=?1 AND team_id=?2 AND kind='worker' AND role_slug=?3)",rusqlite::params![granted,c.team_id,role],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !actual {
                return Err("GPU izninin worker'ı istenen rolde değil".into());
            }
            return Ok((granted, None));
        }
        return Ok((worker, Some(role)));
    }
    Ok((pick(conn, &c.team_id, None)?, None))
}

fn check_member(conn: &rusqlite::Connection, c: &Caller, agent: &str) -> Result<(), String> {
    let other = delegate::caller(conn, agent)?;
    if other.team_id != c.team_id {
        return Err(format!("{agent} senin takımında değil"));
    }
    Ok(())
}

fn run(core: &Core, agent: &str, job: Option<i64>, name: &str, args: &Value) -> Result<Value, String> {
    let conn = lock(core)?;
    let c = delegate::caller(&conn, agent)?;
    let lead_only = |what: &str| -> Result<(), String> {
        if c.is_leader() {
            Ok(())
        } else {
            Err(format!("yetki yok: {what} yalnız takım liderine açık ({} bir worker)", c.name))
        }
    };
    match name {
        "project_list" | "project_context" => project_read(&conn, &core.paths.data_dir, &c, job, name, args),
        "memory_search" => {
            let fields = args.as_object().ok_or("arguments nesne olmalı")?;
            if let Some(key) = fields.keys().find(|k| !matches!(k.as_str(), "query" | "limit")) {
                return Err(format!("bilinmeyen alan: {key}"));
            }
            let query = s(args, "query").map(str::trim).filter(|q| !q.is_empty()).ok_or("query gerekli")?;
            let limit = match args.get("limit") {
                None => 8,
                Some(v) => v.as_u64().filter(|v| (1..=100).contains(v)).ok_or("limit 1..100 arasında tam sayı olmalı")? as usize,
            };
            let ctx = if !c.is_leader() && job.is_some() {
                crate::project_context::resolve(&conn, &core.paths.data_dir, crate::project_context::ResolveRequest {
                    team_id: &c.team_id, agent_id: Some(agent), delegation_id: job, project_id: None,
                    mode: crate::project_context::ContextMode::Selection,
                }).map_err(|_| crate::projects::ProjectError::NotFound.to_string())?
            } else {
                crate::hooks::session_context(&conn, &core.paths.data_dir, agent, !c.is_leader()).map_err(|e| e.to_string())?
            };
            let scope = crate::memory::SearchScope::for_context(&conn, &ctx)?;
            drop(conn);
            Ok(json!(crate::memory::search(&scope, query, limit)))
        }
        "dex_status" => {
            drop(conn);
            core.core_status_for(Some(&c.team_id))
        }
        "task_list" => {
            let mut all = tasks::list(&conn, Some(&c.team_id))?;
            if let Some(st) = s(args, "status") {
                all.retain(|t| t.status == st);
            }
            Ok(json!({ "tasks": all }))
        }
        "task_create" => {
            let t: tasks::NewTask = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            if let Some(a) = &t.agent_id {
                check_member(&conn, &c, a)?;
            }
            let t = tasks::create(&conn, tasks::NewTask { team_id: Some(c.team_id.clone()), ..t })?;
            drop(conn);
            let _ = core.events.emit("tasks-changed", &());
            Ok(json!(t))
        }
        "task_update" => {
            let id = args.get("id").and_then(Value::as_i64).ok_or("id gerekli")?;
            own_task(&conn, &c, id)?;
            let p: tasks::TaskPatch = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            if let Some(Some(a)) = &p.agent_id {
                check_member(&conn, &c, a)?;
            }
            let t = tasks::update(&conn, id, p)?;
            drop(conn);
            let _ = core.events.emit("tasks-changed", &());
            Ok(json!(t))
        }
        "delegate" => {
            lead_only("delegate")?;
            drop(conn);
            let d = delegate_call(core, &c, args, false)?;
            if d.task_id.is_some() {
                let _ = core.events.emit("tasks-changed", &());
            }
            summarize(core, core.delegation_dispatch(d)?)
        }
        "delegate_to_service" => {
            lead_only("delegate_to_service")?;
            drop(conn);
            let d = delegate_call(core, &c, args, true)?;
            summarize(core, core.delegation_dispatch(d)?)
        }
        "note" => {
            let id = args.get("delegationId").and_then(Value::as_i64).ok_or("delegationId gerekli")?;
            let reply = crate::worker_notes::send(&conn, &c, id, s(args, "text").ok_or("text gerekli")?)?;
            drop(conn);
            let _ = core.events.emit("delegations-changed", &());
            Ok(reply)
        }
        "delegation_status" => match args.get("id").and_then(Value::as_i64) {
            Some(id) => {
                let d = delegate::get(&conn, id)?;
                let visible = d.from_agent == c.id || d.to_agent == c.id || (c.is_leader() && d.team_id == c.team_id);
                if !visible {
                    return Err(format!("#{id} seni ilgilendirmiyor"));
                }
                Ok(json!(crate::worker_notes::status(&conn, &d)?))
            }
            None => {
                let ds = delegate::visible_to(&conn, &c, 20)?;
                let statuses = ds.iter().map(|d| crate::worker_notes::status(&conn, d)).collect::<Result<Vec<_>,_>>()?;
                Ok(json!({ "delegations": statuses }))
            }
        },
        "report" => {
            report_args(args)?;
            let id = args.get("delegationId").and_then(Value::as_i64).ok_or("delegationId gerekli")?;
            let d = delegate::report_with_content(&conn, &c, id, s(args, "summary").unwrap_or(""), s(args, "resultPath"), s(args, "content"))?;
            drop(conn);
            let _ = core.events.emit("delegations-changed", &Some(&d));
            core.worker_delivered(&d);
            core.review_after_report(&d);
            Ok(report_reply(&d))
        }
        "delegation_merge" => {
            let id = args.get("delegationId").and_then(Value::as_i64).ok_or("delegationId gerekli")?;
            let force = args.get("forcePolicy").and_then(Value::as_bool).unwrap_or(false);
            let force_review = args.get("forceReview").and_then(Value::as_bool).unwrap_or(false);
            drop(conn);
            core.delegation_merge(&c, id, force, force_review)
        }
        // Görev #289: GPU request (leader) and job tools (granted worker); authorization in gpu.rs.
        "gpu_request" => {
            lead_only("gpu_request")?;
            drop(conn);
            core.gpu_request_checked(&c, args)
        }
        n if n.starts_with("gpu_") => {
            drop(conn);
            core.gpu_mcp(&c, n, args, job)
        }
        // Görev #51: the leader's hand-over note is written; Dex replaces its session.
        "reset_ready" => {
            lead_only("reset_ready")?;
            drop(conn);
            core.leader_reset_ready(&c.id)
        }
        _ => {
            drop(conn);
            crate::plan::mcp_call_with_ptys(&core.db, &core.events, &core.ptys, &c, name, args)
                .or_else(|| crate::questions::mcp_call(&core.db, &core.events, &core.permits, &core.answers, &c, name, args))
                .unwrap_or_else(|| Err(format!("bilinmeyen araç: {name}")))
        }
    }
}

impl Core {
    pub(crate) fn plan_create_validated(&self, c: &Caller, mut p: crate::plan::NewPlan) -> Result<i64, String> {
        let _gate = self.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
        let contexts = {
            let conn = lock(self)?;
            crate::job_model::snapshot_plan(&conn, &c.team_id, &mut p)?;
            crate::plan::resolve_projects(&conn, &self.paths.data_dir, c, &p)?
        };
        drop(_gate);
        crate::job_model::validate_plan_catalog(&p, Some(self))?;
        let ready = contexts.iter().map(crate::project_context::validate).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let _gate = self.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
        let conn = lock(self)?;
        crate::plan::create_prepared(&conn, c, p, &contexts, &ready)
    }

    /// The context is resolved/validated in MCP, never reconstructed from an args/env path; the
    /// request row is written with its project_id (#899: legacy single-project too, no NULL window).
    /// The start gate covers resolve + validate, is released for the broker call (network I/O:
    /// neither the gate nor the DB mutex is held, #882), and is retaken for revalidate + insert.
    pub(crate) fn gpu_request_checked(&self, c: &Caller, args: &Value) -> Result<Value, String> {
        let context = {
            let _gate = self.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
            let context = {
                let conn = lock(self)?;
                delegate::resolve_context(&conn, &self.paths.data_dir, c, project_arg(args)?, None)?
            };
            crate::project_context::validate(&context).map_err(|e| e.to_string())?;
            context
        };
        let available = self.gpu_agent_types()?;
        let r = {
            let _gate = self.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
            let conn = lock(self)?;
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            let context = delegate::revalidate_context(&tx, c, &context, None)?;
            let r = crate::gpu::request(&tx, c, args, &available, context.project_id.as_deref())?;
            tx.commit().map_err(|e| e.to_string())?;
            r
        };
        Ok(self.gpu_request_created(&r))
    }
}

/// Anything beside these would be dropped silently — the report body once was (Hata A).
fn report_args(args: &Value) -> Result<(), String> {
    if args.get("content").is_some() && args.get("resultPath").is_some() {
        return Err("content ve resultPath birlikte verilemez; content yolunu Dex belirler".into());
    }
    if args.get("content").is_some_and(|v| !v.is_string()) {
        return Err("content metin olmalı".into());
    }
    match args.as_object().and_then(|o| o.keys().find(|k| !matches!(k.as_str(), "delegationId" | "summary" | "resultPath" | "content"))) {
        Some(k) => Err(format!("report bilinmeyen alan: {k}. Yalnız delegationId, summary, content veya resultPath.")),
        None => Ok(()),
    }
}

fn pick(conn: &rusqlite::Connection, team: &str, key: Option<&str>) -> Result<String, String> {
    delegate::pick_worker(conn, team, key)
}

/// Görev #20: the next job starts in a fresh session; what lasts is the memory. One line, and only
/// for real lessons — no memory clutter.
const REPORT_MEMORY_NOTE: &str =
    "Kalıcı bir şey öğrendiysen (kök neden, karar, tuzak) şimdi hafızana yaz; yoksa yazma. Sonraki işin temiz oturumda başlar.";

fn report_reply(d: &delegate::Delegation) -> Value {
    json!({ "ok": true, "delegation": d.id, "status": d.status, "note": REPORT_MEMORY_NOTE })
}

fn summarize(core: &Core, d: delegate::Delegation) -> Result<Value, String> {
    let name = {
        let conn = lock(core)?;
        delegate::caller(&conn, &d.to_agent).map(|c| c.name).unwrap_or_else(|_| d.to_agent.clone())
    };
    let note = if d.model_approval.as_deref() == Some("pending") {
        "patron model onayı bekleniyor; iş başlamadı".into()
    } else if d.to_agent.is_empty() {
        format!("rol kuyruğunda: {}, sıra {}", d.target_role.as_deref().unwrap_or("?"), d.queue_position.unwrap_or(0))
    } else {
        match d.status.as_str() {
            "running" => "worker başladı",
            "queued" => "worker meşgul; mevcut işi bitince bu iş ona verilecek",
            "failed" => "başlatılamadı",
            _ => "",
        }.into()
    };
    Ok(json!({ "delegationId": d.id, "to": d.to_agent, "toName": name, "targetRole": d.target_role, "queuePosition": d.queue_position, "status": d.status, "note": note, "summary": d.summary,
        "model": d.model, "modelApproval": d.model_approval, "actualModel": d.actual_model, "actualProvider": d.actual_provider, "actualEngine": d.actual_engine }))
}
