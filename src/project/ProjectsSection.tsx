// #838 H: the team panel's project list — in place of the single "Çalışma dizini" row. Bind an
// existing folder/repo (or make one under the category folder in the picker), rename, re-bind the
// root/branch, worktrees, select, archive / reopen. Every refusal is the core's (busy, conflict…).
import { useEffect, useRef, useState } from "react";
import { callHost, hostInfo } from "../api/hosts";
import { patchTeam, useTeams } from "../team/store";
import { categoryFolder, projectContext } from "./api";
import { FolderButton } from "./FolderButton";
import { askConfirm } from "../confirm";
import { activeProjects, folderName, projectErrorText, projectState, STATE_TEXT, type Project } from "./model";
import {
  archiveProject, createProject, leaderNote, reloadProjects, reopenProject, selectedProject, setDefaultProject, updateProject, useChecks,
  useTeamProjects, verifyProject,
} from "./store";
import "./project.css";

export function ProjectsSection({ teamId }: { teamId: string }) {
  const { status, list, error: loadError } = useTeamProjects(teamId);
  const team = useTeams().teams.find((t) => t.id === teamId);
  const checks = useChecks();
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [archived, setArchived] = useState(false);
  const [busy, setBusy] = useState(false);
  const [workspaceRoot, setWorkspaceRoot] = useState<string | null>(null);
  const sectionRef = useRef<HTMLElement>(null);
  const editingRef = useRef<string | null>(null);
  useEffect(() => {
    const previous = editingRef.current;
    editingRef.current = editing;
    if (previous && !editing) {
      Array.from(sectionRef.current?.querySelectorAll<HTMLButtonElement>("[data-edit-project]") ?? [])
        .find(button => button.dataset.editProject === previous)?.focus();
    }
  }, [editing]);
  const selected = selectedProject(teamId)?.id ?? null;
  useEffect(() => {
    let live = true;
    void projectContext(teamId).then(c => { if (live) setWorkspaceRoot(c.workspaceRoot); },
      e => { if (live) setError(projectErrorText(e)); });
    return () => { live = false; };
  }, [teamId, team?.workspaceRoot]);

  // The list never touches the disk: check each open project's folder/branch once per panel.
  const ids = activeProjects(list).map((p) => `${p.id}:${p.rootPath}:${p.defaultBranch}`).join("\n");
  useEffect(() => {
    let live = true;
    void (async () => {
      for (const p of activeProjects(list)) {
        if (!live) return;
        if (p.rootPath) await verifyProject(p).catch((e) => live && setError(projectErrorText(e)));
      }
    })();
    return () => void (live = false);
  }, [ids]);

  const run = async (f: () => Promise<unknown>) => {
    if (busy) return false;
    setBusy(true);
    setError(null);
    try {
      await f();
      return true;
    } catch (err) {
      setError(projectErrorText(err));
      return false;
    } finally { setBusy(false); }
  };

  if (!team) return null;
  if (status === "loading") return <section className="project-list" aria-label="Projeler"><h4>Projeler</h4><p className="team-note">Projeler yükleniyor…</p></section>;
  if (status === "error" && !list.length) {
    return (
      <section className="project-list" aria-label="Projeler">
        <h4>Projeler</h4>
        <p className="team-err" role="alert">Projeler okunamadı: {loadError}</p>
        <div><button type="button" onClick={() => void reloadProjects(teamId)}>Yeniden dene</button></div>
      </section>
    );
  }

  const open = activeProjects(list);
  const closed = list.filter((p) => p.archivedAt !== null);
  const shown = archived ? list : open;
  return (
    <section ref={sectionRef} className="project-list" aria-label="Projeler" aria-busy={busy}>
      <h4>
        Projeler <span className="team-kind">{open.length}</span>
        {closed.length > 0 && (
          <button type="button" className="project-link" aria-pressed={archived} onClick={() => setArchived((a) => !a)}>
            {archived ? "Arşivdekileri gizle" : `Arşivdekiler (${closed.length})`}
          </button>
        )}
      </h4>
      {status === "error" && <p className="team-err" role="alert">Liste yenilenemedi: {loadError}</p>}
      {!shown.length && <p className="team-note">Henüz proje yok — bir klasör ya da repo bağla.</p>}
      <fieldset className="project-fields" disabled={busy}>
      <ul>
        {shown.map((p) => editing === p.id ? (
          <ProjectEdit key={p.id} project={p} start={workspaceRoot} onDone={() => setEditing(null)} run={run} />
        ) : (
          <ProjectRow key={p.id} project={p} selected={p.id === selected} check={checks[p.id]}
            onEdit={() => { setError(null); setEditing(p.id); }}
            onSelect={() => void run(async () => {
              await setDefaultProject(teamId, p.id);
              leaderNote(teamId, p.name);
            })}
            onArchive={async () => {
              if (!(await askConfirm(`"${p.name}" arşivlensin mi?\n\nKlasör, git, worktree, rapor ve hafıza silinmez; yeniden açılabilir.`))) return;
              void run(() => archiveProject(p));
            }}
            onReopen={() => void run(() => reopenProject(p))} />
        ))}
      </ul>
      <AddProject key={teamId} teamId={teamId} host={team.host} start={workspaceRoot} categoryRoot={team.workspaceRoot ?? null} suggestedRoot={team.suggestedWorkspaceRoot ?? null} run={run} />
      </fieldset>
      {busy && <p className="team-note" role="status">İşlem sürüyor…</p>}
      {error && <p className="team-err" role="alert">{error}</p>}
    </section>
  );
}

