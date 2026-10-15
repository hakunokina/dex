// #2055: a worker's progress on its running job, read from the Claude CLI's own task list — no new
// tool or rule for the worker. The CLI keeps that list with `TodoWrite` (whole list each call) or with
// `TaskCreate` / `TaskUpdate` / `TaskList` (CLI 2.1.300 has one or the other); their PostToolUse
// bodies are mirrored here the way the CLI rebuilds its own list. In memory only: a core restart
// closes every running job anyway. Item texts go to the UI, never to a log; they are redacted and cut.
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

pub const EVENT: &str = "delegation-progress";

/// Longest step text, in characters.
const TEXT_MAX: usize = 80;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub agent_id: String,
    pub delegation_id: i64,
    pub total: usize,
    pub done: usize,
    /// The step in progress (its `activeForm`, else its text).
    pub active: Option<String>,
    /// Unix ms of the last list change.
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
struct Item {
    id: Option<String>,
    content: String,
    active_form: Option<String>,
    status: String,
}

struct List {
    delegation_id: i64,
    items: Vec<Item>,
    updated_at: i64,
}

/// Agent → its list on the current job. A new job starts it empty.
#[derive(Default)]
pub struct Tracker(Mutex<HashMap<String, List>>);

/// A task-list tool's PostToolUse from the main thread (a subagent's own list is not the job's).
pub fn wanted(body: &Value) -> bool {
    matches!(body["tool_name"].as_str(), Some("TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList"))
        && !body["agent_id"].is_string()
}

impl Tracker {
    /// Applies one hook body to `agent`'s list on job `delegation_id`; the new state if it changed.
    pub fn apply(&self, agent: &str, delegation_id: i64, body: &Value, now: i64) -> Option<Progress> {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let list = all.entry(agent.to_string()).or_insert_with(|| List { delegation_id, items: Vec::new(), updated_at: now });
        if list.delegation_id != delegation_id {
            *list = List { delegation_id, items: Vec::new(), updated_at: now };
        }
        if !apply_body(&mut list.items, body) {
            return None;
        }
        list.updated_at = now;
        Some(snapshot(agent, list))
    }

    /// Every agent's last list that has items (the UI keeps only running jobs').
    pub fn list(&self) -> Vec<Progress> {
        let all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = all.iter().filter(|(_, l)| !l.items.is_empty()).map(|(a, l)| snapshot(a, l)).collect();
        out.sort_by_key(|p| p.delegation_id);
        out
    }
}

fn snapshot(agent: &str, list: &List) -> Progress {
    let active = list.items.iter().find(|i| i.status == "in_progress").map(|i| {
        let text = i.active_form.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&i.content);
        let text = crate::permit::redact(text);
        match text.char_indices().nth(TEXT_MAX) {
            Some((at, _)) => format!("{}…", &text[..at]),
            None => text,
        }
    });
    Progress {
        agent_id: agent.into(),
        delegation_id: list.delegation_id,
        total: list.items.len(),
        done: list.items.iter().filter(|i| i.status == "completed").count(),
        active,
        updated_at: list.updated_at,
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Task ids arrive as strings ("1"); a number is taken too.
fn id(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string()))
}

fn status(v: &Value) -> String {
    v.as_str().unwrap_or("pending").to_string()
}

