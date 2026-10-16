// #838 H: the active team's project picker in the top bar (only when it has more than one open
// project). The choice is the team's selected project: new patron shells open in it and the
// leader's next session takes it — running workers and a running leader are never switched.
// #2141: a select-only combobox (not a native <select>) so its list can end in a "+" row that
// turns into a name field: Enter makes that folder under the category root and binds it, the same
// call as Takım ayarları → Projeler → "Oluştur ve bağla" (which does not select it either).
import { toast } from "../toast/store";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, Plus } from "lucide-react";
import { teamAccent } from "../team/accent";
import { useTeams } from "../team/store";
import { bulkSpot, type BulkSpot } from "../team/bulkPlace";
import { activeProjects, folderCategoryReady, pickStep, projectErrorText, showsProjects, type Project } from "./model";
import { createFolderProject, leaderNote, selectedProject, setDefaultProject, useTeamProjects } from "./store";
import "./project.css";

const ADD = "+"; // the "+" row's key among the project ids

export function ProjectPicker({ teamId }: { teamId: string }) {
  const { status, list } = useTeamProjects(teamId);
  const team = useTeams().teams.find((t) => t.id === teamId); // the selection is its `defaultProjectId`
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const listId = useId();
  if (status === "error") return <span className="project-chip" role="alert" title="Takım panelinden yeniden dene">Projeler okunamadı</span>;
  if (!team || status !== "ready" || !showsProjects(list)) return null;
  const projects = activeProjects(list);
  const selected = selectedProject(teamId);
  const close = () => { setOpen(false); btnRef.current?.focus(); };
  const choose = (p: Project) => {
    close();
    if (p.id === selected?.id) return;
    setBusy(true);
    void setDefaultProject(teamId, p.id).then(
      () => leaderNote(teamId, p.name),
      (err) => toast({ key: `project-select:${teamId}`, tone: "err", title: "Proje seçilemedi", body: projectErrorText(err) }),
    ).finally(() => setBusy(false));
  };
  return (
    <>
      <button ref={btnRef} type="button" className="project-pick" role="combobox" aria-label="Aktif proje"
        aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? listId : undefined}
        disabled={busy} data-accent={teamAccent(team)}
        title="Aktif proje: yeni terminal ve liderin yeni oturumu burada açılır; çalışan işler değişmez"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => { if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); setOpen(true); } }}>
        <span className="project-pick-name">{selected?.name ?? "Proje seçili değil"}</span>
        <ChevronDown aria-hidden />
      </button>
      {open && <PickerList accent={teamAccent(team)} listId={listId} anchorRef={btnRef} projects={projects} selected={selected?.id ?? null}
        teamId={teamId} host={team.host} categoryRoot={folderCategoryReady(team.workspaceRoot ?? null, team.suggestedWorkspaceRoot ?? null) ? team.workspaceRoot! : null}
        onChoose={choose} onClose={close} onDismiss={() => setOpen(false)} />}
    </>
  );
}

