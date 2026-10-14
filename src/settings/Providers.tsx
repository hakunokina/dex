// Sunucu › Sağlayıcılar: anahtar, adres ve model kaydı sunucu düzeyinde saklanır
// (provider_list(host), keyring `dex-provider`). Takım panelinden buraya taşındı:
// aynı sunucudaki her takım aynı anahtarları paylaşır, takıma özel anahtar yok.
import { useEffect, useRef, useState } from "react";
import { Trash2 } from "lucide-react";
import { callHost, LOCAL, onEvent } from "../api/hosts";
import {
  importProviders, listProviders, localAdd, localDelete, localStatus, setLocalBaseUrl,
  setProviderToken, type LocalStatus, type ProviderImportRow, type ProviderInfo,
} from "../team/api";
import { FirstReplyField } from "./FirstReply";
import { Row, Pad } from "./kit/Row";
import { Section } from "./kit/Section";
import { SecretField } from "./kit/SecretField";
import { TextField } from "./kit/TextField";
import { ConfirmDialog } from "./kit/ConfirmDialog";
import { providerScopeKey } from "./providerScope";
import { ErrorBand, FieldMsg, LoadingRows } from "./kit/states";

type Secs = Record<string, number>;

export function ProvidersSection({ host, hostLabel }: { host: string; hostLabel: string }) {
  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [firstReply, setFirstReply] = useState<Secs | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<ProviderImportRow[] | null>(null);
  const [importMsg, setImportMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const request = useRef(0);

  const reload = async () => {
    const n = ++request.current;
    try {
      // Gecikme uyarısı haritası kart başına değil bir kez alınır (N+1 yok).
      const [ps, secs] = await Promise.all([
        listProviders(host),
        callHost<Secs>(host, "first_reply_get").catch(() => null),
      ]);
      if (request.current !== n) return;
      setProviders(ps);
      setFirstReply(secs);
      setError(null);
      const lp = ps.find((p) => p.id === "local");
      const st = lp?.baseUrl && lp.hasToken ? await localStatus(host).catch(() => null) : null;
      if (request.current === n) setLocal(st);
    } catch (e) {
      if (request.current === n) setError(String(e));
    }
  };
  useEffect(() => {
    setProviders(null);
    setLocal(null);
    setFirstReply(null);
    void reload();
    const off = onEvent("providers-changed", (_, eventHost) => { if (eventHost === host) void reload(); });
    return () => { off(); request.current++; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  async function importRoutes() {
    try {
      setRows(await importProviders(host));
      setImportMsg(null);
      void reload();
    } catch (e) {
      setImportMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }

  const anthropic = providers?.find((p) => p.id === "anthropic");
  const rest = (providers ?? []).filter((p) => p.id !== "anthropic" && p.id !== "local");
  const localProvider = providers?.find((p) => p.id === "local");

  return (
    <Section
      label="Sağlayıcılar"
      description={host === LOCAL
        ? "Anahtarlar bu bilgisayarın kilit deposunda (OS keyring) saklanır."
        : `Anahtarlar ${hostLabel} sunucusunda saklanır; bu bilgisayara inmez.`}
    >
      <Pad>
        <span className="set-field-btns">
          <button type="button" onClick={() => void importRoutes()}>routes.conf'tan içe aktar</button>
        </span>
        {importMsg && <FieldMsg ok={importMsg.ok} text={importMsg.text} />}
        {rows && (
          <ul>
            {rows.map((r) => (
              <li key={r.provider}>
                <b>{r.provider}</b>: {r.token ? `anahtar ${r.token}` : "anahtar alınamadı"} — {r.note}
              </li>
            ))}
          </ul>
        )}
      </Pad>
      {error && <Pad><ErrorBand text={`Sağlayıcılar okunamadı: ${error}`} onRetry={() => void reload()} /></Pad>}
      {!providers && !error && <LoadingRows />}
      {anthropic && <ProviderCard key={providerScopeKey(host, "anthropic")} host={host} p={anthropic} builtIn initialSecs={firstReply?.anthropic} onSaved={() => void reload()} />}
      {rest.map((p) => <ProviderCard key={providerScopeKey(host, p.id)} host={host} p={p} initialSecs={firstReply?.[p.id]} onSaved={() => void reload()} />)}
      {localProvider && (
        <LocalCard
          key={providerScopeKey(host, "local")}
          host={host}
          provider={localProvider}
          status={local}
          setStatus={setLocal}
          initialSecs={firstReply?.local}
          reload={() => void reload()}
        />
      )}
    </Section>
  );
}

function ProviderCard({ host, p, builtIn, initialSecs, onSaved }: {
  host: string;
  p: ProviderInfo;
  builtIn?: boolean;
  initialSecs?: number | null;
  onSaved: () => void;
}) {
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  async function save(token: string) {
    try {
      await setProviderToken(host, p.id, token);
      setMsg(null);
      onSaved();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }
  return (
    <div data-setting={`provider-${p.id}`}>
      <Row
        label={p.label}
        description={builtIn ? "Yerleşik · anahtar gerekmez." : (p.baseUrl ?? undefined)}
        control={builtIn ? <span className="set-value">—</span> : (
          <SecretField
            label={`${p.label} API anahtarı`}
            hasSecret={p.hasToken}
            mask={p.tokenMask}
            onSave={(t) => void save(t)}
          />
        )}
        feedback={msg && <FieldMsg ok={msg.ok} text={msg.text} />}
      />
      <Row
        label="Yanıt gecikmesi uyarısı"
        description="Ajan bu süre sessiz kalırsa 'yanıt gecikiyor' işareti çıkar."
        control={<FirstReplyField host={host} provider={p.id} label={p.label} initial={initialSecs} />}
      />
    </div>
  );
}

function LocalCard({ host, provider, status, setStatus, initialSecs, reload }: {
  host: string;
  provider: ProviderInfo;
  status: LocalStatus | null;
  setStatus: (s: LocalStatus | null) => void;
  initialSecs?: number | null;
  reload: () => void;
}) {
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [confirmModel, setConfirmModel] = useState<string | null>(null);

  async function saveKey(token: string) {
    try {
      await setProviderToken(host, "local", token);
      setMsg(null);
      reload();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }
  async function saveUrl(url: string) {
    try {
      await setLocalBaseUrl(host, url);
      setMsg(null);
      reload();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  }
  async function test() {
    setBusy(true);
    setMsg(null);
    try {
      setStatus(await localStatus(host));
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }
  async function addModel() {
    if (!name.trim()) return;
    setBusy(true);
    setMsg(null);
    try {
      setStatus(await localAdd(host, name.trim()));
      setName("");
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div data-setting="provider-local">
      <Row
        label="Yerel LLM"
        control={
          <SecretField
            label="Yerel LLM API anahtarı"
            hasSecret={provider.hasToken}
            mask={provider.tokenMask}
            onSave={(t) => void saveKey(t)}
          />
        }
        feedback={msg && <FieldMsg ok={msg.ok} text={msg.text} />}
      />
      <Row
        label="Adres"
        control={
          <span className="set-field">
            <TextField
              label="Yerel LLM adresi"
              value={provider.baseUrl ?? ""}
              placeholder="http://192.0.2.10:4000"
              onSave={(v) => void saveUrl(v)}
            />
            <button
              type="button"
              disabled={!provider.baseUrl || !provider.hasToken || busy}
              title={!provider.hasToken ? "Önce anahtar gir" : ""}
              onClick={() => void test()}
            >
              {busy ? "Deneniyor…" : "Bağlantıyı test et"}
            </button>
          </span>
        }
      />
      <details className="set-fold">
        <summary><b>Modeller ({status?.models.length ?? 0})</b><span>Harici: LiteLLM</span></summary>
        <div className="set-pad">
          {!status ? (
            <p className="set-note">Bağlantıyı test edin ya da adres ve anahtarı girin.</p>
          ) : (
            <>
              <p className="set-note" role="status">
                Bağlı · {status.models.length} model · kayıt {status.registry ? "açık" : "kapalı (sunucuda LiteLLM veritabanı yok)"}
              </p>
              <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 8 }}>
                {status.models.map((m) => (
                  <li key={m.id} className="set-field">
                    <code>{m.id}</code>
                    {m.backend && <span className="set-note">{m.backend}</span>}
                    <button
                      type="button"
                      className="set-icon-btn"
                      disabled={!m.removable || busy}
                      title={m.removable ? "Sunucudan sil" : "Sunucunun config dosyasında — Dex'ten silinmez"}
                      aria-label={`${m.id} modelini sil`}
                      onClick={() => setConfirmModel(m.id)}
                    >
                      <Trash2 />
                    </button>
                  </li>
                ))}
              </ul>
              <span className="set-field">
                <input
                  value={name} aria-label="Kaydedilecek model adı" placeholder="model adı (motordaki kimlik)"
                  onChange={(e) => setName(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void addModel(); } }}
                />
                <button type="button" className="primary" disabled={busy || !name.trim()} onClick={() => void addModel()}>Model ekle</button>
              </span>
            </>
          )}
        </div>
      </details>
      <Row
        label="Yanıt gecikmesi uyarısı"
        description="Ajan bu süre sessiz kalırsa 'yanıt gecikiyor' işareti çıkar."
        control={<FirstReplyField host={host} provider="local" label="Yerel LLM" initial={initialSecs} />}
      />
      {confirmModel && (
        <Pad>
          <ConfirmDialog
            title={`${confirmModel} sunucudan silinsin mi?`}
            lines={["Model LiteLLM kaydından kaldırılır.", "Config dosyasındaki modeller Dex'ten silinmez."]}
            confirmLabel="Modeli sil"
            busy={busy}
            onCancel={() => setConfirmModel(null)}
            onConfirm={() => {
              const id = confirmModel;
              setConfirmModel(null);
              void (async () => {
                setBusy(true);
                try { setStatus(await localDelete(host, id)); }
                catch (e) { setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) }); }
                finally { setBusy(false); }
              })();
            }}
          />
        </Pad>
      )}
    </div>
  );
}
