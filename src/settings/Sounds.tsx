// Bu cihaz › Bildirimler › Olay sesleri: bu bilgisayarın tercihi, anında uygulanır.
import { useState } from "react";
import { Play } from "lucide-react";
import { previewSound, setSoundSettings, useSoundSettings } from "../sound";
import { SOUND_LABELS, type SoundKind, type SoundSettings } from "../sound/model";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { Switch } from "./kit/Switch";
import { FieldMsg } from "./kit/states";

export function SoundsSection() {
  const { settings, error, audioError } = useSoundSettings();
  const [busy, setBusy] = useState<SoundKind | null>(null);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);
  const problem = error || (message?.error ? message.text : audioError);
  const save = (next: SoundSettings) => { setMessage(null); setSoundSettings(next); };
  const listen = async (kind: SoundKind) => {
    setBusy(kind);
    setMessage(null);
    try {
      await previewSound(kind);
      setMessage({ error: false, text: `${SOUND_LABELS[kind]} çalındı.` });
    } catch (e) {
      setMessage({ error: true, text: e instanceof Error ? e.message : "Ses çalınamadı." });
    } finally { setBusy(null); }
  };
  return (
    <Section label="Olay sesleri" description="Dex arka plandayken de çalar.">
      <Row
        label="Sesler"
        control={<Switch label="Sesleri aç" checked={settings.enabled} onChange={(enabled) => save({ ...settings, enabled })} />}
      />
      <Row
        label="Ses düzeyi"
        control={
          <span className="set-field">
            <input
              type="range" aria-label="Ses düzeyi" min={0} max={100} step={5} value={settings.volume * 100}
              style={{ accentColor: "var(--text-2)" }}
              onChange={(e) => save({ ...settings, volume: Number(e.target.value) / 100 })}
            />
            <output className="set-value">%{Math.round(settings.volume * 100)}</output>
          </span>
        }
      />
      {(Object.keys(SOUND_LABELS) as SoundKind[]).map((kind) => (
        <Row
          key={kind}
          label={SOUND_LABELS[kind]}
          control={
            <>
              <Switch
                label={SOUND_LABELS[kind]}
                checked={settings.events[kind]}
                onChange={(on) => save({ ...settings, events: { ...settings.events, [kind]: on } })}
              />
              <button
                type="button"
                className="set-icon-btn"
                aria-label={`${SOUND_LABELS[kind]} sesini dinle`}
                disabled={busy !== null}
                onClick={() => void listen(kind)}
              >
                <Play />
              </button>
            </>
          }
        />
      ))}
      {problem ? <div className="set-pad"><FieldMsg ok={false} text={problem} /></div>
        : message && !message.error ? <div className="set-pad"><FieldMsg ok text={message.text} /></div>
        : <div className="set-pad"><p className="set-note">Dinle, kapalı sesleri de seçili ses düzeyinde çalar.</p></div>}
    </Section>
  );
}
