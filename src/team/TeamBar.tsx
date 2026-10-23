// Top-bar team tabs, "+ new team" form and the active team's settings/member panel.
import { Fragment, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { hostName, hostStatus, hostSupports, LOCAL, onEvent, useHosts, useLocalStatus } from "../api/hosts";
import { hasLocalCore } from "../api/platform";
import { CoreDownReason, StartCoreButton } from "./CoreBanner";
import { HOST_STATUS_TEXT } from "../settings/Hosts";
import { FolderButton } from "../project/FolderButton";
import { ProjectPicker } from "../project/ProjectPicker";
import { ProjectsSection } from "../project/ProjectsSection";
import { FolderPreview } from "./FolderPreview";
import { useTeamProjects } from "../project/store";
import { getStatus } from "../agents";
import {
  DEFAULT_WORKER_ROLES, listProviders, localStatus, teamDeleteInfo, teamCloneTemplate, supportsEducation,
  type AgentKind, type LocalStatus, type PendingModel, type ProviderInfo, type Role,
  type TeamKind, type TeamProfile, type TeamCloneTemplate,
} from "./api";
import {
  addAgent, chainBack, createTeam, isRunning, membersOf, patchAgent, patchTeam, removeAgent, removeTeam, selectTeam,
  setAgentChain, useTeams,
} from "./store";
import "./team.css";
import { Plus, Server, X } from "lucide-react";
import { getSnapshot as getLayoutSnapshot, subscribe as subscribeLayout } from "../layout/store";
import { applyFavoriteLayout, applyFavoriteToAll, saveFavoriteLayout } from "../layout/panels";
import { isActive } from "../layout/rackOrder";
import { teamAccent } from "./accent";
import { providerModels } from "./providerModels";
import { fitConvertedModels, normalizeCloneTemplate } from "./cloneTemplate";
import { TestLevelToggle } from "./TestLevelToggle";
import { nextTestLevel } from "./testLevel";
import { assistantTaken, memberRoles } from "./rolePolicy";
import { ConfirmDialog } from "../settings/kit/ConfirmDialog";
import { askConfirm } from "../confirm";

/** DEX-25: swap the local provider's (empty) static catalog for the live server list. */
function withLocalModels(providers: ProviderInfo[], st: LocalStatus | null): ProviderInfo[] {
  if (!st) return providers;
  return providers.map((p) => p.id === "local" ? { ...p, models: st.models.map((m) => ({ id: m.id, label: m.id })) } : p);
}

/** Re-render periodically: run state lives outside React (hooks → agents.ts). */
function useTick(ms = 500) {
  const [, set] = useState(0);
  useEffect(() => {
    const t = setInterval(() => set((n) => n + 1), ms);
    return () => clearInterval(t);
  }, [ms]);
}

/** S4: a team on a server shows it on its tab (dimmed while the host is not connected). */
function HostBadge({ host }: { host: string }) {
  const status = hostStatus(host);
  return (
    <span className={`team-host ${status}`} title={`${hostName(host)} sunucusu · ${HOST_STATUS_TEXT[status]}`}>
      <Server />
    </span>
  );
}

const CATEGORY_TEXT: Record<string, string> = {
  leadership: "Liderlik", eng: "Mühendislik", qa: "Kalite", research: "Araştırma", design: "Tasarım",
  product: "Ürün", ops: "Operasyon", security: "Güvenlik", automation: "Otomasyon", marketing: "Pazarlama",
  support: "Destek", game: "Oyun", education: "Eğitim", personal: "Kişisel",
};

/** Roles kept in the catalog but demoted from the new-team form (DEX-21). */
export const DEMOTED_ROLES = ["support", "marketing", "seo", "n8n-automation"];

/** Role picker grouped by category (catalog order). With `other`, those slugs move to a trailing "Diğer" group. */
function RoleSelect({ value, onChange, disabled, label, other, profile = "dev", allowAssistant = false, assistantUnavailable = false }: {
  value: string; onChange: (slug: string) => void; disabled?: boolean; label?: string; other?: string[];
  profile?: TeamProfile;
  allowAssistant?: boolean; assistantUnavailable?: boolean;
}) {
  const { roles } = useTeams();
  const available = memberRoles(roles, profile, allowAssistant);
  const groups = new Map<string, Role[]>();
  const rest: Role[] = [];
  for (const r of available) {
    if (other?.includes(r.slug)) rest.push(r);
    else groups.set(r.category, [...(groups.get(r.category) ?? []), r]);
  }
  return (
    <select value={value} disabled={disabled} aria-label={label ?? "Rol"} onChange={(e) => onChange(e.target.value)}>
      {!available.some((r) => r.slug === value) && <option value={value}>{roles.find(r => r.slug === value)?.name ?? (value || "—")}</option>}
      {[...groups].map(([cat, rs]) => (
        <optgroup key={cat} label={CATEGORY_TEXT[cat] ?? cat}>
          {rs.map((r) => <option key={r.slug} value={r.slug} disabled={r.slug === "assistant" && assistantUnavailable}
            title={r.slug === "assistant" && assistantUnavailable ? "Takımda zaten bir asistan var" : r.description}>{r.name}</option>)}
        </optgroup>
      ))}
      {rest.length > 0 && (
        <optgroup key="__other" label="Diğer">
          {rest.map((r) => <option key={r.slug} value={r.slug} title={r.description}>{r.name}</option>)}
        </optgroup>
      )}
    </select>
  );
}

type TeamMenu = "new" | "team";
const MENU_EVENT = "dex:team-menu";

/** Opens (never toggles) a team menu from outside the tabs, e.g. the palette (DEX-6). Resolves once painted. */
export async function openTeamMenu(which: TeamMenu, host?: string, sourceId?: string) {
  const detail = { which, host: host ?? null as string | null, sourceId, handled: false };
  window.dispatchEvent(new CustomEvent(MENU_EVENT, { detail }));
  if (!detail.handled) throw new Error("Takım menüsü bulunamadı");
  await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
}

export function TeamTabs({ onOpenTeamSettings }: { onOpenTeamSettings: () => void }) {
  const { teams, agents, activeId } = useTeams();
  const [menu, setMenu] = useState<TeamMenu | null>(null);
  const [presetHost, setPresetHost] = useState<string | null>(null);
  const [sourceId, setSourceId] = useState<string | undefined>();
  const [context, setContext] = useState<{ id: string; host: string; trigger: HTMLButtonElement } | null>(null);
  const contextRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { if (context) contextRef.current?.focus(); }, [context]);
  const closeContext = () => { context?.trigger.focus(); setContext(null); };
  useHosts();
  useTick();
  useEffect(() => {
    const onOpen = (e: Event) => {
      const req = (e as CustomEvent<{ which: TeamMenu; host: string | null; sourceId?: string; handled: boolean }>).detail;
      req.handled = true;
      if (req.which === "team") onOpenTeamSettings();
      else { setPresetHost(req.host ?? null); setSourceId(req.sourceId); setMenu(req.which); }
    };
    window.addEventListener(MENU_EVENT, onOpen);
    return () => window.removeEventListener(MENU_EVENT, onOpen);
  }, [onOpenTeamSettings]);

  return (
    <nav className="team-tabs" aria-label="Takımlar">
      <div className="team-tabs-scroll">
        {teams.map((t) => {
          // Only the red ones (working/waiting, leader included); idle sessions that delivered don't count.
          const live = agents.filter((a) => a.teamId === t.id && isActive(getStatus(a.id))).length;
          // #838: the active team's project picker sits beside its tab (row-reverse: to its left).
          return (
            <Fragment key={t.id}>
              <button
                className={`team-tab ${t.id === activeId ? "active" : ""} ${hostStatus(t.host) === "connected" ? "" : "down"}`}
                data-accent={teamAccent(t)}
                onClick={() => selectTeam(t.id)}
                onContextMenu={(e) => { e.preventDefault(); setContext({ id: t.id, host: t.host, trigger: e.currentTarget }); }}
                onKeyDown={(e) => {
                  if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                    e.preventDefault(); setContext({ id: t.id, host: t.host, trigger: e.currentTarget });
                  }
                }}
                title={t.host === LOCAL ? t.workdir ?? "klasör seçilmedi" : `${hostName(t.host)}: ${t.workdir ?? "klasör seçilmedi"}`}
              >
                {t.host !== LOCAL && <HostBadge host={t.host} />}
                {t.kind === "service" && <span className="team-kind">alt</span>}
                {t.name}
                {live > 0 && <span className="team-live">{live}</span>}
              </button>
              {t.id === activeId && <ProjectPicker teamId={t.id} />}
            </Fragment>
          );
        })}
        {/* #2138: last in DOM = leftmost (row-reverse), right beside the newest team; scrolls with the tabs. */}
        {teams.length > 0 && <button type="button" className="team-tab team-new" onClick={() => { setPresetHost(null); setSourceId(undefined); setMenu("new"); }}
          title="Yeni takım" aria-label="Yeni takım">
          <Plus aria-hidden="true" />
        </button>}
      </div>
      {context && <div className="team-pop team-context" role="menu" aria-label="Takım işlemleri"
        onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); closeContext(); } }}
        onBlur={e => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setContext(null); }}>
        <button ref={contextRef} role="menuitem" type="button" disabled={!hostSupports(context.host, "team_clone_template")}
          onClick={() => { setPresetHost(context.host); setSourceId(context.id); setMenu("new"); closeContext(); }}>Takımı kopyala</button>
      </div>}
      {menu === "new" && <NewTeamForm key={`${presetHost ?? LOCAL}:${sourceId ?? ""}`} initialHost={presetHost ?? LOCAL} sourceId={sourceId} onDone={() => setMenu(null)} />}
    </nav>
  );
}

