// #2040: the team note page — the patron's note plus the leader's two appends (system prompt, every
// message) and the door to Dex's own prompt parts. The appends save like the note (debounced, on blur,
// on leaving the page); a change the leader only picks up on restart shows the amber band.
import { lazy, Suspense, useEffect, useId, useRef, useState } from "react";
import { ScrollText } from "lucide-react";
import { callHost, hostSupports, splitId, useHosts } from "../api/hosts";
import { useTeams } from "../team/store";
import { NotesEditor } from "./Notes";
import { setLeaderPrompt, useLeaderPrompt } from "./noteMarks";
import { openAgent } from "./panels";
import type { AppendField, LeaderPrompt } from "./notePrompts";
import "./notes.css";

const PromptPartsDialog = lazy(() => import("./PromptParts").then(m => ({ default: m.PromptPartsDialog })));

const SAVE_MS = 500;

export function TeamNotes({ teamId, placeholder }: { teamId: string; placeholder: string }) {
  const host = splitId(teamId)[0];
  useHosts(); // a host's hello (its methods) arrives after the first render
  const lp = useLeaderPrompt(teamId);
  const leader = useTeams().agents.find(a => a.teamId === teamId && a.kind === "leader");
  const [partsOpen, setPartsOpen] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const appends = hostSupports(host, "leader_prompt_get");
  const parts = hostSupports(host, "prompt_parts");

  return (
    <div className="team-notes">
      {lp?.pendingRestart && (
        <div className="pane-alert notes-band" role="status">
          <span>⚠ Lider oturumu yeniden başlamadan değişiklikler uygulanmaz.</span>
          {leader && (
            <button className="pane-act" onClick={() => openAgent(teamId, leader.id)}
              title="Lider panelini açar; oradaki “Temiz oturum” lideri yeni istemle yeniden başlatır">
              Lider paneline git
            </button>
          )}
        </div>
      )}
      <NotesEditor scope={teamId} placeholder={placeholder} />
      {(appends || parts) && (
        <section className="notes-appends" aria-label="Lider eklemeleri">
          <div className="notes-appends-head">
            <h3>Lider eklemeleri</h3>
            {parts && (
              <button ref={opener} className="notes-btn" onClick={() => setPartsOpen(true)} aria-haspopup="dialog"
                title="Dex'in yerleşik istem parçalarını oku ve düzenle">
                <ScrollText /> Dex istemleri
              </button>
            )}
          </div>
          {appends && (lp ? <>
            <AppendBox key={`${teamId}:s`} teamId={teamId} lp={lp} field="systemAppend" label="Lider sistem istemine ekle"
              hint="Liderin sistem isteminin sonuna eklenir; lider yeniden başlayınca geçerli olur." />
            <AppendBox key={`${teamId}:m`} teamId={teamId} lp={lp} field="messageAppend" label="Her mesaja ekle"
              hint="Lidere yazdığın her mesaja bağlam olarak eklenir; hemen geçerli." />
          </> : <p className="notes-hint" role="status">Eklemeler okunuyor…</p>)}
        </section>
      )}
      {partsOpen && (
        <Suspense fallback={null}>
          <PromptPartsDialog host={host} onClose={() => {
            setPartsOpen(false);
            opener.current?.focus();
          }} />
        </Suspense>
      )}
    </div>
  );
}

function AppendBox({ teamId, lp, field, label, hint }: {
  teamId: string; lp: LeaderPrompt; field: AppendField; label: string; hint: string;
}) {
  const id = useId();
  const [text, setText] = useState(lp[field]);
  const [err, setErr] = useState<string | null>(null);
  /** Typed text the core hasn't confirmed: a remote write never overwrites it. */
  const pending = useRef({ dirty: false, text: lp[field], timer: undefined as ReturnType<typeof setTimeout> | undefined });
  const server = lp[field];
  useEffect(() => {
    if (pending.current.dirty) return;
    pending.current.text = server;
    setText(server);
  }, [server]);

  const save = () => {
    const p = pending.current;
    clearTimeout(p.timer);
    p.timer = undefined;
    if (!p.dirty) return;
    const value = p.text;
    const [host, bare] = splitId(teamId);
    callHost<LeaderPrompt>(host, "leader_prompt_set", { teamId: bare, [field]: value })
      .then(next => {
        if (p.text === value && !p.timer) p.dirty = false;
        setErr(null);
        setLeaderPrompt(teamId, next);
      })
      .catch(e => setErr(`Kaydedilemedi: ${e}`));
  };
  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const flush = () => saveRef.current();
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, []);

  return (
    <div className="notes-append">
      <label htmlFor={id}>{label}</label>
      <textarea id={id} rows={3} spellCheck={false} value={text} aria-describedby={`${id}-hint${err ? ` ${id}-err` : ""}`}
        aria-invalid={err ? true : undefined}
        onChange={e => {
          const p = pending.current;
          p.dirty = true;
          p.text = e.target.value;
          setText(e.target.value);
          clearTimeout(p.timer);
          p.timer = setTimeout(() => saveRef.current(), SAVE_MS);
        }}
        onBlur={() => saveRef.current()} />
      <p id={`${id}-hint`} className="notes-hint">{hint}</p>
      {err && <p id={`${id}-err`} className="notes-err" role="alert">{err}</p>}
    </div>
  );
}
