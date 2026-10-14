//! Per-job model approval. The saved agent model is never changed.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

// Versioned migration: ROLLBACK before commit; after commit keep the additive schema.
// To disable the feature, reject pending proposals; do not downgrade user_version.
pub(crate) const SCHEMA: &str = "
ALTER TABLE delegations ADD COLUMN model TEXT;
ALTER TABLE delegations ADD COLUMN model_approval TEXT;
ALTER TABLE delegations ADD COLUMN actual_model TEXT;
ALTER TABLE delegations ADD COLUMN actual_provider TEXT;
ALTER TABLE delegations ADD COLUMN actual_engine TEXT;
ALTER TABLE plan_items ADD COLUMN model TEXT;
ALTER TABLE plan_items ADD COLUMN model_reason TEXT;
CREATE TABLE IF NOT EXISTS job_models (
 delegation_id INTEGER PRIMARY KEY REFERENCES delegations(id) ON DELETE CASCADE,
 proposal_id INTEGER NOT NULL UNIQUE REFERENCES member_proposals(id) ON DELETE CASCADE,
 engine TEXT NOT NULL, provider TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS job_model_start_guard BEFORE UPDATE OF status ON delegations
WHEN NEW.status='running' AND NEW.model IS NOT NULL AND NEW.model_approval IS NOT 'approved'
BEGIN SELECT RAISE(ABORT,'model_patron_approval_required'); END;
CREATE TRIGGER IF NOT EXISTS job_model_withdraw AFTER UPDATE OF status ON delegations
WHEN NEW.status NOT IN ('queued','running') AND NEW.model_approval='pending'
BEGIN
 UPDATE member_proposals SET status='rejected',decided_at=NEW.updated_at
 WHERE id=(SELECT proposal_id FROM job_models WHERE delegation_id=NEW.id) AND status='pending';
 UPDATE delegations SET model_approval='rejected' WHERE id=NEW.id;
END;
";

// Additive/versioned: ROLLBACK before commit; after commit retain these nullable columns.
// Older waiting plans have no proven route, so they keep the chain model at dispatch.
pub(crate) const PLAN_ROUTE_SCHEMA: &str = "
ALTER TABLE plan_items ADD COLUMN model_engine TEXT;
ALTER TABLE plan_items ADD COLUMN model_provider TEXT;
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub engine: String,
    pub provider: String,
}

impl Route {
    fn of(a: &crate::teams::Agent) -> Self {
        Self {
            engine: a.active_engine.as_deref().unwrap_or(&a.engine).into(),
            provider: a.active_provider.as_deref().unwrap_or(&a.provider).into(),
        }
    }

    pub(crate) fn validate_catalog(&self, model: &str) -> Result<(), String> {
        catalog(&self.engine, &self.provider, model.trim())
    }
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    worker: String,
    team: String,
    role: Option<String>,
    pub route: Route,
}

pub(crate) fn snapshot(conn: &Connection, worker: &str, model: Option<&str>, reason: Option<&str>, role: Option<&str>) -> Result<Option<Snapshot>, String> {
    args(&json!({"model":model,"modelReason":reason}))?;
    if model.is_none() { return Ok(None); }
    let a = crate::teams::get_agent(conn, worker)?;
    if a.kind != "worker" { return Err("iş modeli yalnız worker için seçilebilir".into()); }
    Ok(Some(Snapshot { route: Route::of(&a), worker: a.id, team: a.team_id, role: role.map(str::to_owned) }))
}

impl Snapshot {
    /// Membership is authority; a changed chain route is allowed and keeps its own model.
    pub(crate) fn revalidate(&self, conn: &Connection) -> Result<bool, String> {
        let a = crate::teams::get_agent(conn, &self.worker)?;
        if a.kind != "worker" || a.team_id != self.team || self.role.as_ref().is_some_and(|role| a.role_slug.as_ref() != Some(role)) {
            return Err(crate::projects::ProjectError::NotFound.to_string());
        }
        Ok(Route::of(&a) == self.route)
    }