/** FIX-5: a connected host with no teams yet — empty state with a create action (fresh `dexd`). */
export function HostEmptyStates() {
  const { ready, teams } = useTeams();
  const hosts = useHosts();
  const local = useLocalStatus();
  // #203: the shell finds / starts the core before the first answer (a `tauri dev` build of dexd
  // first, up to START_TIMEOUT) — say so instead of an empty stage.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (ready) return;
    const t = setTimeout(() => setSlow(true), 1500);
    return () => clearTimeout(t);
  }, [ready]);
  if (!ready) {
    return hasLocalCore && slow ? (
      <div className="host-empty" role="status">
        {local === "reconnecting" ? "Yerel çekirdeğe bağlanılamıyor — yeniden deneniyor…" : "Yerel çekirdek açılıyor…"}
      </div>
    ) : null;
  }
  const empty = hosts.filter((h) => h.status === "connected" && !teams.some((t) => t.host === h.rec.id));
  // #47: no team at all → no Kart either, so its "Yeni takım" button is gone; offer it here.
  const none = teams.length === 0;
  // #151: the local core is down and none of its teams is known (first run): no band to show it on.
  const coreDown = hasLocalCore && local === "down" && !teams.some((t) => t.host === LOCAL);
  if (!empty.length && !none && !coreDown) return null;
  return (
    <>
      {coreDown && (
        <div className="host-empty" role="status">
          Yerel çekirdek çalışmıyor.
          <CoreDownReason />
          <StartCoreButton />
        </div>
      )}
      {none && !coreDown && (
        <div className="host-empty" role="status">
          Henüz takım yok.
          <button onClick={() => void openTeamMenu("new", LOCAL)}>Takım oluştur…</button>
        </div>
      )}
      {empty.map((h) => (
        <div className="host-empty" role="status" key={h.rec.id}>
          {h.rec.name} sunucusunda henüz takım yok.
          <button onClick={() => void openTeamMenu("new", h.rec.id)}>Takım oluştur…</button>
        </div>
      ))}
    </>
  );
}

