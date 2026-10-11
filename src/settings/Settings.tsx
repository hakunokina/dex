// DEX-31: app settings as a dock panel. #44: a short tab list on the left (Genel / Ajanlar / Sunucular /
// Gelişmiş); everyday settings first, rare technical ones folded under Gelişmiş. Resource limits (one total
// CPU cap over all agents, optional per-agent CPU cap and memory ceiling) are stored in the DB by dex-core.
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { callHost, hostName, LOCAL } from "../api/hosts";
import { FirstReplySection } from "./FirstReply";
import { ChainSection } from "./Chain";
import { HostsSection } from "./Hosts";
import { SnipSection } from "./Snip";
import { DensitySection } from "./Density";
import { UsageStripSection } from "./UsageStrip";
import { DiagnosticsSection } from "./Diagnostics";
import { CoreSection } from "./Core";
import { GpuSection } from "./Gpu";
import { GatewaySection } from "./Gateway";
import { SoundsSection } from "./Sounds";
import { MOBILE } from "../api/platform";
import { useTeams } from "../team/store";
import { selectSettingsTab, TAB_EVENT, TAB_KEY, type SettingsTab } from "./tab";
import "./settings.css";

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
interface TailnetView {
  enabled: boolean;
  port: number;
  listening: string | null;
  error: string | null;
  firewallCommand: string;
}
interface Device { id: string; name: string; createdAt: number; lastSeen: number | null; }

const GIB = 1024 ** 3;
const pct = (s: string) => Math.max(0, Math.min(100, Math.round(Number(s) || 0)));

type Tab = SettingsTab;
const TABS: { id: Tab; label: string }[] = [
  { id: "general", label: "Genel" },
  { id: "agents", label: "Ajanlar" },
  { id: "providers", label: "Sağlayıcılar" },
  { id: "hosts", label: "Sunucular" },
  { id: "advanced", label: "Gelişmiş" },
];
function storedTab(): Tab {
  try {
    const t = localStorage.getItem(TAB_KEY);
    if (TABS.some((x) => x.id === t)) return t as Tab;
  } catch { /* storage blocked: the default */ }
  return "general";
}

export function SettingsView({ host }: { host: string }) {
  const [tab, setTab] = useState(storedTab);
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  // Clicks and openSettings(…, tab) both go through the event, so the view follows either.
  useEffect(() => {
    const on = (e: Event) => setTab((e as CustomEvent<Tab>).detail);
    window.addEventListener(TAB_EVENT, on);
    return () => window.removeEventListener(TAB_EVENT, on);
  }, []);
  const pick = selectSettingsTab;
  // WAI-ARIA tabs on a vertical list: ↑/↓/Home/End move focus and select.
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const i = TABS.findIndex((x) => x.id === tab);
    const next = ({ ArrowDown: i + 1, ArrowUp: i - 1, Home: 0, End: TABS.length - 1 } as Record<string, number>)[e.key];
    if (next === undefined) return;
    e.preventDefault();
    const j = (next + TABS.length) % TABS.length;
    pick(TABS[j].id);
    tabs.current[j]?.focus();
  };
  const remote = host !== LOCAL && (
    <p className="settings-note settings-host" role="note">
      Bu takım <b>{hostName(host)}</b> sunucusunda: bu sekmedeki ayarlar o sunucunun ayarlarıdır.
    </p>
  );
  // Panels stay mounted (hidden) so an unsaved field survives a tab switch.
  const panel = (id: Tab, body: ReactNode) => (
    <div className="settings-panel" role="tabpanel" id={`settings-${id}`} aria-labelledby={`settings-tab-${id}`} hidden={tab !== id}>
      {body}
    </div>
  );
  return (
    <div className="settings">
      <div className="settings-nav" role="tablist" aria-label="Ayar bölümleri" aria-orientation="vertical" onKeyDown={onKey}>
        {TABS.map((t, i) => (
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
        ))}
      </div>
      {panel("general", <>
        <DensitySection />
        {!MOBILE && <SoundsSection />}
        {host === LOCAL && <SnipSection />}
        {host === LOCAL && <UsageStripSection />}
      </>)}
      {panel("agents", <>
        {remote}
        <LimitsSection host={host} />
        <GpuSection host={host} />
      </>)}
      {panel("hosts", <HostsSection />)}
      {panel("providers", <GatewaySection host={host} />)}
      {panel("advanced", <>
        {remote}
        <p className="settings-note">Nadiren gerekenler; varsayılanlar çoğu kullanım için yeterli.</p>
        {host === LOCAL && <CoreSection />}
        <WorktreeCleanupSection host={host} />
        <FirstReplySection host={host} />
        <DiagnosticsSection host={host} />
        <ChainSection host={host} />
        {host === LOCAL && <TailnetSection />}
      </>)}
    </div>
  );
}

