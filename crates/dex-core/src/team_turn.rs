//! Completion is evaluated only at a reconciled leader turn end, never on worker reports.
use crate::{agents::{State, Status}, db::with, events::TeamTurnDone, Core};
use rusqlite::OptionalExtension;

impl Core {
    pub(crate) fn team_turn_ended(&self, state: &State) -> Result<(), String> {
        if state.status != Status::Idle { return Ok(()) }
        // Paste acknowledgement takes its reservation lock before DB; keep that order here.
        let pending_chat = self.chat_queues.lock().unwrap().get(&state.agent_id)
            .is_some_and(|queue| !queue.is_empty());
        let pending_paste = self.leader_pastes.lock().unwrap().get(&state.agent_id)
            .is_some_and(|paste| paste.pending());
        let team = with(&self.db, |conn| {
            let row = conn.query_row(
                "SELECT a.team_id,
                    EXISTS(SELECT 1 FROM delegations d
                        WHERE (d.team_id = a.team_id OR d.service_team_id = a.team_id)
                        AND d.status IN ('running', 'queued'))
                    OR EXISTS(SELECT 1 FROM plans p JOIN plan_items i ON i.plan_id = p.id
                        WHERE p.team_id = a.team_id AND p.status = 'active'
                        AND i.status IN ('running', 'waiting'))
                    OR EXISTS(SELECT 1 FROM delegations d WHERE d.from_agent = a.id
                        AND d.notified = 0 AND d.status IN ('done', 'done_no_report', 'failed'))
                    OR EXISTS(SELECT 1 FROM leader_notes n WHERE n.agent_id = a.id AND n.notified = 0)
                    OR EXISTS(SELECT 1 FROM questions q WHERE q.leader = a.id
                        AND q.status IN ('open', 'escalated'))
                 FROM agents a WHERE a.id = ?1 AND a.kind = 'leader'",
                [&state.agent_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
            let Some((team, busy)) = row else { return Ok(None) };
            // Consume even a busy end: a later worker report must not reconsider this idle.
            let claimed = self.agent_states.claim_team_turn_end(state);
            Ok((claimed && !busy && !pending_chat && !pending_paste).then_some(team))
        })?;
        let Some(team_id) = team else { return Ok(()) };
        self.events.emit("team_turn_done", &TeamTurnDone {
            team_id,
            leader_id: state.agent_id.clone(),
            timestamp: crate::diagnostics::now_ms(),
        })
    }
}
