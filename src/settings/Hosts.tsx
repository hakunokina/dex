// S4: Settings → "Sunucular". Pair a `dexd` (address + the one-time code `dexd pair` prints), see
// each host's connection, pair again, remove. Device tokens go to the OS keyring (src/api/hosts.ts).
import { useState } from "react";
import { addHost, hostInfo, LOCAL, removeHost, repairHost, useHosts, type HostView } from "../api/hosts";
import { describe, isOlder } from "../api/serverInfo";

export const HOST_STATUS_TEXT: Record<HostView["status"], string> = {
  connected: "bağlı",
  connecting: "bağlanıyor…",
  reconnecting: "yeniden bağlanıyor…",
  unauthorized: "yetkisiz — yeniden eşleyin",
  blocked: "adres engellendi — şifresiz bağlantı yalnız yerel/Tailscale; sunucuyu wss:// ile yeniden ekleyin",
  "no-token": "cihaz jetonu yok — yeniden eşleyin",
  down: "çekirdek çalışmıyor",
};

const DEVICE = "Dex masaüstü";

export function HostsSection() {
  const hosts = useHosts();
  const [form, setForm] = useState({ name: "", address: "", code: "", device: DEVICE });
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function add() {
    setBusy(true);
    setErr(null);
    try {
      await addHost(form.name, form.address, form.code.trim(), form.device.trim() || DEVICE);
      setForm({ name: "", address: "", code: "", device: form.device });
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-label="Sunucular">
      <h4>Sunucular</h4>
      <p className="settings-note">
        Takımı başka bir makinede çalıştırın: oturumları, hafızası ve kodu orada kalır. Yeni takım açarken sunucuyu seçersiniz.
      </p>
      {hosts.length > 0 && (
        <ul className="hosts">
          {hosts.map((h) => <HostRow key={h.rec.id} h={h} />)}
        </ul>
      )}
      <details className="settings-fold" open={hosts.length === 0}>
        <summary>
          <b>Sunucu ekle</b>
          <span>Sunucuda <code>dexd pair</code> çalıştırın, verdiği tek kullanımlık kodu girin.</span>
        </summary>
        <label className="settings-row settings-row-wide">
          <span>Ad</span>
          <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="ör. server.example.invalid" />
        </label>
        <label className="settings-row settings-row-wide">
          <span>Adres</span>
          <input value={form.address} onChange={(e) => setForm({ ...form, address: e.target.value })} placeholder="wss://server.example.invalid:7717" aria-describedby="hosts-address-hint" />
        </label>
        <p className="settings-note" id="hosts-address-hint">Uzak adreslerde wss:// kullanılır. Şifresiz ws:// yalnız yerel/Tailscale adreslerinde kullanılabilir.</p>
        <label className="settings-row settings-row-wide">
          <span>Eşleme kodu</span>
          <input value={form.code} onChange={(e) => setForm({ ...form, code: e.target.value })} placeholder="ABCD-EFGH" />
        </label>
        <label className="settings-row settings-row-wide">
          <span>Cihaz adı</span>
          <input value={form.device} onChange={(e) => setForm({ ...form, device: e.target.value })} />
        </label>
        <div className="settings-actions">
          <button className="primary" disabled={busy || !form.address.trim() || !form.code.trim()} onClick={() => void add()}>
            {busy ? "Eşleniyor…" : "Sunucu ekle"}
          </button>
        </div>
      </details>
      {err && <p className="settings-err" role="alert">{err}</p>}
    </section>
  );
}

function HostRow({ h }: { h: HostView }) {
  const [code, setCode] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const needsPair = h.status === "unauthorized" || h.status === "no-token";
  // #142: the host's build vs this app's core (pre-#142 dexd: no commit → older).
  const build = describe(h.info);
  const stale = isOlder(h.info, hostInfo(LOCAL));
  const act = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(null);
    try {
      await f();
      setCode("");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <li className="host-row">
      <span className={`host-dot ${h.status}`} aria-hidden />
      <b>{h.rec.name}</b>
      <code>{h.rec.url}</code>
      {build && <code className="host-build">{build}</code>}
      <span className="host-status" role="status">{HOST_STATUS_TEXT[h.status]}</span>
      {stale && (
        <span className="host-stale" role="note" title="Sunucudaki dexd bu uygulamadan eski: yeni özellikler o sunucunun takımlarında gizlenir">
          {h.rec.name} dexd eski: {h.info?.commit ?? "sürüm bilgisi yok"} — güncelle
        </span>
      )}
      {needsPair && (
        <span className="host-repair">
          <input value={code} onChange={(e) => setCode(e.target.value)} placeholder="yeni eşleme kodu" aria-label={`${h.rec.name} eşleme kodu`} />
          <button disabled={busy || !code.trim()} onClick={() => void act(() => repairHost(h.rec.id, code.trim(), DEVICE))}>Yeniden eşle</button>
        </span>
      )}
      <button
        className="danger"
        disabled={busy}
        title="Bağlantıyı, cihaz jetonunu ve bu sunucunun takımlarını bu pencereden kaldırır; sunucudaki ajanlar çalışmaya devam eder"
        onClick={() => {
          if (confirm(`"${h.rec.name}" kaldırılsın mı? Takımları bu pencereden kalkar, sunucuda çalışmaya devam eder.`)) void act(() => removeHost(h.rec.id));
        }}
      >
        Kaldır
      </button>
      {err && <p className="settings-err" role="alert">{err}</p>}
    </li>
  );
}
