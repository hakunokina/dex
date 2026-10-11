// DEX-24: a worker's question its leader escalated (`escalate`). The patron's answer goes to the
// worker the same way a leader's does, and the leader hears it in its next hook context.
import { useState } from "react";
import { answerQuestion, useQuestions, type Question } from "../questions";
import "./proposals.css";

/** #247: `hide` drops a leader's cards while its Core balloon carries them. */
export function EscalatedQuestions({ hide }: { hide?: (leader: string) => boolean }) {
  const rows = useQuestions().filter((q) => q.status === "escalated" && !hide?.(q.leader));
  if (!rows.length) return null;
  return (
    <aside className="proposals asks" aria-label="Patrona çıkan sorular">
      {rows.map((q) => <Card key={q.key} q={q} />)}
    </aside>
  );
}

/** #243: also the Core balloon's content; `onError` lets Core show `!` (the card keeps its own line). */
export function Card({ q, onError }: { q: Question; onError?: (e: string) => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const send = async (value: string) => {
    if (!value.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await answerQuestion(q, value.trim());
    } catch (e) {
      setError(String(e));
      setBusy(false);
      onError?.(String(e));
    }
  };
  return (
    <div className="proposal" role="alertdialog" aria-label={`${q.workerName} soruyor`}>
      <h4>
        Soru #{q.id} <span className="proposal-team">{q.teamName}</span>
      </h4>
      <p className="proposal-who">
        <b>{q.workerName}</b> sordu, <b>{q.leaderName}</b> sana çıkardı
        <span className="proposal-model">devretme #{q.delegationId}</span>
      </p>
      <p className="proposal-reason">{q.question}</p>
      {q.context && <p className="proposal-who ask-muted">Bağlam: {q.context}</p>}
      {q.note && <p className="proposal-who ask-muted">{q.leaderName}: {q.note}</p>}
      {q.options.length > 0 && (
        <div className="proposal-actions ask-options">
          {q.options.map((o, i) => (
            <button key={o} disabled={busy} data-key={i < 9 ? String(i + 1) : undefined} aria-keyshortcuts={i < 9 ? String(i + 1) : undefined}
              onClick={() => void send(o)}>{o}</button>
          ))}
        </div>
      )}
      <label>
        Cevap
        <input
          value={text}
          placeholder="cevabın worker'a gider"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void send(text)}
          aria-label="Soruya cevap"
        />
      </label>
      {error && <p className="team-err">{error}</p>}
      <div className="proposal-actions">
        <button className="primary" disabled={busy || !text.trim()} data-key="Enter" onClick={() => void send(text)}>Gönder</button>
      </div>
    </div>
  );
}