function ProjectRow({ project: p, selected, check, onEdit, onSelect, onArchive, onReopen }: {
  project: Project; selected: boolean; check: Parameters<typeof projectState>[1];
  onEdit: () => void; onSelect: () => void; onArchive: () => void; onReopen: () => void;
}) {
  const state = projectState(p, check);
  return (
    <li className={`project-row${selected ? " selected" : ""}${p.archivedAt !== null ? " archived" : ""}`}>
      <div className="project-row-head">
        <b>{p.name}</b>
        {selected && <span className="project-badge on">aktif</span>}
        <span className={`project-badge state-${state}`}>{STATE_TEXT[state]}</span>
        {p.repoKey && <span className="project-badge">{p.worktrees ? "worktree açık" : "worktree kapalı"}</span>}
        {p.worktreeCleanup > 0 && <span className="project-badge warn" title="Birleşmiş ama silinmemiş worktree'ler">temizlik bekliyor: {p.worktreeCleanup}</span>}
      </div>
      <div className="project-row-meta">
        <code title={p.rootPath ?? ""}>{p.rootPath ?? "klasör bağlı değil"}</code>
        {p.defaultBranch && <span className="project-branch" title="Merge dalı">⎇ {p.defaultBranch}</span>}
      </div>
      <div className="project-row-actions">
        {p.archivedAt !== null ? (
          <button type="button" onClick={onReopen}>Yeniden aç</button>
        ) : (
          <>
            {!selected && <button type="button" onClick={onSelect} title="Yeni terminaller ve liderin yeni oturumu bu projede açılır; çalışan işler değişmez">Aktif yap</button>}
            <button type="button" data-edit-project={p.id} onClick={onEdit}>Düzenle</button>
            <button type="button" className="danger" onClick={onArchive}>Arşivle</button>
          </>
        )}
      </div>
    </li>
  );
}

function ProjectEdit({ project: p, start, onDone, run }: {
  project: Project; start?: string | null; onDone: () => void; run: (f: () => Promise<unknown>) => Promise<boolean>;
}) {
  const [name, setName] = useState(p.name);
  const [branch, setBranch] = useState(p.defaultBranch ?? "");
  const [root, setRoot] = useState(p.rootPath);
  const [worktrees, setWorktrees] = useState(p.worktrees);

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const patch = {
      ...(name.trim() !== p.name ? { name: name.trim() } : {}),
      ...(worktrees !== p.worktrees ? { worktrees } : {}),
    };
    const change = {
      ...(Object.keys(patch).length ? { patch } : {}),
      ...(root && root !== p.rootPath ? { root } : {}),
      ...(branch.trim() && branch.trim() !== (p.defaultBranch ?? "") ? { branch: branch.trim() } : {}),
    };
    if (!Object.keys(change).length) return onDone();
    if (await run(() => updateProject(p, change))) onDone();
  }

  return (
    <li className="project-row editing">
      <form className="project-edit" aria-label={`${p.name} düzenle`} onSubmit={save} onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); onDone(); } }}>
        <label>Ad<input autoFocus value={name} onChange={(e) => setName(e.target.value)} /></label>
        <label>Merge dalı<input value={branch} placeholder="checkout'taki dal" onChange={(e) => setBranch(e.target.value)} /></label>
        <div className="team-dir">
          <span title={root ?? ""}>{root ?? "klasör bağlı değil"}</span>
          <FolderButton host={p.host} teamId={p.teamId} label="Kökü değiştir…" start={root ?? start} onPick={setRoot} />
        </div>
        {p.repoKey && (
          <label className="project-check">
            <input type="checkbox" checked={worktrees} onChange={(e) => setWorktrees(e.target.checked)} />
            İşler kendi git worktree'sinde
          </label>
        )}
        <p className="team-note">Kök ya da dal değişimi süren iş/oturum varken reddedilir; dosyalar taşınmaz.</p>
        <div className="team-actions">
          <button type="button" onClick={onDone}>Vazgeç</button>
          <button type="submit" className="primary" disabled={!name.trim()}>Kaydet</button>
        </div>
      </form>
    </li>
  );
}