    pub(crate) fn note_changed_route(&self, conn: &Connection, leader: &str, item: &str, requested: &str) -> Result<(), String> {
        let a = crate::teams::get_agent(conn, &self.worker)?;
        let current = Route::of(&a);
        crate::plan::note(conn, leader, &format!(
            "{item}: istenen model {}; kullanılan rota {}/{}/{}. Neden: katalog doğrulamasından sonra worker/sağlayıcı rotası değişti. Plan kabul edildi; iş zincir modeliyle devam edecek.",
            requested.trim(), current.engine, current.provider, a.active_model.as_deref().unwrap_or(&a.model)
        ))
    }
}

pub fn fields() -> Value {
    json!({"model":{"type":"string","description":"Yalnız bu işin modeli; hedef motor/sağlayıcı kataloğundan. Patron onayı olmadan başlamaz."},
        "modelReason":{"type":"string","description":"model verilince zorunlu; patronun göreceği gerekçe."}})
}

pub fn args(args: &Value) -> Result<(Option<&str>, Option<&str>), String> {
    let field = |key: &str| -> Result<Option<&str>, String> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim())),
            _ => Err(format!("{key} boş olmayan metin olmalı")),
        }
    };
    let model = field("model")?;
    let reason = field("modelReason")?;
    if model.is_some() && reason.is_none() {
        return Err("modelReason gerekli (patron bunu görecek)".into());
    }
    if model.is_none() && reason.is_some() {
        return Err("modelReason için model gerekli".into());
    }
    Ok((model, reason))
}

#[cfg(test)]
thread_local! {
    pub(crate) static CATALOG_HOOK: std::cell::RefCell<Option<Box<dyn Fn() -> Result<(), String>>>> = Default::default();
}

fn catalog(engine: &str, provider: &str, model: &str) -> Result<(), String> {
    #[cfg(test)]
    if let Some(result) = CATALOG_HOOK.with(|hook| hook.borrow().as_ref().map(|f| f())) {
        return result;
    }
    let valid = if engine != "claude" {
        return Err(format!("iş modeli bu motorda desteklenmiyor: {engine}"));
    } else if provider == "local" {
        crate::providers::local_status()?.models.iter().any(|m| m.id == model)
    } else {
        crate::providers::get(provider)
            .ok_or_else(|| format!("geçersiz sağlayıcı: {provider}"))?
            .models
            .iter()
            .any(|m| m.id == model)
    };
    if valid {
        Ok(())
    } else {
        Err(format!("{engine}/{provider} kataloğunda model yok: {model}"))
    }
}

/// Caller holds the insertion transaction: no queued row is visible before its approval gate.
pub(crate) fn request(conn: &Connection, id: i64, model: Option<&str>, reason: Option<&str>, role: Option<&str>, route: Option<&Route>) -> Result<(), String> {
    let Some(model) = model else { return Ok(()) };
    let d = crate::delegate::get(conn, id)?;
    // Never substitute today's route for the route validated at ingress. NULL on an old plan
    // is unproven, represented by an empty route so apply preserves the current chain model.
    let engine = route.map(|r| r.engine.as_str()).unwrap_or("");
    let provider = route.map(|r| r.provider.as_str()).unwrap_or("");
    args(&json!({"model":model,"modelReason":reason}))?;
    let a = crate::teams::get_agent(conn, &d.to_agent)?;
    if a.kind != "worker" {
        return Err("iş modeli yalnız worker için seçilebilir".into());
    }
    let target = if let Some(role) = role {
        format!("Rol: {role}")
    } else {
        format!("Worker: {} ({})", a.name, a.title)
    };
    let reason = format!(
        "İş #{}: {}\n{}\nModel: {}\nGerekçe: {}",
        id,
        d.task_id
            .and_then(|id| crate::db::tasks::get(conn, id).ok())
            .map(|t| t.title)
            .unwrap_or_else(|| d.prompt.clone()),
        target,
        model.trim(),
        reason.unwrap_or_default().trim()
    );
    conn.execute(
        "INSERT INTO member_proposals(team_id,leader,role_slug,name,model,provider,reason,created_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            d.team_id,
            d.from_agent,
            role.map(str::to_owned).or(a.role_slug.clone()).unwrap_or_default(),
            if role.is_some() { None } else { Some(&a.name) },
            model.trim(),
            provider,
            reason,
            d.created_at
        ],
    )
    .map_err(|e| e.to_string())?;
    let proposal = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO job_models(delegation_id,proposal_id,engine,provider) VALUES(?1,?2,?3,?4)",
        params![
            id,
            proposal,
            engine,
            provider
        ],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE delegations SET model=?2,model_approval='pending' WHERE id=?1",
        params![id, model.trim()],
    )
    .map_err(|e| e.to_string())?;
    if route != Some(&Route::of(&a)) {
        crate::plan::note(conn, &d.from_agent, &format!(
            "İş #{id}: istenen model {}; kullanılan rota {}/{}/{}. Neden: {}. İş patron onayından sonra zincir modeliyle devam edecek.",
            model.trim(), a.active_engine.as_deref().unwrap_or(&a.engine),
            a.active_provider.as_deref().unwrap_or(&a.provider), a.active_model.as_deref().unwrap_or(&a.model),
            if route.is_some() { "katalog doğrulamasından sonra worker/sağlayıcı rotası değişti" } else { "eski planın doğrulanan rotası kayıtlı değil" }
        ))?;
    }
    Ok(())
}

