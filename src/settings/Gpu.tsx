// Sunucu › GPU: ev GPU makinesi (görsel üretim, arka plan silme, LLM).
// Bağlantı birlikte doğrulanan formdur ("Kaydet ve bağlan"); Limitler yönetici
// şifreli formdur; Ajanlar adresi ve anahtarı görmez.
import { useEffect, useRef, useState, type FormEvent } from "react";
import { callHost } from "../api/hosts";
import { configPatch, connectionLine, limitsPatch, modelGroups, type GpuConfig, type GpuLimits, type GpuLimitsForm, type GpuModel, type GpuOverview } from "../gpu/view";
import { Row, Pad } from "./kit/Row";
import { Section } from "./kit/Section";
import { FieldMsg, LoadingRows } from "./kit/states";

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
    setConfig(null);
    setView(null);
    void run(async () => {
      fill(await callHost<GpuConfig>(host, "gpu_config_get"));
      await look(false);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    <>
      <Section
        label="GPU bağlantısı"
        description="Ajanlar adresi ve anahtarı görmez; lider bir iş için izin ister, sen onaylarsın."
      >
        {!config && !err && <LoadingRows rows={3} />}
        {config && (
          <>
            <Row
              id="gpu-primary"
              label="Birincil adres"
              control={
                <span className="set-field">
                  <input aria-label="Birincil adres" value={form.primary}
                    onChange={(e) => setForm({ ...form, primary: e.target.value })} placeholder="http://192.0.2.10:8100" />
                </span>
              }
            />
            <Row
              id="gpu-fallback"
              label="Yedek adres"
              control={
                <span className="set-field">
                  <input aria-label="Yedek adres" value={form.fallback}
                    onChange={(e) => setForm({ ...form, fallback: e.target.value })} placeholder="boş → yok" />
                </span>
              }
            />
            <Row
              id="gpu-token"
              label="GPU anahtarı"
              control={editToken ? (
                <span className="set-field">
                  <input type="password" autoComplete="off" aria-label="GPU anahtarı" value={form.token}
                    onChange={(e) => setForm({ ...form, token: e.target.value })}
                    placeholder={config.tokenSet ? "yeni anahtar" : "broker anahtarı"} />
                </span>
              ) : (
                <span className="set-secret">
                  <code>kayıtlı</code>
                  <button type="button" onClick={() => setEditToken(true)}>Değiştir</button>
                </span>
              )}
              feedback={err && <FieldMsg ok={false} text={err} />}
            />
            <Pad>
              <span className="set-field-btns">
                <button type="button" className="primary" disabled={busy || !config} onClick={() => void save()}>
                  {busy ? "Kontrol ediliyor…" : "Kaydet ve bağlan"}
                </button>
                <button type="button" disabled={busy || !config} onClick={() => void run(() => look(true))}>Yenile</button>
              </span>
            </Pad>
          </>
        )}
        {err && !config && (
          <Pad><FieldMsg ok={false} text={err} /></Pad>
        )}
      </Section>

      <Section label="GPU durumu">
        {!view && !err && <LoadingRows rows={2} />}
        {line && (
          <Row
            label="Bağlantı"
            control={<span className={`set-dot ${line.tone === "ok" ? "ok" : line.tone === "down" ? "bad" : ""}`} aria-hidden />}
            feedback={<p className="set-note" role="status">{line.text}</p>}
          />
        )}
        {s && (
          <Row
            label="Kullanım"
            control={
              <span className="set-value">
                sahip {s.owner ?? "boşta"}{s.owner === "llm" && s.llmModel ? ` (${s.llmModel})` : ""} · kuyruk {s.queued}
                {s.runningJob ? " · bir iş çalışıyor" : ""} · bugün {s.dailyUsed}/{s.dailyLimit}
              </span>
            }
          />
        )}
        {!view && err && <Pad><FieldMsg ok={false} text={err} /></Pad>}
      </Section>

      <LimitsSection key={`${host}:${limitsRevision}`} host={host} configBusy={busy} />

      {view?.authorized && (
        <Section label="GPU modelleri">
          {view.models.length === 0
            ? <Pad><p className="set-note">Broker model bildirmedi.</p></Pad>
            : <Models models={view.models} />}
        </Section>
      )}
    </>
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
    <Section
      label="GPU limitleri"
      description="Harici: GPU makinesi. Her değişiklikte yönetici şifresi sorulur; şifre kaydedilmez. Boş alan değiştirilmez."
    >
      <fieldset style={{ border: 0, margin: 0, padding: 0, display: "contents" }} disabled={busy || configBusy}>
        {([
          ["daily_image_limit", "Günlük görsel"], ["max_pending_jobs", "Kuyruk"], ["max_count_per_request", "İstek başına"],
        ] as const).map(([key, label]) => (
          <Row
            key={key}
            label={label}
            control={
              <span className="set-field">
                <input type="number" aria-label={label} min={1} max={key === "max_count_per_request" ? 4 : undefined} step={1}
                  disabled={confirm} value={form[key]} onChange={(e) => setForm({ ...form, [key]: e.target.value })} />
              </span>
            }
          />
        ))}
        <Row
          label="Sebep"
          control={
            <span className="set-field">
              <input aria-label="Sebep" value={reason} disabled={confirm}
                onChange={(e) => setReason(e.target.value)} placeholder="opsiyonel" />
            </span>
          }
        />
        {confirm ? (
          <Pad>
            <form onSubmit={(e) => void save(e)}>
              <Row
                label="Yönetici şifresi"
                control={
                  <span className="set-field">
                    <input ref={password} type="password" autoComplete="off" aria-label="Yönetici şifresi" required minLength={1} maxLength={9} autoFocus />
                    <span className="set-field-btns">
                      <button type="submit" className="primary">Onayla</button>
                      <button type="button" onClick={() => { if (password.current) password.current.value = ""; setConfirm(false); }}>Vazgeç</button>
                    </span>
                  </span>
                }
              />
            </form>
          </Pad>
        ) : (
          <Pad>
            <span className="set-field-btns">
              <button
                type="button"
                className="primary"
                onClick={() => {
                  setError(null); setMessage(null);
                  try { limitsPatch(form, saved); setConfirm(true); }
                  catch (e) { setError(e instanceof Error ? e.message : String(e)); }
                }}
              >
                Kaydet
              </button>
              <button type="button" onClick={() => void refresh()}>Limitleri yenile</button>
            </span>
            {error && <FieldMsg ok={false} text={error} />}
            {message && <FieldMsg ok text={message} />}
          </Pad>
        )}
      </fieldset>
    </Section>
  );
}