function NewTeamForm({ onDone, initialHost, sourceId }: { onDone: () => void; initialHost?: string; sourceId?: string }) {
  const hosts = useHosts();
  const [host, setHost] = useState(initialHost ?? LOCAL);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<TeamKind>("main");
  const [profile, setProfile] = useState<TeamProfile>("dev");
  const [education, setEducation] = useState<{ host: string; supported: boolean } | null>(null);
  const [cwd, setCwd] = useState<string | null>(null);
  const [workerRoles, setWorkerRoles] = useState<string[]>(DEFAULT_WORKER_ROLES);
  const [error, setError] = useState<string | null>(null);
  const [templateError, setTemplateError] = useState<string | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [catalogLoading, setCatalogLoading] = useState(!!sourceId);
  const [catalogRetry, setCatalogRetry] = useState(0);
  const [busy, setBusy] = useState(false);
  const [template, setTemplate] = useState<TeamCloneTemplate | null>(null);
  const [codexConverted, setCodexConverted] = useState<number[]>([]);
  const [loading, setLoading] = useState(!!sourceId);
  const [retry, setRetry] = useState(0);
  const formRef = useRef<HTMLFormElement>(null);
  const [returnFocus] = useState(() => document.activeElement as HTMLElement | null);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  useEffect(() => {
    return () => { if (returnFocus?.isConnected) returnFocus.focus(); };
  }, [returnFocus]);
  useEffect(() => {
    if (loading) formRef.current?.focus();
    else formRef.current?.querySelector<HTMLInputElement>("input")?.focus();
  }, [loading]);
  useEffect(() => {
    if (!sourceId) return;
    let live = true;
    setLoading(true); setTemplateError(null);
    void teamCloneTemplate(sourceId).then(t => {
      if (!live) return;
      // Normalize on arrival: the catalog may still be loading when the user submits.
      const r = normalizeCloneTemplate(t, []);
      setTemplate(r.template); setCodexConverted(r.converted); setKind(t.kind); setLoading(false);
    },
      e => { if (live) { setTemplateError(String(e)); setLoading(false); } });
    return () => { live = false; };
  }, [sourceId, retry]);
  useEffect(() => {
    if (!sourceId) return;
    let live = true;
    setCatalogLoading(true); setCatalogError(null);
    void listProviders(host).then(async ps => {
      const lp = ps.find(p => p.id === "local");
      const st = lp?.baseUrl && lp.hasToken ? await localStatus(host).catch(() => null) : null;
      return withLocalModels(ps, st);
    }).then(ps => {
      if (live) { setProviders(ps); setCatalogLoading(false); }
    }, e => { if (live) { setCatalogError(`Model kataloğu alınamadı: ${String(e)}`); setCatalogLoading(false); } });
    return () => { live = false; };
  }, [host, sourceId, catalogRetry]);
  useEffect(() => {
    if (!template || catalogLoading || !codexConverted.length) return;
    const t = fitConvertedModels(template, codexConverted, providers);
    if (t !== template) setTemplate(t);
  }, [template, providers, catalogLoading, codexConverted]);
  // Modern hosts create the team/Main folders when cwd is omitted; old hosts still need a picker.
  const projects = hostSupports(host, "project_list");
  const educationSupport = education?.host === host ? education.supported : null;
  const educationSelected = (template?.profile ?? profile) === "edu";
  useEffect(() => {
    let live = true;
    setEducation(null);
    void supportsEducation(host).then(supported => { if (live) setEducation({ host, supported }); });
    return () => { live = false; };
  }, [host]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy || loading || (sourceId && !template) || (educationSelected && educationSupport !== true)) return;
    setBusy(true); setError(null);
    try {
      if (educationSelected && !await supportsEducation(host)) {
        setEducation({ host, supported: false });
        return;
      }
      await createTeam(host, { name: name.trim(), kind, profile: template?.profile ?? profile, cwd: kind === "main" ? cwd : null,
        ...(template ? { members: normalizeCloneTemplate(template, providers).template.members } : profile === "dev" ? { workerRoles } : {}) });
      onDone();
    } catch (err) {
      setError(String(err));
    } finally { setBusy(false); }
  }

  // Portal: the top bar's stacking context sits under the workspace's floating groups (Takım ayarları),
  // so "Takımı kopyala" from that panel opened behind it.
  return createPortal(
    <>
    <div className="team-create-backdrop" aria-hidden="true" />
    <form ref={formRef} tabIndex={-1} className="team-pop" role="dialog" aria-label={sourceId ? "Takımı kopyala" : "Yeni takım"} aria-modal="true" aria-busy={busy || loading} onSubmit={submit} onKeyDown={(e) => {
      if (e.key === "Escape" && !busy) { e.stopPropagation(); onDone(); }
      if (e.key === "Tab") {
        const controls = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("input:not(:disabled), select:not(:disabled), button:not(:disabled)"));
        const first = controls[0], last = controls[controls.length - 1];
        if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last?.focus(); }
        else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first?.focus(); }
      }
    }}>
      <h3>{sourceId ? "Takımı kopyala" : "Yeni takım"}
        <button type="button" className="icon-btn close" disabled={busy} onClick={onDone} aria-label="Kapat" title="Kapat (Esc)"><X /></button>
      </h3>
      <fieldset className="team-form-fields" disabled={busy || loading}>
      <label>
        Ad
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="ör. Dex" />
      </label>
      {hosts.length > 0 && (
        <label>
          Nerede çalışsın
          <select value={host} aria-label="Takımın host'u" onChange={(e) => { setHost(e.target.value); setCwd(null); }}>
            <option value={LOCAL}>Yerel (bu bilgisayar)</option>
            {hosts.map((h) => (
              <option key={h.rec.id} value={h.rec.id} disabled={h.status !== "connected"}>
                {h.rec.name}{h.status === "connected" ? "" : ` (${HOST_STATUS_TEXT[h.status]})`}
              </option>
            ))}
          </select>
        </label>
      )}
      <label>
        Tür
        <select value={kind} onChange={(e) => setKind(e.target.value as TeamKind)}>
          <option value="main">{projects ? "Ana takım (projeler)" : "Proje takımı"}</option>
          <option value="service">Alt takım (hizmet)</option>
        </select>
      </label>
      {!sourceId && <label>
        Profil
        <select value={profile} onChange={e => setProfile(e.target.value as TeamProfile)}>
          <option value="dev">Geliştirici</option>
          <option value="edu">Eğitim</option>
        </select>
      </label>}
      {kind === "main" ? (
        <div className="team-dir">
          <span title={cwd ?? ""}>{cwd ?? (projects ? "Varsayılan klasör düzeni" : "klasör seçilmedi")}</span>
          <FolderButton host={host} label="Klasör seç…" onPick={setCwd} />
          {cwd && <button type="button" onClick={() => setCwd(null)}>Varsayılan</button>}
        </div>
      ) : (
        <p className="team-note">Projeye bağlı değil; {host === LOCAL ? "uygulama" : "sunucu"} verisinde kendi çalışma alanı açılır.</p>
      )}
      {kind === "main" && !cwd && projects && <FolderPreview host={host} name={name} />}
      {template ? <fieldset className="team-clone-crew">
        <legend>Ekip: {template.members.length} üye</legend>
        {template.members.map((m, i) => {
          const update = (patch: Partial<typeof m>) => setTemplate(t => t && ({ ...t, members: t.members.map((member, j) => j === i ? { ...member, ...patch } : member) }));
          return <fieldset key={i} className="team-clone-member">
            <legend>Üye {i + 1} · {m.kind === "leader" ? "lider" : "worker"}</legend>
            <label>Ad<input aria-label={`Üye ${i + 1} adı`} value={m.name} onChange={e => update({ name: e.target.value })} /></label>
            <label>Rol<RoleSelect profile={template.profile} label={`Üye ${i + 1} rolü`} value={m.roleSlug ?? ""} onChange={roleSlug => update({ roleSlug, title: "" })} /></label>
            <label>Sağlayıcı<select aria-label={`Üye ${i + 1} sağlayıcısı`} value={m.provider ?? "anthropic"} onChange={e => update({ provider: e.target.value, model: providerModels(providers, e.target.value)[0]?.id ?? "" })}>
              {!providers.some(p => p.id === (m.provider ?? "anthropic")) && <option value={m.provider ?? "anthropic"}>{m.provider ?? "anthropic"}</option>}
              {providers.map(p => <option key={p.id} value={p.id}>{p.label}</option>)}
            </select></label>
            <label>Model<select aria-label={`Üye ${i + 1} modeli`} value={m.model} onChange={e => update({ model: e.target.value })}>
              {!m.model && <option value="">Model seç</option>}
              {providerModels(providers, m.provider, m.model).map(x => <option key={x.id} value={x.id}>{x.label}</option>)}
            </select></label>
          </fieldset>;
        })}
      </fieldset> : !sourceId && (profile === "edu" ? <p className="team-note">Öğretmen (lider) + Görsel Anlatıcı</p> : <fieldset className="team-crew">
        <legend>Ekip: 1 lider (Lead) + 4 worker</legend>
        {workerRoles.map((slug, i) => (
          <RoleSelect
            key={i}
            label={`Worker ${i + 1} rolü`}
            other={DEMOTED_ROLES}
            value={slug}
            onChange={(v) => setWorkerRoles(workerRoles.map((r, j) => (j === i ? v : r)))}
          />
        ))}
      </fieldset>)}
      </fieldset>
      {catalogLoading && <p className="team-note" role="status">Model kataloğu yükleniyor…</p>}
      {catalogError && <div className="team-form-fields">
        <p className="team-err" role="alert">{catalogError}</p>
        <button type="button" disabled={busy} onClick={() => { formRef.current?.focus(); setCatalogRetry(r => r + 1); }}>Model kataloğunu yeniden dene</button>
      </div>}
      {loading && <p className="team-note" role="status">Takım üyeleri yükleniyor…</p>}
      {templateError && <p className="team-err" role="alert">{templateError}</p>}
      {sourceId && !loading && !template && <button type="button" onClick={() => setRetry(r => r + 1)}>Yeniden dene</button>}
      {codexConverted.length > 0 && <p className="team-note" role="status">Codex üyeleri Claude'a çevrildi</p>}
      {template && !template.members.length && <p className="team-note">Kaynak takımda üye yok; yeni takım boş başlayacak.</p>}
      {sourceId && <p className="team-note">Hafıza, oturumlar ve projeler yeni takımda sıfırdan başlar.</p>}
      {educationSelected && (educationSupport === null
        ? <p className="team-note" role="status">Eğitim desteği kontrol ediliyor…</p>
        : !educationSupport && <p className="team-err" role="alert">Eğitim için bu hostun çekirdeğini güncelleyin.</p>)}
      {error && <p className="team-err" role="alert">{error}</p>}
      <div className="team-actions">
        <button type="button" disabled={busy} onClick={onDone}>Vazgeç</button>
        <button type="submit" className="primary" disabled={busy || loading || (educationSelected && educationSupport !== true) || !name.trim() || (!!sourceId && !template) || template?.members.some(m => !m.name.trim() || !m.model) || (kind === "main" && !cwd && !projects)}>{busy ? "Oluşturuluyor…" : "Oluştur"}</button>
      </div>
    </form>
    </>,
    document.body,
  );
}

