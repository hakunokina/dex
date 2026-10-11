// DEX-41: the patron's notes — one Markdown body per scope ("shared" or a team id), kept by the core
// (`note_get` / `note_set`, `notes-changed`) so a remote client sees the same text. Never an agent's input.
// A plain textarea (native undo/redo, no dependency) over a colour-only highlight layer: the layer keeps
// the textarea's exact font metrics, so headings/lists are tinted, never re-sized.
import { useEffect, useRef } from "react";
import { callHost, qid, splitId } from "../api/hosts";
import { kept, listenKept } from "../hot";
import "./notes.css";

export interface Note {
  scope: string;
  body: string;
  updatedAt: number;
}

const hot = import.meta.hot;
/** Mounted editors by scope — the shared note is open once per team workspace. */
const editors = kept(hot, "notes:editors", () => new Map<string, Set<(n: Note) => void>>());

// S4: a team's note is on the team's host (scope = its qualified id); "shared" is the local core's.
listenKept<Note>(hot, "notes-changed", ({ payload: n }, host) => editors.get(qid(host, n.scope))?.forEach(f => f(n)));

const SAVE_MS = 500;

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

function line(l: string): string {
  if (/^#{1,6}\s/.test(l)) return `<span class="md-h">${esc(l)}</span>`;
  const q = /^(\s*>\s?)(.*)$/.exec(l);
  if (q) return `<span class="md-mark">${esc(q[1])}</span><span class="md-q">${inline(q[2])}</span>`;
  const li = /^(\s*(?:[-*+]|\d+[.)])\s(?:\[[ xX]\]\s)?)(.*)$/.exec(l);
  if (li) return `<span class="md-mark">${esc(li[1])}</span>${inline(li[2])}`;
  return inline(l);
}

function inline(s: string): string {
  return esc(s).replace(/`[^`]+`/g, m => `<span class="md-code">${m}</span>`);
}

/** Trailing "\n " keeps the layer as tall as the textarea when the text ends in a newline. */
const highlight = (text: string) => text.split("\n").map(line).join("\n") + "\n ";

export function NotesEditor({ scope, placeholder }: { scope: string; placeholder: string }) {
  const ta = useRef<HTMLTextAreaElement>(null);
  const layer = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ta.current!;
    let alive = true;
    /** Newest `updatedAt` this editor holds; older echoes (own or remote) are ignored. */
    let seen = -1;
    /** Text typed here that the core hasn't confirmed yet — a remote write never overwrites it. */
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const paint = () => {
      layer.current!.innerHTML = highlight(el.value);
      layer.current!.scrollTop = el.scrollTop;
    };
    const apply = (n: Note) => {
      if (!alive || dirty || n.updatedAt <= seen) return;
      seen = n.updatedAt;
      if (n.body === el.value) return;
      const { selectionStart: a, selectionEnd: b } = el;
      el.value = n.body; // replaces the undo history; only happens while nothing local is pending
      if (document.activeElement === el) el.setSelectionRange(a, b);
      paint();
    };
    const [host, bare] = splitId(scope);
    const save = () => {
      clearTimeout(timer);
      timer = undefined;
      if (!dirty) return;
      const body = el.value;
      callHost<Note>(host, "note_set", { scope: bare, body })
        .then(n => {
          seen = Math.max(seen, n.updatedAt);
          if (el.value === body && !timer) dirty = false;
        })
        .catch(e => console.warn("not kaydedilemedi:", e));
    };
    const onInput = () => {
      dirty = true;
      paint();
      clearTimeout(timer);
      timer = setTimeout(save, SAVE_MS);
    };
    const onScroll = () => (layer.current!.scrollTop = el.scrollTop);

    let subs = editors.get(scope);
    if (!subs) editors.set(scope, (subs = new Set()));
    subs.add(apply);
    el.addEventListener("input", onInput);
    el.addEventListener("scroll", onScroll);
    el.addEventListener("blur", save);
    window.addEventListener("pagehide", save);
    callHost<Note>(host, "note_get", { scope: bare }).then(apply, e => console.warn("not okunamadı:", e));
    paint();
    return () => {
      save();
      alive = false;
      subs.delete(apply);
      el.removeEventListener("input", onInput);
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("blur", save);
      window.removeEventListener("pagehide", save);
    };
  }, [scope]);

  return (
    <div className="notes">
      <div className="notes-layer" ref={layer} aria-hidden />
      <textarea className="notes-editor" ref={ta} spellCheck={false} placeholder={placeholder} aria-label={placeholder} />
    </div>
  );
}
