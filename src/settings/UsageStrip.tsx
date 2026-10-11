// #54: kullanım şeridi — görünürlük bu cihazın tercihi (localStorage); TM hub anahtarı OS keyring'inde
// (src-tauri/src/usage.rs). Anahtar yalnız TM "Host hub on this device" modundaysa gerekir.
import { useState } from "react";
import { setHubSecret, setStripVisible, useStripVisible, useUsage } from "../usage/store";

export function UsageStripSection() {
  const visible = useStripVisible();
  const u = useUsage();
  const [secret, setSecret] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  async function save(value: string) {
    try {
      await setHubSecret(value);
      setSecret("");
      setMsg({ ok: true, text: value ? "Anahtar kaydedildi; yeniden bağlanılıyor." : "Anahtar silindi." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  return <section className="settings-section" aria-label="Kullanım şeridi">
    <h4>Kullanım şeridi</h4>
    <label className="settings-row settings-row-wide">
      <span>Sağ kenarda göster</span>
      <input type="checkbox" checked={visible} onChange={(e) => setStripVisible(e.target.checked)} />
    </label>
    <label className="settings-row settings-row-wide">
      <span>TM hub anahtarı</span>
      <input type="password" value={secret} spellCheck={false} autoComplete="off"
        placeholder={u.hasSecret ? "kayıtlı — değiştirmek için yazın" : "Token Monitor → Multi-device Sync → Host"}
        onChange={(e) => setSecret(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && secret.trim()) void save(secret); }} />
    </label>
    <p className="settings-note">Token Monitor'ün hub'ını (127.0.0.1:17321) dinler. TM'de Ayarlar → Multi-device Sync → "Host hub on this device" seçili olmalı; oradaki anahtarı buraya yapıştırın. Yalnız bu cihazda saklanır.</p>
    <div className="settings-actions">
      <button className="primary" disabled={!secret.trim()} onClick={() => void save(secret)}>Kaydet</button>
      {u.hasSecret && <button onClick={() => void save("")}>Anahtarı sil</button>}
    </div>
    {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
  </section>;
}
