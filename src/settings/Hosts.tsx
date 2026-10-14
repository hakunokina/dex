// Bu cihaz › Bağlantılar › Sunucular: "Bu bilgisayar (yerel çekirdek)" ilk satır,
// ardından uzak sunucular, en altta ekleme formu. Her zaman görünür; çağrılar
// zaten LOCAL'a gider, o yüzden uzak takımdan açılınca gizlenmez.
import { useEffect, useState } from "react";
import {
  addHost, callHost, hostInfo, LOCAL, removeHost, repairHost, useHosts, useLocalStatus, type HostView,
} from "../api/hosts";
import { UI_BUILD } from "../api/build";
import { describe, isOlder } from "../api/serverInfo";
import { StartCoreButton } from "../team/CoreBanner";
import type { CoreStatus } from "../api/status";
import { Row } from "./kit/Row";
import { Section } from "./kit/Section";
import { ConfirmDialog } from "./kit/ConfirmDialog";
import { FieldMsg } from "./kit/states";

export const HOST_STATUS_TEXT: Record<HostView["status"], string> = {
  connected: "bağlı",
  connecting: "bağlanıyor…",
  reconnecting: "yeniden bağlanıyor…",
  unauthorized: "yetkisiz — yeniden eşleyin",
  blocked: "adres engellendi — şifresiz bağlantı yalnız yerel/Tailscale; sunucuyu wss:// ile yeniden ekleyin",
  "no-token": "cihaz anahtarı yok — yeniden eşleyin",
  down: "çekirdek çalışmıyor",
};

const DEVICE = "Dex masaüstü";

export interface CoreLife {
  pid: number;
  commit: string;
  commitTime: number;
  startedAt: number;
  clients: number;
  graceSecs: number;
  shutdownInSecs: number | null;
  agents: number;
  draining?: boolean;
  updatePending?: boolean;
  drainBlockers?: string[];
}

const when = (secs: number) => (secs ? new Date(secs * 1000).toLocaleString("tr-TR") : "bilinmiyor");

export function ServersSection() {
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
    <Section
      label="Sunucular"
      description="Takımı başka bir makinede çalıştırın: oturumları, hafızası ve kodu orada kalır. Yeni takım açarken sunucuyu seçersiniz."
    >
      <LocalCoreItem />
      {hosts.map((h) => <HostItem key={h.rec.id} h={h} />)}
      <details className="set-fold" open={hosts.length === 0}>
        <summary>
          <b>Sunucu ekle</b>
          <span>Sunucuda <code>dexd pair</code> çalıştırın, verdiği tek kullanımlık kodu girin.</span>
        </summary>
        <div className="set-pad" style={{ gap: 8 }}>
          <Row
            label="Ad"
            control={<span className="set-field"><input value={form.name} aria-label="Ad" onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="ör. server.example.invalid" /></span>}
          />
          <Row
            label="Adres"
            description={<span id="hosts-address-hint">Uzak adreslerde wss:// kullanılır. Şifresiz ws:// yalnız yerel/Tailscale adreslerinde kullanılabilir.</span>}
            control={<span className="set-field"><input value={form.address} aria-label="Adres" aria-describedby="hosts-address-hint" onChange={(e) => setForm({ ...form, address: e.target.value })} placeholder="wss://server.example.invalid:7717" /></span>}
          />
          <Row
            label="Eşleme kodu"
            control={<span className="set-field"><input value={form.code} aria-label="Eşleme kodu" onChange={(e) => setForm({ ...form, code: e.target.value })} placeholder="ABCD-EFGH" /></span>}
          />
          <Row
            label="Cihaz adı"
            control={<span className="set-field"><input value={form.device} aria-label="Cihaz adı" onChange={(e) => setForm({ ...form, device: e.target.value })} /></span>}
          />
          <div className="set-pad">
            <span className="set-field-btns">
              <button type="button" className="primary" disabled={busy || !form.address.trim() || !form.code.trim()} onClick={() => void add()}>
                {busy ? "Eşleniyor…" : "Sunucu ekle"}
              </button>
            </span>
            {err && <FieldMsg ok={false} text={err} />}
          </div>
        </div>
      </details>
    </Section>
  );
}

function dot(status: HostView["status"]) {
  return <span className={`set-dot${status === "connected" ? " ok" : status === "unauthorized" || status === "no-token" || status === "down" ? " bad" : ""}`} aria-hidden />;
}

