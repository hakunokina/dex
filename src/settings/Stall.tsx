// Bu cihaz › Bildirimler › Takılma uyarısı: olay gelmeden sessiz kalan
// "çalışıyor" ajan işaretlenir. Bu cihazın tercihi (localStorage), anında uygulanır.
import { useState } from "react";
import { setStallMinutes, stallMinutes, STALL_CHOICES } from "../stall";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { SavedFlash } from "./kit/states";

const LABEL = (m: number) => (m ? `${m} dk` : "Kapalı");

export function StallSection() {
  const [value, setValue] = useState(stallMinutes);
  const [saved, setSaved] = useState(0);
  return (
    <Section label="Takılma uyarısı">
      <Row
        id="stall"
        label="Süre"
        description="Bu süre sessiz kalan çalışan ajan 'takılmış olabilir' diye işaretlenir. Varsayılan: 20 dk."
        control={
          <>
            <select
              aria-label="Takılma uyarısı süresi"
              value={value}
              onChange={(e) => { const m = Number(e.target.value); setStallMinutes(m); setValue(m); setSaved((n) => n + 1); }}
            >
              {STALL_CHOICES.map((m) => <option key={m} value={m}>{LABEL(m)}</option>)}
            </select>
            <SavedFlash stamp={saved} />
          </>
        }
      />
    </Section>
  );
}
