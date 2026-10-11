import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { LOCAL, onEvent, onHostUp } from "../api/hosts";
import { gatewayError, type GatewayStatus } from "./gatewayView";
import { gatewayLogout, gatewayModels, gatewayStart, gatewayStatus } from "./gatewayApi";

export function GatewaySection({ host }: { host: string }) {
  if (host !== LOCAL) return <section className="settings-section gateway" aria-label="ChatGPT ile giriş (Claude Code + OpenAI)">
    <h4>ChatGPT ile giriş (Claude Code + OpenAI)</h4>
    <p className="settings-note">Giriş yalnız yerel Dex’ten yapılabilir. Bu sunucunun hesabını kendi bilgisayarındaki Dex’ten yönetin.</p>
  </section>;
  return <GatewayLocal />;
}

function GatewayLocal() {
  const [view, setView] = useState<GatewayStatus | null>(null);
  const [revision, setRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [loginExpiry, setLoginExpiry] = useState<number | null>(null);
  const [browserFailed, setBrowserFailed] = useState(false);
  const loadedAccount = useRef<string | null>(null);
  const lock = useRef(false);

  useEffect(() => {
    const refresh = (_event: unknown, host: string) => { if (host === LOCAL) { setError(null); setRevision(n => n + 1); } };
    const offAuth = onEvent("gateway-auth-changed", (event, host) => {
      if (host === LOCAL) loadedAccount.current = null;
      refresh(event, host);
    });
    const offProviders = onEvent("providers-changed", refresh);
    const offHost = onHostUp(host => { if (host === LOCAL) { setError(null); setRevision(n => n + 1); } });
    return () => { offAuth(); offProviders(); offHost(); };
  }, []);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const next = await gatewayStatus(LOCAL);
        if (!live) return;
        setView(next);
        if (next.state !== "signing_in") { setLoginExpiry(null); setBrowserFailed(false); }
        if (next.state !== "ready") { loadedAccount.current = null; setCatalogLoading(false); return; }
        if (loadedAccount.current === next.activeAccountId) return;
        // Only a successful catalog load is cached; a cancelled effect can retry it.
        setCatalogLoading(true);
        const models = await gatewayModels(LOCAL);
        const current = await gatewayStatus(LOCAL);
        if (live) {
          loadedAccount.current = current.state === "ready" ? current.activeAccountId : null;
          setView(current.state === "ready" && current.activeAccountId === next.activeAccountId ? { ...current, models } : current);
          setCatalogLoading(false);
        }
      } catch (e) {
        if (!live) return;
        setError(gatewayError(e));
        setCatalogLoading(false);
        // A failed catalog fetch can revoke the grant (401); reflect that immediately.
        try { const current = await gatewayStatus(LOCAL); if (live) setView(current); } catch { /* the visible error already explains the failed connection */ }
      }
    })();
    return () => { live = false; };
  }, [revision]);

  async function run(action: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError(null); setNote(null);
    try { await action(); }
    catch (e) { setError(gatewayError(e)); }
    finally { lock.current = false; setBusy(false); setRevision(n => n + 1); }
  }

  const account = view?.accounts.find(a => a.id === view.activeAccountId);
  const signing = view?.state === "signing_in";
  const connected = account?.signedIn ?? false;
  const accountExpiry = account?.expiresAt && Number.isFinite(account.expiresAt) ? new Date(account.expiresAt).toLocaleString("tr-TR") : null;
  return <section className="settings-section gateway" aria-label="ChatGPT ile giriş (Claude Code + OpenAI)" aria-busy={busy || catalogLoading || (!view && !error)}>
    <h4>ChatGPT ile giriş (Claude Code + OpenAI)</h4>
    <p className="settings-note">Claude Code’u ChatGPT hesabınızdaki modellerle kullanın. Giriş sistem tarayıcısında tamamlanır.</p>
    <p role="status">{!view ? error ? "Durum alınamadı" : "Durum yükleniyor…" : signing ? "Tarayıcıda girişi tamamlayın" : connected ? "Bağlı" : "Giriş yok"}</p>
    {account && <dl className="gateway-account">
      <dt>Hesap</dt><dd>{account.label}{account.email && <> · {account.email}</>}</dd>
      {account.plan && <><dt>Plan</dt><dd>{account.plan}</dd></>}
      {accountExpiry && <><dt>Jeton süresi</dt><dd>{accountExpiry}</dd></>}
    </dl>}
    {signing && loginExpiry && <p className="settings-note">Giriş bağlantısı {new Date(loginExpiry).toLocaleTimeString("tr-TR")} saatine kadar geçerli.</p>}
    <div className="settings-actions">
      <button className="primary" disabled={busy || (!!signing && !browserFailed)} onClick={() => void run(async () => {
        const started = await gatewayStart(LOCAL);
        setLoginExpiry(started.expiresAt); setBrowserFailed(false);
        setRevision(n => n + 1);
        try {
          const url = new URL(started.authorizationUrl);
          if (url.origin !== "https://auth.openai.com" || url.username || url.password) throw new Error("invalid authorization URL");
          await openUrl(url.href);
        } catch {
          setBrowserFailed(true); setError("Tarayıcı açılamadı. ChatGPT ile giriş düğmesiyle yeniden deneyin.");
        }
      })}>{connected ? "Başka hesapla giriş" : "ChatGPT ile giriş"}</button>
      <button disabled={busy || catalogLoading} onClick={() => void run(async () => {
        if (view?.state === "ready") await gatewayModels(LOCAL);
      })}>Durumu yenile</button>
      {account?.signedIn && <button disabled={busy} onClick={() => void run(async () => {
        const out = await gatewayLogout(LOCAL, account.id);
        loadedAccount.current = null;
        setNote(out.remoteRevoked ? "Çıkış yapıldı." : "Yerel çıkış yapıldı. Uzak izin kaldırılamadı; ChatGPT ayarlarındaki bağlı uygulamalar bölümünden Dex iznini kaldırın.");
      })}>Çıkış yap</button>}
    </div>
    {view?.state === "ready" && <>
      <h5>Hesaptaki modeller</h5>
      {catalogLoading ? <p className="settings-note" role="status">Model listesi yükleniyor…</p> : view.models.length ? <ul>{view.models.map(m => <li key={m.id}>{m.label}</li>)}</ul> : <p className="settings-note">Bu hesapta kullanılabilir model yok. Durumu yenileyin veya hesap erişiminizi kontrol edin.</p>}
    </>}
    {(error || view?.error) && <p className="settings-err" role="alert">{error || gatewayError(view?.error)}</p>}
    {note && <p className="settings-note" role="status">{note}</p>}
  </section>;
}