function AddProject({ teamId, host, start, categoryRoot, suggestedRoot, run }: {
  teamId: string; host: string; start?: string | null; categoryRoot: string | null; suggestedRoot: string | null;
  run: (f: () => Promise<unknown>) => Promise<boolean>;
}) {
  const [existing, setExisting] = useState(false);
  const [root, setRoot] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [branch, setBranch] = useState("");

  async function add(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim() || (existing ? !root : !categoryRoot || !!suggestedRoot)) return;
    const ok = await run(async () => {
      // project_list alone predates createFolder; require the newer folder API's explicit advertisement.
      const target = existing ? root! : hostInfo(host)?.methods?.includes("team_folder_preview")
        ? { createFolder: true as const }
        : await callHost<string>(host, "fs.mkdir", { parent: categoryRoot!, name: name.trim() });
      return createProject(teamId, { name: name.trim() }, target, branch.trim() || null);
    });
    if (ok) { setRoot(null); setName(""); setBranch(""); }
  }

  return (
    <form className="project-add" onSubmit={add} aria-label="Proje ekle">
      <div className="project-add-modes">
        <button type="button" aria-pressed={!existing} onClick={() => { setExisting(false); setRoot(null); setName(""); }}>Yeni klasör aç</button>
        <button type="button" aria-pressed={existing} onClick={() => { setExisting(true); setName(""); }}>Mevcut klasör bağla</button>
      </div>
      {!existing && <>
        {suggestedRoot && <div className="project-category">
          <p className="team-note">Kategori kökü: <code>{suggestedRoot}</code></p>
          <button type="button" onClick={() => void run(() => patchTeam(teamId, { workspaceRoot: suggestedRoot }))}>Kullan</button>
        </div>}
        {!categoryRoot && !suggestedRoot && <FolderButton host={host} label="Kategori kökü seç" title="Main'in yanına klasör açmak için kategori klasörünü seç"
          onPick={dir => void run(async () => { await patchTeam(teamId, { workspaceRoot: await categoryFolder(host, dir) }); })} />}
        {categoryRoot && !suggestedRoot && <>
          <p className="team-note">Yeni klasör takımın kategori klasöründe oluşturulur.</p>
          <details>
            <summary>Kategori kökünü değiştir</summary>
            <FolderButton host={host} label="Kategori kökü seç" onPick={dir => void run(async () => {
              await patchTeam(teamId, { workspaceRoot: await categoryFolder(host, dir) });
            })} />
          </details>
        </>}
      </>}
      {existing && <div className="team-dir">
        <span title={root ?? ""}>{root ?? "klasör seçilmedi"}</span>
        <FolderButton host={host} teamId={teamId} label="Klasör seç…" start={categoryRoot ?? start} onPick={dir => { setRoot(dir); if (!name.trim()) setName(folderName(dir)); }} />
      </div>}
      <div className="project-add-row">
        {!existing && <label>Yeni klasör adı<input aria-label="Yeni klasör adı" placeholder="ör. Site" value={name} onChange={e => setName(e.target.value)} /></label>}
        {existing && root && <label>Proje adı<input aria-label="Proje adı" value={name} onChange={e => setName(e.target.value)} /></label>}
        <label>Merge dalı<input aria-label="Merge dalı" placeholder="Boş: checkout’taki dal" value={branch} onChange={e => setBranch(e.target.value)} /></label>
      </div>
      <div className="team-actions">
        <button type="submit" className="primary" disabled={!name.trim() || (existing ? !root : !categoryRoot || !!suggestedRoot)}>{existing ? "Proje ekle" : "Oluştur ve bağla"}</button>
      </div>
    </form>
  );
}
