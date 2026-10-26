// "Tüm worker'lar" — Kart araç çubuğundaki düğmeden, tıklanan yerde açılan küçük popover.
// Mantık takım ayarlarındaki eski satırla aynı (DEX-30); yalnız yerleşim değişti.
// #2443: başlık kapsam seçicidir — tüm worker'lar, uzmanlığa göre ya da Kart'ta seçilenler.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, UserCog } from "lucide-react";
import { LOCAL, onEvent } from "../api/hosts";
import { openSettings } from "../layout/panels";
import { askConfirm } from "../confirm";
import {
  listProviders, localStatus,
  type LocalStatus, type ProviderInfo,
} from "./api";
import { isRunning, membersOf, restartPending, setWorkersModel, useTeams } from "./store";
import { providerModels } from "./providerModels";
import { bulkSpot, type BulkSpot } from "./bulkPlace";
import { isAssignable } from "./rolePolicy";
import { familiesOf, scopeIds, scopeTitle, type Scope } from "../office/workerPick";
import { endPick, picking, startPick, usePick } from "../office/pickStore";
import "./team.css";

/** The workers a bulk change may reach: never leaders, never the personal assistant. */
const bulkWorkers = (teamId: string) => membersOf(teamId).filter((a) => a.kind === "worker" && isAssignable(a));

type View = "form" | "menu" | "roles" | "pick";

/** DEX-25: swap the local provider's (empty) static catalog for the live server list. */
function withLocalModels(providers: ProviderInfo[], st: LocalStatus | null): ProviderInfo[] {
  if (!st) return providers;
  return providers.map((p) => p.id === "local" ? { ...p, models: st.models.map((m) => ({ id: m.id, label: m.id })) } : p);
}

export function WorkersBulkButton({ teamId }: { teamId: string }) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  /** #2443: the popover's scope views step back on Esc first; true = handled. */
  const back = useRef<(() => boolean) | null>(null);
  const close = () => { setOpen(false); btnRef.current?.focus(); };

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (picking(teamId)) return; // the Kart's clicks pick workers now
      if (!wrapRef.current?.contains(t) && !(t instanceof Element && t.closest(".workers-bulk-pop"))) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      if (!back.current?.()) close();
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open, teamId]);

  return (
    <span className="workers-bulk-wrap" ref={wrapRef}>
      <button
        ref={btnRef}
        className={`office-bar-btn${open ? " on" : ""}`}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label="Tüm worker'lar"
        title="Tüm worker'lar — sağlayıcı/model"
        onClick={() => setOpen((v) => !v)}
      >
        <UserCog />
      </button>
      {open && <WorkersBulkPopover teamId={teamId} onClose={close} anchorRef={btnRef} back={back} />}
    </span>
  );
}

