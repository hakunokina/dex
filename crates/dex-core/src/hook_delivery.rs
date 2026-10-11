//! Stop payloads stay pending until the CLI proves that it continued.
use rusqlite::{params, Connection};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Rows {
    pub results: Vec<(i64, i64)>,
    pub questions: Vec<i64>,
    pub answers: Vec<i64>,
    pub notes: Vec<i64>,
    pub worker_notes: Vec<i64>,
}

#[derive(Default)]
pub(crate) struct Pending(pub Mutex<Rows>);

impl Rows {
    pub(crate) fn accept(&self, conn: &Connection, agent: &str) -> Result<(), String> {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let ts = crate::diagnostics::now_ms();
        for (id, version) in &self.results {
            tx.execute("UPDATE delegations SET notified=1, delivered_at=?3, read_at=NULL WHERE id=?1 AND from_agent=?2 AND notified=0 AND updated_at=?4 AND status IN ('done','done_no_report','failed')", params![id, agent, ts, version]).map_err(|e| e.to_string())?;
        }
        for id in &self.questions {
            tx.execute("UPDATE questions SET leader_delivered_at=?3 WHERE id=?1 AND leader=?2 AND status='open' AND leader_delivered_at IS NULL", params![id, agent, ts]).map_err(|e| e.to_string())?;
        }
        for id in &self.answers {
            tx.execute("UPDATE questions SET answer_delivered_at=?3 WHERE id=?1 AND worker=?2 AND status='answered' AND answer_delivered_at IS NULL", params![id, agent, ts]).map_err(|e| e.to_string())?;
        }
        for id in &self.notes {
            tx.execute(
                "UPDATE leader_notes SET notified=1 WHERE id=?1 AND agent_id=?2 AND notified=0",
                params![id, agent],
            )
            .map_err(|e| e.to_string())?;
        }
        for id in &self.worker_notes {
            tx.execute("UPDATE worker_notes SET delivered_at=?3 WHERE id=?1 AND delivered_at IS NULL
                AND delegation_id IN (SELECT id FROM delegations WHERE to_agent=?2 AND status='running')",
                params![id,agent,ts]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}
