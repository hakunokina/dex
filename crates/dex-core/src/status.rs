//! Metadata-only, read-only core snapshots. MCP callers are restricted to their own team.
use crate::{diagnostics, Core};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex};

/// Kadro satırı: görünen ad, rol ve etkin motor/sağlayıcı/model. Gizli hiçbir şey yok
/// (system_prompt, description, delegation/question içeriği bu sorguya girmez).
/// `active_*` NULL ise ayarlanan (asıl) değer geçerlidir.
pub(crate) struct RosterEntry {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub kind: String,
    pub title: String,
    pub role_slug: Option<String>,
    pub provider: String,
    pub model: String,
    pub engine: String,
    pub chain_off: bool,
    pub chain_temp: bool,
    pub chain_return_pending: bool,
}

pub(crate) fn team_roster(
    conn: &rusqlite::Connection,
    team: Option<&str>,
) -> Result<Vec<RosterEntry>, String> {
    let err = |e: rusqlite::Error| e.to_string();
    let mut st = conn
        .prepare(
            "SELECT id, team_id, name, kind, title, role_slug, \
             COALESCE(active_provider, provider), COALESCE(active_model, model), COALESCE(active_engine, engine), \
             chain_off, active_provider IS NOT NULL OR active_engine IS NOT NULL, chain_return_pending \
             FROM agents WHERE (?1 IS NULL OR team_id = ?1) ORDER BY sort, id",
        )
        .map_err(err)?;
    let rows = st.query_map([team], |r| {
        Ok(RosterEntry {
            id: r.get(0)?,
            team_id: r.get(1)?,
            name: r.get(2)?,
            kind: r.get(3)?,
            title: r.get(4)?,
            role_slug: r.get(5)?,
            provider: r.get(6)?,
            model: r.get(7)?,
            engine: r.get(8)?,
            chain_off: r.get(9)?,
            chain_temp: r.get(10)?,
            chain_return_pending: r.get(11)?,
        })
    });
    let mut out = Vec::new();
    for r in rows.map_err(err)? {
        out.push(r.map_err(err)?);
    }
    Ok(out)
}

