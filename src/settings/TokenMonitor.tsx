// Bu cihaz › Bağlantılar › Token Monitor: hub anahtarı OS keyring'inde
// (src-tauri/src/usage.rs). Yalnız bu cihazda saklanır.
import { useState } from "react";
import { setHubSecret } from "../usage/store";
import { useUsage } from "../usage/store";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { SecretField } from "./kit/SecretField";
import { ConfirmDialog } from "./kit/ConfirmDialog";
import { Pad } from "./kit/Row";
import { FieldMsg } from "./kit/states";

export function TokenMonitorSection() {
  const u = useUsage();
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const status = !u.hasSecret
    ? "Anahtar gerekli"
    : u.status === "unauthorized" ? "TM anahtarı yanlış"
    : u.status === "connecting" ? "TM'ye bağlanıyor"
    : u.status === "live" ? "Bağlı" : "TM bağlı değil";

  async function save(value: string) {
    setBusy(true);
    try {
      await setHubSecret(value);
      setMsg({ ok: true, text: "Anahtar kaydedildi; yeniden bağlanılıyor." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Section label="Token Monitor">
      <Row
        id="tm-key"
        label={`TM hub anahtarı · ${status}`}
        description="Hub'ı (127.0.0.1:17321) dinler."
        control={
          <SecretField
            label="TM hub anahtarı"
            hasSecret={u.hasSecret}
            placeholder="Token Monitor → Multi-device Sync → Host"
            onSave={(v) => void save(v)}
            onDelete={() => setConfirmDelete(true)}
          />
        }
        feedback={msg && <FieldMsg ok={msg.ok} text={msg.text} />}
      />
      <details className="set-fold">
        <summary><b>Nasıl kurulur?</b></summary>
        <div className="set-pad">
          <p className="set-note">
            TM'de Ayarlar → Multi-device Sync → "Host hub on this device" seçili olmalı; oradaki anahtarı buraya yapıştırın.
          </p>
        </div>
      </details>
      {confirmDelete && (
        <Pad>
          <ConfirmDialog
            title="TM hub anahtarı silinsin mi?"
            lines={["Kullanım şeridi veri almayı durdurur.", "TM'deki anahtar değişmez; isterseniz yeniden girebilirsiniz."]}
            confirmLabel="Anahtarı sil"
            busy={busy}
            onCancel={() => setConfirmDelete(false)}
            onConfirm={() => {
              setConfirmDelete(false);
              void (async () => {
                setBusy(true);
                try {
                  await setHubSecret("");
                  setMsg({ ok: true, text: "Anahtar silindi." });
                } catch (e) {
                  setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
                } finally {
                  setBusy(false);
                }
              })();
            }}
          />
        </Pad>
      )}
    </Section>
  );
}