function WorktreeCleanupSection({ host }: { host: string }) {
  const { teams } = useTeams();
  const pending = teams.filter((t) => t.host === host).flatMap((t) => t.worktreeCleanup ?? []);
  if (!pending.length) return null;
  return (
    <section className="settings-section" aria-label="Bekleyen worktree temizliği">
      <h4>Bekleyen worktree temizliği</h4>
      {pending.map((path) => (
        <p className="settings-warn" key={path}>Temizlenemeyen worktree: <code>{path}</code> — elle silinebilir.</p>
      ))}
    </section>
  );
}

function LimitsSection({ host }: { host: string }) {
  const [view, setView] = useState<LimitsView | null>(null);
  const [form, setForm] = useState({ total: "", agent: "", mem: "" });
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const fill = (v: LimitsView) => {
    setView(v);
    setForm({ total: String(v.stored.totalCpuPercent), agent: String(v.stored.agentCpuPercent), mem: String(v.stored.memBytes / GIB) });
  };
  useEffect(() => {
    callHost<LimitsView>(host, "limits_get").then(fill, (e) => setMsg({ ok: false, text: String(e) }));
  }, [host]);

  async function save() {
    const limits: Limits = { totalCpuPercent: pct(form.total), agentCpuPercent: pct(form.agent), memBytes: Math.round((Number(form.mem) || 0) * GIB) };
    try {
      fill(await callHost<LimitsView>(host, "limits_set", { limits }));
      setMsg({ ok: true, text: "Kaydedildi. Toplam tavan çalışan ajanlara hemen uygulandı; ajan başı tavan ve bellek yeni açılan pane'lerde geçerli." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  const eff = view?.effective;
  return (
    <section className="settings-section" aria-label="Kaynak sınırları">
      <h4>Kaynak sınırları</h4>
      <p className="settings-note">
        Ajanların bilgisayarı ne kadar kullanabileceği. Tavana gelince hepsi birlikte yavaşlar; durdurulmaz, yük düşünce hızlanır.
      </p>
      <label className="settings-row">
        <span>Toplam CPU tavanı (tüm ajanlar)</span>
        <input type="number" min={0} max={100} value={form.total} onChange={(e) => setForm({ ...form, total: e.target.value })} />
        <em>% · 0 = sınırsız</em>
      </label>
      <label className="settings-row">
        <span>Ajan başı CPU tavanı</span>
        <input type="number" min={0} max={100} value={form.agent} onChange={(e) => setForm({ ...form, agent: e.target.value })} />
        <em>% · 0 = kapalı</em>
      </label>
      <label className="settings-row">
        <span>Ajan başı bellek tavanı</span>
        <input type="number" min={0} step={1} value={form.mem} onChange={(e) => setForm({ ...form, mem: e.target.value })} />
        <em>GiB · 0 = kapalı (aşılırsa ajan çökebilir)</em>
      </label>
      <div className="settings-actions">
        <button className="primary" disabled={!view} onClick={() => void save()}>Kaydet</button>
      </div>
      {view && view.env.length > 0 && eff && (
        <p className="settings-warn" role="status">
          Ortam değişkeni geçersiz kılıyor ({view.env.join(", ")}): etkin değerler toplam %{eff.totalCpuPercent || "∞"}, ajan başı %
          {eff.agentCpuPercent || "—"}, bellek {eff.memBytes ? `${(eff.memBytes / GIB).toFixed(1)} GiB` : "kapalı"}.
        </p>
      )}
      {view && !view.enforced && <p className="settings-warn">Bu platformda sınırlar henüz uygulanmıyor (yalnız Windows).</p>}
      {msg && <p className={msg.ok ? "settings-note" : "settings-err"} role={msg.ok ? "status" : "alert"}>{msg.text}</p>}
    </section>
  );
}

function TailnetSection() {
  const [view, setView] = useState<TailnetView | null>(null);
  const [port, setPort] = useState("7717");
  const [code, setCode] = useState<string | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [msg, setMsg] = useState<string | null>(null);
  const refresh = async () => {
    const next = await callHost<TailnetView>(LOCAL, "tailnet_get");
    setView(next); setPort(String(next.port));
    setDevices(next.listening ? await callHost<Device[]>(LOCAL, "tailnet_devices") : []);
  };
  useEffect(() => { void refresh().catch((e) => setMsg(String(e))); }, []);
  async function toggle(enabled: boolean) {
    try {
      const next = await callHost<TailnetView>(LOCAL, "tailnet_set", { enabled, port: Number(port) });
      setView(next); setPort(String(next.port)); setCode(null); setMsg(null);
      setDevices(next.listening ? await callHost<Device[]>(LOCAL, "tailnet_devices") : []);
    } catch (e) { setMsg(String(e)); void refresh().catch(() => {}); }
  }
  async function pair() {
    try { setCode(await callHost<string>(LOCAL, "tailnet_pair_code")); setMsg("Kod 10 dakika geçerli ve tek kullanımlık."); }
    catch (e) { setMsg(String(e)); }
  }
  async function revoke(id: string) {
    try { await callHost(LOCAL, "tailnet_device_revoke", { id }); await refresh(); }
    catch (e) { setMsg(String(e)); }
  }
  return <details className="settings-section settings-fold" aria-label="Bu bilgisayarı ağa aç" open={view?.enabled || undefined}>
    <summary>
      <b>Telefondan erişim (Tailscale)</b>
      <span>Bu bilgisayarı yalnız Tailscale ağınızdaki cihazlara açar; yerel ağa ve internete açılmaz.</span>
    </summary>
    <label className="settings-row settings-row-wide"><span>Port</span><input type="number" min={1} max={65535} value={port} disabled={view?.enabled} onChange={(e) => setPort(e.target.value)} /></label>
    <label className="settings-row settings-row-wide"><span>Ağ erişimi</span><input type="checkbox" checked={view?.enabled ?? false} onChange={(e) => void toggle(e.target.checked)} /></label>
    {view?.listening && <p className="settings-note" role="status">Dinliyor: <code>{view.listening}</code></p>}
    {(msg || view?.error) && <p className="settings-err" role="alert">{msg || view?.error}</p>}
    {view?.enabled && <>
      <div className="settings-actions"><button className="primary" onClick={() => void pair()}>Eşleme kodu üret</button></div>
      {code && <p className="settings-pair-code" role="status">{code}</p>}
      <p className="settings-note">Windows Güvenlik Duvarı engellerse PowerShell’i yönetici olarak açıp yalnız tailnet kaynaklarına izin veren şu komutu çalıştırın:</p>
      <code className="settings-command">{view.firewallCommand}</code>
      <h5>Eşlenmiş cihazlar</h5>
      {devices.length === 0 ? <p className="settings-note">Henüz cihaz eşlenmedi.</p> : <ul className="hosts">{devices.map((d) => <li className="host-row" key={d.id}><span>{d.name}</span><code>{d.id}</code><button onClick={() => void revoke(d.id)}>İptal et</button></li>)}</ul>}
    </>}
  </details>;
}
