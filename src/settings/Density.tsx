// #33: arayüz yoğunluğu — bu cihazın görünüm tercihi (src/density.ts), anında uygulanır.
import { useEffect, useId, useState } from "react";
import { density, setDensity, type Density } from "../density";
import { termSize, ZOOM_EVENT } from "../termZoom";

const OPTIONS: { value: Density; label: string }[] = [
  { value: "compact", label: "Kompakt" },
  { value: "normal", label: "Normal" },
];

export function DensitySection() {
  // Ayarlar her takım dock'unda mount'lu: sabit name/id kopyaları tek radyo grubuna bağlar.
  const uid = useId();
  const [value, setValue] = useState(density);
  const [size, setSize] = useState(termSize);
  useEffect(() => {
    const on = (e: Event) => setSize((e as CustomEvent<number>).detail);
    window.addEventListener(ZOOM_EVENT, on);
    return () => window.removeEventListener(ZOOM_EVENT, on);
  }, []);
  const pick = (d: Density) => { setDensity(d); setValue(d); };
  return <section className="settings-section" aria-label="Görünüm">
    <h4>Görünüm</h4>
    <div className="settings-row settings-row-wide" role="radiogroup" aria-labelledby={`${uid}-label`}>
      <span id={`${uid}-label`}>Yoğunluk</span>
      <div className="density-opts">
        {OPTIONS.map((o) => (
          <label key={o.value}>
            <input type="radio" name={`${uid}-density`} value={o.value} checked={value === o.value} onChange={() => pick(o.value)} />
            {o.label}
          </label>
        ))}
      </div>
    </div>
    <p className="settings-note">Kompakt daha çok satır gösterir. Yalnız bu cihazda saklanır.</p>
    <div className="settings-row settings-row-wide">
      <span>Terminal yazı boyutu</span>
      <span className="settings-value">{size} px</span>
    </div>
    <p className="settings-note">Terminalde Ctrl+tekerlek ile büyütün/küçültün, Ctrl+0 ile sıfırlayın.</p>
  </section>;
}
