// Sunucu › Kaynak sınırları: ajanların bilgisayarı ne kadar kullanabileceği.
// Her alan kendi satır içi Kaydet'iyle yazılır (DB `settings.limits`).
import { useEffect, useState } from "react";
import { callHost } from "../api/hosts";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { TextField } from "./kit/TextField";
import { FieldMsg, LoadingRows, SavedFlash } from "./kit/states";

interface Limits {
  totalCpuPercent: number;
  agentCpuPercent: number;
  memBytes: number;
}
interface LimitsView {
  stored: Limits;
  effective: Limits;
  env: string[];
  enforced: boolean;
}

const GIB = 1024 ** 3;
const pct = (s: string) => Math.max(0, Math.min(100, Math.round(Number(s) || 0)));

export function LimitsSection({ host }: { host: string }) {
  const [view, setView] = useState<LimitsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [flash, setFlash] = useState(0);

  const refresh = () => callHost<LimitsView>(host, "limits_get").then(
    (v) => { setView(v); setError(null); },
    (e) => setError(String(e)),
  );
  useEffect(() => {
    setView(null);
    setError(null);
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  async function save(patch: Partial<Limits>) {
    if (!view) return;
    const limits: Limits = { ...view.stored, ...patch };
    try {
      setView(await callHost<LimitsView>(host, "limits_set", { limits }));
      setFlash((n) => n + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      void refresh();
    }
  }

  const eff = view?.effective;
  return (
    <Section
      label="Kaynak sınırları"
      description="Tavana gelince ajanlar birlikte yavaşlar; durdurulmaz, yük düşünce hızlanır."
    >
      {error && <Row label="Okuma hatası" control={<button type="button" onClick={() => void refresh()}>Yeniden dene</button>} feedback={<FieldMsg ok={false} text={error} />} />}
      {!view && !error && <LoadingRows />}
      {view && (
        <>
          <Row
            id="limits-total"
            label="Toplam CPU tavanı (tüm ajanlar)"
            control={
              <>
                <TextField label="Toplam CPU tavanı" type="number" min={0} max={100} unit="% · 0 = sınırsız"
                  value={String(view.stored.totalCpuPercent)} onSave={(v) => void save({ totalCpuPercent: pct(v) })} />
                <SavedFlash stamp={flash} />
              </>
            }
          />
          <Row
            id="limits-agent"
            label="Ajan başı CPU tavanı"
            description="Yeni açılan pane'lerde geçerli."
            control={
              <>
                <TextField label="Ajan başı CPU tavanı" type="number" min={0} max={100} unit="% · 0 = kapalı"
                  value={String(view.stored.agentCpuPercent)} onSave={(v) => void save({ agentCpuPercent: pct(v) })} />
                <SavedFlash stamp={flash} />
              </>
            }
          />
          <Row
            id="limits-mem"
            label="Ajan başı bellek tavanı"
            description="Yeni açılan pane'lerde geçerli."
            control={
              <>
                <TextField label="Ajan başı bellek tavanı" type="number" min={0} unit="GiB · 0 = kapalı"
                  value={String(view.stored.memBytes / GIB)} onSave={(v) => void save({ memBytes: Math.round((Number(v) || 0) * GIB) })} />
                <SavedFlash stamp={flash} />
              </>
            }
            feedback={<p className="set-warn">0'dan farklı tavan aşılırsa ajan çökebilir.</p>}
          />
          {view.env.length > 0 && eff && (
            <Row
              label="Ortam değişkeni geçersiz kılıyor"
              control={<span className="set-value">{view.env.join(", ")}</span>}
              feedback={
                <p className="set-warn" role="status">
                  Etkin değerler toplam %{eff.totalCpuPercent || "∞"}, ajan başı %{eff.agentCpuPercent || "—"},
                  bellek {eff.memBytes ? `${(eff.memBytes / GIB).toFixed(1)} GiB` : "kapalı"}.
                </p>
              }
            />
          )}
          {!view.enforced && (
            <Row label="Platform" control={<span className="set-value">—</span>}
              feedback={<p className="set-warn">Bu platformda sınırlar henüz uygulanmıyor (yalnız Windows).</p>} />
          )}
        </>
      )}
    </Section>
  );
}
