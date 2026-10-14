// #54: Dex penceresinin sağ kenarındaki kullanım şeridi (TM Edge Dock'u gibi): tok/s, bugünün
// token + $'ı, sağlayıcı başına kalan kota halkası. #139: yer ayırmaz, sahnenin üstünde yüzer (usage.css).
// Aşama 2: tıklama ajan bazlı döküm açacak; şimdilik ayrıntı tooltip'te.
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { PlugZap } from "lucide-react";
import { openSettings } from "../layout/panels";
import { getActiveTeamId } from "../team/store";
import { callHost, LOCAL, onEvent } from "../api/hosts";
import { gpuStripCell, type GpuOverview } from "../gpu/view";
import { useStripVisible, useUsage } from "./store";
import { providerMeta } from "./providers";
import { WINDOW_SHORT, resetShort, updatedAgo } from "./format";
import type { ProviderUsage, UsageSnapshot } from "./types";
import "./usage.css";

/** TM'nin eşikleri: bu kadar yeni örnek "canlı", bundan eski ama RATE_CLEAR'dan yeni olan sönük. */
const RATE_ACTIVE_MS = 8_000;
const RATE_CLEAR_MS = 180_000;

const WINDOW_LABEL: Record<string, string> = { session: "5 saatlik pencere", daily: "günlük", weekly: "haftalık", billing: "dönem" };

export function compactTokens(n: number): string {
  const units: [number, string][] = [[1e9, "B"], [1e6, "M"], [1e3, "K"]];
  for (const [v, u] of units) if (n >= v) return `${(n / v).toFixed(1)}${u}`;
  return String(Math.round(n));
}

function untilText(iso: string, now: number): string {
  const at = Date.parse(iso);
  if (!Number.isFinite(at)) return "";
  const clock = new Date(at).toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  const mins = Math.max(0, Math.round((at - now) / 60_000));
  const rel = mins >= 60 * 24 ? `${Math.floor(mins / 1440)} gün` : mins >= 60 ? `${Math.floor(mins / 60)} sa ${mins % 60} dk` : `${mins} dk`;
  return `sıfırlanma ${clock} (${rel} sonra)`;
}

function Ring({ p, now, onHover }: { p: ProviderUsage; now: number; onHover: (id: string | null, anchor: DOMRect | null) => void }) {
  const m = providerMeta(p.id);
  const r = 15;
  const c = 2 * Math.PI * r;
  const pct = p.remainingPercent;
  const sev = pct === null ? "unknown" : pct <= 10 ? "critical" : pct <= 25 ? "low" : "ok";
  const title = pct === null
    ? `${m.label} — şu an veri yok`
    : [`${m.label} — ${WINDOW_LABEL[p.window ?? ""] ?? p.window ?? ""}: %${Math.round(pct)} kalan`, p.resetsAt && untilText(p.resetsAt, now)].filter(Boolean).join("\n");
  // TM dock'u gibi: halka marka renginde, harcanan kısım o rengin soluk tonu, ortada logo metin renginde.
  const brand = m.color.startsWith("#") ? m.color : undefined;
  return (
    <div className={`usage-prov ${sev}`} title={title} data-provider={p.id} tabIndex={0}
      onMouseEnter={(e) => onHover(p.id, e.currentTarget.getBoundingClientRect())}
      onMouseLeave={() => onHover(null, null)}
      onFocus={(e) => onHover(p.id, e.currentTarget.getBoundingClientRect())}
      onBlur={() => onHover(null, null)}>
      <div className="usage-ring" style={{ "--ring-color": m.color } as CSSProperties}>
        <svg viewBox="0 0 36 36" aria-hidden="true">
          <circle className="usage-ring-track" cx="18" cy="18" r={r} style={brand ? { stroke: `${brand}33` } : undefined} />
          {pct !== null && (
            <circle className="usage-ring-fill" cx="18" cy="18" r={r}
              strokeDasharray={`${(c * pct) / 100} ${c}`} transform="rotate(-90 18 18)" />
          )}
        </svg>
        {m.icon
          ? <span className="usage-mark" data-icon={p.id} style={{ maskImage: `url("${m.icon}")`, WebkitMaskImage: `url("${m.icon}")` }} />
          : <span className="usage-glyph" data-icon="">{m.glyph}</span>}
      </div>
      <span className="usage-pct">{pct === null ? "--" : `${Math.round(pct)}%`}</span>
    </div>
  );
}

