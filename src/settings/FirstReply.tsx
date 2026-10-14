// Yanıt gecikmesi uyarısı: sağlayıcı başına saniye (0 = kapalı).
// Sağlayıcı kartının içinde durur; alan bazlı Kaydet ile yazılır (DB `settings.first_reply_secs`).
import { useEffect, useState } from "react";
import { callHost } from "../api/hosts";
import { TextField } from "./kit/TextField";
import { FieldMsg, SavedFlash } from "./kit/states";

type Secs = Record<string, number>;

export function FirstReplyField({ host, provider, label }: { host: string; provider: string; label: string }) {
  const [saved, setSaved] = useState<number | null>(null);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [flash, setFlash] = useState(0);

  useEffect(() => {
    callHost<Secs>(host, "first_reply_get").then(
      (v) => setSaved(v[provider] ?? 0),
      (e) => setMsg({ ok: false, text: String(e) }),
    );
  }, [host, provider]);

  async function save(next: string) {
    const secs = Math.round(Number(next) || 0);
    try {
      const v = await callHost<Secs>(host, "first_reply_set", { provider, secs });
      setSaved(v[provider] ?? secs);
      setMsg(null);
      setFlash((n) => n + 1);
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  return (
    <span className="set-field">
      {saved === null && !msg ? (
        <span className="set-note" role="status">Yükleniyor…</span>
      ) : (
        <>
          <TextField
            label={`Yanıt gecikmesi uyarısı (${label})`}
            type="number"
            min={0}
            max={3600}
            unit="sn · 0 = kapalı"
            value={saved === null ? "" : String(saved)}
            onSave={(v) => void save(v)}
          />
          <SavedFlash stamp={flash} />
        </>
      )}
      {msg && <FieldMsg ok={msg.ok} text={msg.text} />}
    </span>
  );
}
