//! Member proposals (DEX-22): a leader asks for a new worker (`propose_member`); only the patron
//! adds it (approval card in the UI). The decision reaches the leader as a note in its next hook
//! context (`plan::note`). Roles come from the catalog only.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;

use crate::db::with;
use crate::delegate::Caller;
use crate::Core;

/// Open proposals per team; more is noise for the patron.
const MAX_PENDING: i64 = 5;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: i64,
    pub delegation_id: Option<i64>,
    pub team_id: String,
    pub team_name: String,
    pub leader: String,
    pub leader_name: String,
    pub role_slug: String,
    pub role_name: String,
    pub name: Option<String>,
    pub model: String,
    pub provider: String,
    pub reason: String,
    pub status: String,
    pub agent_id: Option<String>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

const COLS: &str = "p.id, p.team_id, IFNULL(t.name, p.team_id), p.leader, IFNULL(a.name, p.leader), p.role_slug,
    IFNULL(r.name, p.role_slug), p.name, p.model, p.provider, p.reason, p.status, p.agent_id, p.created_at, p.decided_at, (SELECT delegation_id FROM job_models WHERE proposal_id=p.id)";
const FROM: &str = "member_proposals p LEFT JOIN teams t ON t.id = p.team_id
    LEFT JOIN agents a ON a.id = p.leader LEFT JOIN roles r ON r.slug = p.role_slug";