type Member = ReturnType<typeof membersOf>[number];

/** Üye işlemleri: yıkıcı "sil" ⋯ menüde ve ConfirmDialog'la (yanlış satırda tek tıkla silme yok). */
function MemberMenu({ member, running, onRemove }: {
  member: Member;
  running: boolean;
  onRemove: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { if (open) itemRef.current?.focus(); }, [open]);
  const close = () => { setOpen(false); triggerRef.current?.focus(); };
  return (
    <span className="team-member-menu">
      <button
        type="button"
        className="icon-btn"
        ref={triggerRef}
        disabled={running}
        title={running ? "Çalışan ajan silinemez" : `${member.name} işlemleri`}
        aria-label={`${member.name} işlemleri`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        ⋯
      </button>
      {open && (
        <div
          className="team-menu"
          role="menu"
          aria-label={`${member.name} işlemleri`}
          onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOpen(false); }}
          onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } }}
        >
          <button
            type="button"
            ref={itemRef}
            role="menuitem"
            className="danger"
            onClick={() => { setOpen(false); setConfirm(true); }}
          >
            Üyeyi sil
          </button>
        </div>
      )}
      {confirm && (
        <ConfirmDialog
          title={`"${member.name}" takımdan çıkarılsın mı?`}
          lines={[
            running
              ? `${member.name} çalışıyor — oturumu kapanacak.`
              : `${member.name} takımdan çıkarılacak.`,
            ...(member.kind === "leader" ? ["Bu üye lider; takımsız kalan oturum olmaz."] : []),
            "Bu işlem geri alınamaz.",
          ]}
          confirmLabel="Üyeyi sil"
          onCancel={() => { setConfirm(false); triggerRef.current?.focus(); }}
          onConfirm={() => { setConfirm(false); onRemove(); }}
        />
      )}
    </span>
  );
}