function WorkersBulkPopover({ teamId, onClose, anchorRef, back }: {
  teamId: string; onClose: () => void; anchorRef: React.RefObject<HTMLButtonElement | null>;
  back: React.RefObject<(() => boolean) | null>;
}) {
  const { teams } = useTeams();
  const team = teams.find((t) => t.id === teamId);
  const host = team?.host ?? LOCAL;
  const [baseProviders, setProviders] = useState<ProviderInfo[]>([]);
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [providerError, setProviderError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const request = useRef(0);
  const firstRef = useRef<HTMLSelectElement>(null);
  // #2134: portal + fixed — dar/alçak Kart paneli (overflow:hidden) popover'ı kırpmasın.
  const popRef = useRef<HTMLDivElement>(null);
  const [spot, setSpot] = useState<BulkSpot | null>(null);

  useLayoutEffect(() => {
    const el = popRef.current, anchor = anchorRef.current;
    if (!el || !anchor) return;
    const next = bulkSpot(anchor.getBoundingClientRect(),
      { w: el.offsetWidth, h: el.scrollHeight }, { w: window.innerWidth, h: window.innerHeight });
    setSpot((prev) => prev && prev.left === next.left && prev.top === next.top
      && prev.maxHeight === next.maxHeight && prev.above === next.above ? prev : next);
  });

  useEffect(() => {
    const onResize = () => setSpot(null); // ölçüm bir sonraki layout etkisinde tazelenir
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => { firstRef.current?.focus(); }, []);

  useEffect(() => {
    let live = true;
    const id = ++request.current;
    setLoading(true);
    void (async () => {
      try {
        const ps = await listProviders(host);
        if (!live || id !== request.current) return;
        setProviders(ps); setProviderError(null);
        const lp = ps.find((p) => p.id === "local");
        const status = lp?.baseUrl && lp.hasToken ? await localStatus(host).catch(() => null) : null;
        if (live && id === request.current) setLocal(status);
      } catch {
        if (live && id === request.current) {
          setProviderError("Sağlayıcı listesi alınamadı. Bağlantıyı kontrol edin.");
        }
      } finally {
        if (live && id === request.current) setLoading(false);
      }
    })();
    const off = onEvent("providers-changed", (_, eventHost) => {
      if (eventHost === host) setRetry((r) => r + 1);
    });
    return () => { live = false; off(); };
  }, [host, retry]);

  const providers = withLocalModels(baseProviders, local);

  // #2443: who the change reaches. The form stays mounted (hidden) under the scope views, so its
  // provider/model choice survives a scope change.
  const [scope, setScope] = useState<Scope>({ kind: "all" });
  const [view, setView] = useState<View>("form");
  const titleRef = useRef<HTMLButtonElement>(null);
  const toForm = () => {
    if (picking(teamId)) endPick();
    setView("form");
    requestAnimationFrame(() => titleRef.current?.focus());
  };
  back.current = () => (view === "form" ? false : (toForm(), true));
  useEffect(() => () => {
    back.current = null;
    if (picking(teamId)) endPick();
  }, [teamId, back]);
  const title = scopeTitle(scope, bulkWorkers(teamId));
  // Picking: the popover steps aside (it would cover the Kart's top-right workers); the pick bar
  // takes the Kart's toolbar row instead.
  const bar = view === "pick" ? anchorRef.current?.closest(".office-bar") : null;

  return <>{createPortal(
    <div ref={popRef} className="workers-bulk-pop" role="dialog" aria-label="Tüm worker'lar"
      data-above={spot?.above || undefined}
      style={{
        left: spot?.left ?? -9999, top: spot?.top ?? -9999, maxHeight: spot?.maxHeight,
        visibility: spot ? "visible" : "hidden", display: view === "pick" ? "none" : undefined,
      }}>
      {view === "menu" && (
        <ScopeMenu teamId={teamId} scope={scope} onCancel={toForm}
          onPick={(kind) => {
            if (kind === "all") {
              setScope({ kind: "all" });
              toForm();
            } else if (kind === "roles") setView("roles");
            else {
              startPick(teamId, scope.kind === "picked" ? scope.ids : []);
              setView("pick");
            }
          }} />
      )}
      {view === "roles" && (
        <RolePicker teamId={teamId} initial={scope.kind === "roles" ? scope.keys : []} onCancel={toForm}
          onDone={(keys) => { setScope({ kind: "roles", keys }); toForm(); }} />
      )}
      {view === "form" && (loading && !providers.length ? (
        <p className="team-note" role="status">Sağlayıcılar yükleniyor…</p>
      ) : providerError && !providers.length ? (
        <>
          <p className="team-err" role="alert">{providerError}</p>
          <button type="button" onClick={() => setRetry((r) => r + 1)}>Yeniden dene</button>
        </>
      ) : null)}
      {providers.length > 0 && (
        <WorkersBulkForm teamId={teamId} providers={providers} firstRef={firstRef} onDone={onClose}
          scope={scope} hidden={view !== "form"}
          title={
            <button ref={titleRef} type="button" className="workers-bulk-scope" aria-haspopup="true"
              aria-label={`Kapsam: ${title}. Değiştir`} title="Kime uygulanacağını seç"
              onClick={() => setView("menu")}>
              <b>{title}</b>
              <ChevronDown aria-hidden="true" />
            </button>
          } />
      )}
      {view === "form" && providerError && providers.length > 0 && <p className="team-err" role="alert">{providerError}</p>}
    </div>, document.body)}
    {bar && createPortal(
      <PickBar teamId={teamId} onCancel={toForm}
        onDone={(ids) => { setScope({ kind: "picked", ids }); toForm(); }} />, bar)}
  </>;
}

/** DEX-30: DEX-13's wording — without a token the session fails at start, so say it before. */
function tokenWarning(providers: ProviderInfo[], provider: string): string | null {
  if (provider === "anthropic") return null;
  const p = providers.find((x) => x.id === provider);
  if (p?.account) return p.hasToken ? null : `${p.label} hesabı bağlı değil — Ayarlar › Sağlayıcılar'dan bağla`;
  return p && !p.hasToken ? `${p.label} anahtarı yok — Ayarlar › Sağlayıcılar'dan içe aktar ya da gir` : null;
}

const RESTART_CONFIRM = (n: number) =>
  `${n} çalışan worker durdurulup yeni ayarla yeniden başlatılsın mı?\n\n` +
  "Model/sağlayıcısı değişenler aynı oturumu yeni modelle sürdürür.\n" +
  "Yarıdaki devredilmiş işleri raporsuz kapanır; lider bunu görür.";

/** DEX-30: one provider/model for the scope's workers; leaders are never touched. */
function WorkersBulkForm({ teamId, providers, firstRef, onDone, scope, hidden, title }: {
  teamId: string;
  providers: ProviderInfo[];
  firstRef: React.RefObject<HTMLSelectElement | null>;
  onDone: () => void;
  scope: Scope;
  hidden: boolean;
  title: React.ReactNode;
}) {
  const [provider, setProvider] = useState("anthropic");
  const [model, setModel] = useState("claude-sonnet-5-5");
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const models = providerModels(providers, provider);
  // The list can change under the choice (live catalogs); never send a model it doesn't hold.
  const chosen = models.some((m) => m.id === model) ? model : models[0]?.id ?? "";
  const all = bulkWorkers(teamId);
  const ids = scopeIds(scope, all);
  const workers = ids ? all.filter((a) => ids.includes(a.id)) : all;
  const waiting = workers.filter((a) => a.pending && isRunning(a.id));
  const warn = tokenWarning(providers, provider);
  const noModels = !chosen ? "Bu sağlayıcının model listesi boş" : null;
  useEffect(() => { firstRef.current?.focus(); }, [firstRef]);
  // A new scope: the last outcome was about another set of workers.
  useEffect(() => { setNote(null); setError(null); }, [scope]);

  const act = async (f: () => Promise<string>) => {
    setBusy(true);
    setError(null);
    try {
      setNote(await f());
    } catch (err) {
      setNote(null);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="team-bulk" aria-label="Tüm worker'lar" hidden={hidden}>
      {title}
      <label>
        Sağlayıcı
        <select ref={firstRef} value={provider} aria-label="Worker'ların sağlayıcısı" disabled={busy}
          onChange={(e) => { setProvider(e.target.value); setModel(""); }}>
          {providers.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
        </select>
      </label>
      <label>
        Model
        <select value={chosen} aria-label="Worker'ların modeli" disabled={busy || !models.length}
          onChange={(e) => setModel(e.target.value)}>
          {models.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
        </select>
      </label>
      <button className="primary" disabled={busy || !chosen || !workers.length}
        title={noModels ?? (workers.length ? "Yalnız worker'lar değişir; lider(ler)e dokunulmaz"
          : all.length ? "Kapsamda worker yok" : "Takımda worker yok")}
        onClick={() => void act(async () => {
          const out = await setWorkersModel(teamId, { engine: "claude", provider, model: chosen }, ids ?? undefined);
          return `${out.applied.length} worker değişti` +
            (out.pending.length ? ` · ${out.pending.length} çalışan worker oturumu kapanınca geçecek` : "");
        })}>
        Uygula
      </button>
      {waiting.length > 0 && (
        <button disabled={busy} title="Bekleyen değişikliği şimdi uygula: çalışanları durdurup yeniden başlatır"
          onClick={async () => {
            if (!(await askConfirm(RESTART_CONFIRM(waiting.length)))) return;
            void act(async () => {
              const unstarted = await restartPending(teamId, waiting.map((a) => a.id));
              const names = unstarted.map((id) => workers.find((a) => a.id === id)?.name ?? id);
              return `${waiting.length - unstarted.length} worker yeniden başlatıldı` +
                (names.length ? ` · pane'i olmadığı için başlatılamadı: ${names.join(", ")}` : "");
            });
          }}>
          Çalışanları yeniden başlat ({waiting.length})
        </button>
      )}
      {warn && (
        <p className="team-warn" role="status">
          {warn}{" "}
          <button
            type="button"
            className="team-link"
            onClick={() => { onDone(); void openSettings(teamId, "providers", `provider-${provider}`); }}
          >
            Ayarlar › Sağlayıcılar'da ilgili karta git →
          </button>
        </p>
      )}
      {noModels && <p className="team-warn" role="status">{noModels}</p>}
      {note && <p className="team-note" role="status">{note}</p>}
      {error && <p className="team-err" role="alert">{error}</p>}
    </section>
  );
}

/** #2443: the scope chooser behind the popover's title. */
function ScopeMenu({ teamId, scope, onPick, onCancel }: {
  teamId: string; scope: Scope; onPick: (kind: Scope["kind"]) => void; onCancel: () => void;
}) {
  useTeams();
  const n = bulkWorkers(teamId).length;
  const opt = (kind: Scope["kind"], label: string, hint: string) => (
    <button type="button" className="workers-bulk-opt" aria-pressed={scope.kind === kind}
      autoFocus={scope.kind === kind} onClick={() => onPick(kind)}>
      <span>{label}</span>
      <small>{hint}</small>
    </button>
  );
  return (
    <section className="team-bulk" aria-label="Kapsam">
      <b>Kime uygulansın?</b>
      <div className="workers-bulk-opts" role="group" aria-label="Kapsam">
        {opt("all", "Tüm worker'lar", `${n} worker`)}
        {opt("roles", "Uzmanlığa göre", "Kernel, Lens, Probe…")}
        {opt("picked", "Seçili worker'lar", "Kartta tıkla ya da sürükle")}
      </div>
      <button type="button" onClick={onCancel}>Vazgeç</button>
    </section>
  );
}

/** #2443: worker families (one per role); several at once. */
function RolePicker({ teamId, initial, onDone, onCancel }: {
  teamId: string; initial: string[]; onDone: (keys: string[]) => void; onCancel: () => void;
}) {
  useTeams();
  const families = familiesOf(bulkWorkers(teamId));
  const [keys, setKeys] = useState(initial);
  const chosen = families.filter((f) => keys.includes(f.key));
  const count = chosen.reduce((n, f) => n + f.ids.length, 0);
  return (
    <section className="team-bulk" aria-label="Uzmanlığa göre">
      <b>Uzmanlığa göre</b>
      {families.length ? (
        <fieldset className="workers-bulk-roles" aria-label="Uzmanlıklar">
          {families.map((f, i) => (
            <label key={f.key} className="workers-bulk-check">
              <input type="checkbox" autoFocus={i === 0} checked={keys.includes(f.key)}
                onChange={(e) => setKeys((ks) => e.target.checked ? [...ks, f.key] : ks.filter((k) => k !== f.key))} />
              <span className="workers-bulk-fam">{f.label}</span>
              {f.hint && <small>{f.hint}</small>}
              <span className="workers-bulk-count" aria-label={`${f.ids.length} worker`}>{f.ids.length}</span>
            </label>
          ))}
        </fieldset>
      ) : <p className="team-note">Takımda worker yok.</p>}
      <div className="workers-bulk-actions">
        <button type="button" className="primary" disabled={!chosen.length}
          onClick={() => onDone(chosen.map((f) => f.key))}>
          Tamam{chosen.length ? ` (${count})` : ""}
        </button>
        <button type="button" onClick={onCancel}>Vazgeç</button>
      </div>
    </section>
  );
}

/** #2443: the Kart is in pick mode (Office toggles and drags into the pick store); this bar over the
 * Kart's toolbar row confirms or drops it. */
function PickBar({ teamId, onDone, onCancel }: {
  teamId: string; onDone: (ids: string[]) => void; onCancel: () => void;
}) {
  useTeams();
  const pick = usePick();
  const ids = bulkWorkers(teamId).filter((a) => pick.ids.has(a.id)).map((a) => a.id);
  return (
    <div className="workers-pick-bar" role="group" aria-label="Seçili worker'lar"
      title="Kartta worker'a tıkla ya da basılı tutup sürükleyerek alan seç. Klavyede: ok tuşları, Boşluk.">
      <span className="workers-pick-text">
        <span className="workers-pick-hint">Kartta tıkla ya da sürükle · </span>
        <span role="status">{ids.length ? `${ids.length} worker seçili` : "Henüz seçim yok"}</span>
      </span>
      <button type="button" className="primary" disabled={!ids.length} onClick={() => onDone(ids)}>
        Tamam ({ids.length})
      </button>
      <button type="button" autoFocus onClick={onCancel}>Vazgeç</button>
    </div>
  );
}