fn row(r: &Row) -> rusqlite::Result<Proposal> {
    Ok(Proposal {
        id: r.get(0)?,
        delegation_id: r.get(15)?,
        team_id: r.get(1)?,
        team_name: r.get(2)?,
        leader: r.get(3)?,
        leader_name: r.get(4)?,
        role_slug: r.get(5)?,
        role_name: r.get(6)?,
        name: r.get(7)?,
        model: r.get(8)?,
        provider: r.get(9)?,
        reason: r.get(10)?,
        status: r.get(11)?,
        agent_id: r.get(12)?,
        created_at: r.get(13)?,
        decided_at: r.get(14)?,
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

pub fn get(conn: &Connection, id: i64) -> Result<Proposal, String> {
    conn.query_row(&format!("SELECT {COLS} FROM {FROM} WHERE p.id = ?1"), [id], row)
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("öneri yok: {id}"))
}

pub fn list_pending(conn: &Connection) -> Result<Vec<Proposal>, String> {
    let mut st = conn.prepare(&format!("SELECT {COLS} FROM {FROM} WHERE p.status = 'pending' ORDER BY p.id")).map_err(e)?;
    let rows = st.query_map([], row).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}

/// Catalog slug for a slug or a role name (case-insensitive); the error lists the catalog.
fn role_slug(conn: &Connection, key: &str) -> Result<String, String> {
    let k = key.trim().to_lowercase();
    let hit: Option<String> = conn
        .query_row("SELECT slug FROM roles WHERE lower(slug) = ?1 OR lower(name) = ?1 ORDER BY sort LIMIT 1", [&k], |r| r.get(0))
        .optional()
        .map_err(e)?;
    hit.ok_or_else(|| {
        format!("rol katalogda yok: {key}. Geçerli roller: {}. Uygun rol yoksa önce patrona sor (escalate).", crate::roles::catalog_text(conn))
    })
}

/// Provider (default anthropic) + a model from its catalog (default: Sonnet / the first one).
fn provider_model(provider: Option<&str>, model: Option<&str>) -> Result<(String, String), String> {
    let p = provider.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("anthropic");
    let def = crate::providers::get(p).ok_or_else(|| format!("geçersiz sağlayıcı: {p}"))?;
    let model = match model.map(str::trim).filter(|s| !s.is_empty()) {
        Some(m) if def.live || def.models.iter().any(|x| x.id == m) => m.to_string(),
        Some(m) => {
            let ids: Vec<&str> = def.models.iter().filter(|x| !x.legacy).map(|x| x.id).collect();
            return Err(format!("{p} sağlayıcısında model yok: {m}. Seçenekler: {}", ids.join(", ")));
        }
        None if def.live => return Err(format!("{p} sağlayıcısında model adı verilmeli (canlı liste: Takım → Sağlayıcılar)")),
        None if p == "anthropic" => crate::providers::CLAUDE_MID.into(),
        None if def.models.iter().any(|x| x.id == "sonnet") => "sonnet".into(),
        None => def.models.first().map(|x| x.id.to_string()).ok_or_else(|| format!("{p} sağlayıcısının modeli yok"))?,
    };
    Ok((p.to_string(), model))
}

pub fn propose(conn: &Connection, lead: &Caller, args: &Value) -> Result<Proposal, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let role = role_slug(conn, s("role").ok_or("role gerekli")?)?;
    let reason = s("reason").map(str::trim).filter(|r| !r.is_empty()).ok_or("reason gerekli (patron bunu görecek)")?;
    let (provider, model) = provider_model(s("provider"), s("model"))?;
    let name = s("name").map(str::trim).filter(|n| !n.is_empty());
    let pending: i64 = conn
        .query_row("SELECT COUNT(*) FROM member_proposals WHERE team_id = ?1 AND status = 'pending'", [&lead.team_id], |r| r.get(0))
        .map_err(e)?;
    if pending >= MAX_PENDING {
        return Err(format!("takımın {pending} önerisi zaten patron onayında; önce onlar karara bağlansın"));
    }
    conn.execute(
        "INSERT INTO member_proposals (team_id, leader, role_slug, name, model, provider, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![lead.team_id, lead.id, role, name, model, provider, reason, now()],
    )
    .map_err(e)?;
    get(conn, conn.last_insert_rowid())
}

/// The patron's answer. Approve → the worker is created (name: the patron's, else the leader's,
/// else the role's template name); both ways the leader gets a note.
pub fn decide(conn: &Connection, data_dir: &std::path::Path, id: i64, approve: bool, name: Option<&str>) -> Result<Proposal, String> {
    let p = get(conn, id)?;
    if p.status != "pending" {
        return Err(format!("öneri #{id} zaten karara bağlandı ({})", p.status));
    }
    let tx = conn.unchecked_transaction().map_err(e)?;
    if p.delegation_id.is_some() {
        crate::job_model::decision(&tx, &p, approve)?;
        tx.commit().map_err(e)?;
        return get(conn, id);
    }
    let text = if approve {
        // None → create_agent picks the role's template name.
        let name = name.map(str::trim).filter(|n| !n.is_empty()).or(p.name.as_deref()).unwrap_or_default().to_string();
        let a = crate::teams::create_agent(&tx, data_dir, crate::teams::NewAgent {
            team_id: p.team_id.clone(),
            name,
            kind: "worker".into(),
            title: String::new(),
            model: p.model.clone(),
            provider: Some(p.provider.clone()),
            role_slug: Some(p.role_slug.clone()),
            engine: None,
        })?;
        tx.execute(
            "UPDATE member_proposals SET status = 'approved', agent_id = ?2, decided_at = ?3 WHERE id = ?1",
            params![id, a.id, now()],
        )
        .map_err(e)?;
        format!(
            "Üye önerin #{id} ONAYLANDI: {} ({}, {}) takıma eklendi, id {}. Artık delegate/plan ile iş verebilirsin.",
            a.name, p.role_name, a.model, a.id
        )
    } else {
        tx.execute("UPDATE member_proposals SET status = 'rejected', decided_at = ?2 WHERE id = ?1", params![id, now()])
            .map_err(e)?;
        format!("Üye önerin #{id} ({}) patron tarafından REDDEDİLDİ; üye eklenmedi. İşi mevcut takımla yürüt.", p.role_name)
    };
    crate::plan::note(&tx, &p.leader, &text)?;
    tx.commit().map_err(e)?;
    get(conn, id)
}

impl Core {
    pub fn proposal_list(&self) -> Result<Vec<Proposal>, String> {
        with(&self.db, list_pending)
    }

    pub fn proposal_decide(&self, id: i64, approve: bool, name: Option<&str>) -> Result<Proposal, String> {
        let p = with(&self.db, |c| decide(c, &self.paths.data_dir, id, approve, name))?;
        if let Some(job) = p.delegation_id {
            self.changed("delegations-changed");
            self.changed("tasks-changed");
            if approve { self.delegation_dispatch(with(&self.db, |c| crate::delegate::get(c,job))?)?; }
        } else if approve {
            let _ = self.events.emit("teams-changed", &());
        }
        let _ = self.events.emit("proposals-changed", &());
        self.leader_notes_wake(&p.leader);
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn propose_then_patron_decides_and_leader_hears() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let lead = crate::delegate::caller(&c, "timur").unwrap();
        let unknown = propose(&c, &lead, &json!({ "role": "ios-dev", "reason": "x" })).unwrap_err();
        assert!(unknown.contains("katalogda yok") && unknown.contains("Geçerli roller:") && unknown.contains("backend — Backend"), "{unknown}");
        assert!(propose(&c, &lead, &json!({ "role": "qa" })).unwrap_err().contains("reason"));
        assert!(propose(&c, &lead, &json!({ "role": "qa", "reason": "x", "model": "gpt-9" })).unwrap_err().contains("model yok"));

        let p = propose(&c, &lead, &json!({ "role": "QA", "reason": "test yükü arttı" })).unwrap();
        assert_eq!((p.role_slug.as_str(), p.model.as_str(), p.provider.as_str(), p.status.as_str()), ("qa", "claude-sonnet-5-5", "anthropic", "pending"));
        assert_eq!(list_pending(&c).unwrap().len(), 1);
        let before = crate::teams::list_agents(&c).unwrap().len();

        let done = decide(&c, std::path::Path::new("/data"), p.id, true, Some("Nil")).unwrap();
        assert_eq!(done.status, "approved");
        let a = crate::teams::get_agent(&c, done.agent_id.as_deref().unwrap()).unwrap();
        assert_eq!((a.name.as_str(), a.kind.as_str(), a.role_slug.as_deref(), a.team_id.as_str()), ("Nil", "worker", Some("qa"), "default"));
        assert_eq!(crate::teams::list_agents(&c).unwrap().len(), before + 1);
        assert!(decide(&c, std::path::Path::new("/data"), p.id, false, None).is_err(), "decided once");
        assert!(crate::plan::take_notes(&c, "timur").unwrap().unwrap().contains("ONAYLANDI: Nil"));

        let r = propose(&c, &lead, &json!({ "role": "backend", "reason": "y" })).unwrap();
        decide(&c, std::path::Path::new("/data"), r.id, false, None).unwrap();
        assert_eq!(crate::teams::list_agents(&c).unwrap().len(), before + 1);
        assert!(crate::plan::take_notes(&c, "timur").unwrap().unwrap().contains("REDDEDİLDİ"));
        assert!(list_pending(&c).unwrap().is_empty());
    }
}