fn apply_body(items: &mut Vec<Item>, body: &Value) -> bool {
    let (input, response) = (&body["tool_input"], &body["tool_response"]);
    match body["tool_name"].as_str() {
        // The whole list every call (an all-completed list ends it in the CLI; here it stays 100%).
        Some("TodoWrite") => {
            let Some(todos) = input["todos"].as_array() else { return false };
            *items = todos
                .iter()
                .map(|t| Item { id: None, content: text(&t["content"]).unwrap_or_default(), active_form: text(&t["activeForm"]), status: status(&t["status"]) })
                .collect();
            true
        }
        Some("TaskCreate") => {
            let Some(content) = text(&input["subject"]) else { return false };
            let tid = id(&response["task"]["id"]);
            if tid.is_some() && items.iter().any(|i| i.id == tid) {
                return false;
            }
            items.push(Item { id: tid, content, active_form: text(&input["activeForm"]), status: "pending".into() });
            true
        }
        // Only a task created on this job: an older one belongs to an earlier job of the session.
        Some("TaskUpdate") => {
            let Some(tid) = id(&input["taskId"]) else { return false };
            let Some(at) = items.iter().position(|i| i.id.as_deref() == Some(tid.as_str())) else { return false };
            if input["status"] == "deleted" {
                items.remove(at);
                return true;
            }
            let item = &mut items[at];
            if let Some(s) = input["status"].as_str() { item.status = s.into(); }
            if let Some(s) = text(&input["subject"]) { item.content = s; }
            if let Some(s) = text(&input["activeForm"]) { item.active_form = Some(s); }
            true
        }
        // The CLI's truth for this job's tasks; tasks of earlier jobs in the list are not taken.
        Some("TaskList") => {
            let Some(tasks) = response["tasks"].as_array() else { return false };
            if items.is_empty() {
                return false;
            }
            let seen: HashMap<String, &Value> = tasks.iter().filter_map(|t| Some((id(&t["id"])?, t))).collect();
            items.retain(|i| i.id.as_ref().is_none_or(|k| seen.contains_key(k)));
            for item in items.iter_mut() {
                let Some(t) = item.id.as_ref().and_then(|k| seen.get(k)) else { continue };
                if let Some(s) = t["status"].as_str() { item.status = s.into(); }
                if let Some(s) = text(&t["subject"]) { item.content = s; }
            }
            true
        }
        _ => false,
    }
}

impl crate::Core {
    /// PostToolUse of a task-list tool → the running job's progress (`delegation-progress`). Other
    /// tools cost one string match; no job running → nothing kept.
    pub(crate) fn progress_on_hook(&self, agent: &str, body: &Value) {
        if !wanted(body) {
            return;
        }
        let Ok(Some(job)) = crate::db::with(&self.db, |c| crate::delegate::running_id(c, agent)) else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        if let Some(p) = self.progress.apply(agent, job, body, now) {
            let _ = self.events.emit(EVENT, &p);
        }
    }

