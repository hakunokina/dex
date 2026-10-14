// Bu cihaz › Genel › Ekran alıntısı: global kısayol (src-tauri/src/snip_hotkey.rs).
// Yalnız bu bilgisayarın ayarı; her zaman görünür (LOCAL çağrısı her yerden çalışır).
import { useEffect, useState, type KeyboardEvent } from "react";
import { callHost, LOCAL } from "../api/hosts";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { FieldMsg } from "./kit/states";

interface HotkeyView { hotkey: string; active: boolean; error: string | null; }

/** Basılan kombinasyon → "Ctrl+Alt+Shift+S" (global-hotkey'in ayrıştırdığı biçim); yalnız değiştirici → null. */
function combo(e: KeyboardEvent): string | null {
  if (["Control", "Alt", "Shift", "Meta", "AltGraph"].includes(e.key)) return null;
  const key = e.code.replace(/^Key/, "").replace(/^Digit/, "");
  return [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super", key].filter(Boolean).join("+");
}

export function SnipSection() {
  const [view, setView] = useState<HotkeyView | null>(null);
  const [draft, setDraft] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const fill = (v: HotkeyView) => { setView(v); setDraft(v.hotkey); };
  useEffect(() => {
    callHost<HotkeyView>(LOCAL, "snip_hotkey_get").then(fill, (e) => setMsg({ ok: false, text: String(e) }));
  }, []);
  const dirty = !!view && draft !== view.hotkey;

  async function save() {
    try {
      fill(await callHost<HotkeyView>(LOCAL, "snip_hotkey_set", { hotkey: draft }));
      setMsg({ ok: true, text: "Kaydedildi." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  return (
    <Section label="Ekran alıntısı">
      <Row
        id="snip-shortcut"
        label="Kısayol"
        description="Bölge seçer, PNG'yi panoya kopyalar. Başka uygulamanın kısayolunu ezebilir."
        control={
          <span className="set-field">
            <input
              aria-label="Ekran alıntısı kısayolu"
              value={draft}
              readOnly
              title="Kutuya tıklayıp yeni tuşlara basın"
              onKeyDown={(e) => { const c = combo(e); if (c) { e.preventDefault(); setDraft(c); } }}
            />
            {dirty && (
              <span className="set-field-btns">
                <button type="button" className="primary" onClick={() => void save()}>Kaydet</button>
                <button type="button" onClick={() => view && setDraft(view.hotkey)}>Vazgeç</button>
              </span>
            )}
          </span>
        }
        feedback={<>
          {view && !view.active && view.error && <FieldMsg ok={false} text={view.error} />}
          {msg && <FieldMsg ok={msg.ok} text={msg.text} />}
        </>}
      />
    </Section>
  );
}