/** TM edge panelindeki Muse kartının sade şablonu, her sağlayıcıda: logo + ad (+ varsa
 *  durum etiketi), güncellenme, pencere satırı (% kaldı + çubuk + sıfırlanma). `position: fixed`
 *  ile açılır çünkü şerit `overflow-x: hidden` — mutlak konumlu kart kesilirdi. Dikey konum
 *  halka ortalanır, pencere kenarından taşmayacak şekilde kırpılır. */
function ProviderCard({ p, snap, now, anchor, onHover }: {
  p: ProviderUsage; snap: UsageSnapshot; now: number; anchor: DOMRect;
  onHover: (id: string | null, anchor: DOMRect | null) => void;
}) {
  const m = providerMeta(p.id);
  const pct = p.remainingPercent;
  const cardRef = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState<number | null>(null);
  const [cardH, setCardH] = useState(0);
  // Kart şeridin sol kenarına yaslanır (halka div'i şeritten içeride başlar).
  const [right, setRight] = useState<number | null>(null);
  useLayoutEffect(() => {
    const h = cardRef.current?.offsetHeight ?? 0;
    setCardH(h);
    const want = anchor.top + anchor.height / 2 - h / 2;
    setTop(Math.min(Math.max(8, want), Math.max(8, window.innerHeight - h - 8)));
    const stripLeft = cardRef.current?.closest(".usage-strip")?.getBoundingClientRect().left
      ?? anchor.left;
    setRight(Math.max(8, window.innerWidth - stripLeft + 8));
  }, [anchor]);
  // Ok halkayı gösterir; kart kenara yaslanınca kart içinde kalır.
  const arrowTop = top === null ? 0 : Math.min(Math.max(14, anchor.top + anchor.height / 2 - top), Math.max(14, cardH - 14));
  return (
    <div ref={cardRef} className="usage-card" data-card={p.id} role="status"
      style={{ top: top ?? -9999, right: right ?? -9999, visibility: top === null ? "hidden" : "visible" } as CSSProperties}
      onMouseEnter={() => onHover(p.id, anchor)}
      onMouseLeave={() => onHover(null, null)}>
      <div className="usage-card-head">
        {m.icon
          ? <span className="usage-card-mark" style={{ maskImage: `url("${m.icon}")`, WebkitMaskImage: `url("${m.icon}")` }} />
          : <span className="usage-card-glyph">{m.glyph}</span>}
        <span className="usage-card-name">{m.label}</span>
        {p.statusLabel && <span className="usage-card-badge">{p.statusLabel}</span>}
      </div>
      {updatedAgo(snap.updatedAt, now) && (
        <div className="usage-card-updated">{updatedAgo(snap.updatedAt, now)}</div>
      )}
      {pct === null || !p.window ? (
        <div className="usage-card-empty">şu an veri yok</div>
      ) : (
        <div className="usage-card-win">
          <div className="usage-card-winrow">
            <span>{WINDOW_SHORT[p.window] ?? p.window}</span>
            <b>%{Math.round(pct)} kaldı</b>
          </div>
          <div className="usage-card-bar" style={{ "--ring-color": m.color } as CSSProperties}>
            <i style={{ width: `${Math.round(pct)}%` }} />
          </div>
          {resetShort(p.resetsAt, now) && (
            <div className="usage-card-reset">{resetShort(p.resetsAt, now)}</div>
          )}
        </div>
      )}
      <span className="usage-card-arrow" aria-hidden="true" style={{ top: arrowTop }} />
    </div>
  );
}

/** dexd caches the broker look for 30 s; a faster poll would only read the cache. */
const GPU_POLL_MS = 30_000;