function LocalCoreItem() {
  const status = useLocalStatus();
  const [life, setLife] = useState<CoreLife | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [update, setUpdate] = useState(false);
  const [choose, setChoose] = useState(false);
  const [confirmShutdown, setConfirmShutdown] = useState(false);

  const refresh = () => {
    void callHost<{ updatePending?: boolean }>(LOCAL, "local_status").then(
      (s) => setUpdate(s.updatePending === true),
      () => setUpdate(false),
    );
    return callHost<CoreLife>(LOCAL, "core_life").then(
      (l) => setLife(l),
      (e) => {
        setLife(null);
        if (status === "connected") setMsg({ ok: false, text: String(e) });
      },
    );
  };
  useEffect(() => {
    setMsg(null);
    if (status === "connected") void refresh();
    else { setLife(null); setUpdate(false); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  async function copyDiagnostics() {
    setBusy(true);
    setMsg(null);
    try {
      const s = await callHost<CoreStatus>(LOCAL, "core_status");
      await navigator.clipboard.writeText(JSON.stringify(s, null, 2));
      setMsg({ ok: true, text: "Durum dökümü kopyalandı." });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function openLogs() {
    setBusy(true);
    setMsg(null);
    try {
      await callHost(LOCAL, "logs_open");
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function restart(whenIdle: boolean) {
    setBusy(true);
    setMsg(null);
    try {
      await callHost(LOCAL, "local_restart", { whenIdle });
      setChoose(false);
      void refresh();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function shutdown() {
    setBusy(true);
    setMsg(null);
    try {
      await callHost(LOCAL, "local_shutdown");
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
      setConfirmShutdown(false);
    }
  }

  const up = status === "connected";
  const state = !up
    ? (status === "down" ? "çalışmıyor" : "bağlanılıyor…")
    : life?.shutdownInSecs != null ? `kapanıyor (${life.shutdownInSecs} sn)` : "çalışıyor";
  const version = life
    ? `${life.commit} · ${when(life.commitTime)}${life.commit === UI_BUILD.commit ? "" : ` (arayüz ${UI_BUILD.commit})`}`
    : null;
  const meta = up
    ? [state, version, life ? `süreç ${life.pid}` : null, life ? `başlangıç ${when(life.startedAt)}` : null, life ? `pencere ${life.clients} · ajan ${life.agents}` : null]
      .filter(Boolean).join(" · ")
    : state;

  return (
    <div className="set-item" data-setting="local-core">
      <div className="set-item-main">
        <b>Bu bilgisayar (yerel çekirdek)</b>
        <p className="set-item-meta" role="status">{dot(up ? "connected" : "down")}{meta}</p>
        {life?.draining && <p className="set-note">Yeniden başlatma boşalınca gerçekleşecek{life.drainBlockers?.length ? ` (${life.drainBlockers.join(", ")})` : ""}.</p>}
        {msg && <FieldMsg ok={msg.ok} text={msg.text} />}
      </div>
      <div className="set-item-actions">
        {up ? (
          <>
            <button type="button" disabled={busy} onClick={() => void refresh()}>Yenile</button>
            <button type="button" disabled={busy} onClick={() => void copyDiagnostics()}>Durum dökümü</button>
            <button type="button" disabled={busy} onClick={() => void openLogs()}>Log klasörü</button>
            {update && !life?.draining && !choose && (
              <button type="button" onClick={() => setChoose(true)}>Güncelleme hazır — yeniden başlat</button>
            )}
            {choose && (
              <>
                <button type="button" disabled={busy} onClick={() => void restart(true)}>Boşalınca yeniden başlat</button>
                <button type="button" disabled={busy} onClick={() => void restart(false)} title="Koşan işler yarıda kalır">Şimdi yeniden başlat</button>
                <button type="button" disabled={busy} onClick={() => setChoose(false)}>Vazgeç</button>
              </>
            )}
            <button type="button" className="danger" disabled={busy} onClick={() => setConfirmShutdown(true)}>Çekirdeği kapat</button>
          </>
        ) : (
          status === "down" && <StartCoreButton />
        )}
      </div>
      {confirmShutdown && (
        <ConfirmDialog
          title="Yerel çekirdek kapatılsın mı?"
          lines={[
            `Bu bilgisayarda çalışan ${life?.agents ?? "bilinmeyen sayıda"} ajan kapanır.`,
            "Açık pencereler 'Çekirdeği başlat' ile yeniden açabilir.",
          ]}
          confirmLabel="Çekirdeği kapat"
          busy={busy}
          onCancel={() => setConfirmShutdown(false)}
          onConfirm={() => void shutdown()}
        />
      )}
    </div>
  );
}

function HostItem({ h }: { h: HostView }) {
  const [code, setCode] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const needsPair = h.status === "unauthorized" || h.status === "no-token";
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
    <div className="set-item">
      <div className="set-item-main">
        <b>{h.rec.name}</b>
        <p className="set-item-meta">
          {dot(h.status)}
          <code>{h.rec.url}</code>
          {build && <code>{build}</code>}
          <span role="status">{HOST_STATUS_TEXT[h.status]}</span>
        </p>
        {stale && (
          <p className="set-warn" role="note">
            {h.rec.name} dexd eski: {h.info?.commit ?? "sürüm bilgisi yok"} — güncelleyin.
          </p>
        )}
        {needsPair && (
          <span className="set-field">
            <input
              value={code} aria-label={`${h.rec.name} eşleme kodu`} placeholder="yeni eşleme kodu"
              onChange={(e) => setCode(e.target.value)} />
            <button type="button" disabled={busy || !code.trim()} onClick={() => void act(() => repairHost(h.rec.id, code.trim(), DEVICE))}>
              Yeniden eşle
            </button>
          </span>
        )}
        {err && <FieldMsg ok={false} text={err} />}
      </div>
      <div className="set-item-actions">
        <button type="button" className="danger" disabled={busy} onClick={() => setConfirmRemove(true)}>Kaldır</button>
      </div>
      {confirmRemove && (
        <ConfirmDialog
          title={`"${h.rec.name}" kaldırılsın mı?`}
          lines={[
            "Bağlantı ve cihaz anahtarı silinir; bu sunucunun takımları bu pencereden kalkar.",
            "Sunucudaki ajanlar çalışmaya devam eder.",
          ]}
          confirmLabel="Kaldır"
          busy={busy}
          onCancel={() => setConfirmRemove(false)}
          onConfirm={() => void act(() => removeHost(h.rec.id)).then(() => setConfirmRemove(false))}
        />
      )}
    </div>
  );
}
