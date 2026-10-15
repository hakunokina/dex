// Ayarlar: V1 kartlı düzen. Sol gezinme iki grup (BU CİHAZ / SUNUCU), her bölüm
// ayrı kart ve kapsam rozetli. Kaydetmeyi kontrol türü belirler: anahtar/seçim/
// sıralama anında, yazı alanı satır içi Kaydet'le, yıkıcı işlem onaylı.
// Davranışlar değişmez: aynı saklama yeri, aynı RPC — yalnız yer ve görünüm.
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { hostName, LOCAL, useHosts, hostStatus } from "../api/hosts";
import { ChainSection } from "./Chain";
import { ServersSection } from "./Hosts";
import { SnipSection } from "./Snip";
import { AppearanceSection } from "./Density";
import { TailnetSection } from "./Tailnet";
import { LayoutSection } from "./Layout";
import { LimitsSection } from "./Limits";
import { GpuSection } from "./Gpu";
import { ProvidersSection } from "./Providers";
import { SoundsSection } from "./Sounds";
import { StallSection } from "./Stall";
import { MOBILE } from "../api/platform";
import { ScopeBadge } from "./kit/ScopeBadge";
import { FOCUS_EVENT, TAB_EVENT, migrateTab, selectSettingsTab, takePendingFocus, type SettingsTab } from "./tab";
import "./kit/kit.css";

type Tab = SettingsTab;

const GROUPS: { title: string; tabs: { id: Tab; label: string }[] }[] = [
  {
    title: "Bu cihaz",
    tabs: [
      { id: "general", label: "Genel" },
      { id: "notifications", label: "Bildirimler" },
      { id: "connections", label: "Bağlantılar" },
    ],
  },
  {
    title: "Sunucu",
    tabs: [
      { id: "providers", label: "Sağlayıcılar" },
      { id: "chain", label: "Kota zinciri" },
      { id: "resources", label: "Kaynak sınırları" },
      { id: "gpu", label: "GPU" },
    ],
  },
];
const ALL_TABS = GROUPS.flatMap((g) => g.tabs);

function storedTab(): Tab {
  try {
    return migrateTab(localStorage.getItem("dex.settingsTab"));
  } catch {
    return "general";
  }
}

