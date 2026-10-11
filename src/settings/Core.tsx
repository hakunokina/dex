// #151: Ayarlar → Gelişmiş → Çekirdek: the local core (`dexd --local`) — its process, build, start,
// connected windows (dexd's `core_life`) — and "Çekirdeği kapat" (the shell's `local_shutdown`).
// #494: a dev update is only told ("Güncelleme hazır"); the patron restarts — once drained or now.
import { useEffect, useState } from "react";
import { callHost, LOCAL, useLocalStatus } from "../api/hosts";
import { UI_BUILD } from "../api/build";
import { StartCoreButton } from "../team/CoreBanner";

export interface CoreLife {
  pid: number;
  commit: string;
  commitTime: number;
  startedAt: number;
  clients: number;
  graceSecs: number;
  shutdownInSecs: number | null;
  agents: number;
  /** #161: a restart waits for the core to drain; what still runs. (Absent: an older core.) */
  draining?: boolean;
  /** #400: dexd's quiet wait (no longer asked for, #494); drainBlockers says what a drain waits on. */
  updatePending?: boolean;
  drainBlockers?: string[];
}

const when = (secs: number) => (secs ? new Date(secs * 1000).toLocaleString("tr-TR") : "bilinmiyor");

export function CoreSection() {
  const status = useLocalStatus();
  const [life, setLife] = useState<CoreLife | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  // #494: the shell found this core outdated (dev); `choose`: the restart options are shown.
  const [update, setUpdate] = useState(false);
  const [choose, setChoose] = useState(false);
  const refresh = () => {
    void callHost<{ updatePending?: boolean }>(LOCAL, "local_status").then(
      (s) => setUpdate(s.updatePending === true),
      () => setUpdate(false),
    );
    return callHost<CoreLife>(LOCAL, "core_life").then(
      (l) => setLife(l),
      (e) => {
        setLife(null);
        // An older core has no core_life: the rest still works.
        if (status === "connected") setMsg(String(e));
      },
    );
  };
  useEffect(() => {
    setMsg(null);
    if (status === "connected") void refresh();
    else {
      setLife(null);
      setUpdate(false);
    }
  }, [status]);

  async function shutdown() {
    if (!confirm("Çekirdek kapatılsın mı? Bu bilgisayarda çalışan tüm ajanlar kapanır; açık pencereler 'Çekirdeği başlat' ile yeniden açabilir.")) return;
    setBusy(true);
    setMsg(null);
    try {
      await callHost(LOCAL, "local_shutdown");
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
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
      setMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const up = status === "connected";
  const state = !up ? (status === "down" ? "çalışmıyor" : "bağlanılıyor…") : life?.shutdownInSecs != null ? `kapanıyor (${life.shutdownInSecs} sn)` : "çalışıyor";
  const row = (label: string, value: string | number) => (
    <div className="settings-row settings-row-wide">
      <span>{label}</span>
      <span className="settings-value">{value}</span>
    </div>
  );
  return (
    <section className="settings-section" aria-label="Çekirdek">
      <b>Çekirdek</b>
      <p className="settings-note">
        Ajanları çalıştıran arka plan süreci. Son pencere kapanınca {life?.graceSecs ?? 120} sn daha çalışır, sonra ajanlarla birlikte kapanır.
      </p>
      {row("Durum", state)}
      {life && (
        <>
          {row("Süreç (PID)", life.pid)}
          {row("Sürüm", `${life.commit} · ${when(life.commitTime)}${life.commit === UI_BUILD.commit ? "" : ` (arayüz ${UI_BUILD.commit})`}`)}
          {row("Başlangıç", when(life.startedAt))}
          {row("Bağlı pencere", life.clients)}
          {row("Çalışan ajan", life.agents)}
          {life.draining && row("Yeniden başlatma", `boşalınca${life.drainBlockers?.length ? ` (${life.drainBlockers.join(", ")})` : ""}`)}
        </>
      )}
      {up && update && !life?.draining && (
        <div className="settings-actions">
          {choose ? (
            <>
              <button disabled={busy} onClick={() => void restart(true)}>Boşalınca yeniden başlat</button>
              <button disabled={busy} onClick={() => void restart(false)} title="Koşan işler yarıda kalır">Şimdi yeniden başlat</button>
              <button disabled={busy} onClick={() => setChoose(false)}>Vazgeç</button>
            </>
          ) : (
            <button onClick={() => setChoose(true)}>Güncelleme hazır — yeniden başlatmak için tıkla</button>
          )}
        </div>
      )}
      <div className="settings-actions">
        {up ? (
          <>
            <button disabled={busy} onClick={() => void refresh()}>Yenile</button>
            <button className="danger" disabled={busy} onClick={() => void shutdown()}>
              {busy ? "Kapatılıyor…" : "Çekirdeği kapat"}
            </button>
          </>
        ) : (
          status === "down" && <StartCoreButton />
        )}
      </div>
      {msg && <p className="settings-err" role="alert">{msg}</p>}
    </section>
  );
}
