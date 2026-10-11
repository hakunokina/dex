import { useId, useState } from "react";
import { previewSound, setSoundSettings, useSoundSettings } from "../sound";
import { SOUND_LABELS, type SoundKind, type SoundSettings } from "../sound/model";

export function SoundsSection() {
  const uid = useId();
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
  return <section className="settings-section sounds" aria-label="Olay sesleri">
    <h4>Olay sesleri</h4>
    <p className="settings-note">Dex arka plandayken de çalar. Tercihler bu cihazda saklanır; değişiklikler hemen uygulanır.</p>
    <label className="sound-toggle">
      <input type="checkbox" checked={settings.enabled} onChange={e => save({ ...settings, enabled: e.target.checked })} />
      <span>Sesleri aç</span>
    </label>
    <label className="sound-volume" htmlFor={`${uid}-volume`}>
      <span>Ses düzeyi</span>
      <input id={`${uid}-volume`} type="range" min={0} max={100} step={5} value={settings.volume * 100}
        onChange={e => save({ ...settings, volume: Number(e.target.value) / 100 })} />
      <output htmlFor={`${uid}-volume`}>%{Math.round(settings.volume * 100)}</output>
    </label>
    {(Object.keys(SOUND_LABELS) as SoundKind[]).map(kind => <div className="sound-row" key={kind}>
      <label className="sound-toggle">
        <input type="checkbox" checked={settings.events[kind]}
          onChange={e => save({ ...settings, events: { ...settings.events, [kind]: e.target.checked } })} />
        <span>{SOUND_LABELS[kind]}</span>
      </label>
      <button aria-label={`${SOUND_LABELS[kind]} dinle`} disabled={busy !== null} onClick={() => void listen(kind)}>
        {busy === kind ? "Çalınıyor…" : "Dinle"}
      </button>
    </div>)}
    <p className="settings-note">Dinle, kapalı sesleri de seçili ses düzeyinde çalar.</p>
    {problem && <p className="settings-err" role="alert">{problem}</p>}
    {message && !message.error && <p className="settings-note" role="status">{message.text}</p>}
  </section>;
}