export function SettingsView({ host, teamId }: { host: string; teamId: string }) {
  const [tab, setTab] = useState<Tab>(storedTab);
  const [server, setServer] = useState(host);
  const [serverTick, setServerTick] = useState(0);
  const hosts = useHosts();
  const serverDown = server !== LOCAL && hosts.find((h) => h.rec.id === server)?.status !== "connected";
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Panel başka bir takımın dock'unda açıldıysa sunucu seçimini sıfırla.
  useEffect(() => { setServer(host); }, [host]);
  useEffect(() => {
    const on = (e: Event) => setTab((e as CustomEvent<Tab>).detail);
    window.addEventListener(TAB_EVENT, on);
    return () => window.removeEventListener(TAB_EVENT, on);
  }, []);
  // Derin bağlantı: ilgili satıra kaydır, odağı kontrole ver, kısaca vurgula.
  useEffect(() => {
    const apply = (id: string) => {
      const root = bodyRef.current?.querySelector(`[data-setting="${id}"]`);
      if (!root) return;
      root.scrollIntoView({ block: "nearest" });
      const target = root.querySelector<HTMLElement>("input, select, button:not(:disabled), [tabindex]");
      target?.focus({ preventScroll: true });
      root.classList.remove("set-flash");
      void (root as HTMLElement).offsetWidth;
      root.classList.add("set-flash");
    };
    const pending = takePendingFocus();
    if (pending) requestAnimationFrame(() => apply(pending));
    const on = (e: Event) => {
      takePendingFocus();
      const id = (e as CustomEvent<string>).detail;
      requestAnimationFrame(() => apply(id));
    };
    window.addEventListener(FOCUS_EVENT, on);
    return () => window.removeEventListener(FOCUS_EVENT, on);
  }, [tab]);

  const pick = selectSettingsTab;
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const i = ALL_TABS.findIndex((x) => x.id === tab);
    const next = ({ ArrowDown: i + 1, ArrowUp: i - 1, Home: 0, End: ALL_TABS.length - 1 } as Record<string, number>)[e.key];
    if (next === undefined) return;
    e.preventDefault();
    const j = (next + ALL_TABS.length) % ALL_TABS.length;
    pick(ALL_TABS[j].id);
    tabs.current[j]?.focus();
  };
  const page = (id: Tab, scope: ReactNode, body: ReactNode) => (
    // Sayfalar mount'lu kalır (gizli): yazılmamış alan sekme değişiminde kaybolmaz.
    <div className="set-page" role="tabpanel" id={`settings-${id}`} aria-labelledby={`settings-tab-${id}`} hidden={tab !== id}>
      <div className="set-head">
        <h2>{ALL_TABS.find((t) => t.id === id)?.label} {scope}</h2>
      </div>
      {body}
    </div>
  );

  let flat = 0;
  const navButton = (t: { id: Tab; label: string }) => {
    const i = flat++;
    return (
      <button
        key={t.id}
        ref={(el) => { tabs.current[i] = el; }}
        role="tab"
        id={`settings-tab-${t.id}`}
        aria-selected={tab === t.id}
        aria-controls={`settings-${t.id}`}
        tabIndex={tab === t.id ? 0 : -1}
        className={tab === t.id ? "on" : ""}
        onClick={() => pick(t.id)}
      >
        {t.label}
      </button>
    );
  };

  return (
    <div className="set-shell">
      <nav className="set-nav">
        <div className="set-nav-list" role="tablist" aria-label="Ayar bölümleri" aria-orientation="vertical" onKeyDown={onKey}>
          {GROUPS.map((g) => (
            <div key={g.title}>
              <h2 className="set-nav-group" role="presentation">{g.title.toLocaleUpperCase("tr-TR")}</h2>
              {g.tabs.map(navButton)}
            </div>
          ))}
        </div>
        <select
          className="set-nav-select"
          aria-label="Ayar sayfası"
          value={tab}
          onChange={(e) => pick(e.target.value as Tab)}
        >
          {GROUPS.map((g) => (
            <optgroup key={g.title} label={g.title}>
              {g.tabs.map((t) => <option key={t.id} value={t.id}>{t.label}</option>)}
            </optgroup>
          ))}
        </select>
      </nav>
      <div className="set-body" ref={bodyRef}>
        {page("general", <ScopeBadge text="Bu cihaz" />, <>
          <AppearanceSection />
          <LayoutSection teamId={teamId} />
          <SnipSection />
        </>)}
        {page("notifications", <ScopeBadge text="Bu cihaz" />, <>
          {!MOBILE && <SoundsSection />}
          <StallSection />
        </>)}
        {page("connections", <ScopeBadge text="Bu cihaz" />, <>
          <ServersSection />
          <TailnetSection />
        </>)}
        {page("providers", <ScopeBadge text={`Sunucu: ${hostName(server)}`} />, <>
          <ServerPicker server={server} onPick={setServer} />
          <ProvidersSection host={server} hostLabel={hostName(server)} />
        </>)}
        {page("chain", <ScopeBadge text={`Sunucu: ${hostName(server)}`} />, <>
          <ServerPicker server={server} onPick={setServer} />
          <ServerDownBand server={server} onRetry={() => setServerTick((n) => n + 1)} />
          <div key={`chain:${server}:${serverTick}`} {...(serverDown ? { inert: true } : {})} aria-disabled={serverDown}>
            <ChainSection host={server} />
          </div>
        </>)}
        {page("resources", <ScopeBadge text={`Sunucu: ${hostName(server)}`} />, <>
          <ServerPicker server={server} onPick={setServer} />
          <ServerDownBand server={server} onRetry={() => setServerTick((n) => n + 1)} />
          <div key={`limits:${server}:${serverTick}`} {...(serverDown ? { inert: true } : {})} aria-disabled={serverDown}>
            <LimitsSection host={server} />
          </div>
        </>)}
        {page("gpu", <ScopeBadge text={`Sunucu: ${hostName(server)}`} />, <>
          <ServerPicker server={server} onPick={setServer} />
          <ServerDownBand server={server} onRetry={() => setServerTick((n) => n + 1)} />
          <div key={`gpu:${server}:${serverTick}`} {...(serverDown ? { inert: true } : {})} aria-disabled={serverDown}>
            <GpuSection host={server} />
          </div>
        </>)}
      </div>
    </div>
  );
}

/** SUNUCU sayfalarında ortak seçim: panel ömrü boyunca kalır, kaydedilmez. */
function ServerPicker({ server, onPick }: { server: string; onPick: (h: string) => void }) {
  const hosts = useHosts();
  if (hosts.length === 0) {
    return <p className="set-note">Sunucu: Yerel (bu bilgisayar)</p>;
  }
  return (
    <div className="set-head-row">
      <label className="set-label" htmlFor="settings-server">Sunucu</label>
      <select id="settings-server" value={server} onChange={(e) => onPick(e.target.value)}>
        <option value={LOCAL}>Yerel (bu bilgisayar)</option>
        {hosts.map((h) => (
          <option key={h.rec.id} value={h.rec.id} disabled={h.status !== "connected"}>
            {h.rec.name}{h.status === "connected" ? "" : " (bağlı değil)"}
          </option>
        ))}
      </select>
    </div>
  );
}

function ServerDownBand({ server, onRetry }: { server: string; onRetry: () => void }) {
  const status = server === LOCAL ? "connected" : hostStatus(server);
  if (status === "connected") return null;
  return (
    <div className="set-band" role="status">
      {hostName(server)} sunucusuna bağlantı yok — ayarlar okunamadı. Ajanlar sunucuda çalışmaya devam ediyor.{" "}
      <button type="button" onClick={onRetry}>Yeniden dene</button>
    </div>
  );
}
