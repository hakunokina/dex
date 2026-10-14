// Ortak kit: satır içi kaydetli metin/sayı alanı. Değer değişince satırda
// Kaydet / Vazgeç belirir; Enter kaydeder, Esc son kayıtlı değere döner.
import { useEffect, useState, type KeyboardEvent } from "react";

export function TextField({ label, value, onSave, type = "text", unit, placeholder, disabled, min, max, inputWidth }: {
  label: string;
  value: string;
  onSave: (next: string) => void | Promise<void>;
  type?: "text" | "number" | "password";
  unit?: string;
  placeholder?: string;
  disabled?: boolean;
  min?: number;
  max?: number;
  inputWidth?: number;
}) {
  const [draft, setDraft] = useState(value);
  const [busy, setBusy] = useState(false);
  useEffect(() => { setDraft(value); }, [value]);
  const dirty = draft !== value;
  const cancel = () => setDraft(value);
  const save = async () => {
    if (!dirty || busy) return;
    setBusy(true);
    try { await onSave(draft); } finally { setBusy(false); }
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") { e.preventDefault(); void save(); }
    else if (e.key === "Escape" && dirty) { e.preventDefault(); cancel(); }
  };
  return (
    <span className="set-field">
      <input
        type={type}
        aria-label={label}
        value={draft}
        min={min}
        max={max}
        placeholder={placeholder}
        disabled={disabled || busy}
        style={inputWidth ? { width: inputWidth } : undefined}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKey}
        onBlur={() => { if (!dirty) setDraft(value); }}
      />
      {unit && <span className="set-unit">{unit}</span>}
      {dirty && (
        <span className="set-field-btns">
          <button type="button" className="primary" disabled={busy} onClick={() => void save()}>Kaydet</button>
          <button type="button" disabled={busy} onClick={cancel}>Vazgeç</button>
        </span>
      )}
    </span>
  );
}
