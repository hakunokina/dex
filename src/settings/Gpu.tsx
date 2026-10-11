// #290: Settings → Ajanlar → "GPU kaynağı". The patron's view of the GPU broker dexd talks to
// (crates/dex-core/src/gpu.rs): addresses, a write-only token (dexd keeps it in its secret store and
// only says `tokenSet`), whether it answers, which models it serves and who may use them.
// #305: models in three groups — ajana açık, servis ediliyor (patronun), yalnız diskte (folded: ~50 rows).
import { useEffect, useRef, useState, type FormEvent } from "react";
import { callHost } from "../api/hosts";
import { configPatch, connectionLine, limitsPatch, modelGroups, type GpuConfig, type GpuLimits, type GpuLimitsForm, type GpuModel, type GpuOverview } from "../gpu/view";

export function GpuSection({ host }: { host: string }) {
  const [config, setConfig] = useState<GpuConfig | null>(null);
  const [view, setView] = useState<GpuOverview | null>(null);
  const [form, setForm] = useState({ primary: "", fallback: "", token: "" });
  const [editToken, setEditToken] = useState(false);
  const [busy, setBusy] = useState(false);
  const [limitsRevision, setLimitsRevision] = useState(0);
  const [err, setErr] = useState<string | null>(null);

  const fill = (c: GpuConfig) => {
    setConfig(c);
    setForm({ primary: c.primary, fallback: c.fallback, token: "" });
    setEditToken(!c.tokenSet);
  };
  const look = async (refresh: boolean) => setView(await callHost<GpuOverview>(host, "gpu_overview", { refresh }));
  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    setErr(null);
    try { await f(); } catch (e) { setErr(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); }
  };
  useEffect(() => {
    void run(async () => {
      fill(await callHost<GpuConfig>(host, "gpu_config_get"));
      await look(false);
    });
  }, [host]);

  const save = () => run(async () => {
    if (!config) return;
    fill(await callHost<GpuConfig>(host, "gpu_config_set", { config: configPatch(config, form) }));
    setLimitsRevision((revision) => revision + 1);
    await look(true);
  });

  const line = view && connectionLine(view);
  const s = view?.authorized ? view.status : null;
  return (
    <section className="settings-section" aria-label="GPU kaynağı">
      <h4>GPU kaynağı</h4>
      <p className="settings-note">
        Ev GPU makinesi (görsel üretim, arka plan silme, LLM). Ajanlar adresi ve token'ı görmez; lider bir iş için izin ister, sen onaylarsın.
      </p>
      <label className="settings-row settings-row-wide">
        <span>Birincil adres</span>
        <input value={form.primary} onChange={(e) => setForm({ ...form, primary: e.target.value })} placeholder="http://192.0.2.10:8100" />
      </label>
      <label className="settings-row settings-row-wide">
        <span>Yedek adres</span>
        <input value={form.fallback} onChange={(e) => setForm({ ...form, fallback: e.target.value })} placeholder="boş → yok" />
      </label>
      {editToken ? (
        <label className="settings-row settings-row-wide">
          <span>Token</span>
          <input type="password" autoComplete="off" value={form.token} onChange={(e) => setForm({ ...form, token: e.target.value })}
            placeholder={config?.tokenSet ? "yeni token" : "broker token'ı"} />
        </label>
      ) : (
        <div className="settings-row settings-row-wide">
          <span>Token</span>
          <span className="gpu-token">
            <span className="settings-value">kayıtlı</span>
            <button onClick={() => setEditToken(true)}>Değiştir</button>
          </span>
        </div>
      )}
      <div className="settings-actions">
        <button className="primary" disabled={busy || !config} onClick={() => void save()}>{busy ? "Kontrol ediliyor…" : "Kaydet"}</button>
        <button disabled={busy || !config} onClick={() => void run(() => look(true))}>Yenile</button>
      </div>
      {err && <p className="settings-err" role="alert">{err}</p>}
      {!view && !err && <p className="settings-note" role="status">Kontrol ediliyor…</p>}
      {line && (
        <p className="gpu-line" role="status">
          <span className={`host-dot gpu-${line.tone}`} aria-hidden />
          {line.text}
        </p>
      )}
      {s && (
        <p className="settings-note gpu-status">
          GPU sahibi: <b>{s.owner ?? "boşta"}</b>{s.owner === "llm" && s.llmModel ? ` (${s.llmModel})` : ""} · kuyruk {s.queued}
          {s.runningJob ? " · bir iş çalışıyor" : ""} · bugün {s.dailyUsed}/{s.dailyLimit}
        </p>
      )}
      <LimitsSection key={`${host}:${limitsRevision}`} host={host} configBusy={busy} />
      {view?.authorized && (view.models.length === 0
        ? <p className="settings-note">Broker model bildirmedi.</p>
        : <Models models={view.models} />)}
    </section>
  );
}