pub fn can_start(conn: &Connection, id: i64) -> Result<bool, String> {
    conn.query_row(
        "SELECT model IS NULL OR model_approval='approved' FROM delegations WHERE id=?1",
        [id],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn snapshot_plan(conn: &Connection, team: &str, plan: &mut crate::plan::NewPlan) -> Result<(), String> {
    for task in &mut plan.tasks {
        args(&json!({"model":task.model,"modelReason":task.model_reason}))?;
        if task.model.is_none() { continue; }
        let (worker, role) = if task.agent_id.is_some() {
            (crate::delegate::pick_worker(conn, team, task.agent_id.as_deref())?, None)
        } else {
            let (role, worker) = crate::delegate::role_target(conn, team, task.role.as_deref().unwrap_or(""))?;
            (worker, Some(role))
        };
        task.model_snapshot = snapshot(conn, &worker, task.model.as_deref(), task.model_reason.as_deref(), role.as_deref())?;
    }
    Ok(())
}

pub(crate) fn validate_plan_catalog(plan: &crate::plan::NewPlan) -> Result<(), String> {
    for task in &plan.tasks {
        if let (Some(model), Some(snapshot)) = (&task.model, &task.model_snapshot) {
            snapshot.route.validate_catalog(model)?;
        }
    }
    Ok(())
}

pub fn decision(conn: &Connection, p: &crate::proposals::Proposal, approve: bool) -> Result<(), String> {
    let id = p.delegation_id.ok_or("iş modeli önerisi değil")?;
    let d = crate::delegate::get(conn, id)?;
    if d.status != "queued" || d.model_approval.as_deref() != Some("pending") {
        return Err("model onayı artık geçerli değil; iş kapandı".into());
    }
    let state = if approve { "approved" } else { "rejected" };
    conn.execute(
        "UPDATE member_proposals SET status=?2,decided_at=?3 WHERE id=?1",
        params![p.id, state, now()],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("UPDATE delegations SET model_approval=?2 WHERE id=?1", params![id, state])
        .map_err(|e| e.to_string())?;
    if approve && d.target_role.is_some() {
        crate::delegate::assign_role_job(conn, id)?;
    }
    if !approve {
        conn.execute(
            "UPDATE delegations SET status='failed',summary='Patron model isteğini reddetti',updated_at=?2 WHERE id=?1",
            params![id, now()],
        )
        .map_err(|e| e.to_string())?;
        if let Some(task) = d.task_id {
            conn.execute("UPDATE tasks SET status='todo' WHERE id=?1", [task])
                .map_err(|e| e.to_string())?;
        }
    }
    crate::plan::note(
        conn,
        &p.leader,
        &format!(
            "İş #{id} model isteği {}: {} ({}). {}",
            if approve { "ONAYLANDI" } else { "REDDEDİLDİ" },
            p.model,
            p.name.as_deref().unwrap_or(&p.role_name),
            if approve {
                "İş onaylandı; model sağlayıcı zincirine göre seçilecek."
            } else {
                "İş başlamadı."
            }
        ),
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Written only after a successful spawn. Requested model and approval remain unchanged.
pub fn record_spawn(
    conn: &Connection,
    agent: &str,
    job: Option<i64>,
    engine: &str,
    provider: &str,
    model: &str,
    reason: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(id) = job else { return Ok(None) };
    let d = crate::delegate::get(conn, id)?;
    if d.to_agent != agent {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    let Some(requested) = d.model else { return Ok(None) };
    if !matches!(d.status.as_str(), "queued" | "running") {
        return Ok(None);
    }
    if d.model_approval.as_deref() != Some("approved") {
        return Err("model_patron_approval_required".into());
    }
    if d.actual_model.as_deref() == Some(model)
        && d.actual_provider.as_deref() == Some(provider)
        && d.actual_engine.as_deref() == Some(engine)
    {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE delegations SET actual_model=?2,actual_provider=?3,actual_engine=?4 WHERE id=?1",
        params![id, model, provider, engine],
    )
    .map_err(|e| e.to_string())?;
    let (approved_engine, approved_provider): (String, String) = tx
        .query_row("SELECT engine,provider FROM job_models WHERE delegation_id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;
    if requested != model || engine != approved_engine || provider != approved_provider || d.actual_model.is_some() {
        crate::plan::note(&tx,&d.from_agent,&format!("İş #{id}: istenen model {requested}; fiilen çalışan {engine}/{provider}/{model}. Neden: {}. İş devam ediyor; yeni onay gerekmiyor.",
            reason.unwrap_or(if d.target_role.is_some() && d.actual_model.is_none() {
                "rol havuzunda atanan worker'ın model/sağlayıcı rotası"
            } else {
                "onay sonrası sağlayıcı zinciri/kota yönlendirmesi"
            })))?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(d.from_agent))
}

/// Select by explicit job or running job only; a completed session never carries the override.
pub fn apply(conn: &Connection, agent: &mut crate::teams::Agent, job: Option<i64>) -> Result<bool, String> {
    let id = match job {
        Some(id) => Some(id),
        None => conn
            .query_row(
                "SELECT id FROM delegations WHERE to_agent=?1 AND status='running' ORDER BY id DESC LIMIT 1",
                [&agent.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?,
    };
    let Some(id) = id else { return Ok(false) };
    let d = crate::delegate::get(conn, id)?;
    if d.to_agent != agent.id {
        return Err(crate::projects::ProjectError::NotFound.to_string());
    }
    let Some(model) = d.model.filter(|_| matches!(d.status.as_str(), "queued" | "running")) else {
        return Ok(false);
    };
    if d.model_approval.as_deref() != Some("approved") {
        return Err("model_patron_approval_required".into());
    }
    let (engine, provider): (String, String) = conn
        .query_row("SELECT engine,provider FROM job_models WHERE delegation_id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;
    // Approval is for the original route. A later chain/quota route keeps its own model.
    if agent.active_engine.as_deref().unwrap_or(&agent.engine) != engine
        || agent.active_provider.as_deref().unwrap_or(&agent.provider) != provider
    {
        return Ok(false);
    }
    if engine == "claude" && crate::providers::get(&provider).is_some_and(|p| !p.live && !p.models.iter().any(|m| m.id == model)) {
        return Ok(false);
    }
    agent.active_model = Some(model);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        crate::db::open(std::path::Path::new(":memory:")).unwrap()
    }
    fn job(c: &Connection, model: Option<&str>) -> crate::delegate::Delegation {
        let lead = crate::delegate::caller(c, "timur").unwrap();
        let tx = c.unchecked_transaction().unwrap();
        let d = crate::delegate::insert(
            &tx,
            crate::delegate::NewDelegation {
                from: &lead,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "Analiz".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        let snapshot = snapshot(&tx, "kaan", model, model.map(|_| "Daha derin analiz"), None).unwrap();
        request(&tx, d.id, model, model.map(|_| "Daha derin analiz"), None, snapshot.as_ref().map(|s| &s.route)).unwrap();
        tx.commit().unwrap();
        crate::delegate::get(c, d.id).unwrap()
    }

    #[test]
    fn approval_is_required_at_dispatch_ack_and_spawn() {
        let c = db();
        let d = job(&c, Some("opus"));
        assert!(!can_start(&c, d.id).unwrap());
        assert!(crate::delegate::start_request(&c, &d).err().unwrap().contains("approval"));
        assert!(crate::delegate::set_status(&c, d.id, "running").unwrap_err().contains("approval"));
        let mut a = crate::teams::get_agent(&c, "kaan").unwrap();
        assert!(apply(&c, &mut a, Some(d.id)).unwrap_err().contains("approval"));
        let p = &crate::proposals::list_pending(&c).unwrap()[0];
        assert_eq!(p.delegation_id, Some(d.id));
        assert!(
            p.reason.contains("Analiz") && p.reason.contains("Kaan") && p.reason.contains("opus") && p.reason.contains("Daha derin analiz")
        );
    }

    #[test]
    fn approved_model_is_transient_and_completed_job_restores_saved_model() {
        let c = db();
        let d = job(&c, Some("opus"));
        let original = crate::teams::get_agent(&c, "kaan").unwrap();
        let p = crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c, std::path::Path::new("."), p.id, true, None).unwrap();
        assert!(can_start(&c, d.id).unwrap());
        let mut spawn = crate::teams::get_agent(&c, "kaan").unwrap();
        assert!(apply(&c, &mut spawn, Some(d.id)).unwrap());
        assert_eq!(spawn.active_model.as_deref(), Some("opus"));
        crate::delegate::set_status(&c, d.id, "running").unwrap();
        let mut respawn = crate::teams::get_agent(&c, "kaan").unwrap();
        assert!(apply(&c, &mut respawn, None).unwrap());
        assert_eq!(respawn.active_model.as_deref(), Some("opus"));
        crate::delegate::set_status(&c, d.id, "done").unwrap();
        let mut after = crate::teams::get_agent(&c, "kaan").unwrap();
        assert!(!apply(&c, &mut after, None).unwrap());
        assert!(!apply(&c, &mut after, Some(d.id)).unwrap());
        assert_eq!(after.model, original.model);
        assert_eq!(after.active_model, original.active_model);
        assert!(crate::plan::take_notes(&c, "timur").unwrap().unwrap().contains("ONAYLANDI"));
    }

    #[test]
    fn rejection_never_starts_and_notifies_leader() {
        let c = db();
        let d = job(&c, Some("opus"));
        let p = crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c, std::path::Path::new("."), p.id, false, None).unwrap();
        let rejected = crate::delegate::get(&c, d.id).unwrap();
        assert_eq!(rejected.status, "failed");
        assert_eq!(rejected.model_approval.as_deref(), Some("rejected"));
        assert!(!can_start(&c, d.id).unwrap());
        assert!(crate::plan::take_notes(&c, "timur").unwrap().unwrap().contains("REDDEDİLDİ"));
        assert!(crate::proposals::decide(&c, std::path::Path::new("."), p.id, true, None).is_err());
    }

    #[test]
    fn invalid_model_and_missing_reason_are_rejected_without_writes() {
        let c = db();
        let count = || {
            c.query_row("SELECT COUNT(*) FROM member_proposals", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let before = count();
        assert!(snapshot(&c, "kaan", Some("does-not-exist"), Some("gerekçe"), None).unwrap().unwrap().route.validate_catalog("does-not-exist")
            .unwrap_err()
            .contains("kataloğunda model yok"));
        assert!(snapshot(&c, "kaan", Some("opus"), None, None).unwrap_err().contains("modelReason"));
        assert!(snapshot(&c, "timur", Some("opus"), Some("x"), None).is_err());
        assert_eq!(before, count());
        for args in [
            json!({"model":7}),
            json!({"model":" "}),
            json!({"modelReason":"x"}),
            json!({"model":"opus","modelReason":" "}),
        ] {
            assert!(super::args(&args).is_err());
        }
    }

    #[test]
    fn snapshot_allows_chain_route_changes_but_rechecks_worker_membership() {
        let c = db();
        let role = crate::teams::get_agent(&c, "kaan").unwrap().role_slug.unwrap();
        let snapshot = snapshot(&c, "kaan", Some("opus"), Some("analysis"), Some(&role)).unwrap().unwrap();
        assert!(snapshot.revalidate(&c).unwrap());
        c.execute("UPDATE agents SET active_provider='glm',active_model='GLM-5.3' WHERE id='kaan'", []).unwrap();
        assert!(!snapshot.revalidate(&c).unwrap(), "route change remains authorized");
        c.execute("UPDATE agents SET role_slug=NULL WHERE id='kaan'", []).unwrap();
        assert_eq!(snapshot.revalidate(&c).unwrap_err(), crate::projects::ProjectError::NotFound.to_string());
        c.execute("UPDATE agents SET role_slug=?1,kind='leader' WHERE id='kaan'", [&role]).unwrap();
        assert_eq!(snapshot.revalidate(&c).unwrap_err(), crate::projects::ProjectError::NotFound.to_string());
    }

    #[test]
    fn plain_job_needs_no_approval_and_pending_survives_restart() {
        let c = db();
        let plain = job(&c, None);
        let pending = job(&c, Some("opus"));
        assert!(can_start(&c, plain.id).unwrap());
        crate::delegate::set_status(&c, plain.id, "running").unwrap();
        crate::delegate::reconcile_on_start(&c, false).unwrap();
        assert_eq!(crate::delegate::get(&c, pending.id).unwrap().status, "queued");
        assert_eq!(crate::proposals::list_pending(&c).unwrap().len(), 1);
    }

    #[test]
    fn cancelled_job_removes_pending_card() {
        let c = db();
        let d = job(&c, Some("opus"));
        crate::delegate::set_status(&c, d.id, "failed").unwrap();
        assert!(crate::proposals::list_pending(&c).unwrap().is_empty());
    }

    #[test]
    fn approved_job_continues_on_chain_route_without_reapproval() {
        let c = db();
        let d = job(&c, Some("opus"));
        let p = crate::proposals::list_pending(&c).unwrap().remove(0);
        crate::proposals::decide(&c, std::path::Path::new("."), p.id, true, None).unwrap();
        crate::delegate::set_status(&c, d.id, "running").unwrap();
        c.execute("UPDATE agents SET active_provider='glm',active_model='GLM-5.3' WHERE id='kaan'", [])
            .unwrap();
        let mut a = crate::teams::get_agent(&c, "kaan").unwrap();
        assert!(!apply(&c, &mut a, Some(d.id)).unwrap(), "chain route keeps its own model");
        assert_eq!(a.active_model.as_deref(), Some("GLM-5.3"));
        record_spawn(&c, "kaan", Some(d.id), "claude", "glm", "GLM-5.3", Some("kota")).unwrap();
        let actual = crate::delegate::get(&c, d.id).unwrap();
        assert_eq!(actual.model.as_deref(), Some("opus"));
        assert_eq!(actual.actual_model.as_deref(), Some("GLM-5.3"));
        assert_eq!(actual.actual_provider.as_deref(), Some("glm"));
        let note = crate::plan::take_notes(&c, "timur").unwrap().unwrap();
        assert!(note.contains("opus") && note.contains("GLM-5.3") && note.contains("glm") && note.contains("kota"));
        assert_eq!(crate::delegate::get(&c, d.id).unwrap().status, "running");
        assert_eq!(crate::proposals::get(&c, p.id).unwrap().status, "approved");
    }
}
