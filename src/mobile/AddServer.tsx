// S5-A: first-open pairing screen (and the Sunucular sheet): name, server address, the one-time
// `dexd pair` code. The token that comes back goes to the Android Keystore bridge
// (src-tauri/src/mobile_hosts.rs) — never localStorage. Also asks for the Android 13+ notification
// permission here, in context, so the background "waiting"/"done" notifications can fire later.
import { useState } from "react";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { addHost, removeHost, repairHost, useHosts } from "../api/hosts";
import { HOST_STATUS_TEXT } from "../settings/Hosts";

const DEVICE = "Dex telefon";

export function AddServer({ onDone, first }: { onDone: () => void; first?: boolean }) {
  const hosts = useHosts();
  const [form, setForm] = useState({ name: "", address: "", code: "" });
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function add() {
    setBusy(true);
    setErr(null);
    try {
      await addHost(form.name, form.address, form.code.trim(), DEVICE);
      setForm({ name: "", address: "", code: "" });
      // In-context ask: the first pairing is when the user learns why notifications exist.
      if (!(await isPermissionGranted().catch(() => true))) await requestPermission().catch(() => {});
      onDone();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="m-add">
      {first ? (
        <header className="m-add-hero">
          <img src="/dex-icon.svg" alt="" />
          <h1>DEX</h1>
          <p>
            Telefon bu ağda yalnız izleyici: takımlar sunucuda çalışır. Bağlanmak için sunucuda
            <code> dexd pair</code> çalıştırıp verdiği kodu girin.
          </p>
        </header>
      ) : (
        <h2>Sunucular</h2>
      )}

      {hosts.length > 0 && (
        <ul className="m-hosts">
          {hosts.map((h) => (
            <HostRow key={h.rec.id} id={h.rec.id} name={h.rec.name} url={h.rec.url} status={h.status} />
          ))}
        </ul>
      )}

      {!first && <h3 className="m-add-title">Yeni sunucu ekle</h3>}
      <label>
        <span>Ad</span>
        <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="ör. sunucu" autoCapitalize="off" />
      </label>
      <label>
        <span>Adres</span>
        <input
          value={form.address}
          onChange={(e) => setForm({ ...form, address: e.target.value })}
          placeholder="wss://server.example.invalid:7717"
          aria-describedby="mobile-address-hint"
          inputMode="url"
          autoCapitalize="off"
          autoCorrect="off"
        />
      </label>
      <p className="m-hint" id="mobile-address-hint">Uzak adreslerde wss:// kullanılır. Şifresiz ws:// yalnız yerel/Tailscale adreslerinde kullanılabilir.</p>
      <label>
        <span>Eşleme kodu</span>
        <input value={form.code} onChange={(e) => setForm({ ...form, code: e.target.value })} placeholder="ABCD-EFGH" autoCapitalize="characters" autoCorrect="off" />
      </label>
      <button className="m-primary" disabled={busy || !form.address.trim() || !form.code.trim()} onClick={() => void add()}>
        {busy ? "Eşleniyor…" : "Sunucu ekle"}
      </button>
      {err && <p className="m-err" role="alert">{err}</p>}
    </div>
  );
}

function HostRow({ id, name, url, status }: { id: string; name: string; url: string; status: keyof typeof HOST_STATUS_TEXT }) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const needsPair = status === "unauthorized" || status === "no-token";
  const act = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(null);
    try {
      await f();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <li className="m-host">
      <span className={`m-host-dot ${status}`} />
      <div className="m-host-main">
        <b>{name}</b>
        <code>{url}</code>
        <span className="m-host-status">{HOST_STATUS_TEXT[status]}</span>
        {needsPair && (
          <span className="m-repair">
            <input value={code} onChange={(e) => setCode(e.target.value)} placeholder="yeni kod" autoCapitalize="characters" autoCorrect="off" aria-label={`${name} eşleme kodu`} />
            <button disabled={busy || !code.trim()} onClick={() => void act(() => repairHost(id, code.trim(), DEVICE))}>Yeniden eşle</button>
          </span>
        )}
        {err && <p className="m-err" role="alert">{err}</p>}
      </div>
      <button
        className="m-danger"
        disabled={busy}
        onClick={() => {
          if (confirm(`"${name}" kaldırılsın mı? Takımları telefondan düşer; sunucuda çalışmaya devam eder.`)) void act(() => removeHost(id));
        }}
      >
        Kaldır
      </button>
    </li>
  );
}
