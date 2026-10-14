// Ortak kit: yıkıcı işlem onayı. Uygulama içi pencere sonuçları listeler,
// düğmede fiil yazar, varsayılan odak Vazgeç'tedir. Native confirm() yok.
import { useEffect, useRef } from "react";

export function ConfirmDialog({ title, lines, confirmLabel, busy, onConfirm, onCancel }: {
  title: string;
  lines: string[];
  confirmLabel: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { cancelRef.current?.focus(); }, []);
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); onCancel(); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);
  return (
    <div className="set-dialog-backdrop" onMouseDown={(e) => { if (e.target === e.currentTarget) onCancel(); }}>
      <div className="set-dialog" role="alertdialog" aria-modal="true" aria-label={title}>
        <h3>{title}</h3>
        <ul>
          {lines.map((l, i) => <li key={i}>{l}</li>)}
        </ul>
        <div className="set-dialog-actions">
          <button type="button" ref={cancelRef} disabled={busy} onClick={onCancel}>Vazgeç</button>
          <button type="button" className="danger" disabled={busy} onClick={onConfirm}>
            {busy ? "Çalışıyor…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
