// #2235: "Sen yokken" — Core's tab group (Core | Notlar | Sen yokken | Echo). Three short
// sections over the opening snapshot; a row opens its source panel. The cursor advances to
// the shown snapshot's `generatedAt` (never "now") after 10 s on screen with the window
// focused, or at once via "Okudum"; the list itself never changes under the reader.
import { useEffect, useId, useRef, useState } from "react";
import { Check } from "lucide-react";
import { hostOf } from "../api/hosts";
import type { CatchupItem } from "../api/catchup";
import { getDelegations } from "../delegations";
import { getQuestions } from "../questions";
import { getProposals } from "../team/Proposals";
import { openAgent, openPanel, openSettings } from "../layout/panels";
import { ACK_MS, ackDue, isEmpty, kindLabel, sections, targetFor, visibleItems, whenAgo, type Resolved, type Section } from "./model";
import { ackShown, ensureCatchup, retryCatchup, useCatchup } from "./store";
import "./catchup.css";

/** Resolve a row's `ref` to its agents from the lists the client already keeps (no extra fetch). */
function resolve(teamId: string, item: CatchupItem): Resolved {
  if (typeof item.ref !== "number") return {};
  const host = hostOf(teamId);
  const ref = item.ref;
  if (item.kind === "question_escalated" || item.kind === "question_open") {
    const q = getQuestions().find(q => q.host === host && q.id === ref);
    return q ? { worker: q.worker, leader: q.leader } : {};
  }
  if (item.kind === "member_proposal" || item.kind === "model_approval" || item.kind === "job_change_approval") {
    const p = getProposals().find(p => p.host === host && p.id === ref);
    if (p) return { leader: p.leader };
    // A proposal-less model approval carries the delegation id instead: fall through.
  }
  const d = getDelegations().find(d => d.host === host && d.id === ref);
  return d ? { worker: d.toAgent || undefined, leader: d.fromAgent } : {};
}

function go(teamId: string, item: CatchupItem) {
  const t = targetFor(item, resolve(teamId, item));
  if (t.type === "agent") openAgent(teamId, t.agentId);
  else if (t.type === "board") void openPanel(teamId, "board");
  else if (t.type === "memory") void openPanel(teamId, "memory");
  else openSettings(teamId, "gpu");
}

export function CatchupPage({ teamId, visible }: { teamId: string; visible: boolean }) {
  const entry = useCatchup(teamId);
  const [focused, setFocused] = useState(() => document.hasFocus());
  const [acked, setAcked] = useState(false);
  const [ackErr, setAckErr] = useState<string | null>(null);
  const acking = useRef(false);

  // Hidden teams' pages stay mounted; only the one on screen fetches (each call scans the memory graph).
  useEffect(() => { if (visible) ensureCatchup(teamId); }, [visible, teamId]);
  // A new team (or a snapshot replaced wholesale) starts un-acked.
  useEffect(() => { setAcked(false); setAckErr(null); }, [teamId]);

  useEffect(() => {
    const on = () => setFocused(true);
    const off = () => setFocused(false);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    return () => { window.removeEventListener("focus", on); window.removeEventListener("blur", off); };
  }, []);

  const ack = async () => {
    if (acking.current || acked) return;
    acking.current = true;
    try {
      if (await ackShown(teamId)) { setAcked(true); setAckErr(null); }
    } catch (e) {
      setAckErr(`"Okudum" iletilemedi: ${e}`);
    } finally {
      acking.current = false;
    }
  };

  const due = ackDue({ visible, focused, acked, hasData: entry?.status === "ready" });
  useEffect(() => {
    if (!due) return;
    const t = setTimeout(() => void ack(), ACK_MS);
    return () => clearTimeout(t);
    // `ack` reads refs/state at fire time; the timer restarts only when the conditions flip.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [due, teamId]);

  if (!entry || entry.status === "loading") {
    return <div className="catchup"><p className="catchup-state" role="status">Sen yokken olanlar okunuyor…</p></div>;
  }
  if (entry.status === "unsupported") {
    return <div className="catchup"><p className="catchup-state" role="status">Bu sunucu "Sen yokken" özetini henüz desteklemiyor.</p></div>;
  }
  if (entry.status === "error" || !entry.data) {
    return (
      <div className="catchup">
        <p className="catchup-state" role="alert">Özet okunamadı: {entry.error}</p>
        <button className="catchup-btn" onClick={() => retryCatchup(teamId)}>Yeniden dene</button>
      </div>
    );
  }

  const data = entry.data;
  const empty = isEmpty(data);
  return (
    <div className="catchup">
      {empty
        ? <p className="catchup-state" role="status">Sen yokken yeni bir şey olmadı.</p>
        : sections(data).map(s => <CatchupSection key={s.key} teamId={teamId} section={s} now={data.generatedAt} />)}
      {entry.error && <p className="catchup-err" role="alert">Yenileme başarısız, liste eski olabilir: {entry.error}</p>}
      {ackErr && <p className="catchup-err" role="alert">{ackErr}</p>}
      <div className="catchup-foot">
        <button className="catchup-btn" disabled={acked} onClick={() => void ack()} aria-disabled={acked || undefined}>
          {acked ? <><Check /> Okundu</> : "Okudum"}
        </button>
        {acked && <span className="catchup-hint" role="status">Bir sonraki açılışta yalnız yeniler gelir.</span>}
      </div>
    </div>
  );
}

function CatchupSection({ teamId, section, now }: { teamId: string; section: Section; now: number }) {
  const uid = useId();
  const [expanded, setExpanded] = useState(false);
  const { shown, hidden } = visibleItems(section.bucket, expanded);
  // "Seni bekleyenler" is state-based: its heading stays even at zero. The other two hide when empty.
  if (section.key !== "waiting" && !shown.length && !hidden) return null;
  return (
    <section className="catchup-sec" aria-labelledby={`${uid}-h`}>
      <h3 id={`${uid}-h`} className="catchup-sec-head">{section.title}</h3>
      {shown.length ? (
        <ul className="catchup-list">
          {shown.map(item => (
            <li key={item.id}>
              <button className="catchup-row" onClick={() => go(teamId, item)}
                aria-label={`${kindLabel(item.kind)}: ${item.title} — kaynağını aç`}>
                <span className={`catchup-kind k-${section.key}`}>{kindLabel(item.kind)}</span>
                <span className="catchup-title">{item.title}</span>
                <time className="catchup-at" dateTime={new Date(item.at).toISOString()} title={new Date(item.at).toLocaleString()}>
                  {whenAgo(item.at, now)}
                </time>
              </button>
            </li>
          ))}
        </ul>
      ) : <p className="catchup-none">Bekleyen bir şey yok.</p>}
      {hidden > 0 && (expanded
        ? <p className="catchup-more-note">+{hidden} daha (listede yok)</p>
        : <button className="catchup-more" onClick={() => setExpanded(true)} aria-expanded={false}>+{hidden} daha</button>)}
    </section>
  );
}
