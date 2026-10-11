// Görev #120: açılışta sağlayıcıların sunduğu, Dex listesinde olmayan modeller — yalnız haber;
// katalog değişmez. Kapatılan kart bir daha çıkmaz (crates/dex-core/src/model_news.rs).
import { useEffect, useState } from "react";
import { callLocal } from "../api/hosts";
import "./proposals.css";

interface NewModel {
  provider: string;
  providerLabel: string;
  id: string;
  label: string;
}

const keyOf = (m: NewModel) => `${m.provider}:${m.id.toLowerCase()}`;

export function ModelNews() {
  const [rows, setRows] = useState<NewModel[]>([]);
  useEffect(() => {
    // Açılışı bekletmez: ilk boyamadan sonra arka planda; ağ yoksa/sağlayıcı susarsa sessiz.
    const t = setTimeout(() => void callLocal<NewModel[]>("model_news").then(setRows, () => {}), 3000);
    return () => clearTimeout(t);
  }, []);
  if (!rows.length) return null;
  const dismiss = (m: NewModel) => {
    setRows((r) => r.filter((x) => keyOf(x) !== keyOf(m)));
    void callLocal("model_news_dismiss", { keys: [keyOf(m)] }).catch(() => {});
  };
  return (
    <aside className="proposals model-news" aria-label="Yeni modeller">
      {rows.map((m) => (
        <div key={keyOf(m)} className="proposal" role="status">
          <p className="proposal-who">
            Yeni model: <b>{m.label}</b> ({m.providerLabel}) — listede yok
            <span className="proposal-model">{m.id}</span>
          </p>
          <div className="proposal-actions">
            <button onClick={() => dismiss(m)} aria-label={`${m.id} uyarısını kapat`}>Kapat</button>
          </div>
        </div>
      ))}
    </aside>
  );
}