/** #311: GPU owner (boş / LLM / iş), queue and today's images. No broker configured → nothing. */
function GpuCell() {
  const [o, setO] = useState<GpuOverview | null>(null);
  useEffect(() => {
    let alive = true;
    const look = () =>
      void callHost<GpuOverview>(LOCAL, "gpu_overview", { refresh: false })
        .then((v) => alive && setO(v))
        .catch(() => alive && setO(null)); // pre-#289 dexd: no GPU
    look();
    const t = window.setInterval(look, GPU_POLL_MS);
    const off = onEvent("gpu-waiting", look);
    return () => { alive = false; window.clearInterval(t); off(); };
  }, []);
  const c = gpuStripCell(o);
  if (!c) return null;
  return (
    <div className={`usage-cell usage-gpu ${c.tone}`} title={c.title} aria-label={c.title} tabIndex={0} data-gpu-owner={c.owner}>
      <span className="usage-cap">gpu</span>
      <span className="usage-big">{c.owner}</span>
      <span className="usage-sub usage-gpu-daily">{c.daily}</span>
      {c.queued ? <span className="usage-sub">{c.queued} sırada</span> : null}
    </div>
  );
}

export function UsageStrip() {
  const visible = useStripVisible();
  const u = useUsage();
  const [now, setNow] = useState(Date.now);
  // Hover kartı: tek kart, şerit düzeyinde; halkadan karta fare geçişinde titremesin diye
  // kapatma kısa gecikmeli, karta giriş sayacı iptal eder.
  const [hoverId, setHoverId] = useState<string | null>(null);
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const hideTimer = useRef(0);
  useEffect(() => () => window.clearTimeout(hideTimer.current), []);
  const onHover = (id: string | null, rect: DOMRect | null) => {
    window.clearTimeout(hideTimer.current);
    if (id === null) {
      hideTimer.current = window.setTimeout(() => { setHoverId(null); setAnchor(null); }, 120);
    } else {
      setHoverId(id);
      setAnchor(rect);
    }
  };
  // Hız örneği yeni itme gelmeden de eskir; yalnız bir örnek varken saniyede bir yeniden çiz.
  const live = u.rateAt !== null && now - u.rateAt < RATE_CLEAR_MS;
  useEffect(() => {
    setNow(Date.now());
    if (u.rateAt === null) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [u.rateAt]);
  if (!visible) return null;

  if (u.status !== "live") {
    const unauthorized = u.status === "unauthorized";
    const text = unauthorized ? (u.hasSecret ? "TM anahtarı yanlış" : "TM anahtarı gerekli") : u.status === "connecting" ? "TM'ye bağlanıyor" : "TM bağlı değil";
    return (
      <aside className="usage-strip off" aria-label="Kullanım şeridi" data-status={u.status}>
        {/* #314: GPU durumu TM'den bağımsız; kullanım verisi gelmese de görünür. */}
        <GpuCell />
        <button className="usage-off" title={`${text} — Ayarlar → Genel → Kullanım şeridi`}
          onClick={() => { const team = getActiveTeamId(); if (team) void openSettings(team, "general"); }}>
          <PlugZap size={16} />
          <span>{text}</span>
        </button>
      </aside>
    );
  }

  const idle = !live || now - (u.rateAt ?? 0) > RATE_ACTIVE_MS;
  // #1713: Codex is gone from Dex; TM may still report the subscription, the strip doesn't show it.
  const providers = u.providers.filter((p) => p.id !== "codex");
  return (
    <aside className="usage-strip" aria-label="Kullanım şeridi" data-status={u.status}>
      <div className={`usage-cell usage-rate ${idle ? "idle" : ""}`} title="Canlı çıktı hızı (token/sn)">
        <span className="usage-cap">tok/s</span>
        <span className="usage-big">{live && u.tokPerSec !== null ? Math.round(u.tokPerSec) : "--"}</span>
      </div>
      <div className="usage-cell" title={`Bugün: ${u.todayTokens?.toLocaleString("tr-TR") ?? "--"} token · $${u.todayCostUsd?.toFixed(2) ?? "--"}`}>
        <span className="usage-cap">bugün</span>
        <span className="usage-big">{u.todayTokens === null ? "--" : compactTokens(u.todayTokens)}</span>
        <span className="usage-sub">{u.todayCostUsd === null ? "" : `$${u.todayCostUsd.toFixed(1)}`}</span>
      </div>
      <GpuCell />
      <div className="usage-provs">
        {providers.map((p) => <Ring key={p.id} p={p} now={now} onHover={onHover} />)}
      </div>
      {hoverId && anchor && (() => {
        const p = providers.find((x) => x.id === hoverId);
        return p ? <ProviderCard p={p} snap={u} now={now} anchor={anchor} onHover={onHover} /> : null;
      })()}
    </aside>
  );
}
