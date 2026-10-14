// Görev #120/#1728: sağlayıcının sunduğu, Dex'in sabit listesinde olmayan güncel modeller. GLM/Muse'de
// model seçiciye kendiliğinden girer (kart yalnız haber verir); Claude'da yalnız haber. Kapatılan kart
// bir daha çıkmaz (crates/dex-core/src/model_news.rs).
import { useEffect, useState } from "react";
import { callLocal, LOCAL, onEvent } from "../api/hosts";
import "./proposals.css";

interface NewModel {
  provider: string;
  providerLabel: string;
  id: string;
  label: string;
  /** Model seçicide (canlı katalog). */
  listed?: boolean;
}

const keyOf = (m: NewModel) => `${m.provider}:${m.id.toLowerCase()}`;
const HOUR = 60 * 60 * 1000;

export function ModelNews() {
  const [rows, setRows] = useState<NewModel[]>([]);
  useEffect(() => {
    // Ağ yoksa/sağlayıcı susarsa sessiz. Çekirdek sağlayıcıya saatte en çok bir kez sorar; arada önbellekten döner.
    const load = () => void callLocal<NewModel[]>("model_news").then(setRows, () => {});
    // Açılışı bekletmez: ilk boyamadan sonra arka planda.
    const t = setTimeout(load, 3000);
    const every = setInterval(load, HOUR);
    // Seçici açılışındaki arka plan tazelemesi yeni model bulunca da kart çıksın.
    const off = onEvent("providers-changed", (_, host) => { if (host === LOCAL) load(); });
    return () => { clearTimeout(t); clearInterval(every); off(); };
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
            Yeni model: <b>{m.label}</b> ({m.providerLabel}) — {m.listed ? "model seçicide" : "listede yok"}
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
