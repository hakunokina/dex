// #47: the Kart's one-line toolbar — the active team's status count and the team/app controls
// that used to sit in the top bar. Kart closed → the palette has the same commands.
import { useEffect, useState } from "react";
import { Brain, Settings, SquareKanban, Users } from "lucide-react";
import { AGENTS, getStatus } from "../agents";
import { toggleBoard, useBoardOpen } from "../board/store";
import { openPanel } from "../layout/panels";
import { watchTeamCost } from "../usage/costStore";
import { useCountUp } from "../motion";
import { useDelegations } from "../delegations";
import { roleQueueSummary } from "../delegationsModel";
import { useTeams } from "../team/store";
import { WorkersBulkButton } from "../team/WorkersBulk";

export function OfficeBar({ teamId, active }: { teamId: string; active: boolean }) {
  const [counts, setCounts] = useState({ working: 0, idle: 0 });
  const boardOpen = useBoardOpen();
  // Çip/hover maliyeti (agentToday) izlenen takımdan okunur; token metni şeritte (#2139).
  useEffect(() => (active ? watchTeamCost(teamId) : undefined), [active, teamId]);
  const delegations = useDelegations();
  const { roles } = useTeams();
  const queue = roleQueueSummary(delegations, teamId, roles);
  const [queueDismissed, setQueueDismissed] = useState(false);
  useEffect(() => {
    if (!queue.count) return;
    const dismiss = (event: KeyboardEvent) => {
      if (event.key === "Escape") setQueueDismissed(true);
    };
    window.addEventListener("keydown", dismiss);
    return () => window.removeEventListener("keydown", dismiss);
  }, [queue.count]);
  useEffect(() => {
    if (!active) return;
    const tick = () => {
      const s = AGENTS.map((a) => getStatus(a.id));
      setCounts({ working: s.filter((x) => x === "working").length, idle: s.filter((x) => x === "idle").length });
    };
    tick();
    const t = setInterval(tick, 500);
    return () => clearInterval(t);
  }, [active]);
  const working = useCountUp(counts.working);
  const idle = useCountUp(counts.idle);
  return (
    <div className="office-bar" role="group" aria-label="Kart araçları">
      <span className={`office-queue${queueDismissed ? " dismissed" : ""}`}
        onMouseEnter={() => setQueueDismissed(false)} onFocus={() => setQueueDismissed(false)}
        onBlur={() => setQueueDismissed(true)}>
        <span className="stat" tabIndex={queue.count ? 0 : undefined}
          aria-label={queue.count ? `${counts.working} çalışıyor · ${counts.idle} boşta · ${queue.count} kuyrukta. ${queue.title}` : undefined}>
          <b className="red">{working}</b> çalışıyor · <b>{idle}</b> boşta
          {queue.count > 0 && <> · <b>{queue.count}</b> kuyrukta</>}
        </span>
        {queue.count > 0 && <span className="office-queue-tip" aria-hidden="true">{queue.title}</span>}
      </span>
      {/* #70: the board button left the top bar; the palette and ` still open it too. */}
      <button className={`office-bar-btn${boardOpen ? " on" : ""}`} onClick={toggleBoard} aria-pressed={boardOpen} title="Görevler (` · terminaldeyken Ctrl+`)" aria-label="Görevler">
        <SquareKanban />
      </button>
      <WorkersBulkButton teamId={teamId} />
      <button className="office-bar-btn" onClick={() => void openPanel(teamId, "team")} title="Takım ayarları" aria-label="Takım ayarları">
        <Users />
      </button>
      <button className="office-bar-btn" onClick={() => void openPanel(teamId, "memory")} title="Hafıza haritası" aria-label="Hafıza haritası">
        <Brain />
      </button>
      <button className="office-bar-btn" onClick={() => void openPanel(teamId, "settings")} title="Ayarlar" aria-label="Ayarlar">
        <Settings />
      </button>
    </div>
  );
}
