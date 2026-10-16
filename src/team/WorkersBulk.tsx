// "Tüm worker'lar" — Kart araç çubuğundaki düğmeden, tıklanan yerde açılan küçük popover.
// Mantık takım ayarlarındaki eski satırla aynı (DEX-30); yalnız yerleşim değişti.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { UserCog } from "lucide-react";
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
import "./team.css";

/** DEX-25: swap the local provider's (empty) static catalog for the live server list. */
function withLocalModels(providers: ProviderInfo[], st: LocalStatus | null): ProviderInfo[] {
  if (!st) return providers;
  return providers.map((p) => p.id === "local" ? { ...p, models: st.models.map((m) => ({ id: m.id, label: m.id })) } : p);
}

export function WorkersBulkButton({ teamId }: { teamId: string }) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const close = () => { setOpen(false); btnRef.current?.focus(); };

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!wrapRef.current?.contains(t) && !(t instanceof Element && t.closest(".workers-bulk-pop"))) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); close(); }
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open ]);

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
      {open && <WorkersBulkPopover teamId={teamId} onClose={close} anchorRef={btnRef} />}
    </span>
  );
}

function WorkersBulkPopover({ teamId, onClose, anchorRef }: {
  teamId: string; onClose: () => void; anchorRef: React.RefObject<HTMLButtonElement | null>;
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

  return createPortal(
    <div ref={popRef} className="workers-bulk-pop" role="dialog" aria-label="Tüm worker'lar"
      data-above={spot?.above || undefined}
      style={{
        left: spot?.left ?? -9999, top: spot?.top ?? -9999, maxHeight: spot?.maxHeight,
        visibility: spot ? "visible" : "hidden",
      }}>
      {loading && !providers.length ? (
        <p className="team-note" role="status">Sağlayıcılar yükleniyor…</p>
      ) : providerError && !providers.length ? (
        <>
          <p className="team-err" role="alert">{providerError}</p>
          <button type="button" onClick={() => setRetry((r) => r + 1)}>Yeniden dene</button>
        </>
      ) : (
        <WorkersBulkForm teamId={teamId} providers={providers} firstRef={firstRef} onDone={onClose} />
      )}
      {providerError && providers.length > 0 && <p className="team-err" role="alert">{providerError}</p>}
    </div>, document.body);
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

/** DEX-30: one provider/model for every worker of the team; leaders are never touched. */
function WorkersBulkForm({ teamId, providers, firstRef, onDone }: {
  teamId: string;
  providers: ProviderInfo[];
  firstRef: React.RefObject<HTMLSelectElement | null>;
  onDone: () => void;
}) {
  const [provider, setProvider] = useState("anthropic");
  const [model, setModel] = useState("claude-sonnet-5-5");
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const models = providerModels(providers, provider);
  // The list can change under the choice (live catalogs); never send a model it doesn't hold.
  const chosen = models.some((m) => m.id === model) ? model : models[0]?.id ?? "";
  const workers = membersOf(teamId).filter((a) => a.kind === "worker" && isAssignable(a));
  const waiting = workers.filter((a) => a.pending && isRunning(a.id));
  const warn = tokenWarning(providers, provider);
  const noModels = !chosen ? "Bu sağlayıcının model listesi boş" : null;
  useEffect(() => { firstRef.current?.focus(); }, [firstRef]);

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
    <section className="team-bulk" aria-label="Tüm worker'lar">
      <b>Tüm worker'lar</b>
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
        title={noModels ?? (workers.length ? "Yalnız worker'lar değişir; lider(ler)e dokunulmaz" : "Takımda worker yok")}
        onClick={() => void act(async () => {
          const out = await setWorkersModel(teamId, { engine: "claude", provider, model: chosen });
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
              const unstarted = await restartPending(teamId);
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
