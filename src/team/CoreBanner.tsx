// #151: the local core (`dexd --local`) on the stage — down ("Çekirdeği başlat", like a remote
// host's band) and a build that is not this UI's ("Çekirdek güncel değil"). #161: restart it as this
// app's build — once drained (jobs wait in the queue until nothing runs) or now; leaders come back.
import { useState } from "react";
import { callHost, hostInfo, LOCAL, startLocalCore, useLocalDownReason, type HostView } from "../api/hosts";
import { coreDrift, type Drift } from "../api/serverInfo";
import { UI_BUILD } from "../api/build";

/** "Çekirdeği başlat": finds or starts the core; its error stays next to the button. */
export function StartCoreButton({ className }: { className?: string }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const start = async () => {
    setBusy(true);
    setErr(null);
    try {
      await startLocalCore();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {err && <span className="core-start-err" role="alert">{err}</span>}
      <button className={className} disabled={busy} onClick={() => void start()}>
        {busy ? "Başlatılıyor…" : "Çekirdeği başlat"}
      </button>
    </>
  );
}

/** #203: why the local core is down (the shell's last start failure: exit code, dexd's words, log path). */
export function CoreDownReason() {
  const why = useLocalDownReason();
  return why ? <span className="core-down-why" title={why}>{why}</span> : null;
}

/** The local team's band while its core is not connected. */
export function LocalCoreBanner({ status }: { status: HostView["status"] }) {
  const down = status === "down";
  return (
    <div className="host-banner" role="status">
      {down ? "Yerel çekirdek çalışmıyor — bu bilgisayardaki ajanlar kapalı." : "Yerel çekirdeğe bağlantı koptu — yeniden bağlanılıyor."}
      {down && <CoreDownReason />}
      {down && <StartCoreButton />}
    </div>
  );
}

const DRIFT_TEXT: Record<Drift, string> = {
  older: "çalışan çekirdek bu arayüzden eski",
  newer: "çalışan çekirdek bu arayüzden yeni",
  other: "çalışan çekirdek bu arayüzle aynı derlemeden değil",
};

/** The connected local core's build vs this UI's; null when they match (or can't tell). */
export function localDrift(): { drift: Drift; commit: string } | null {
  const info = hostInfo(LOCAL);
  const drift = coreDrift(info, UI_BUILD);
  return drift && { drift, commit: info?.commit ?? "sürüm bilgisi yok" };
}

export function CoreDriftBanner({ drift, commit, onDismiss }: { drift: Drift; commit: string; onDismiss: () => void }) {
  const [draining, setDraining] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const act = async (cmd: string, args: Record<string, unknown>, after: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await callHost(LOCAL, cmd, args);
      setDraining(after);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  // A newer core than this UI: restarting would bring this (older) build back.
  const update = drift !== "newer";
  return (
    <div className="host-banner" role="status" title="Yeni özellikler eksik ya da hatalı çalışabilir; çekirdek yeniden başlayınca bu sürüm açılır. Kuyruktaki işler korunur, liderler kaldıkları oturumla geri gelir.">
      <span>
        Çekirdek güncel değil: {DRIFT_TEXT[drift]} (<code>{commit}</code> · arayüz <code>{UI_BUILD.commit}</code>)
        {draining && " — boşalınca yeniden başlayacak; yeni işler kuyrukta bekliyor"}
      </span>
      <span className="core-drift-actions">
        {err && <span className="core-start-err" role="alert">{err}</span>}
        {update && !draining && (
          <>
            <button disabled={busy} onClick={() => void act("local_restart", { whenIdle: true }, true)}>Boşalınca yeniden başlat</button>
            <button disabled={busy} onClick={() => void act("local_restart", { whenIdle: false }, false)} title="Koşan işler yarıda kalır">Şimdi</button>
          </>
        )}
        {draining && <button disabled={busy} onClick={() => void act("local_drain_cancel", {}, false)}>Vazgeç</button>}
        <button onClick={onDismiss} aria-label="Çekirdek sürüm uyarısını gizle">Gizle</button>
      </span>
    </div>
  );
}
