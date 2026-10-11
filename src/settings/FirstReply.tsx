// FIX-4: "İlk yanıt bekçisi" (FIX-3's first-reply watchdog) — seconds per provider before a turn
// with no event yet is marked "yanıt gecikiyor"; 0 = off. Stored by dex-core (`first_reply_set`).
import { useEffect, useState } from "react";
import { callHost } from "../api/hosts";
import { listProviders } from "../team/api";

type Secs = Record<string, number>;

export function FirstReplySection({ host }: { host: string }) {
  const [labels, setLabels] = useState<Record<string, string>>({});
  const [saved, setSaved] = useState<Secs | null>(null);
  const [form, setForm] = useState<Record<string, string>>({});
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const fill = (v: Secs) => {
    setSaved(v);
    setForm(Object.fromEntries(Object.entries(v).map(([k, s]) => [k, String(s)])));
  };
  useEffect(() => {
    callHost<Secs>(host, "first_reply_get").then(fill, (e) => setMsg({ ok: false, text: String(e) }));
    listProviders(host).then((ps) => setLabels(Object.fromEntries(ps.map((p) => [p.id, p.label]))), () => {});
  }, [host]);

  async function save() {
    if (!saved) return;
    const changed = Object.keys(saved).filter((k) => Math.round(Number(form[k]) || 0) !== saved[k]);
    try {
      let v = saved;
      for (const provider of changed) v = await callHost<Secs>(host, "first_reply_set", { provider, secs: Math.round(Number(form[provider]) || 0) });
      fill(v);
      setMsg({ ok: true, text: changed.length ? "Kaydedildi. Yeni eşik bir sonraki turdan itibaren geçerli." : "Değişiklik yok." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
      callHost<Secs>(host, "first_reply_get").then(fill, () => {});
    }
  }

  return (
    <details className="settings-section settings-fold" aria-label="İlk yanıt bekçisi">
      <summary>
        <b>Yanıt gecikme uyarısı</b>
        <span>Ajan mesajdan sonra bu kadar saniye sessiz kalırsa "yanıt gecikiyor" işareti çıkar (yeniden dene / iptal).</span>
      </summary>
      {saved &&
        Object.keys(saved).map((id) => (
          <label className="settings-row" key={id}>
            <span>{labels[id] ?? id}</span>
            <input type="number" min={0} max={3600} value={form[id] ?? ""} onChange={(e) => setForm({ ...form, [id]: e.target.value })} />
            <em>sn · 0 = kapalı, aksi hâlde 10–3600</em>
          </label>
        ))}
      <div className="settings-actions">
        <button className="primary" disabled={!saved} onClick={() => void save()}>Kaydet</button>
      </div>
      {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
    </details>
  );
}
