// #54: Dex penceresinin sağ kenarındaki kullanım şeridi (TM Edge Dock'u gibi): bugünün (#2139: ya da 7 günün) token'ı,
// sağlayıcı başına kalan kota halkası. #139: yer ayırmaz, sahnenin üstünde yüzer (usage.css).
// #1861: veri etkin takımın sunucusundan (dexd); halkada dış yay 5 saat, iç yay haftalık pencere.
// #2034: konum bu cihazın tercihi — sağ/sol kenarda dikey (sahnede) ya da üst başlıkta yatay.
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { useTeams } from "../team/store";
import { callHost, hostOf, LOCAL, onEvent } from "../api/hosts";
import { gpuStripCell, type GpuOverview } from "../gpu/view";
import { setStripRange, useStripPlace, useStripRange, useStripVisible, useUsage } from "./store";
import { cardSpot, type CardSpot, type StripPlace } from "./place";
import { providerMeta } from "./providers";
import { WINDOWS, resetShort, severity, tokenCell, updatedAgo } from "./format";
import type { ProviderUsage, UsageStatus } from "./types";
import "./usage.css";

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

const pctText = (n: number | undefined) => (n === undefined ? "--" : `${Math.round(n)}%`);

/** Halka yayları: dış 5 saat, iç haftalık (ortada logo). */
const ARCS = [{ k: "fiveHour", r: 15.5 }, { k: "weekly", r: 11.5 }] as const;

function Ring({ p, now, onHover }: { p: ProviderUsage; now: number; onHover: (id: string | null, anchor: DOMRect | null) => void }) {
  const m = providerMeta(p.id);
  const sev = severity(p);
  const title = sev === "unknown"
    ? `${m.label} — şu an veri yok${p.note ? ` (${p.note})` : ""}`
    : [m.label, ...WINDOWS.map(([k, label]) => {
      const w = p[k];
      return w ? [`${label}: %${Math.round(w.remainingPercent)} kalan`, w.resetsAt && untilText(w.resetsAt, now)].filter(Boolean).join(", ") : `${label}: --`;
    })].join("\n");
  // TM dock'u gibi: halka marka renginde, harcanan kısım o rengin soluk tonu, ortada logo metin renginde.
  const brand = m.color.startsWith("#") ? m.color : undefined;
  return (
    <div className={`usage-prov ${sev}`} aria-label={title} data-provider={p.id} tabIndex={0}
      onMouseEnter={(e) => onHover(p.id, e.currentTarget.getBoundingClientRect())}
      onMouseLeave={() => onHover(null, null)}
      onFocus={(e) => onHover(p.id, e.currentTarget.getBoundingClientRect())}
      onBlur={() => onHover(null, null)}>
      <div className="usage-ring" style={{ "--ring-color": m.color } as CSSProperties}>
        <svg viewBox="0 0 36 36" aria-hidden="true">
          {ARCS.map(({ k, r }) => {
            const w = p[k];
            const c = 2 * Math.PI * r;
            return (
              <g key={k} data-window={k}>
                <circle className="usage-ring-track" cx="18" cy="18" r={r} style={brand ? { stroke: `${brand}33` } : undefined} />
                {w && (
                  <circle className="usage-ring-fill" cx="18" cy="18" r={r}
                    strokeDasharray={`${(c * w.remainingPercent) / 100} ${c}`} transform="rotate(-90 18 18)" />
                )}
              </g>
            );
          })}
        </svg>
        {m.icon
          ? <span className="usage-mark" data-icon={p.id} style={{ maskImage: `url("${m.icon}")`, WebkitMaskImage: `url("${m.icon}")` }} />
          : <span className="usage-glyph" data-icon="">{m.glyph}</span>}
        {m.badge && <span className="usage-badge" aria-hidden="true">{m.badge}</span>}
      </div>
      <span className="usage-pct" data-window="fiveHour">{pctText(p.fiveHour?.remainingPercent)}</span>
      <span className="usage-pct usage-pct-week" data-window="weekly">{pctText(p.weekly?.remainingPercent)}</span>
    </div>
  );
}

/** TM edge panelindeki Muse kartının sade şablonu, her sağlayıcıda: logo + ad, güncellenme, iki
 *  pencere satırı (% kaldı + çubuk + sıfırlanma), bugünün token'ı. `position: fixed` ile açılır
 *  çünkü şerit taşmayı keser — mutlak konumlu kart kesilirdi. Yer `cardSpot`'tan: dikey şeritte
 *  yanında, başlıkta altında; halkaya ortalı, pencere içinde kırpılı. */
