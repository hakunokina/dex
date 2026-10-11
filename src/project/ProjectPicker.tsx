// #838 H: the active team's project picker in the top bar (only when it has more than one open
// project). The choice is the team's selected project: new patron shells open in it and the
// leader's next session takes it — running workers and a running leader are never switched.
import { toast } from "../toast/store";
import { useState } from "react";
import { teamAccent } from "../team/accent";
import { useTeams } from "../team/store";
import { activeProjects, projectErrorText, showsProjects } from "./model";
import { leaderNote, selectedProject, setDefaultProject, useTeamProjects } from "./store";
import "./project.css";

export function ProjectPicker({ teamId }: { teamId: string }) {
  const { status, list } = useTeamProjects(teamId);
  const team = useTeams().teams.find((t) => t.id === teamId); // the selection is its `defaultProjectId`
  const [busy, setBusy] = useState(false);
  if (status === "error") return <span className="project-chip" role="alert" title="Takım panelinden yeniden dene">Projeler okunamadı</span>;
  if (!team || status !== "ready" || !showsProjects(list)) return null;
  const open = activeProjects(list);
  const selected = selectedProject(teamId)?.id ?? "";
  return (
    <select className="project-pick" aria-label="Aktif proje" value={selected} disabled={busy} data-accent={teamAccent(team)}
      title="Aktif proje: yeni terminal ve liderin yeni oturumu burada açılır; çalışan işler değişmez"
      onChange={(e) => {
        const p = open.find((x) => x.id === e.target.value);
        if (!p) return;
        setBusy(true);
        void setDefaultProject(teamId, p.id).then(
          () => leaderNote(teamId, p.name),
          (err) => toast({ key: `project-select:${teamId}`, tone: "err", title: "Proje seçilemedi", body: projectErrorText(err) }),
        ).finally(() => setBusy(false));
      }}>
      {!selected && <option value="">Proje seçili değil</option>}
      {open.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
    </select>
  );
}
