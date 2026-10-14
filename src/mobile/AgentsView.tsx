// S5-A: the team's members — leader(s) first, then workers. Status dot (çalışıyor/bekliyor/boşta/
// kapalı) plus the same marks the desktop rack shows: green `✓` finished-but-unread result, amber
// `!`, `?` an escalated question. Tap → that agent's screen (terminal / chat).
import { useEffect, useState } from "react";
import { getStatus, type AgentStatus } from "../agents";
import { membersOf, useTeams } from "../team/store";
import { useDelegations, unreadResult, lastResultLine } from "../delegations";
import { getQuestions, workerQuestion } from "../questions";
import { stalledFor } from "../stall";
import type { AgentRec } from "../team/api";
import { ChevronRight } from "lucide-react";

const STATUS_DOT: Record<AgentStatus, string> = {
  working: "working",
  waiting: "waiting",
  idle: "idle",
  offline: "offline",
};
const STATUS_TEXT: Record<AgentStatus, string> = {
  working: "çalışıyor",
  waiting: "bekliyor",
  idle: "boşta",
  offline: "kapalı",
};

export function AgentsView({ teamId, onOpen }: { teamId: string; onOpen: (id: string) => void }) {
  useTeams(); // member list changes repaint this view
  const [, setTick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setTick((n) => n + 1), 500);
    return () => clearInterval(t);
  }, []);
  const members = membersOf(teamId);
  const leaders = members.filter((a) => a.kind === "leader");
  const workers = members.filter((a) => a.kind === "worker");
  return (
    <div className="m-agents">
      {leaders.length > 0 && (
        <section>
          <h3>Lider</h3>
          {leaders.map((a) => <Row key={a.id} a={a} onOpen={onOpen} leader />)}
        </section>
      )}
      <section>
        <h3>Worker'lar</h3>
        {workers.map((a) => <Row key={a.id} a={a} onOpen={onOpen} />)}
      </section>
    </div>
  );
}

function Row({ a, onOpen, leader }: { a: AgentRec; onOpen: (id: string) => void; leader?: boolean }) {
  const status = getStatus(a.id);
  const delegations = useDelegations();
  const result = unreadResult(delegations, a.id);
  const question = workerQuestion(getQuestions(), a.id);
  const stalled = stalledFor(a.id);
  // Chat preview from what the phone already has: the open question, else the newest result line.
  const preview = question?.question ?? lastResultLine(delegations, a.id) ?? "";
  return (
    <button className={`m-agent${leader ? " leader" : ""}`} onClick={() => onOpen(a.id)}>
      <span className={`m-dot ${STATUS_DOT[status]}`} />
      <span className="m-agent-main">
        <b>{a.name}</b>
        <span className="m-agent-sub">{a.title || a.roleName || ""}</span>
        {preview && <span className={`m-agent-preview${question || result ? " unread" : ""}`}>{preview}</span>}
      </span>
      {(result || question || stalled) && (
        <span className="m-marks">
          {result && <i className="mark done" title="Biten iş — sonucu okunmadı">✓</i>}
          {question && <i className="mark ask" title="Sorusu var">?</i>}
          {stalled && <i className="mark warn" title={`Takıldı: ${stalled}`}>!</i>}
        </span>
      )}
      <span className="m-agent-side">
        <span className="m-engine">Claude</span>
        <span className="m-status-text">{STATUS_TEXT[status]}</span>
      </span>
      <ChevronRight size={16} className="m-chev-agent" />
    </button>
  );
}