function ProviderCard({ p, now, anchor, place, onHover }: {
  p: ProviderUsage; now: number; anchor: DOMRect; place: StripPlace;
  onHover: (id: string | null, anchor: DOMRect | null) => void;
}) {
  const m = providerMeta(p.id);
  const cardRef = useRef<HTMLDivElement>(null);
  const [spot, setSpot] = useState<CardSpot | null>(null);
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const strip = el.closest(".usage-strip")?.getBoundingClientRect() ?? anchor;
    setSpot(cardSpot(place, anchor, strip, { w: el.offsetWidth, h: el.offsetHeight }, { w: window.innerWidth, h: window.innerHeight }));
  }, [anchor, place]);
  return (
    <div ref={cardRef} className="usage-card" data-card={p.id} data-arrow={spot?.arrow} role="status"
      style={{ top: spot?.top ?? -9999, left: spot?.left ?? -9999, maxHeight: spot?.maxHeight, visibility: spot ? "visible" : "hidden" } as CSSProperties}
      onMouseEnter={() => onHover(p.id, anchor)}
      onMouseLeave={() => onHover(null, null)}>
      <div className="usage-card-head">
        {m.icon
          ? <span className="usage-card-mark" style={{ maskImage: `url("${m.icon}")`, WebkitMaskImage: `url("${m.icon}")` }} />
          : <span className="usage-card-glyph">{m.glyph}</span>}
        <span className="usage-card-name">{m.label}</span>
      </div>
      {updatedAgo(p.measuredAt, now) && (
        <div className="usage-card-updated">{updatedAgo(p.measuredAt, now)}</div>
      )}
      {severity(p) === "unknown" ? (
        <div className="usage-card-empty">şu an veri yok{p.note ? ` — ${p.note}` : ""}</div>
      ) : WINDOWS.map(([k, label]) => {
        const w = p[k];
        return (
          <div className="usage-card-win" key={k} data-window={k}>
            <div className="usage-card-winrow">
              <span>{label}</span>
              <b>{w ? `%${Math.round(w.remainingPercent)} kaldı` : "--"}</b>
            </div>
            {w && (
              <div className="usage-card-bar" style={{ "--ring-color": m.color } as CSSProperties}>
                <i style={{ width: `${Math.round(w.remainingPercent)}%` }} />
              </div>
            )}
            {w && resetShort(w.resetsAt, now) && (
              <div className="usage-card-reset">{resetShort(w.resetsAt, now)}</div>
            )}
          </div>
        );
      })}
      {p.todayTokens !== null && (
        <div className="usage-card-today">Bugün {p.todayTokens.toLocaleString("tr-TR")} token</div>
      )}
      <span className="usage-card-arrow" aria-hidden="true" style={spot?.arrow === "top" ? { left: spot.arrowAt } : { top: spot?.arrowAt ?? 0 }} />
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
      {c.owner !== "boş" && <span className="usage-big">{c.owner}</span>}
      <span className="usage-sub usage-gpu-daily">{c.daily}</span>
      {c.queued ? <span className="usage-sub">{c.queued} sırada</span> : null}
    </div>
  );
}

const OFF_TEXT: Record<Exclude<UsageStatus, "live">, string> = {
  connecting: "yükleniyor",
  offline: "sunucu yok",
  unsupported: "sunucu eski",
};

/** `slot`: "stage" sahnede (sağ/sol kenar), "top" başlıkta; şerit yalnız seçili konumun yuvasında çizilir. */
export function UsageStrip({ slot }: { slot: "stage" | "top" }) {
  const visible = useStripVisible();
  const place = useStripPlace();
  const range = useStripRange();
  const { activeId } = useTeams();
  const { status, snap } = useUsage(activeId ? hostOf(activeId) : LOCAL);
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
      setNow(Date.now());
      setHoverId(id);
      setAnchor(rect);
    }
  };
  // Sıfırlanma/güncellenme metinleri yeni veri gelmeden de ilerlesin.
  useEffect(() => {
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, [snap]);
  if (!visible || (place === "top") !== (slot === "top")) return null;
  // Başlıkta şeridin boşluğu pencere sürükleme alanı olarak kalır.
  const shell = { className: `usage-strip ${place}`, "aria-label": "Kullanım şeridi", "data-status": status, "data-place": place, "data-tauri-drag-region": place === "top" ? true : undefined };

  if (status !== "live" || !snap) {
    const text = OFF_TEXT[status === "live" ? "connecting" : status];
    return (
      <aside {...shell} className={`${shell.className} off`}>
        {/* #314: GPU durumu kullanım verisinden bağımsız; veri gelmese de görünür. */}
        <GpuCell />
        <div className="usage-off" title={`Kullanım verisi yok: ${text}`}>
          <span>{text}</span>
        </div>
      </aside>
    );
  }

  return (
    <aside {...shell}>
      {(() => {
        // #2139: tıklama (ya da Enter/Space) bugün ↔ 7 gün.
        const c = tokenCell(snap, range);
        return (
          <button type="button" className="usage-cell usage-today" data-range={range} title={c.title} aria-label={c.title}
            onClick={() => setStripRange(range === "today" ? "week" : "today")}>
            <span className="usage-cap">{c.cap}</span>
            <span className="usage-big">{c.tokens === null ? "--" : compactTokens(c.tokens)}</span>
          </button>
        );
      })()}
      <GpuCell />
      <div className="usage-provs" data-tauri-drag-region={place === "top" ? true : undefined}>
        {snap.providers.map((p) => <Ring key={p.id} p={p} now={now} onHover={onHover} />)}
      </div>
      {hoverId && anchor && (() => {
        const p = snap.providers.find((x) => x.id === hoverId);
        return p ? <ProviderCard p={p} now={now} anchor={anchor} place={place} onHover={onHover} /> : null;
      })()}
    </aside>
  );
}
