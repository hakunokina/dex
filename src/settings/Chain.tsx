// DEX-17: "Motor zinciri" — kota/kredi bitince otomatik sağlayıcı geçişi + resume.
// Sıra Ayarlar'da (`chain_set_order`), sağlayıcı zincirden kapatılabilir (`chain_set_disabled`),
// ajan başına kapatılabilir (üye satırında).
// Tükenen sağlayıcılar burada görünür (sıfırlanma saatiyle); sıfırlanınca ajan otomatik geri döner.
import { useEffect, useRef, useState } from "react";
import { chainGet, chainSetDisabled, chainSetOrder, type ChainView } from "../team/api";
import { listProviders, type ProviderInfo } from "../team/api";
import { onEvent } from "../api/hosts";
import { onGatewayCatalogChanged } from "./gatewayApi";

export function ChainSection({ host }: { host: string }) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [view, setView] = useState<ChainView | null>(null);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const labels: Record<string, string> = { codex: "Codex", ...Object.fromEntries(providers.map(p => [p.id, p.label])) };
  const gateway = providers.find(p => p.id === "gateway");

  useEffect(() => {
    let mounted = true;
    let catalogRequest = 0;
    const refresh = () => chainGet(host).then(
      (next) => { if (mounted) setView(next); },
      (e) => { if (mounted) setMsg({ ok: false, text: String(e) }); },
    );
    void refresh();
    const catalog = () => {
      const request = ++catalogRequest;
      return listProviders(host).then(
        ps => { if (mounted && request === catalogRequest) setProviders(ps); },
        e => { if (mounted && request === catalogRequest) setMsg({ ok: false, text: String(e) }); },
      );
    };
    void catalog();
    const off = onEvent("dex-chain-available", (_, eventHost) => {
      if (eventHost === host) void refresh();
    });
    const offProviders = onEvent("providers-changed", (_, eventHost) => { if (eventHost === host) void catalog(); });
    const offCatalog = onGatewayCatalogChanged(eventHost => { if (eventHost === host) void catalog(); });
    return () => {
      mounted = false;
      off();
      offProviders(); offCatalog();
    };
  }, [host]);

  async function change(action: () => Promise<ChainView>, text: string) {
    if (lock.current) return;
    lock.current = true; setBusy(true);
    try { setView(await action()); setMsg({ ok: true, text }); }
    catch (e) { setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) }); }
    finally { lock.current = false; setBusy(false); }
  }

  async function move(id: string, dir: -1 | 1) {
    if (!view) return;
    const order = [...view.order];
    const i = order.indexOf(id);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= order.length) return;
    [order[i], order[j]] = [order[j], order[i]];
    await change(() => chainSetOrder(host, order), "Zincir sırası kaydedildi.");
  }

  async function toggle(id: string) {
    if (!view) return;
    const disabled = view.disabled.includes(id)
      ? view.disabled.filter((d) => d !== id)
      : [...view.disabled, id];
    await change(() => chainSetDisabled(host, disabled), "Zincir güncellendi.");
  }

  const fmtReset = (ms: number) => {
    const d = new Date(ms);
    return Number.isNaN(d.getTime())
      ? "?"
      : d.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  };

  return (
    <details className="settings-section settings-fold" aria-label="Motor zinciri">
      <summary>
        <b>Kota bitince sağlayıcı değiştir</b>
        <span>Bir sağlayıcının kotası biterse ajan aynı işe listedeki sıradaki sağlayıcıyla devam eder.</span>
      </summary>
      <p className="settings-note">
        Kota/kredi bitince devreye girer; zincirdeki geçici sağlayıcıda ağ/timeout olursa o sağlayıcı 15 dk atlanır. Sunucu hataları tetiklemez. Sırada Codex varsa konuşmanın özetiyle yeni
        oturum açılır. Codex'in kendi kotası biterse zincir çalışmaz. Kapalı sağlayıcı atlanır; sıfırlanan sağlayıcıya ajan
        otomatik geri döner (çalışıyorsa oturumu kapanınca).
      </p>
      {view && (
        <ol>
          {view.order.map((id, index) => {
            const off = view.disabled.includes(id);
            return (
              <li key={id} aria-disabled={off}>
                {off ? <s>{labels[id] ?? id}</s> : (labels[id] ?? id)}{" "}
                <button disabled={busy || index === 0} onClick={() => void move(id, -1)} aria-label={`${labels[id] ?? id} yukarı`}>↑</button>
                <button disabled={busy || index === view.order.length - 1} onClick={() => void move(id, 1)} aria-label={`${labels[id] ?? id} aşağı`}>↓</button>
                <button disabled={busy} onClick={() => void toggle(id)} aria-label={`${labels[id] ?? id} zinciri ${off ? "aç" : "kapat"}`}>
                  {off ? "aç" : "kapat"}
                </button>
                {id === "gateway" && <button disabled={busy} aria-label={`${labels[id] ?? id} zincirden çıkar`}
                  onClick={() => void change(() => chainSetOrder(host, view.order.filter(p => p !== id)), "Gateway zincirden çıkarıldı.")}>Çıkar</button>}
              </li>
            );
          })}
        </ol>
      )}
      {!view && !msg && <p className="settings-note" role="status">Zincir yükleniyor…</p>}
      {gateway && <div className="chain-gateway">
        <p className="settings-note">{gateway.label}: {!gateway.hasToken ? "Giriş yok — Ayarlar → Sağlayıcılar’dan giriş yapın." : gateway.models.length ? `Hesaptaki modeller: ${gateway.models.map(m => m.label).join(", ")}` : "Model listesi boş — Ayarlar → Sağlayıcılar’dan durumu yenileyin."}</p>
        {view && !view.order.includes("gateway") && <button disabled={busy || !gateway.hasToken || !gateway.models.length}
          aria-label={`${gateway.label} zincire ekle`}
          onClick={() => void change(() => chainSetOrder(host, [...view.order, "gateway"]), "Gateway zincire eklendi.")}>Zincire ekle</button>}
      </div>}
      {view && view.exhausted.length > 0 && (
        <p className="settings-note">
          Tükenen: {view.exhausted.map((e) => `${labels[e.provider] ?? e.provider} (${fmtReset(e.resetAt)}'ye kadar)`).join(", ")}.
        </p>
      )}
      {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
    </details>
  );
}
