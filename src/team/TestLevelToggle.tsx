import { useId, useRef, useState } from "react";
import type { TestLevel } from "./api";
import { nextTestLevel } from "./testLevel";

export function TestLevelToggle({ level, onChange }: {
  level: TestLevel | undefined;
  onChange: (level: TestLevel) => Promise<unknown>;
}) {
  const hintId = useId();
  const pending = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The record itself is the capability check: older hosts omit testLevel.
  const nextLevel = nextTestLevel(level);
  if (nextLevel === null) return null;

  async function toggle() {
    if (pending.current || nextLevel === null) return;
    pending.current = true;
    setBusy(true); setError(null);
    try {
      await onChange(nextLevel);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }

  return (
    <div className="team-test-level" aria-busy={busy}>
      <button type="button" role="switch" aria-label="Tam testler" aria-checked={level === "full"}
        aria-describedby={hintId} aria-disabled={busy} onClick={() => void toggle()}>
        <span>Test: {level === "full" ? "Tam" : "Hafif"}</span>
        <span id={hintId} className="team-test-hint">Tam: testler dahil. Hafif: yalnız derleme + tip kontrolü. Yeni oturumlarda geçerli.</span>
      </button>
      {busy && <span className="team-note" role="status">Kaydediliyor…</span>}
      {error && <p className="team-err" role="alert">{error}</p>}
    </div>
  );
}
