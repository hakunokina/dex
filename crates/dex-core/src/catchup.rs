//! "Sen yokken" özeti (Plan #124): patron geri döndüğünde üç kova — seni bekleyenler (durum
//! tabanlı, imleçsiz), sen yokken değişenler (imleçten beri) ve yeni hafıza kayıtları.
//! Deterministik DB + dosya sorgusu; LLM yok. İmleç yalnız `catchup_ack` ile ilerler (UI panel
//! görünür + pencere odaktayken çağırır); sorgu `since` alır, böylece panel açıkken kalemler
//! kaybolmaz (snapshot UI'da kalır, ack yalnız DB imlecini ilerletir). Kalemler yalnız
//! başlık/durum taşır — rapor/prompt içeriği yok.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use crate::db::with;
use crate::Core;

// 45 (Plan #124): additive, idempotent (IF NOT EXISTS in a versioned migration; reopen is a
// no-op). Before commit: ROLLBACK. After commit: keep the table/indexes; revert only the
// application. Full undo: DROP TABLE catchup_cursor; DROP INDEX delegations_team_status_updated;
// DROP INDEX questions_team_status_created; DROP INDEX proposals_team_status_created;
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS catchup_cursor (
    team_id TEXT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
    seen_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS delegations_team_status_updated ON delegations(team_id, status, updated_at);
CREATE INDEX IF NOT EXISTS questions_team_status_created ON questions(team_id, status, created_at);
CREATE INDEX IF NOT EXISTS proposals_team_status_created ON member_proposals(team_id, status, created_at);";

pub const DAY_MS: i64 = 24 * 3600 * 1000;
pub const MAX_WINDOW_MS: i64 = 7 * DAY_MS;
pub const BUCKET_LIMIT: usize = 50;

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatchupItem {
    /// question_escalated | question_open | member_proposal | model_approval |
    /// job_change_approval | gpu_approval | merge_waiting | review_fix |
    /// delegation_done | delegation_done_no_report | delegation_failed | delegation_merged |
    /// plan_done | plan_cancelled | memory
    pub kind: String,
    /// Stable within the team: `"{kind}:{ref}"`.
    pub id: String,
    pub title: String,
    /// Epoch ms of the event / waiting since.
    pub at: i64,
    /// Source id the UI opens: question/proposal/delegation/plan/gpu-request id (number)
    /// or a memory graph id (string, e.g. `shared/<name>`).
    #[serde(rename = "ref")]
    pub r#ref: Value,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatchupBucket {
    pub items: Vec<CatchupItem>,
    /// Items beyond the first [`BUCKET_LIMIT`].
    pub more: i64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Catchup {
    pub team_id: String,
    /// The effective window start used for `changed`/`memories` (after defaults + 7-day clamp).
    /// The UI keeps this and passes it back as `since` while the panel is open (snapshot).
    pub since: i64,
    /// The stored cursor, if any (what the next open without `since` starts from).
    pub cursor: Option<i64>,
    pub generated_at: i64,
    pub waiting: CatchupBucket,
    pub changed: CatchupBucket,
    pub memories: CatchupBucket,
}

/// One memory node as the bucket needs it (Core feeds these from `memory_graph`).
#[derive(Debug, Clone)]
pub struct MemInput {
    pub id: String,
    pub name: String,
    pub description: String,
    pub updated_at: i64,
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

fn now_ms() -> i64 {
    crate::diagnostics::now_ms() as i64
}

/// First non-empty line, whitespace-collapsed, capped — a title, never a body.
fn title_of(text: &str, fallback: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let flat: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return fallback.to_string();
    }
    let mut out: String = flat.chars().take(120).collect();
    if flat.chars().count() > 120 {
        out.push('…');
    }
    out
}

fn item(kind: &str, r#ref: Value, title: String, at: i64) -> CatchupItem {
    let id = match &r#ref {
        Value::String(s) => format!("{kind}:{s}"),
        other => format!("{kind}:{other}"),
    };
    CatchupItem { kind: kind.into(), id, title, at, r#ref }
}

fn bucket(mut items: Vec<CatchupItem>) -> CatchupBucket {
    items.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    let more = (items.len() as i64 - BUCKET_LIMIT as i64).max(0);
    items.truncate(BUCKET_LIMIT);
    CatchupBucket { items, more }
}

fn team_exists(conn: &Connection, team: &str) -> Result<(), String> {
    let ok: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?1)", [team], |r| r.get(0))
        .map_err(e)?;
    if ok { Ok(()) } else { Err(format!("takım yok: {team}")) }
}

pub fn cursor(conn: &Connection, team: &str) -> Result<Option<i64>, String> {
    conn.query_row("SELECT seen_at FROM catchup_cursor WHERE team_id = ?1", [team], |r| r.get(0))
        .optional()
        .map_err(e)
}

/// Effective window start: explicit `since`, else the cursor, else the last 24 h; always
/// clamped into `[now - 7d, now]`.
pub fn effective_since(since: Option<i64>, cursor: Option<i64>, now: i64) -> i64 {
    let raw = since.or(cursor).unwrap_or(now - DAY_MS);
    raw.clamp(now - MAX_WINDOW_MS, now)
}

/// Moves the cursor forward only (a future `at` clamps to `now`); returns the stored value.
pub fn ack(conn: &Connection, team: &str, at: i64, now: i64) -> Result<i64, String> {
    team_exists(conn, team)?;
    let at = at.min(now);
    conn.execute(
        "INSERT INTO catchup_cursor(team_id, seen_at) VALUES(?1, ?2)
         ON CONFLICT(team_id) DO UPDATE SET seen_at = excluded.seen_at
         WHERE excluded.seen_at > catchup_cursor.seen_at",
        params![team, at],
    )
    .map_err(e)?;
    cursor(conn, team)?.ok_or_else(|| "imleç yazılamadı".to_string())
}

fn collect(conn: &Connection, sql: &str, p: impl rusqlite::Params, mut f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<CatchupItem>) -> Result<Vec<CatchupItem>, String> {
    let mut st = conn.prepare(sql).map_err(e)?;
    let rows = st.query_map(p, |r| f(r)).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

fn waiting(conn: &Connection, team: &str) -> Result<Vec<CatchupItem>, String> {
    let mut out = Vec::new();
    out.extend(collect(
        conn,
        "SELECT id, status, question, created_at FROM questions
         WHERE team_id = ?1 AND status IN ('open','escalated')",
        [team],
        |r| {
            let (id, status, q, at): (i64, String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            let kind = if status == "escalated" { "question_escalated" } else { "question_open" };
            Ok(item(kind, id.into(), title_of(&q, &format!("Soru #{id}")), at))
        },
    )?);
    out.extend(collect(
        conn,
        "SELECT p.id, IFNULL(r.name, p.role_slug), p.model, p.created_at,
                (SELECT action FROM job_changes WHERE proposal_id = p.id),
                (SELECT delegation_id FROM job_changes WHERE proposal_id = p.id),
                (SELECT delegation_id FROM job_models WHERE proposal_id = p.id)
         FROM member_proposals p LEFT JOIN roles r ON r.slug = p.role_slug
         WHERE p.team_id = ?1 AND p.status = 'pending'",
        [team],
        |r| {
            let (id, role, model, at): (i64, String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            let change: Option<String> = r.get(4)?;
            let change_job: Option<i64> = r.get(5)?;
            let model_job: Option<i64> = r.get(6)?;
            let (kind, title) = if let Some(action) = change {
                ("job_change_approval", format!("İş #{} · {action}", change_job.unwrap_or(0)))
            } else if let Some(job) = model_job {
                ("model_approval", format!("İş #{job} · {model}"))
            } else {
                ("member_proposal", format!("{role} · {model}"))
            };
            Ok(item(kind, id.into(), title, at))
        },
    )?);
    // A model approval with no pending proposal row of its own (defensive; normally proposed).
    out.extend(collect(
        conn,
        "SELECT d.id, IFNULL(d.model, ''), d.created_at FROM delegations d
         WHERE d.team_id = ?1 AND d.status = 'queued' AND d.model_approval = 'pending'
           AND NOT EXISTS (SELECT 1 FROM job_models j JOIN member_proposals p ON p.id = j.proposal_id
                           WHERE j.delegation_id = d.id AND p.status = 'pending')",
        [team],
        |r| {
            let (id, model, at): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok(item("model_approval", id.into(), format!("İş #{id} · {model}"), at))
        },
    )?);
    out.extend(collect(
        conn,
        "SELECT g.id, IFNULL(a.name, g.worker), g.created_at FROM gpu_requests g
         LEFT JOIN agents a ON a.id = g.worker
         WHERE g.team_id = ?1 AND g.status = 'pending'",
        [team],
        |r| {
            let (id, worker, at): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok(item("gpu_approval", id.into(), format!("GPU · {worker}"), at))
        },
    )?);
    out.extend(collect(
        conn,
        "SELECT d.id, IFNULL(t.title, ''), d.updated_at,
                (SELECT verdict FROM delegation_reviews WHERE target_id = d.id ORDER BY review_id DESC LIMIT 1)
         FROM delegations d JOIN delegation_worktrees w ON w.delegation_id = d.id
         LEFT JOIN tasks t ON t.id = d.task_id
         WHERE d.team_id = ?1 AND d.status = 'done' AND w.merged_at IS NULL AND w.removed_at IS NULL",
        [team],
        |r| {
            let (id, task, at): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let verdict: Option<String> = r.get(3)?;
            let kind = if verdict.as_deref() == Some("düzelt") { "review_fix" } else { "merge_waiting" };
            Ok(item(kind, id.into(), title_of(&task, &format!("İş #{id}")), at))
        },
    )?);
    Ok(out)
}

fn changed(conn: &Connection, team: &str, since: i64) -> Result<Vec<CatchupItem>, String> {
    let merged = collect(
        conn,
        "SELECT d.id, IFNULL(t.title, ''), w.merged_at FROM delegations d
         JOIN delegation_worktrees w ON w.delegation_id = d.id
         LEFT JOIN tasks t ON t.id = d.task_id
         WHERE d.team_id = ?1 AND w.merged_at > ?2",
        params![team, since],
        |r| {
            let (id, task, at): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok(item("delegation_merged", id.into(), title_of(&task, &format!("İş #{id}")), at))
        },
    )?;
    let merged_ids: std::collections::HashSet<i64> = merged.iter().filter_map(|i| i.r#ref.as_i64()).collect();
    let mut out = collect(
        conn,
        "SELECT d.id, d.status, IFNULL(t.title, ''), d.updated_at FROM delegations d
         LEFT JOIN tasks t ON t.id = d.task_id
         WHERE d.team_id = ?1 AND d.updated_at > ?2 AND d.status IN ('done','done_no_report','failed')",
        params![team, since],
        |r| {
            let (id, status, task, at): (i64, String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            let kind = format!("delegation_{status}");
            Ok(item(&kind, id.into(), title_of(&task, &format!("İş #{id}")), at))
        },
    )?;
    // A merged job's earlier "done" in the same window is the same story; the merge wins.
    out.retain(|i| i.kind != "delegation_done" || !i.r#ref.as_i64().is_some_and(|id| merged_ids.contains(&id)));
    out.extend(merged);
    out.extend(collect(
        conn,
        "SELECT id, status, title, updated_at FROM plans
         WHERE team_id = ?1 AND updated_at > ?2 AND status IN ('done','cancelled')",
        params![team, since],
        |r| {
            let (id, status, title, at): (i64, String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            Ok(item(&format!("plan_{status}"), id.into(), title_of(&title, &format!("Plan #{id}")), at))
        },
    )?);
    Ok(out)
}

fn memories(nodes: Vec<MemInput>, since: i64) -> Vec<CatchupItem> {
    nodes
        .into_iter()
        .filter(|n| n.updated_at > since)
        .map(|n| {
            let title = if n.description.trim().is_empty() {
                title_of(&n.name, &n.id)
            } else {
                title_of(&format!("{} — {}", n.name, n.description.trim()), &n.id)
            };
            item("memory", Value::String(n.id.clone()), title, n.updated_at)
        })
        .collect()
}

/// Pure over a connection (+ memory nodes fed by the caller) so it is testable without a Core.
pub fn catchup(conn: &Connection, team: &str, since: Option<i64>, now: i64, nodes: Vec<MemInput>) -> Result<Catchup, String> {
    team_exists(conn, team)?;
    let stored = cursor(conn, team)?;
    let since = effective_since(since, stored, now);
    Ok(Catchup {
        team_id: team.into(),
        since,
        cursor: stored,
        generated_at: now,
        waiting: bucket(waiting(conn, team)?),
        changed: bucket(changed(conn, team, since)?),
        memories: bucket(memories(nodes, since)),
    })
}

impl Core {
    pub fn catchup(&self, team: &str, since: Option<i64>) -> Result<Catchup, String> {
        // Memory reads the DB itself (`map_agent_ids`); fetch before taking the lock again.
        let nodes = self
            .memory_graph(team)?
            .nodes
            .into_iter()
            .map(|n| MemInput { id: n.id, name: n.name, description: n.description, updated_at: n.updated_at as i64 })
            .collect();
        with(&self.db, |c| catchup(c, team, since, now_ms(), nodes))
    }

    /// UI ack (panel visible + window focused: auto after 10 s, or "Okudum"). Advances the
    /// stored cursor only; returns what is stored now.
    pub fn catchup_ack(&self, team: &str, at: i64) -> Result<i64, String> {
        with(&self.db, |c| ack(c, team, at, now_ms()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const NOW: i64 = 1_800_000_000_000;

    fn conn() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    fn job(c: &Connection, id: i64, status: &str, updated: i64) {
        c.execute(
            "INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at)
             VALUES(?1,'timur','kaan','default','gizli prompt gövdesi',?2,?3,?3)",
            params![id, status, updated],
        )
        .unwrap();
    }

    fn kinds(b: &CatchupBucket) -> Vec<(&str, i64)> {
        b.items.iter().map(|i| (i.kind.as_str(), i.r#ref.as_i64().unwrap_or(-1))).collect()
    }

    #[test]
    fn empty_team_and_unknown_team() {
        let c = conn();
        c.execute("INSERT INTO teams(id,name,kind,created_at) VALUES('empty','Boş','main',1)", []).unwrap();
        let got = catchup(&c, "empty", None, NOW, vec![]).unwrap();
        assert_eq!(got.since, NOW - DAY_MS, "no cursor: last 24 h");
        assert_eq!(got.cursor, None);
        assert!(got.waiting.items.is_empty() && got.changed.items.is_empty() && got.memories.items.is_empty());
        assert_eq!((got.waiting.more, got.changed.more, got.memories.more), (0, 0, 0));
        assert!(catchup(&c, "yok", None, NOW, vec![]).unwrap_err().contains("takım yok"));
        assert!(ack(&c, "yok", NOW, NOW).is_err());
    }

    #[test]
    fn waiting_bucket_is_state_based_however_old() {
        let c = conn();
        let old = NOW - 30 * DAY_MS; // far outside any window: still waiting
        job(&c, 1, "running", old);
        job(&c, 2, "done", old);
        job(&c, 3, "done", old);
        c.execute_batch(&format!(
            "INSERT INTO tasks(id,title,team_id,created_at,updated_at) VALUES(9,'Kart başlığı','default',{old},{old});
             UPDATE delegations SET task_id=9 WHERE id=2;
             INSERT INTO questions(delegation_id,worker,leader,team_id,question,status,created_at,updated_at)
               VALUES(1,'kaan','timur','default','Karar verir misin?\nuzun gövde','escalated',{old},{old}),
                     (1,'kaan','timur','default','Açık soru','open',{old},{old}),
                     (1,'kaan','timur','default','Bitti','answered',{old},{old});
             INSERT INTO member_proposals(id,team_id,leader,role_slug,model,provider,reason,status,created_at)
               VALUES(5,'default','timur','backend','opus','anthropic','gerekçe','pending',{old}),
                     (6,'default','timur','backend','sonnet','anthropic','model','pending',{old}),
                     (7,'default','timur','backend','sonnet','anthropic','iptal','pending',{old});
             INSERT INTO job_models(delegation_id,proposal_id,engine,provider) VALUES(1,6,'claude','anthropic');
             INSERT INTO job_changes(proposal_id,action,delegation_id,reason) VALUES(7,'cancel',1,'r');
             INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,status,created_at)
               VALUES(4,'default','timur','kaan','[\"img\"]','r',3,'pending',{old});
             INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at)
               VALUES(2,'kaan','/r','/r/w2','b2','main',{old}),(3,'kaan','/r','/r/w3','b3','main',{old});
             INSERT INTO delegation_reviews(review_id,target_id,sha,verdict,created_at) VALUES(1,3,'abc','düzelt',{old});"
        ))
        .unwrap();
        let got = catchup(&c, "default", Some(NOW), NOW, vec![]).unwrap();
        let mut got_kinds = kinds(&got.waiting);
        got_kinds.sort();
        assert_eq!(
            got_kinds,
            [("gpu_approval", 4), ("job_change_approval", 7), ("member_proposal", 5), ("merge_waiting", 2),
             ("model_approval", 6), ("question_escalated", 1), ("question_open", 2), ("review_fix", 3)]
        );
        let by_ref = |r: i64| got.waiting.items.iter().find(|i| i.r#ref.as_i64() == Some(r)).unwrap();
        assert_eq!(by_ref(1).title, "Karar verir misin?", "first line only");
        assert_eq!(by_ref(2).title, "Kart başlığı", "task title, never the prompt");
        assert_eq!(by_ref(3).title, "İş #3");
        assert!(got.waiting.items.iter().all(|i| !i.title.contains("gizli") && i.at == old));
        assert!(got.changed.items.is_empty(), "nothing changed after `since = now`");
    }

    #[test]
    fn changed_bucket_since_cursor_and_merged_supersedes_done() {
        let c = conn();
        let since = NOW - DAY_MS;
        job(&c, 1, "done", since + 10);
        job(&c, 2, "done_no_report", since + 20);
        job(&c, 3, "failed", since + 30);
        job(&c, 4, "done", since - 10); // before the window
        job(&c, 5, "running", since + 40); // not finished
        c.execute_batch(&format!(
            "INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at,merged_at)
               VALUES(1,'kaan','/r','/r/w','b','main',{since},{});\
             INSERT INTO plans(id,team_id,leader,title,status,created_at,updated_at)
               VALUES(8,'default','timur','Büyük plan','done',{since},{}),\
                     (9,'default','timur','','active',{since},{});",
            since + 50, since + 60, since + 70
        ))
        .unwrap();
        // The cursor sets the window when no `since` is given.
        ack(&c, "default", since, NOW).unwrap();
        let got = catchup(&c, "default", None, NOW, vec![]).unwrap();
        assert_eq!((got.since, got.cursor), (since, Some(since)));
        assert_eq!(
            kinds(&got.changed),
            [("plan_done", 8), ("delegation_merged", 1), ("delegation_failed", 3), ("delegation_done_no_report", 2)],
            "newest first; merged replaces the same job's done"
        );
    }

    #[test]
    fn ack_only_moves_forward_and_clamps_future() {
        let c = conn();
        assert_eq!(ack(&c, "default", NOW - 1000, NOW).unwrap(), NOW - 1000);
        assert_eq!(ack(&c, "default", NOW - 5000, NOW).unwrap(), NOW - 1000, "never back");
        assert_eq!(ack(&c, "default", NOW - 1000, NOW).unwrap(), NOW - 1000, "idempotent");
        assert_eq!(ack(&c, "default", NOW + 999_999, NOW).unwrap(), NOW, "future clamps to now");
        assert_eq!(cursor(&c, "default").unwrap(), Some(NOW));
        // A snapshot `since` is unaffected by the ack: the panel keeps its items.
        job(&c, 1, "done", NOW - 2000);
        let got = catchup(&c, "default", Some(NOW - 3000), NOW, vec![]).unwrap();
        assert_eq!(got.since, NOW - 3000);
        assert_eq!(kinds(&got.changed), [("delegation_done", 1)]);
    }

    #[test]
    fn seven_day_cap_default_day_and_future_clamp() {
        assert_eq!(effective_since(Some(NOW - 30 * DAY_MS), None, NOW), NOW - MAX_WINDOW_MS);
        assert_eq!(effective_since(None, Some(NOW - 30 * DAY_MS), NOW), NOW - MAX_WINDOW_MS);
        assert_eq!(effective_since(None, None, NOW), NOW - DAY_MS);
        assert_eq!(effective_since(Some(NOW + DAY_MS), None, NOW), NOW);
        assert_eq!(effective_since(Some(NOW - 1000), Some(NOW - 5), NOW), NOW - 1000, "explicit since wins");
    }

    #[test]
    fn memory_bucket_uses_name_and_one_line_description() {
        let c = conn();
        let since = NOW - DAY_MS;
        let nodes = vec![
            MemInput { id: "shared/yeni".into(), name: "yeni".into(), description: " kök neden ".into(), updated_at: since + 5 },
            MemInput { id: "shared/eski".into(), name: "eski".into(), description: "x".into(), updated_at: since - 5 },
            MemInput { id: "agents/kaan/not".into(), name: "not".into(), description: String::new(), updated_at: since + 9 },
        ];
        let got = catchup(&c, "default", Some(since), NOW, nodes).unwrap();
        assert_eq!(got.memories.items.len(), 2);
        assert_eq!(got.memories.items[0].title, "not");
        assert_eq!(got.memories.items[1].title, "yeni — kök neden");
        assert_eq!(got.memories.items[1].r#ref, Value::String("shared/yeni".into()));
        assert_eq!(got.memories.items[1].id, "memory:shared/yeni");
    }

    #[test]
    fn bucket_caps_at_fifty_with_more_count() {
        let items: Vec<_> = (0..53).map(|i| item("memory", Value::String(format!("shared/{i}")), format!("{i}"), i)).collect();
        let b = bucket(items);
        assert_eq!((b.items.len(), b.more), (50, 3));
        assert_eq!(b.items[0].at, 52, "newest first");
    }

    #[test]
    fn schema_is_replayable_and_preserves_the_cursor() {
        let c = conn();
        ack(&c, "default", NOW - 1, NOW).unwrap();
        for _ in 0..2 {
            c.execute_batch(SCHEMA).unwrap();
            assert_eq!(cursor(&c, "default").unwrap(), Some(NOW - 1));
        }
        let n: i64 = c.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // Deleting the team drops its cursor (ON DELETE CASCADE).
        c.execute("DELETE FROM teams WHERE id='default'", []).unwrap();
        assert_eq!(cursor(&c, "default").unwrap(), None);
    }
}