fn database_status(conn: &rusqlite::Connection, team: Option<&str>) -> Result<Vec<Value>, String> {
    let err = |e: rusqlite::Error| e.to_string();
    let mut rows: Vec<Value> = team_roster(conn, team)?
        .into_iter()
        .map(|a| {
            json!({
                "agentId": a.id, "teamId": a.team_id, "name": a.name, "kind": a.kind,
                "title": a.title, "roleSlug": a.role_slug,
                "provider": a.provider, "model": a.model, "engine": a.engine,
                "chain": {"disabled": a.chain_off, "temporary": a.chain_temp, "returnPending": a.chain_return_pending},
                "delegations": [], "questions": [],
            })
        })
        .collect();
    let positions: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, a)| (a["agentId"].as_str().unwrap().to_owned(), i))
        .collect();
    let mut work = conn.prepare("SELECT d.id, d.from_agent, d.to_agent, d.status FROM delegations d WHERE d.status IN ('queued','running') AND (?1 IS NULL OR EXISTS (SELECT 1 FROM agents a WHERE a.team_id = ?1 AND a.id IN (d.from_agent, d.to_agent))) ORDER BY d.id").map_err(err)?;
    for row in work
        .query_map([team], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(err)?
    {
        let (id, from, to, status) = row.map_err(err)?;
        for agent in [&from, &to] {
            if let Some(&i) = positions.get(agent) {
                rows[i]["delegations"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id": id, "status": status}));
            }
        }
    }
    let mut questions = conn.prepare("SELECT q.id, q.worker, q.leader, q.status FROM questions q JOIN delegations d ON d.id = q.delegation_id WHERE q.status IN ('open','escalated') AND d.status IN ('queued','running') AND (?1 IS NULL OR EXISTS (SELECT 1 FROM agents a WHERE a.team_id = ?1 AND a.id IN (q.worker, q.leader))) ORDER BY q.id").map_err(err)?;
    for row in questions
        .query_map([team], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(err)?
    {
        let (id, worker, leader, status) = row.map_err(err)?;
        for agent in [&worker, &leader] {
            if let Some(&i) = positions.get(agent) {
                rows[i]["questions"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id": id, "status": status}));
            }
        }
    }
    Ok(rows)
}

#[derive(Clone, Default)]
struct Observation {
    hook_seq: Option<u64>,
    hook_at: Option<u64>,
    processed_seq: Option<u64>,
    pid: Option<u32>,
    generation: u64,
}
#[derive(Default)]
pub(crate) struct Observations(Mutex<HashMap<String, Observation>>);

impl Observations {
    pub(crate) fn hook(
        &self,
        agent: &str,
        seq: u64,
        hook: &str,
        processed: bool,
        state: crate::agents::Status,
    ) {
        if let Ok(mut map) = self.0.lock() {
            let row = map.entry(agent.into()).or_default();
            if processed {
                row.processed_seq = Some(seq);
            } else if row.hook_seq.is_none_or(|old| seq > old) {
                row.hook_seq = Some(seq);
                row.hook_at = Some(diagnostics::now_ms());
            }
        }
        diagnostics::event(
            if processed {
                "hook_processed"
            } else {
                "hook_received"
            },
            json!({"agent": agent, "seq": seq, "hook": hook, "status": state}),
        );
    }
    pub(crate) fn spawn(&self, agent: &str, pid: Option<u32>, generation: u64) {
        if let Ok(mut map) = self.0.lock() {
            let row = map.entry(agent.into()).or_default();
            row.pid = pid;
            row.generation = generation;
        }
        diagnostics::event(
            "pty_spawn",
            json!({"agent": agent, "pid": pid, "generation": generation}),
        );
    }
}

impl Core {
    pub fn core_status(&self) -> Result<Value, String> {
        self.core_status_for(None)
    }

    pub(crate) fn core_status_for(&self, team: Option<&str>) -> Result<Value, String> {
        // Do not hold DB and runtime locks together: PTY exit and hook handlers take both.
        let (mut rows, maintenance) = crate::db::with(&self.db, |conn| Ok((database_status(conn, team)?, crate::maintenance::last_summary(conn)?)))?;
        let observations = self
            .observations
            .0
            .lock()
            .map_err(|_| "diagnostic lock poisoned")?
            .clone();
        let ptys: HashMap<_, _> = self
            .ptys
            .list()
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        let timers = self
            .idle_close
            .lock()
            .map_err(|_| "idle timer lock poisoned")?
            .clone();
        let chains = self
            .chain_guard
            .lock()
            .map_err(|_| "chain lock poisoned")?
            .clone();
        for row in &mut rows {
            let id = row["agentId"].as_str().unwrap().to_owned();
            let state = self.agent_states.get(&id);
            let observed = observations.get(&id).cloned().unwrap_or_default();
            let pty = ptys.get(&id);
            row["status"] = json!(state.status);
            row["lastHookSeq"] = json!(observed.hook_seq);
            row["lastHookAtMs"] = json!(observed.hook_at);
            row["lastProcessedHookSeq"] = json!(observed.processed_seq);
            row["pty"] = json!({"alive": pty.is_some_and(|p| p.alive), "pid": pty.and(observed.pid), "generation": pty.map(|_| observed.generation), "startedAtMs": pty.map(|p| p.started_at)});
            row["idleClose"] = json!({"armed": pty.is_some() && timers.contains_key(&id), "generation": timers.get(&id), "quietPeriodMs": crate::agents::IDLE_CLOSE_MS, "silentForMs": self.agent_states.silent_for(&id)});
            row["chain"]["transitioning"] = json!(chains.contains(&id));
        }
        Ok(json!({"version": 1, "capturedAtMs": diagnostics::now_ms(), "agents": rows, "maintenance": maintenance}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_status_exposes_last_daily_maintenance_as_counts() {
        let root=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("maintenance-status-{}-{}",std::process::id(),crate::agents::next_seq()));
        let core=Core::start(crate::Paths {data_dir:root.join("data"),config_dir:root.join("config")},crate::Events::new(crate::events::NullSink)).unwrap();
        let status=core.core_status_for(Some("default")).unwrap();
        assert!(status.as_object().unwrap().contains_key("maintenance"));
        assert!(status["maintenance"].is_null());
        let summary=json!({"day":"2026-10-10","startedAtMs":1,"finished":true,"deleted":2,"bytes":50,"skipped":{"active_job":3}});
        crate::db::with(&core.db, |c| c.execute("INSERT INTO settings(key,value) VALUES('maintenance_summary',?1)",[summary.to_string()]).map_err(|e| e.to_string())).unwrap();
        let result=crate::mcp::call_for_test(&core,"kaan","dex_status");
        assert_eq!(result["structuredContent"]["maintenance"],summary);
        core.shutdown();
    }
    #[test]
    fn late_hook_completion_keeps_arrival_and_completion_sequences_distinct() {
        let observations = Observations::default();
        observations.hook(
            "kaan",
            10,
            "PostToolUse",
            false,
            crate::agents::Status::Working,
        );
        observations.hook("kaan", 11, "Stop", false, crate::agents::Status::Working);
        observations.hook("kaan", 11, "Stop", true, crate::agents::Status::Idle);
        observations.hook("kaan", 10, "PostToolUse", true, crate::agents::Status::Idle);
        let map = observations.0.lock().unwrap();
        assert_eq!(map["kaan"].hook_seq, Some(11));
        assert_eq!(map["kaan"].processed_seq, Some(10));
    }
    #[test]
    fn worker_status_tool_exposes_live_metadata_without_deadlock_or_content() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "status-{}-{}",
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
        core.agent_states.hook_seq(
            "kaan",
            Some(crate::agents::Status::Working),
            Some("secret-session"),
            42,
        );
        core.observations
            .hook("kaan", 42, "PreToolUse", false, crate::agents::Status::Idle);
        core.observations.hook(
            "kaan",
            42,
            "PreToolUse",
            true,
            crate::agents::Status::Working,
        );
        let reply = crate::mcp::call_for_test(&core, "kaan", "dex_status");
        assert_ne!(reply["isError"], true, "{reply}");
        let rows = reply["structuredContent"]["agents"].as_array().unwrap();
        let worker = rows.iter().find(|a| a["agentId"] == "kaan").unwrap();
        assert_eq!(worker["status"], "working");
        assert_eq!(worker["lastHookSeq"], 42);
        assert_eq!(worker["lastProcessedHookSeq"], 42);
        assert_eq!(worker["pty"]["alive"], false);
        assert_eq!(worker["pty"]["pid"], Value::Null);
        assert!(!reply.to_string().contains("secret-session"));
        assert_eq!(
            crate::mcp::call_for_test(&core, "missing-agent", "dex_status")["isError"],
            true
        );
        core.shutdown();
    }
    #[test]
    fn snapshot_scopes_team_and_excludes_task_and_question_contents() {
        let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let caller = crate::delegate::caller(&conn, "timur").unwrap();
        let d = crate::delegate::insert(
            &conn,
            crate::delegate::NewDelegation {
                from: &caller,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "TOKEN=private-secret".into(),
                task_id: None, patron_words: None, effort: None,
            },
        )
        .unwrap();
        conn.execute("INSERT INTO questions (delegation_id, worker, leader, team_id, question, options, created_at, updated_at) VALUES (?1, 'kaan', 'timur', 'default', 'private-question', '[]', 1, 1)", [d.id]).unwrap();
        let rows = database_status(&conn, Some("default")).unwrap();
        let worker = rows.iter().find(|a| a["agentId"] == "kaan").unwrap();
        assert_eq!(worker["delegations"][0]["id"], d.id);
        assert_eq!(worker["delegations"][0]["status"], "queued");
        assert_eq!(worker["questions"][0]["status"], "open");
        let leader = rows.iter().find(|a| a["agentId"] == "timur").unwrap();
        assert_eq!(leader["delegations"][0]["id"], d.id);
        assert!(!json!(rows).to_string().contains("private"));
        assert!(database_status(&conn, Some("other-team"))
            .unwrap()
            .is_empty());
    }
    #[test]
    fn status_lists_roster_fields_effective_and_leaks_no_secrets() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "status-roster-{}-{}",
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
        let reply = crate::mcp::call_for_test(&core, "timur", "dex_status");
        let rows = reply["structuredContent"]["agents"].as_array().unwrap();
        let timur = rows.iter().find(|a| a["agentId"] == "timur").unwrap();
        assert_eq!(timur["name"], "Timur");
        assert_eq!(timur["kind"], "leader");
        assert_eq!(timur["roleSlug"], "lead");
        assert_eq!(timur["engine"], "claude");
        assert_eq!(timur["provider"], "anthropic");
        assert_eq!(timur["model"], "opus");
        assert!(!timur["title"].as_str().unwrap().is_empty());
        // Geçici zincir değeri (active_*) ayarlananı ezer.
        crate::db::with(&core.db, |conn| {
            conn.execute(
                "UPDATE agents SET active_engine = 'codex', active_model = 'gpt-5' WHERE id = 'kaan'",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .unwrap();
        let reply = crate::mcp::call_for_test(&core, "timur", "dex_status");
        let rows = reply["structuredContent"]["agents"].as_array().unwrap();
        let kaan = rows.iter().find(|a| a["agentId"] == "kaan").unwrap();
        assert_eq!(kaan["engine"], "codex");
        assert_eq!(kaan["model"], "gpt-5");
        assert_eq!(kaan["name"], "Kaan");
        // Sır sızıntısı yok: rol system_prompt'u ve delegation/question içeriği çıkmaz.
        let secret: String = crate::db::with(&core.db, |conn| {
            conn.query_row("SELECT system_prompt FROM roles WHERE slug = 'lead'", [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())
        })
        .unwrap();
        let text = reply.to_string();
        assert!(!text.contains(&secret), "system_prompt sızdı");
        assert!(!text.contains("systemPrompt"), "{text}");
        core.shutdown();
    }
}
