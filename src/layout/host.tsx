// DEX-36: every agent/shell view lives in a stable slot element; a dockview panel only borrows it.
// Moving, tabbing, floating or closing a panel never remounts an xterm or touches its PTY, and an
// agent whose panel is closed keeps its TerminalPane mounted (office slaps, delegations, resumes).
import { useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import type { DockviewPanelApi } from "dockview-react";
import { kept } from "../hot";
import { TerminalPane } from "../TerminalPane";
import { ShellPane } from "../ShellPane";
import { getStatus, subscribeAgentStatus } from "../agents";
import { useTeams } from "../team/store";
import { getSnapshot, shellPtyId, subscribe, type ShellRec } from "./store";
import { agentPanelId, shellPanelId } from "./panels";

export { agentPanelId, shellPanelId };

const hot = import.meta.hot;
const slots = kept(hot, "slots", () => new Map<string, HTMLDivElement>());
// Off-screen but laid out (visibility, not display): a parked xterm keeps a real size, so it never
// refits its PTY to nothing, and one opened here measures its cells normally.
const parking = kept(hot, "parking", () => {
  const el = document.createElement("div");
  el.className = "slot-parking";
  el.inert = true;
  document.body.appendChild(el);
  return el;
});

function slotFor(key: string): HTMLDivElement {
  let el = slots.get(key);
  if (!el) {
    el = document.createElement("div");
    el.className = "slot";
    parking.appendChild(el);
    slots.set(key, el);
  }
  return el;
}

/**
 * Panel body: adopts the slot while mounted, parks it (at its last size) when the panel goes.
 * FIX-1: a slot is `inert` unless its panel is on screen — a hidden tab's xterm textarea is still in
 * the DOM, and xterm eats Tab, so keyboard focus that lands there could never leave.
 */
export function SlotPanel({ slot, api }: { slot: string; api: DockviewPanelApi }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const box = ref.current!;
    const el = slotFor(slot);
    el.style.width = el.style.height = "";
    box.appendChild(el);
    el.inert = !api.isVisible;
    const vis = api.onDidVisibilityChange(e => { el.inert = !e.isVisible; });
    return () => {
      vis.dispose();
      el.inert = false; // parking is inert as a whole; the next panel sets its own state
      if (el.parentElement !== box) return;
      const { width, height } = el.getBoundingClientRect();
      if (width > 0 && height > 0) {
        el.style.width = `${width}px`;
        el.style.height = `${height}px`;
      }
      parking.appendChild(el);
    };
  }, [slot, api]);
  return <div className="slot-box" ref={ref} />;
}

const NO_SHELLS: ShellRec[] = [];

interface HostProps {
  teamId: string;
  opened: boolean;
  focusedId: string | null;
  onFocus: (id: string) => void;
}

/** Unopened teams watch agent status. Existing shells stay pinned; only a visited workspace creates new ones. */
export function PaneHost({ teamId, opened, focusedId, onFocus }: HostProps) {
  const agents = useTeams().agents.filter(a => a.teamId === teamId);
  const shells = useSyncExternalStore(subscribe, () => getSnapshot().teams[teamId]?.shells ?? NO_SHELLS);
  return (
    <>
      {agents.map(a =>
        <AgentSlot key={a.id} teamId={teamId} agentId={a.id} opened={opened} focused={a.id === focusedId} onFocus={() => onFocus(a.id)} />,
      )}
      {shells.map(s =>
        createPortal(
          <ShellPane
            ptyId={shellPtyId(s.id)}
            cwd={s.cwd}
            focused={s.id === focusedId}
            onFocus={() => onFocus(s.id)}
          />,
          slotFor(shellPanelId(s.id)),
          s.id,
        ),
      )}
    </>
  );
}

function AgentSlot({ teamId, agentId, opened, focused, onFocus }: {
  teamId: string; agentId: string; opened: boolean; focused: boolean; onFocus: () => void;
}) {
  const status = useSyncExternalStore(subscribeAgentStatus, () => getStatus(agentId));
  const [mounted, setMounted] = useState(false);
  const needed = opened || status !== "offline";
  if (needed && !mounted) setMounted(true);
  if (!needed && !mounted) return null;
  return createPortal(
    <TerminalPane teamId={teamId} agentId={agentId} focused={focused} onFocus={onFocus} />,
    slotFor(agentPanelId(agentId)),
    agentId,
  );
}
