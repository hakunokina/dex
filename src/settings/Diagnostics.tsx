import { useState } from "react";
import { callHost, LOCAL } from "../api/hosts";
import type { CoreStatus } from "../api/status";

export function DiagnosticsSection({ host }: { host: string }) {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  async function run(copy: boolean) {
    setBusy(true);
    setMsg(null);
    try {
      if (copy) {
        const status = await callHost<CoreStatus>(host, "core_status");
        await navigator.clipboard.writeText(JSON.stringify(status, null, 2));
        setMsg({ ok: true, text: "Durum dökümü kopyalandı." });
      } else {
        await callHost(host, "logs_open");
      }
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-section" aria-label="Tanılama">
      <b>Tanılama</b>
      <div className="settings-actions">
        <button disabled={busy} onClick={() => void run(true)}>Durum dökümü (JSON kopyala)</button>
        {host === LOCAL && <button disabled={busy} onClick={() => void run(false)}>Log klasörünü aç</button>}
      </div>
      {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
    </section>
  );
}
