// Ortak kit: gizli değer — maskeli görünüm, Değiştirince alan + Kaydet/Vazgeç.
// Yarım yapıştırılan anahtar kaydedilmez; anahtar değiştirmek bir işlemdir.
import { useState } from "react";

export function SecretField({ label, hasSecret, mask, placeholder, onSave, onDelete }: {
  label: string;
  hasSecret: boolean;
  mask?: string | null;
  placeholder?: string;
  onSave: (next: string) => void | Promise<void>;
  onDelete?: () => void;
}) {
  const [edit, setEdit] = useState(false);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const open = () => { setEdit(true); setDraft(""); };
  const close = () => { setEdit(false); setDraft(""); };
  const save = async () => {
    if (!draft.trim() || busy) return;
    setBusy(true);
    try { await onSave(draft); close(); } finally { setBusy(false); }
  };
  if (!edit) {
    return (
      <span className="set-secret">
        <code>{hasSecret ? `•••• ${mask ?? ""}`.trim() : "yok"}</code>
        {!hasSecret && <span className="set-unit" role="status">⚠</span>}
        <button type="button" disabled={busy} onClick={open}>
          {hasSecret ? "Değiştir" : "Anahtar gir"}
        </button>
        {hasSecret && onDelete && <button type="button" disabled={busy} onClick={onDelete}>Sil</button>}
      </span>
    );
  }
  return (
    <span className="set-field">
      <input
        type="password"
        aria-label={label}
        autoComplete="off"
        spellCheck={false}
        autoFocus
        value={draft}
        placeholder={placeholder ?? "yapıştırın"}
        disabled={busy}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") { e.preventDefault(); void save(); }
          else if (e.key === "Escape") { e.preventDefault(); close(); }
        }}
      />
      <span className="set-field-btns">
        <button type="button" className="primary" disabled={busy || !draft.trim()} onClick={() => void save()}>Kaydet</button>
        <button type="button" disabled={busy} onClick={close}>Vazgeç</button>
      </span>
    </span>
  );
}
