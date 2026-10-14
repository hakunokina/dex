// Sunucu › Kota zinciri: kota/kredi bitince ajan aynı işe listedeki sıradaki
// sağlayıcıyla devam eder. Sıra ve aç/kapa anında uygulanır.
import { useEffect, useRef, useState } from "react";
import { chainGet, chainSetDisabled, chainSetOrder, type ChainView } from "../team/api";
import { listProviders, type ProviderInfo } from "../team/api";
import { onEvent } from "../api/hosts";
import { Row, Pad } from "./kit/Row";
import { Section } from "./kit/Section";
import { Switch } from "./kit/Switch";
import { ErrorBand, FieldMsg, LoadingRows, SavedFlash } from "./kit/states";

export function ChainSection({ host }: { host: string }) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [view, setView] = useState<ChainView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [flash, setFlash] = useState(0);
  const lock = useRef(false);
  const labels: Record<string, string> = Object.fromEntries(providers.map((p) => [p.id, p.label]));

  const refresh = () => chainGet(host).then(
    (next) => { setView(next); setError(null); },
    (e) => setError(String(e)),
  );
  useEffect(() => {
    let mounted = true;
    let catalogRequest = 0;
    setView(null);
    setError(null);
    void refresh();
    const catalog = () => {
      const n = ++catalogRequest;
      return listProviders(host).then(
        (ps) => { if (mounted && n === catalogRequest) setProviders(ps); },
        (e) => { if (mounted && n === catalogRequest) setError(String(e)); },
      );
    };
    void catalog();
    const off = onEvent("dex-chain-available", (_, eventHost) => { if (eventHost === host) void refresh(); });
    const offProviders = onEvent("providers-changed", (_, eventHost) => { if (eventHost === host) void catalog(); });
    return () => { mounted = false; off(); offProviders(); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  async function change(action: () => Promise<ChainView>) {
    if (lock.current) return;
    lock.current = true;
    setBusy(true);
    try {
      setView(await action());
      setFlash((n) => n + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      void refresh();
    } finally {
      lock.current = false;
      setBusy(false);
    }
  }

  async function move(id: string, dir: -1 | 1) {
    if (!view) return;
    const order = [...view.order];
    const i = order.indexOf(id);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= order.length) return;
    [order[i], order[j]] = [order[j], order[i]];
    await change(() => chainSetOrder(host, order));
  }

  async function toggle(id: string) {
    if (!view) return;
    const disabled = view.disabled.includes(id)
      ? view.disabled.filter((d) => d !== id)
      : [...view.disabled, id];
    await change(() => chainSetDisabled(host, disabled));
  }

  const fmtReset = (ms: number) => {
    const d = new Date(ms);
    return Number.isNaN(d.getTime()) ? "?" : d.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  };

  return (
    <Section
      label="Kota zinciri"
      description="Bir sağlayıcının kotası biterse ajan aynı işe listedeki sıradaki sağlayıcıyla devam eder. Kapalı sağlayıcı atlanır; sıfırlanan sağlayıcıya ajan otomatik geri döner."
    >
      {error && <Pad><ErrorBand text={`Zincir okunamadı: ${error}`} onRetry={() => void refresh()} /></Pad>}
      {!view && !error && <LoadingRows />}
      {view && view.order.map((id) => {
        const off = view.disabled.includes(id);
        return (
          <Row
            key={id}
            label={off ? `${labels[id] ?? id} (kapalı)` : (labels[id] ?? id)}
            control={
              <>
                <span className="set-field-btns">
                  <button
                    type="button" className="set-icon-btn" aria-label={`${labels[id] ?? id} yukarı`}
                    disabled={busy || view.order.indexOf(id) === 0} onClick={() => void move(id, -1)}>↑</button>
                  <button
                    type="button" className="set-icon-btn" aria-label={`${labels[id] ?? id} aşağı`}
                    disabled={busy || view.order.indexOf(id) === view.order.length - 1} onClick={() => void move(id, 1)}>↓</button>
                </span>
                <Switch
                  label={`${labels[id] ?? id} zincirde`}
                  checked={!off}
                  disabled={busy}
                  onChange={() => void toggle(id)}
                />
                <SavedFlash stamp={flash} />
              </>
            }
          />
        );
      })}
      {view && view.exhausted.length > 0 && (
        <Pad>
          <p className="set-note" role="status">
            Tükenen: {view.exhausted.map((e) => `${labels[e.provider] ?? e.provider} (${fmtReset(e.resetAt)}'ye kadar)`).join(", ")}.
          </p>
        </Pad>
      )}
      {view && (
        <Pad>
          <p className="set-note">Tek tek ajanlar için: Takım ayarları › Üyeler.</p>
          {view.order.length === 0 && <FieldMsg ok text="Zincir boş." />}
        </Pad>
      )}
    </Section>
  );
}
