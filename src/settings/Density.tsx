// Bu cihaz › Genel › Görünüm: yoğunluk, terminal yazı boyutu, kullanım şeridi.
// Hepsi bu cihazın tercihi (localStorage); değişiklik anında uygulanır.
import { useEffect, useState } from "react";
import { density, setDensity, type Density } from "../density";
import { setTermSize, termSize, ZOOM_EVENT } from "../termZoom";
import { setStripPlace, setStripVisible, useStripPlace, useStripVisible } from "../usage/store";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { Segmented } from "./kit/Segmented";
import { Switch } from "./kit/Switch";

export function AppearanceSection() {
  const [value, setValue] = useState<Density>(density);
  const [size, setSize] = useState(termSize);
  const visible = useStripVisible();
  const place = useStripPlace();
  useEffect(() => {
    const on = (e: Event) => setSize((e as CustomEvent<number>).detail);
    window.addEventListener(ZOOM_EVENT, on);
    return () => window.removeEventListener(ZOOM_EVENT, on);
  }, []);
  return (
    <Section label="Görünüm">
      <Row
        id="density"
        label="Yoğunluk"
        description="Kompakt daha çok satır gösterir."
        control={
          <Segmented
            label="Yoğunluk"
            value={value}
            options={[{ value: "compact", label: "Kompakt" }, { value: "normal", label: "Normal" }]}
            onChange={(d) => { setDensity(d); setValue(d); }}
          />
        }
      />
      <Row
        id="termsize"
        label="Terminal yazı boyutu"
        description="Terminalde Ctrl+tekerlek de değiştirir, Ctrl+0 sıfırlar."
        control={
          <span className="set-field" role="group" aria-label="Terminal yazı boyutu">
            <button type="button" className="set-icon-btn" aria-label="Yazıyı küçült" disabled={size <= 9}
              onClick={() => setTermSize(size - 1)}>−</button>
            <span className="set-value" role="status">{size} px</span>
            <button type="button" className="set-icon-btn" aria-label="Yazıyı büyüt" disabled={size >= 24}
              onClick={() => setTermSize(size + 1)}>+</button>
            <button type="button" aria-label="Yazı boyutunu sıfırla" onClick={() => setTermSize(null)}>Sıfırla</button>
          </span>
        }
      />
      <Row
        id="strip"
        label="Kullanım şeridini göster"
        description="Sağlayıcı kotaları (5 saat / haftalık), GPU ve bugünün token'ı."
        control={
          <Switch label="Kullanım şeridini göster" checked={visible} onChange={setStripVisible} />
        }
      />
      <Row
        id="strip-place"
        label="Kullanım şeridi"
        description="Sağ ya da sol kenarda dikey; üst başlıkta pencere düğmelerinin solunda yatay (takım çipleri yer açar)."
        control={
          <Segmented
            label="Kullanım şeridi"
            value={place}
            disabled={!visible}
            options={[{ value: "right", label: "Sağ" }, { value: "left", label: "Sol" }, { value: "top", label: "Üst başlık" }]}
            onChange={setStripPlace}
          />
        }
      />
    </Section>
  );
}