function LimitsSection({ host, configBusy }: { host: string; configBusy: boolean }) {
  const [saved, setSaved] = useState<GpuLimits>({});
  const [form, setForm] = useState<GpuLimitsForm>({ daily_image_limit: "", max_pending_jobs: "", max_count_per_request: "" });
  const [reason, setReason] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const password = useRef<HTMLInputElement>(null);
  const fill = (limits: GpuLimits) => {
    setSaved(limits);
    setForm({
      daily_image_limit: limits.daily_image_limit == null ? "" : String(limits.daily_image_limit),
      max_pending_jobs: limits.max_pending_jobs == null ? "" : String(limits.max_pending_jobs),
      max_count_per_request: limits.max_count_per_request == null ? "" : String(limits.max_count_per_request),
    });
  };
  const refresh = async () => {
    setBusy(true);
    setError(null);
    try { fill(await callHost<GpuLimits>(host, "gpu_limits_get")); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  useEffect(() => { void refresh(); }, [host]);
  const save = async (event: FormEvent) => {
    event.preventDefault();
    const adminToken = password.current?.value ?? "";
    if (password.current) password.current.value = "";
    setConfirm(false);
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const limits = limitsPatch(form, saved);
      fill(await callHost<GpuLimits>(host, "gpu_limits_set", { adminToken, limits, ...(reason.trim() ? { reason: reason.trim() } : {}) }));
      setMessage("Limitler kaydedildi.");
    } catch (e) {
      const text = e instanceof Error ? e.message : String(e);
      setError(adminToken ? text.split(adminToken).join("<gizli>") : text);
    } finally { setBusy(false); }
  };
  return (
    <fieldset aria-label="Limitler" disabled={busy || configBusy}>
      <legend>Limitler</legend>
      <p className="settings-note">Her değişiklikte yönetici şifresi sorulur; şifre kaydedilmez. Boş limit alanı değiştirilmez.</p>
      {([
        ["daily_image_limit", "Günlük görsel"], ["max_pending_jobs", "Kuyruk"], ["max_count_per_request", "İstek başına"],
      ] as const).map(([key, label]) => (
        <label key={key} className="settings-row settings-row-wide">
          <span>{label}</span>
          <input type="number" min={1} max={key === "max_count_per_request" ? 4 : undefined} step={1}
            disabled={confirm} value={form[key]} onChange={(e) => setForm({ ...form, [key]: e.target.value })} />
        </label>
      ))}
      <label className="settings-row settings-row-wide">
        <span>Sebep</span>
        <input value={reason} disabled={confirm} onChange={(e) => setReason(e.target.value)} placeholder="opsiyonel" />
      </label>
      {confirm ? (
        <form onSubmit={(e) => void save(e)}>
          <label className="settings-row settings-row-wide">
            <span>Yönetici şifresi</span>
            <input ref={password} type="password" autoComplete="off" required minLength={1} maxLength={9} autoFocus />
          </label>
          <div className="settings-actions">
            <button type="submit" className="primary">Onayla</button>
            <button type="button" onClick={() => { if (password.current) password.current.value = ""; setConfirm(false); }}>Vazgeç</button>
          </div>
        </form>
      ) : (
        <div className="settings-actions">
          <button className="primary" onClick={() => {
            setError(null); setMessage(null);
            try { limitsPatch(form, saved); setConfirm(true); }
            catch (e) { setError(e instanceof Error ? e.message : String(e)); }
          }}>Kaydet</button>
          <button onClick={() => void refresh()}>Limitleri yenile</button>
        </div>
      )}
      {error && <p className="settings-err" role="alert">{error}</p>}
      {message && <p className="settings-note" role="status">{message}</p>}
    </fieldset>
  );
}

function Models({ models }: { models: GpuModel[] }) {
  const g = modelGroups(models);
  return (
    <>
      <Group title="Ajana açık" note="lider izin isteyince, onayınla" models={g.agent} />
      <Group title="Servis ediliyor" note="senin kullanımında, ajanlara kapalı" models={g.served} />
      {g.diskOnly.length > 0 && (
        <details className="settings-fold gpu-fold">
          <summary><b>Yalnız diskte ({g.diskOnly.length})</b></summary>
          <ModelList label="Yalnız diskte" models={g.diskOnly} />
        </details>
      )}
    </>
  );
}

function Group({ title, note, models }: { title: string; note: string; models: GpuModel[] }) {
  return (
    <div className="gpu-group">
      <h5>{title} ({models.length}) <span>{note}</span></h5>
      {models.length ? <ModelList label={title} models={models} /> : <p className="settings-note">yok</p>}
    </div>
  );
}

function ModelList({ label, models }: { label: string; models: GpuModel[] }) {
  return (
    <ul className="gpu-models" aria-label={label}>
      {models.map((m) => (
        <li key={`${m.name}|${m.path ?? ""}`} title={[m.engine, m.path].filter(Boolean).join(" · ") || undefined}>
          <b>{m.name}</b>
          <code className="gpu-type">{m.type}</code>
        </li>
      ))}
    </ul>
  );
}
