// #2464: in a team with several open projects, the leader's new or renewed session is placed in a
// project the patron picks here (not silently the team default). Dex's own dialog, like askConfirm:
// it renders into its own root, Enter runs the focused button (on a project: picks it), Esc and a click outside cancel, Tab
// stays inside; the arrows move the radio choice (native group).
import { createElement, useEffect, useId, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { Project } from "./model";
import "../settings/kit/dialog.css";
import "./project.css";

export interface LeaderProjectAsk {
  title: string;
  body: string[];
  confirmLabel: string;
  /** Open projects; a folderless one cannot bind a session (shown disabled). */
  projects: Project[];
  initial: string | null;
}

/** The picked project's id, or null when the patron cancelled. */
export function askLeaderProject(ask: LeaderProjectAsk): Promise<string | null> {
  const back = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  return new Promise((resolve) => {
    let done = false;
    const close = (id: string | null) => {
      if (done) return;
      done = true;
      root.unmount();
      host.remove();
      if (back?.isConnected) back.focus();
      resolve(id);
    };
    root.render(createElement(LeaderProjectDialog, { ...ask, onDone: close }));
  });
}

function LeaderProjectDialog({ title, body, confirmLabel, projects, initial, onDone }: LeaderProjectAsk & { onDone: (id: string | null) => void }) {
  const id = useId();
  const boxRef = useRef<HTMLDialogElement>(null);
  const usable = projects.filter((p) => p.rootPath);
  const [pick, setPick] = useState(usable.find((p) => p.id === initial)?.id ?? usable[0]?.id ?? null);
  useEffect(() => {
    const d = boxRef.current;
    if (d && !d.open) d.showModal();
    d?.querySelector<HTMLInputElement>("input:checked")?.focus();
    return () => d?.close();
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); e.preventDefault(); onDone(null); return; }
      if (e.key !== "Tab") return;
      const stops = [...(boxRef.current?.querySelectorAll<HTMLElement>("input:checked, button:not(:disabled)") ?? [])];
      if (!stops.length) { e.preventDefault(); return; }
      const at = stops.indexOf(document.activeElement as HTMLElement);
      const next = at < 0 ? 0 : (at + (e.shiftKey ? -1 : 1) + stops.length) % stops.length;
      e.preventDefault();
      e.stopPropagation();
      stops[next].focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onDone]);
  return (
    <dialog ref={boxRef} className="set-dialog-backdrop" role="alertdialog" aria-modal="true" aria-labelledby={`${id}-t`} aria-describedby={`${id}-d`}
      onCancel={(e) => e.preventDefault()}
      onMouseDown={(e) => { if (e.target === e.currentTarget) onDone(null); }}>
      <div className="set-dialog">
        <h3 id={`${id}-t`}>{title}</h3>
        <div id={`${id}-d`} className="set-dialog-body">
          {body.map((p, i) => <p key={i}>{p}</p>)}
        </div>
        <fieldset className="leader-project-list">
          <legend>Proje</legend>
          {projects.map((p) => (
            <label key={p.id} className="leader-project-opt" title={p.rootPath ?? "klasör bağlı değil"}>
              <input type="radio" name={`${id}-p`} value={p.id} checked={pick === p.id} disabled={!p.rootPath}
                onChange={() => setPick(p.id)}
                onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); onDone(p.id); } }} />
              <span>{p.name}</span>
              {!p.rootPath && <small>klasör yok</small>}
            </label>
          ))}
        </fieldset>
        <div className="set-dialog-actions">
          <button type="button" onClick={() => onDone(null)}>Vazgeç</button>
          <button type="button" className="primary" disabled={!pick} onClick={() => onDone(pick)}>{confirmLabel}</button>
        </div>
      </div>
    </dialog>
  );
}