function PickerList({ accent, listId, anchorRef, projects, selected, teamId, host, categoryRoot, onChoose, onClose, onDismiss }: {
  accent: string; listId: string; anchorRef: React.RefObject<HTMLButtonElement | null>; projects: Project[]; selected: string | null;
  teamId: string; host: string; categoryRoot: string | null;
  onChoose: (p: Project) => void; onClose: () => void; onDismiss: () => void;
}) {
  const popRef = useRef<HTMLDivElement>(null);
  const [spot, setSpot] = useState<BulkSpot | null>(null);
  const keys = [...projects.map((p) => p.id), ...(categoryRoot ? [ADD] : [])];
  const [cur, setCur] = useState(selected ?? keys[0]);
  const [naming, setNaming] = useState<string | null>(null); // the "+" row's field (null = closed)
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // #2134 pattern: portal + fixed — the tab strip scrolls (overflow-x) and would clip an absolute list.
  useLayoutEffect(() => {
    const el = popRef.current, anchor = anchorRef.current;
    if (!el || !anchor) return;
    const next = bulkSpot(anchor.getBoundingClientRect(),
      { w: el.offsetWidth, h: el.scrollHeight }, { w: window.innerWidth, h: window.innerHeight });
    setSpot((prev) => prev && prev.left === next.left && prev.top === next.top
      && prev.maxHeight === next.maxHeight && prev.above === next.above ? prev : next);
  });
  useEffect(() => {
    const onResize = () => setSpot(null);
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!popRef.current?.contains(t) && !anchorRef.current?.contains(t)) onDismiss();
    };
    window.addEventListener("resize", onResize);
    document.addEventListener("pointerdown", onDown);
    return () => { window.removeEventListener("resize", onResize); document.removeEventListener("pointerdown", onDown); };
  }, []);
  // The row under the arrow keys holds the focus (roving tabindex); the field takes it while open.
  useEffect(() => {
    if (naming !== null || !spot) return; // a hidden (unplaced) list cannot take the focus
    popRef.current?.querySelector<HTMLElement>(`[data-pick="${CSS.escape(cur)}"]`)?.focus();
  }, [cur, naming === null, !!spot]);

  const create = () => {
    const name = naming?.trim();
    if (!name || creating || !categoryRoot) return;
    setCreating(true);
    setError(null);
    void createFolderProject(teamId, host, categoryRoot, name).then(
      () => setNaming(null),
      (err) => setError(projectErrorText(err)),
    ).finally(() => setCreating(false));
  };

  return createPortal(
    <div ref={popRef} className="project-pick-pop" data-accent={accent} data-above={spot?.above || undefined}
      style={{ left: spot?.left ?? -9999, top: spot?.top ?? -9999, maxHeight: spot?.maxHeight, visibility: spot ? "visible" : "hidden" }}
      onKeyDown={(e) => {
        if (e.key === "Escape") { e.stopPropagation(); onClose(); return; }
        if (e.key === "Tab") { onDismiss(); return; } // the focus moves on from the trigger's place
        if (e.target instanceof HTMLInputElement) return;
        const next = pickStep(keys.indexOf(cur), e.key, keys.length);
        if (keys[next] !== cur) { e.preventDefault(); setCur(keys[next]); }
        else if (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End") e.preventDefault();
      }}
      onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null) && e.relatedTarget !== anchorRef.current && e.relatedTarget) onDismiss(); }}>
      <ul id={listId} role="listbox" aria-label="Projeler">
        {projects.map((p) => (
          <li key={p.id} role="option" className="project-pick-item" data-pick={p.id} aria-selected={p.id === selected}
            tabIndex={p.id === cur ? 0 : -1} title={p.rootPath ?? "klasör bağlı değil"}
            onClick={() => onChoose(p)} onFocus={() => setCur(p.id)}
            onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onChoose(p); } }}>
            {p.name}
          </li>
        ))}
      </ul>
      {!categoryRoot ? (
        <p className="project-pick-hint">Kategori kökü seç: Takım ayarları</p>
      ) : naming === null ? (
        <button type="button" className="project-pick-item add" data-pick={ADD} tabIndex={cur === ADD ? 0 : -1}
          aria-label="Yeni klasörde proje" title="Takımın kategori kökünde yeni klasör açar ve proje olarak bağlar"
          onFocus={() => setCur(ADD)} onClick={() => { setError(null); setNaming(""); }}>
          <Plus aria-hidden />
        </button>
      ) : (
        <input className="project-pick-item add" autoFocus aria-label="Yeni klasör adı" placeholder="Yeni klasör adı, Enter"
          value={naming} readOnly={creating} aria-busy={creating} aria-invalid={!!error} aria-describedby={error ? `${listId}-err` : undefined}
          onChange={(e) => { setNaming(e.target.value); setError(null); }}
          onKeyDown={(e) => {
            if (e.key === "Enter") { e.preventDefault(); create(); }
            else if (e.key === "Escape") { e.stopPropagation(); setNaming(null); setError(null); }
          }}
          onBlur={() => { if (!creating) setNaming(null); }} />
      )}
      {creating && <p className="project-pick-hint" role="status">Klasör açılıyor…</p>}
      {error && <p id={`${listId}-err`} className="project-pick-err" role="alert">{error}</p>}
    </div>, document.body);
}
