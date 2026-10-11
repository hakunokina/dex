// #34: ekran alıntısının global kısayolu (src-tauri/src/snip_hotkey.rs). Yalnız bu bilgisayarın
// ayarı; kayıt başarısızsa (kombinasyon başka uygulamada) hata burada ve OS bildiriminde görünür.
import { useEffect, useState, type KeyboardEvent } from "react";
import { callHost, LOCAL } from "../api/hosts";

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

  async function save() {
    try {
      fill(await callHost<HotkeyView>(LOCAL, "snip_hotkey_set", { hotkey: draft }));
      setMsg({ ok: true, text: "Kaydedildi." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  return <section className="settings-section" aria-label="Ekran alıntısı">
    <h4>Ekran alıntısı</h4>
    <label className="settings-row settings-row-wide">
      <span>Kısayol</span>
      <input value={draft} readOnly title="Kutuya tıklayıp yeni tuşlara basın" onKeyDown={(e) => { const c = combo(e); if (c) { e.preventDefault(); setDraft(c); } }} />
    </label>
    <p className="settings-note">Her uygulamada çalışır: bölge seçin, PNG Resimler\Screenshots'a kaydedilir ve yolu panoya kopyalanır. Varsayılan Ctrl+Shift+S, Dex açıkken diğer uygulamaların "Farklı kaydet"ini ezer; çakışırsa kutuya tıklayıp yeni tuşlara basın.</p>
    <div className="settings-actions">
      <button className="primary" disabled={!view || (draft === view.hotkey && view.active)} onClick={() => void save()}>Kaydet</button>
    </div>
    {view && !view.active && view.error && <p className="settings-err" role="alert">{view.error}</p>}
    {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
  </section>;
}
