// Bu cihaz › Bağlantılar › Telefondan erişim (Tailscale): bu bilgisayarı yalnız
// Tailscale ağındaki cihazlara açar. Her zaman görünür (LOCAL çağrısı).
import { useEffect, useState } from "react";
import { callHost, LOCAL } from "../api/hosts";
import { Row, Pad } from "./kit/Row";
import { Section } from "./kit/Section";
import { Switch } from "./kit/Switch";
import { TextField } from "./kit/TextField";
import { FieldMsg, SavedFlash } from "./kit/states";

interface TailnetView {
  enabled: boolean;
  port: number;
  listening: string | null;
  error: string | null;
  firewallCommand: string;
}
interface Device { id: string; name: string; createdAt: number; lastSeen: number | null; }

export function TailnetSection() {
  const [view, setView] = useState<TailnetView | null>(null);
  const [code, setCode] = useState<string | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [saved, setSaved] = useState(0);
  const refresh = async () => {
    const next = await callHost<TailnetView>(LOCAL, "tailnet_get");
    setView(next);
    setDevices(next.listening ? await callHost<Device[]>(LOCAL, "tailnet_devices") : []);
  };
  useEffect(() => { void refresh().catch((e) => setMsg({ ok: false, text: String(e) })); }, []);
  async function toggle(enabled: boolean) {
    if (!view) return;
    try {
      const next = await callHost<TailnetView>(LOCAL, "tailnet_set", { enabled, port: view.port });
      setView(next); setCode(null); setMsg(null); setSaved((n) => n + 1);
      setDevices(next.listening ? await callHost<Device[]>(LOCAL, "tailnet_devices") : []);
    } catch (e) { setMsg({ ok: false, text: String(e) }); void refresh().catch(() => {}); }
  }
  async function savePort(port: string) {
    if (!view) return;
    try {
      const next = await callHost<TailnetView>(LOCAL, "tailnet_set", { enabled: view.enabled, port: Number(port) });
      setView(next); setMsg(null); setSaved((n) => n + 1);
    } catch (e) { setMsg({ ok: false, text: String(e) }); }
  }
  async function pair() {
    try { setCode(await callHost<string>(LOCAL, "tailnet_pair_code")); setMsg({ ok: true, text: "Kod 10 dakika geçerli ve tek kullanımlık." }); }
    catch (e) { setMsg({ ok: false, text: String(e) }); }
  }
  async function revoke(id: string) {
    try { await callHost(LOCAL, "tailnet_device_revoke", { id }); await refresh(); }
    catch (e) { setMsg({ ok: false, text: String(e) }); }
  }
  return (
    <Section
      label="Telefondan erişim"
      description="Bu bilgisayarı yalnız Tailscale ağınızdaki cihazlara açar; yerel ağa ve internete açılmaz."
    >
      <Row
        id="tailnet-access"
        label="Ağ erişimi"
        control={
          <>
            <Switch label="Ağ erişimi" checked={view?.enabled ?? false} disabled={!view} onChange={(on) => void toggle(on)} />
            <SavedFlash stamp={saved} />
          </>
        }
        feedback={<>
          {view?.listening && <p className="set-note" role="status">Dinliyor: <code>{view.listening}</code></p>}
          {view?.error && <FieldMsg ok={false} text={view.error} />}
          {msg && <FieldMsg ok={msg.ok} text={msg.text} />}
        </>}
      />
      <Row
        id="tailnet-port"
        label="Port"
        description={view?.enabled ? "Değiştirmek için önce ağ erişimini kapatın." : undefined}
        control={
          <TextField
            label="Telefondan erişim portu"
            type="number"
            min={1}
            max={65535}
            disabled={view?.enabled}
            value={view ? String(view.port) : ""}
            onSave={(v) => void savePort(v)}
          />
        }
      />
      {view?.enabled && (
        <Pad>
          <span className="set-field-btns">
            <button type="button" className="primary" onClick={() => void pair()}>Eşleme kodu üret</button>
          </span>
          {code && <p className="set-value" role="status" style={{ fontFamily: "var(--font-mono)", letterSpacing: ".08em" }}>{code}</p>}
          <details className="set-fold">
            <summary><b>Güvenlik duvarı engellerse</b></summary>
            <div className="set-pad">
              <p className="set-note">PowerShell'i yönetici olarak açıp yalnız tailnet kaynaklarına izin veren şu komutu çalıştırın:</p>
              <code className="set-note" style={{ overflowWrap: "anywhere" }}>{view.firewallCommand}</code>
            </div>
          </details>
          <span className="set-label">Eşlenmiş cihazlar</span>
          {devices.length === 0
            ? <p className="set-note">Henüz cihaz eşlenmedi.</p>
            : <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 8 }}>
              {devices.map((d) => (
                <li key={d.id} className="set-field">
                  <span>{d.name}</span>
                  <code className="set-note">{d.id}</code>
                  <button type="button" onClick={() => void revoke(d.id)}>İptal et</button>
                </li>
              ))}
            </ul>}
        </Pad>
      )}
    </Section>
  );
}