function Models({ models }: { models: GpuModel[] }) {
  const g = modelGroups(models);
  return (
    <>
      <Group title="Ajana açık" note="lider izin isteyince, onayınla" models={g.agent} />
      <Group title="Servis ediliyor" note="senin kullanımında, ajanlara kapalı" models={g.served} />
      {g.diskOnly.length > 0 && (
        <details className="set-fold">
          <summary><b>Yalnız diskte ({g.diskOnly.length})</b></summary>
          <ModelList label="Yalnız diskte" models={g.diskOnly} />
        </details>
      )}
    </>
  );
}

function Group({ title, note, models }: { title: string; note: string; models: GpuModel[] }) {
  return (
    <div className="set-pad">
      <h4 className="set-sub">{title} ({models.length}) <span>{note}</span></h4>
      {models.length ? <ModelList label={title} models={models} /> : <p className="set-note">yok</p>}
    </div>
  );
}

function ModelList({ label, models }: { label: string; models: GpuModel[] }) {
  return (
    <ul aria-label={label} style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 8 }}>
      {models.map((m) => (
        <li key={`${m.name}|${m.path ?? ""}`} title={[m.engine, m.path].filter(Boolean).join(" · ") || undefined} className="set-field">
          <b>{m.name}</b>
          <code className="set-note">{m.type}</code>
        </li>
      ))}
    </ul>
  );
}
