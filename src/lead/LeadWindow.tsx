// #2258: the pop-out window's page (`?pane=lead&team=…&agent=…[&ontop=1]`): one leader pane in a
// native window of its own, PiP-like. State comes from dexd exactly as in the main window (its own
// socket; its own PTY subscription — the main window released its one and takes none while this
// window lives: src/layout/popoutWindows.ts). The layout is read, never written; patron-side
// reactions stay with the main window (src/api/platform.ts POPOUT). This page reports its place
// and the on-top switch to the main window, which remembers them; the shell reports its closing.
import { useEffect, useState } from "react";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Pin, PinOff, X } from "lucide-react";
import { TerminalPane } from "../TerminalPane";
import { setPtyPolicy } from "../pty";
import { initTeams, selectTeam, useTeams } from "../team/store";
import { setLayoutPersistence } from "../layout/store";
import { parseLeadQuery } from "../layout/popout";
import { teamAccent } from "../team/accent";
import "../App.css";
import "./lead.css";

const query = parseLeadQuery(location.search)!;
setLayoutPersistence(false);
setPtyPolicy(id => id === query.agentId);

/** Where this window sits, in logical pixels, for the main window to remember. Not a Tauri window (e2e): nothing. */
async function reportBounds() {
  const win = getCurrentWindow();
  try {
    const [scale, pos, size] = await Promise.all([win.scaleFactor(), win.outerPosition(), win.innerSize()]);
    const p = pos.toLogical(scale);
    const s = size.toLogical(scale);
    await emitTo("main", "dex:lead-bounds", { agentId: query.agentId, bounds: { x: p.x, y: p.y, width: s.width, height: s.height } });
  } catch { /* no window to measure */ }
}

export default function LeadWindow() {
  const { ready, teams, agents } = useTeams();
  const [onTop, setOnTop] = useState(query.onTop);
  useEffect(() => {
    void initTeams().then(() => selectTeam(query.teamId));
  }, []);
  useEffect(() => {
    const win = getCurrentWindow();
    let timer = 0;
    const report = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void reportBounds(), 300);
    };
    const offs = [win.onMoved(report), win.onResized(report)];
    return () => {
      window.clearTimeout(timer);
      offs.forEach(off => void off.then(f => f()).catch(() => {}));
    };
  }, []);

  const team = teams.find(t => t.id === query.teamId);
  const agent = agents.find(a => a.id === query.agentId);
  const title = team && agent ? `${team.name} · ${agent.name}` : "Dex";

  const toggleTop = async () => {
    const next = !onTop;
    setOnTop(next);
    await getCurrentWindow().setAlwaysOnTop(next).catch(() => {});
    await emitTo("main", "dex:lead-ontop", { agentId: query.agentId, onTop: next }).catch(() => {});
  };

  return (
    <div className="lead-window" data-accent={team ? teamAccent(team) : undefined}>
      {/* No native title bar (as the main window): this bar drags the window. */}
      <header className="lead-bar" data-tauri-drag-region>
        <span className="lead-title" data-tauri-drag-region title={title}>{title}</span>
        <button className={`lead-pin${onTop ? " on" : ""}`} aria-pressed={onTop} onClick={() => void toggleTop()}
          title={onTop ? "Üstte tutmayı bırak" : "Pencereyi diğer pencerelerin üstünde tut"}>
          {onTop ? <Pin /> : <PinOff />} Üstte tut
        </button>
        <button className="lead-close" onClick={() => void getCurrentWindow().close()} title="Pencereyi kapat (panel ana pencereye döner)" aria-label="Kapat">
          <X />
        </button>
      </header>
      <main className="lead-body">
        {ready && team && agent
          ? <TerminalPane teamId={query.teamId} agentId={query.agentId} focused onFocus={() => {}} />
          : <p className="lead-note" role="status">{ready ? "Bu lider artık takımda değil — pencereyi kapat." : "Bağlanıyor…"}</p>}
      </main>
    </div>
  );
}
