// Ortak kit: yıkıcı işlem onayı. Uygulama içi pencere sonuçları listeler (lines) ya da paragraf
// yazar (body), düğmede fiil yazar; yıkıcıda (varsayılan) odak Vazgeç'te, değilse birincilde.
// Enter odaktaki düğmeyi çalıştırır, Esc ve dışarı tıklama iptaldir, Tab pencereden çıkmaz. Native confirm() yok;
// <dialog>.showModal() üst katmana çıkar: dockview'ün yüzen panelleri (z 1000+) ve açık overlay'ler altta kalır.
import { useEffect, useId, useRef } from "react";
import "./dialog.css";

export function ConfirmDialog({ title, lines, body, confirmLabel, cancelLabel = "Vazgeç", danger = true, busy, onConfirm, onCancel }: {
  title: string;
  lines?: string[];
  body?: string[];
  confirmLabel: string;
  cancelLabel?: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const id = useId();
  const boxRef = useRef<HTMLDialogElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const d = boxRef.current;
    if (d && !d.open) d.showModal();
    (danger ? cancelRef : confirmRef).current?.focus();
    return () => d?.close();
  }, []);
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); e.preventDefault(); onCancel(); return; }
      if (e.key !== "Tab") return;
      const buttons = [...(boxRef.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
      if (!buttons.length) { e.preventDefault(); return; }
      const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = at < 0 ? 0 : (at + (e.shiftKey ? -1 : 1) + buttons.length) % buttons.length;
      e.preventDefault();
      e.stopPropagation();
      buttons[next].focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);
  const described = lines?.length || body?.length ? `${id}-d` : undefined;
  return (
    <dialog ref={boxRef} className="set-dialog-backdrop" role="alertdialog" aria-modal="true" aria-labelledby={`${id}-t`} aria-describedby={described}
      // Esc is the keydown handler's (an Esc that opened this dialog must not also close it).
      onCancel={(e) => e.preventDefault()}
      onMouseDown={(e) => { if (e.target === e.currentTarget) onCancel(); }}>
      <div className="set-dialog">
        <h3 id={`${id}-t`}>{title}</h3>
        {lines?.length ? (
          <ul id={described}>
            {lines.map((l, i) => <li key={i}>{l}</li>)}
          </ul>
        ) : body?.length ? (
          <div id={described} className="set-dialog-body">
            {body.map((p, i) => <p key={i}>{p}</p>)}
          </div>
        ) : null}
        <div className="set-dialog-actions">
          <button type="button" ref={cancelRef} disabled={busy} onClick={onCancel}>{cancelLabel}</button>
          <button type="button" ref={confirmRef} className={danger ? "danger" : "primary"} disabled={busy} onClick={onConfirm}>
            {busy ? "Çalışıyor…" : confirmLabel}
          </button>
        </div>
      </div>
    </dialog>
  );
}
