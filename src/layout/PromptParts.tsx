// #2040: Dex's built-in prompt parts on one host — read every part, edit the editable ones (an
// override, global to that host), "Varsayılana dön" drops it. Dynamic parts are read-only templates.
// Policy (permits, hook denials) is in code: the dialog says so, the text can't lift it.
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Lock, X } from "lucide-react";
import { callHost, onEvent } from "../api/hosts";
import { audienceLabel, channelLabel, replacePart, type PromptPart, type PromptParts } from "./notePrompts";

type Load = { phase: "loading" } | { phase: "error"; text: string } | { phase: "ready"; data: PromptParts };

export function PromptPartsDialog({ host, onClose }: { host: string; onClose: () => void }) {
  const [load, setLoad] = useState<Load>({ phase: "loading" });
  const [sel, setSel] = useState<string | null>(null);
  /** Unsaved text per part; switching parts keeps it. */
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const textRef = useRef<HTMLTextAreaElement>(null);
  const keepRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();

  const fetchParts = () => {
    setLoad({ phase: "loading" });
    callHost<PromptParts>(host, "prompt_parts", {}).then(
      data => {
        setLoad({ phase: "ready", data });
        setSel(s => s ?? data.parts[0]?.id ?? null);
      },
      e => setLoad({ phase: "error", text: `İstemler okunamadı: ${e}` }),
    );
  };
  useEffect(fetchParts, [host]);
  useEffect(() => onEvent<PromptPart>("prompt-parts-changed", (e, h) => {
    if (h !== host) return;
    setLoad(l => (l.phase === "ready" ? { phase: "ready", data: { ...l.data, parts: replacePart(l.data.parts, e.payload) } } : l));
  }), [host]);
  useEffect(() => box.current?.focus(), []);

  const dirtyCount = Object.keys(drafts).length;
  const close = () => {
    if (dirtyCount && !confirm(`${dirtyCount} parçada kaydedilmemiş değişiklik var. Yine de kapatılsın mı?`)) return;
    onClose();
  };
  const dropDraft = (id: string) => setDrafts(({ [id]: _, ...rest }) => rest);
  const done = (next: PromptPart) => {
    setLoad(l => (l.phase === "ready" ? { phase: "ready", data: { ...l.data, parts: replacePart(l.data.parts, next) } } : l));
    dropDraft(next.id);
  };
  const act = (method: "prompt_part_set" | "prompt_part_reset", params: Record<string, unknown>) => {
    setBusy(true);
    setErr(null);
    callHost<PromptPart>(host, method, params)
      .then(done, e => setErr(`${method === "prompt_part_set" ? "Kaydedilemedi" : "Varsayılana dönülemedi"}: ${e}`))
      .finally(() => {
        setBusy(false);
        endConfirm();
      });
  };
  // The confirm row replaces the buttons: focus follows it there and back, never drops to the page.
  const startConfirm = () => {
    setConfirmReset(true);
    setTimeout(() => keepRef.current?.focus());
  };
  const endConfirm = () => {
    setConfirmReset(false);
    setTimeout(() => textRef.current?.focus());
  };

  // Esc closes (from anywhere: a click may have left focus on the page), backing out of a confirm first.
  const onEsc = useRef<() => void>(() => {});
  onEsc.current = () => (confirmReset ? endConfirm() : close());
  useEffect(() => {
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onEsc.current();
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, []);

  // Tab stays inside the dialog.
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Tab" || !box.current) return;
    const f = [...box.current.querySelectorAll<HTMLElement>("button:not(:disabled), textarea, [tabindex='0']")];
    if (!f.length) return;
    const [first, last] = [f[0], f[f.length - 1]];
    if (e.shiftKey && (document.activeElement === first || document.activeElement === box.current)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  const parts = load.phase === "ready" ? load.data.parts : [];
  const part = parts.find(p => p.id === sel) ?? null;
  const draft = part ? drafts[part.id] : undefined;
  const shown = draft ?? part?.text ?? "";

  // Portal: a floating dock group is transformed, which would trap a fixed overlay inside it.
  return createPortal(
    <div className="pp-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) close(); }}>
      <div className="pp" ref={box} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1} onKeyDown={onKey}>
        <header className="pp-head">
          <h2 id={titleId}>Dex istemleri</h2>
          <button className="icon-btn" onClick={close} aria-label="Kapat" title="Kapat (Esc)"><X /></button>
        </header>
        {load.phase === "ready" && <p className="pp-policy">{load.data.policyNote}</p>}
        {load.phase === "loading" && <p className="pp-state" role="status">Yükleniyor…</p>}
        {load.phase === "error" && (
          <div className="pp-state pane-alert" role="alert">
            <span>{load.text}</span>
            <button className="pane-act" onClick={fetchParts}>Yeniden dene</button>
          </div>
        )}
        {load.phase === "ready" && !parts.length && <p className="pp-state">Bu sunucu istem parçası bildirmedi.</p>}
        {load.phase === "ready" && parts.length > 0 && (
          <div className="pp-body">
            <ul className="pp-list" aria-label="İstem parçaları">
              {parts.map(p => (
                <li key={p.id}>
                  <button className={`pp-item${p.id === sel ? " on" : ""}`} aria-current={p.id === sel ? "true" : undefined}
                    onClick={() => { setSel(p.id); setErr(null); setConfirmReset(false); }}>
                    <span className="pp-item-title">
                      {p.title}
                      {p.id in drafts && <span className="pp-dot" title="Kaydedilmemiş değişiklik" aria-label="kaydedilmemiş"> •</span>}
                    </span>
                    <span className="pp-tags">
                      <span className="pp-tag">{audienceLabel(p)}</span>
                      {p.overridden && <span className="pp-tag pp-tag-on">değiştirildi</span>}
                      {!p.editable && <span className="pp-tag" title="Salt okunur"><Lock aria-label="salt okunur" /></span>}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {part && (
              <div className="pp-detail">
                <div className="pp-meta">
                  <h3>{part.title}</h3>
                  <span>Kime: {audienceLabel(part)} · {channelLabel(part.channel)}{part.overridden ? " · varsayılandan değiştirildi" : ""}{part.editable ? "" : " · salt okunur"}</span>
                  {part.note && <p className="pp-note">{part.note}</p>}
                </div>
                <textarea ref={textRef} className="pp-text" value={shown} readOnly={!part.editable} spellCheck={false}
                  aria-label={`${part.title} metni${part.editable ? "" : " (salt okunur)"}`}
                  onChange={e => {
                    const v = e.target.value;
                    if (v === part.text) dropDraft(part.id);
                    else setDrafts(d => ({ ...d, [part.id]: v }));
                  }} />
                {err && <p className="notes-err" role="alert">{err}</p>}
                {part.editable && (
                  <div className="pp-actions">
                    {confirmReset ? (
                      <>
                        <span className="pp-confirm">Değiştirilmiş metin silinip Dex'in varsayılanı geçerli olacak.</span>
                        <button ref={keepRef} className="notes-btn" onClick={endConfirm} disabled={busy}>Vazgeç</button>
                        <button className="notes-btn danger" onClick={() => act("prompt_part_reset", { id: part.id })} disabled={busy}>
                          {busy ? "Çalışıyor…" : "Evet, varsayılana dön"}
                        </button>
                      </>
                    ) : (
                      <>
                        {part.overridden && (
                          <button className="notes-btn" onClick={startConfirm} disabled={busy}>Varsayılana dön</button>
                        )}
                        {draft !== undefined && (
                          <button className="notes-btn" onClick={() => dropDraft(part.id)} disabled={busy}>Değişikliği at</button>
                        )}
                        <button className="notes-btn primary" disabled={busy || draft === undefined || !draft.trim()}
                          title={draft !== undefined && !draft.trim() ? "Boş metin kaydedilmez; varsayılan için “Varsayılana dön”" : undefined}
                          onClick={() => act("prompt_part_set", { id: part.id, text: draft })}>
                          {busy ? "Kaydediliyor…" : "Kaydet"}
                        </button>
                      </>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