    pub fn delegation_progress(&self) -> Vec<Progress> {
        self.progress.list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(tool: &str, input: Value, response: Value) -> Value {
        json!({ "hook_event_name": "PostToolUse", "tool_name": tool, "tool_input": input, "tool_response": response })
    }

    fn create(t: &Tracker, job: i64, id: &str, subject: &str) -> Option<Progress> {
        t.apply("kaan", job, &post("TaskCreate", json!({ "subject": subject, "description": "x" }), json!({ "task": { "id": id, "subject": subject } })), 1)
    }

    fn update(t: &Tracker, job: i64, input: Value) -> Option<Progress> {
        t.apply("kaan", job, &post("TaskUpdate", input, json!({ "success": true })), 2)
    }

    // Bodies as CLI 2.1.300 sends them (claude -p, CLAUDE_CODE_ENABLE_TODO_TOOLS=1).
    #[test]
    fn task_tools_count_done_and_name_the_running_step() {
        let t = Tracker::default();
        for (id, s) in [("1", "alpha"), ("2", "beta"), ("3", "gamma")] {
            create(&t, 7, id, s).unwrap();
        }
        update(&t, 7, json!({ "taskId": "1", "status": "in_progress" })).unwrap();
        update(&t, 7, json!({ "taskId": "1", "status": "completed" })).unwrap();
        let p = update(&t, 7, json!({ "taskId": "2", "status": "in_progress", "activeForm": "Testleri koşuyor" })).unwrap();
        assert_eq!((p.total, p.done, p.active.as_deref(), p.delegation_id), (3, 1, Some("Testleri koşuyor"), 7));
        let p = update(&t, 7, json!({ "taskId": "3", "status": "deleted" })).unwrap();
        assert_eq!((p.total, p.done), (2, 1));
        assert_eq!(update(&t, 7, json!({ "taskId": "9", "status": "completed" })), None, "unknown task: not this job's");
    }

    #[test]
    fn task_list_syncs_known_tasks_and_ignores_older_ones() {
        let t = Tracker::default();
        let list = |tasks: Value| post("TaskList", json!({}), json!({ "tasks": tasks }));
        let old = json!([{ "id": "1", "subject": "old", "status": "completed" }]);
        assert_eq!(t.apply("kaan", 7, &list(old), 1), None, "an earlier job's tasks are not this job's progress");
        create(&t, 7, "2", "beta").unwrap();
        create(&t, 7, "3", "gamma").unwrap();
        let tasks = json!([{ "id": "1", "subject": "old", "status": "completed" }, { "id": "2", "subject": "beta", "status": "completed" }]);
        let p = t.apply("kaan", 7, &list(tasks), 3).unwrap();
        assert_eq!((p.total, p.done, p.updated_at), (1, 1, 3), "3 left the CLI's list; 1 never was this job's");
    }

    #[test]
    fn todo_write_replaces_the_list_and_a_new_job_starts_empty() {
        let t = Tracker::default();
        let todos = json!({ "todos": [
            { "content": "alpha", "status": "completed", "activeForm": "alpha" },
            { "content": "beta", "status": "in_progress", "activeForm": "Beta yapılıyor" },
            { "content": "gamma", "status": "pending", "activeForm": "gamma" },
        ]});
        let p = t.apply("kaan", 7, &post("TodoWrite", todos.clone(), json!({})), 5).unwrap();
        assert_eq!((p.total, p.done, p.active.as_deref()), (3, 1, Some("Beta yapılıyor")));
        assert_eq!(t.list(), vec![p]);
        assert_eq!(update(&t, 8, json!({ "taskId": "1", "status": "completed" })), None);
        assert!(t.list().is_empty(), "job 8 reset job 7's list");
    }

    #[test]
    fn only_the_main_threads_task_tools_count() {
        assert!(wanted(&json!({ "tool_name": "TodoWrite" })));
        assert!(!wanted(&json!({ "tool_name": "Bash" })));
        assert!(!wanted(&json!({ "tool_name": "Task" })), "the subagent tool is not the list");
        assert!(!wanted(&json!({ "tool_name": "TodoWrite", "agent_id": "sub-1" })));
    }

    /// The hook path: a running job takes the list, no job keeps nothing, a new job starts empty.
    #[test]
    fn core_keeps_the_running_jobs_list_from_post_tool_use() {
        use crate::{db::with, delegate, events, Core, Events, Paths};
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("progress-{}-{}", std::process::id(), crate::agents::next_seq()));
        let c = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let todos = |n: usize| post("TodoWrite", json!({ "todos": (0..n).map(|i| json!({ "content": format!("adım {i}"), "status": if i == 0 { "in_progress" } else { "pending" } })).collect::<Vec<_>>() }), json!({}));
        c.delegation_on_hook("kaan", "PostToolUse", &todos(2), false);
        assert!(c.delegation_progress().is_empty(), "no running job: nothing kept");
        let job = || with(&c.db, |db| {
            let from = delegate::caller(db, "timur")?;
            let d = delegate::insert(db, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })?;
            delegate::set_status(db, d.id, "running")?;
            Ok(d.id)
        }).unwrap();
        let a = job();
        c.delegation_on_hook("kaan", "PreToolUse", &todos(3), false);
        assert!(c.delegation_progress().is_empty(), "PreToolUse is not the list (the call may yet fail)");
        c.delegation_on_hook("kaan", "PostToolUse", &todos(3), false);
        let p = c.delegation_progress();
        assert_eq!((p.len(), p[0].delegation_id, p[0].total, p[0].active.as_deref()), (1, a, 3, Some("adım 0")));
        with(&c.db, |db| delegate::set_status(db, a, "done")).unwrap();
        let b = job();
        c.delegation_on_hook("kaan", "PostToolUse", &post("TaskUpdate", json!({ "taskId": "1", "status": "completed" }), json!({})), false);
        assert!(c.delegation_progress().is_empty(), "job {b} starts with no list");
        c.shutdown();
    }

    #[test]
    fn step_text_is_redacted_and_cut() {
        let t = Tracker::default();
        let long = format!("token=abc {}", "ş".repeat(200));
        let todos = json!({ "todos": [{ "content": long, "status": "in_progress" }] });
        let active = t.apply("kaan", 7, &post("TodoWrite", todos, json!({})), 1).unwrap().active.unwrap();
        assert!(active.starts_with("token=[GİZLİ]") && active.ends_with('…'), "{active}");
        assert_eq!(active.chars().count(), TEXT_MAX + 1);
    }
}