export function TeamPanel({ teamId, onClose, onReset }: { teamId: string; onClose: () => void; onReset: () => void }) {
  const { teams } = useTeams();
  const team = teams.find((t) => t.id === teamId);
  const members = membersOf(teamId);
  const hasFav = useSyncExternalStore(subscribeLayout, () => getLayoutSnapshot().favorite !== null);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<AgentKind>("worker");
  const [role, setRole] = useState(team?.profile === "edu" ? "visual-explainer" : "frontend");
  const [model, setModel] = useState<string>("claude-sonnet-5-5");
  const [provider, setProvider] = useState("anthropic");
  // S4: models, providers and tokens are the team's host's (a remote team → its server).
  const host = team?.host ?? LOCAL;
  /** Provider switch sends provider+model together: the model must fit the provider. */
  const firstModel = (prov: string) => {
    const m = providerModels(providers, prov)[0]?.id;
    if (!m) throw new Error("Model listesi boş");
    return m;
  };
  const [baseProviders, setProviders] = useState<ProviderInfo[]>([]);  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [confirmAll, setConfirmAll] = useState(false);
  const [providerError, setProviderError] = useState<string | null>(null);
  const providerRequest = useRef(0);
  const reloadProviders = async () => {
    const request = ++providerRequest.current;
    try {
      const ps = await listProviders(host);
      if (request !== providerRequest.current) return;
      setProviders(ps); setProviderError(null);
      const lp = ps.find((p) => p.id === "local");
      // Unreachable server = no live list; the settings panel's own test shows the reason.
      const status = lp?.baseUrl && lp.hasToken ? await localStatus(host).catch(() => null) : null;
      if (request === providerRequest.current) setLocal(status);
    } catch {
      if (request === providerRequest.current) setProviderError("Sağlayıcı listesi alınamadı. Bağlantıyı kontrol edin; son alınan katalog gösteriliyor.");
    }
  };
  useEffect(() => {
    void reloadProviders();
    const off = onEvent("providers-changed", (_, eventHost) => {
      if (eventHost === host) void reloadProviders();
    });
    return () => { off(); providerRequest.current++; };
  }, [host]);
  const providers = withLocalModels(baseProviders, local);
  const newModels = providerModels(providers, provider);
  const chosenNewModel = newModels.some(m => m.id === model) ? model : newModels[0]?.id ?? "";
  const projects = useTeamProjects(teamId);
  useTick();

  if (!team) return null;
  const run = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  async function add(e: React.FormEvent) {
    e.preventDefault();
    if (!chosenNewModel) return;
    await run(async () => {
      if (role === "assistant" && assistantTaken(membersOf(teamId))) throw new Error("Takımda zaten bir asistan var");
      await addAgent({ teamId, name, kind, model: chosenNewModel, provider, roleSlug: role });
      setName("");
    });
  }

  const anyRunning = members.some((a) => isRunning(a.id));
  const projectList = team.kind === "main" && projects.status !== "unsupported";
  return (
    <div className="team-pop team-settings" onKeyDown={(e) => e.key === "Escape" && onClose()}>
      <h3>
        {team.name} <span className="team-kind">{team.kind === "main" ? "proje" : "alt takım"}</span>
        <span className="team-head-memory"><span>Hafıza</span><code>{team.memoryDir ?? "—"}</code></span>
        <button className="icon-btn close" onClick={onClose} title="Kapat"><X /></button>
      </h3>
      <dl className="team-info">
        {team.host !== LOCAL && (
          <>
            <dt>Sunucu</dt>
            <dd>{hostName(team.host)} · {HOST_STATUS_TEXT[hostStatus(team.host)]}</dd>
          </>
        )}
        {/* #838: a host with projects lists them below; an older one keeps its single folder. */}
        {!projectList && (
          <>
            <dt>Çalışma dizini</dt>
            <dd>
              <code>{team.workdir ?? "seçilmedi (kullanıcı dizini)"}</code>
              {team.kind === "main" && (
                <FolderButton host={team.host} label="Değiştir…" disabled={anyRunning} title={anyRunning ? "Çalışan ajan varken değişmez" : ""}
                  onPick={(dir) => void run(() => patchTeam(team.id, { cwd: dir }))} />
              )}
            </dd>
          </>
        )}
      </dl>

      {projectList && <ProjectsSection teamId={team.id} />}

      <table className="team-members">
        <thead>
          <tr><th>Ad</th><th>Rol</th><th>Sağlayıcı</th><th>Model</th><th>Zincir</th><th /></tr>
        </thead>
        <tbody>
          {members.map((a) => {
            const busy = isRunning(a.id);
            const activeTarget = a.activeProvider ? labelOf(providers, a.activeProvider) : null;
            const originalProvider = labelOf(providers, a.provider);
            return (
              <tr key={a.id} className={a.kind === "leader" ? "is-leader" : undefined}>
                <td data-label="Ad"><i className="swatch" style={{ background: a.shirt }} />{a.name}</td>
                <td data-label="Rol" title={a.title}>
                  <RoleSelect profile={team.profile} value={a.roleSlug ?? ""} disabled={busy} label={`${a.name} rolü`}
                    allowAssistant={a.kind === "worker"} assistantUnavailable={assistantTaken(members, a.id)}
                    onChange={(v) => run(() => {
                      if (v === "assistant" && assistantTaken(membersOf(teamId), a.id)) throw new Error("Takımda zaten bir asistan var");
                      return patchAgent(a.id, { roleSlug: v });
                    })} />
                </td>
                {/* DEX-13: provider switch sends provider+model together — the pair must stay catalog-valid. */}
                <td data-label="Sağlayıcı">
                  <select value={a.provider || "anthropic"} disabled={busy} aria-label={`${a.name} sağlayıcısı`}
                    onChange={(e) => {
                      const p = e.target.value;
                      run(() => patchAgent(a.id, { provider: p, model: firstModel(p) }));
                    }}>
                    {providers.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
                  </select>
                </td>
                <td data-label="Model">
                  <select value={a.model} disabled={busy} aria-label={`${a.name} modeli`}
                    onChange={(e) => run(() => patchAgent(a.id, { model: e.target.value }))}>
                    {providerModels(providers, a.provider, a.model).map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                  </select>
                  {a.pending && (
                    <span className="team-pending" title={`Oturum kapanınca: ${pendingText(a.pending, providers)}`}>bekliyor</span>
                  )}
                </td>
                {/* DEX-17: zincir — ajan başına aç/kapa; geçici sağlayıcı rozeti + "geri dön". */}
                <td data-label="Zincir">
                  <button className="icon-btn" title={a.chainOff ? "Zinciri aç" : "Zinciri kapat (bu ajan limitte bekler)"}
                    aria-label={`${a.name} zinciri ${a.chainOff ? "kapalı" : "açık"}`}
                    onClick={() => run(() => setAgentChain(a.id, !a.chainOff))}>
                    {a.chainOff ? "⛓️‍💥" : "⛓️"}
                  </button>
                  {activeTarget && (
                    <span className="team-pending" title={`Asıl sağlayıcı: ${originalProvider}. Zincirle geçici: ${activeTarget}`}>
                      →{activeTarget}{" "}
                      {a.chainReturnReady ? (
                        <button className="icon-btn" title={`${originalProvider} yeniden kullanılabilir — geri dön`}
                          aria-label={`${a.name} asıl sağlayıcıya dön`}
                          onClick={() => run(() => chainBack(a.id))}>↩</button>
                      ) : <span title={`${originalProvider} sıfırlanma saati bekleniyor`}>⏱</span>}
                    </span>
                  )}
                </td>
                <td data-label="İşlem">
                  <MemberMenu member={a} running={busy} onRemove={() => run(() => removeAgent(a.id))} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <form className="team-add" onSubmit={add}>
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Ad (boş → rol adı: Kernel-1, Pixel-1…)" />
        <select value={kind} onChange={(e) => {
          const k = e.target.value as AgentKind;
          setKind(k);
          if (team.profile === "edu") setRole(k === "leader" ? "teacher" : "visual-explainer");
          else if (k === "leader") setRole("lead");
        }}>
          <option value="leader">lider</option>
          <option value="worker">worker</option>
        </select>
        <RoleSelect profile={team.profile} value={role} onChange={setRole} label="Yeni üyenin rolü"
          allowAssistant={kind === "worker"} assistantUnavailable={assistantTaken(members)} />
        <select value={provider} aria-label="Yeni üyenin sağlayıcısı"
          onChange={(e) => {
            setProvider(e.target.value);
            setModel(providerModels(providers, e.target.value)[0]?.id ?? "");
          }}>
          {providers.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
        </select>
        <select value={chosenNewModel} disabled={!newModels.length} aria-label="Yeni üyenin modeli" onChange={(e) => setModel(e.target.value)}>
          {newModels.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
        </select>
        <button type="submit" className="primary" disabled={!chosenNewModel || role === "assistant" && assistantTaken(members)}>Ekle</button>
      </form>
      {role === "assistant" && assistantTaken(members) && <p className="team-warn" role="status">Takımda zaten bir asistan var</p>}

      {(nextTestLevel(team.testLevel) !== null || hostSupports(host, "team_clone_template")) && (
        <div className={`team-bottom-actions${nextTestLevel(team.testLevel) !== null && hostSupports(host, "team_clone_template") ? "" : " solo"}`}>
          <TestLevelToggle key={team.id} level={team.testLevel} onChange={(testLevel) => patchTeam(team.id, { testLevel })} />
          {hostSupports(host, "team_clone_template") && (
            <button type="button" className="team-copy-btn" onClick={() => void openTeamMenu("new", host, team.id).catch(e => setError(String(e)))}>Takımı kopyala</button>
          )}
        </div>
      )}
      {providerError && <p className="team-err" role="alert">{providerError}</p>}

      {error && <p className="team-err" role="alert">{error}</p>}
      <div className="team-actions">
        {/* DEX-36: panels back to the default screen; sessions keep running. */}
        <button onClick={onReset} title="Panelleri varsayılan yerleşime döndür (oturumlar sürer)">
          Düzeni sıfırla
        </button>
        <button
          onClick={() => {
            if (!saveFavoriteLayout(teamId)) setError("Panel henüz hazır değil — takım açılınca tekrar deneyin");
            else setError(null);
          }}
          title="Bu takımın düzenini favori yap — yeni takımlar bununla açılır, Kart sabitlenir (oturumlar sürer)"
        >
          {hasFav ? "★ Favori düzeni güncelle" : "★ Bu düzeni favori yap"}
        </button>
        {hasFav && (
          <>
            <button
              onClick={() => {
                if (!applyFavoriteLayout(teamId)) setError("Favori uygulanamadı");
                else setError(null);
              }}
              title="Favori düzeni bu takıma uygula (bu takımın kendi ajanlarıyla, oturumlar sürer)"
            >
              Favoriyi bu takıma uygula
            </button>
            <button
              onClick={() => setConfirmAll(true)}
              title="Favori düzeni tüm takımlara uygula (her takımın kendi ajanlarıyla, oturumlar sürer)"
            >
              Favoriyi tüm takımlara uygula
            </button>
          </>
        )}
        <button
          className="danger"
          disabled={team.host === LOCAL && teams.filter((t) => t.host === team.host).length < 2}
          title={team.host === LOCAL && teams.filter((t) => t.host === team.host).length < 2 ? "Son yerel takım silinemez" : anyRunning ? "Çalışan ajanları durdurup takımı, üyelerini ve görevlerini sil" : "Takımı, üyelerini ve görevlerini sil"}
          onClick={() => void run(async () => {
            // Görev #43: the core says what would stop; running members are stopped first, then the team goes.
            const info = await teamDeleteInfo(team.id);
            const leads = members.filter((a) => a.kind === "leader" && isRunning(a.id)).map((a) => a.name);
            const lines = [`"${team.name}" takımı, üyeleri ve görevleri silinsin mi?`];
            if (!info) lines.push("Bu sunucu eski sürüm; çalışan ajan bilgisi alınamadı.");
            else if (info.running.length) lines.push(`${info.running.join(", ")} çalışıyor — durdurulup takım silinecek.`);
            if (info && (info.openDelegations || info.activePlans)) {
              lines.push(`Açık: ${info.openDelegations} devretme, ${info.activePlans} plan — devretmeler raporsuz kapanır, planlar iptal olur.`);
            }
            if (leads.length) lines.push(`DİKKAT: konuştuğun lider (${leads.join(", ")}) bu takımda; oturumu da kapanacak.`);
            if (!(await askConfirm(lines.join("\n\n")))) return;
            if (info) return removeTeam(team.id, info.running.length > 0);
            // An old server cannot stop agents: the plain delete, refused while any is running.
            await removeTeam(team.id).catch((err) => {
              const msg = err instanceof Error ? err.message : String(err);
              if (!/çalışan ajan/.test(msg)) throw err;
              throw new Error("Sunucu eski sürüm ve takımda çalışan ajan var — sunucudaki ajanları durdurup tekrar deneyin ya da sunucuyu güncelleyin.");
            });
          })}
        >
          Takımı sil
        </button>
      </div>
      {confirmAll && (
        <ConfirmDialog
          title="Favori tüm takımlara uygulansın mı?"
          lines={[
            `${teams.length} takımın panelleri favori düzene döner.`,
            "Oturumlar sürer; her takım kendi ajanlarıyla açılır.",
          ]}
          confirmLabel="Tümüne uygula"
          onCancel={() => setConfirmAll(false)}
          onConfirm={() => {
            const n = applyFavoriteToAll(teams.map((t) => t.id));
            if (!n) setError("Favori uygulanamadı");
            else setError(null);
            setConfirmAll(false);
          }}
        />
      )}
    </div>
  );
}

/** DEX-17: sağlayıcı id → görünen ad. */
function labelOf(providers: ProviderInfo[], id: string): string {
  return providers.find((x) => x.id === id)?.label ?? id;
}

function pendingText(p: PendingModel, providers: ProviderInfo[]): string {
  const prov = providers.find((x) => x.id === (p.provider || "anthropic"))?.label ?? p.provider;
  return `${prov} · ${p.model}`;
}
