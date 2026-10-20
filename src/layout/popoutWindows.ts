// #2258: the main window's side of a leader pop-out. Ownership lives here and in the layout store:
// a popped-out leader has no view in this window (host.tsx), this window drops its PTY
// subscription and takes no new one (pty.ts policy), and the dock panel shows a placeholder with
// the way back (Workspace.tsx). The pop-out window itself is the same bundle routed by
// `?pane=lead` (src/lead/LeadWindow.tsx); it is opened here with `WebviewWindow` — no shell
// command, only the create permission (src-tauri/capabilities/default.json). The shell reports a
// destroyed pop-out (`dex:lead-closed`) and closes them all with the main window (popout.rs).
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { onEvent } from "../api/hosts";
import { releasePty, setPtyPolicy } from "../pty";
import { getTeam, membersOf } from "../team/store";
import { getPopout, getPopouts, isPoppedOut, patchPopout, removePopout, setPopout } from "./store";
import { detachAction, parseBounds, popoutLabel, popoutUrl, popoutWindowOptions, type PopoutRec } from "./popout";

setPtyPolicy(id => !isPoppedOut(id));

const windowOf = (agentId: string) => WebviewWindow.getByLabel(popoutLabel(agentId)).catch(() => null);

/** "Pencereye ayır": the pane leaves this window; the same pane again brings its window forward. */
export async function detachLeader(teamId: string, agentId: string) {
  if (detachAction(getPopouts(), agentId) === "focus") return focusPopout(agentId);
  const prev = getPopout(agentId);
  const rec: PopoutRec = { teamId, open: true, onTop: prev?.onTop ?? false, bounds: prev?.bounds ?? null };
  setPopout(agentId, rec); // the view unmounts and the policy denies new subscriptions from here on
  void releasePty(agentId);
  await openWindow(agentId, rec);
}

async function openWindow(agentId: string, rec: PopoutRec): Promise<boolean> {
  const team = getTeam(rec.teamId);
  const agent = membersOf(rec.teamId).find(a => a.id === agentId);
  if (!team || !agent) {
    removePopout(agentId);
    return false;
  }
  const created = await new Promise<boolean>(resolve => {
    const w = new WebviewWindow(popoutLabel(agentId), popoutWindowOptions(rec, `${team.name} · ${agent.name}`, popoutUrl(rec.teamId, agentId, rec.onTop)));
    void w.once("tauri://created", () => resolve(true));
    void w.once("tauri://error", () => resolve(false));
  });
  if (created) return true;
  // A reloaded main window finds its pop-outs alive: the label exists, the window stays. Otherwise the pane comes back.
  if (await windowOf(agentId)) return true;
  patchPopout(agentId, { open: false });
  return false;
}

/** "Pencereye git": the pop-out window forward (unminimized). */
export async function focusPopout(agentId: string) {
  const w = await windowOf(agentId);
  if (!w) return;
  await w.unminimize().catch(() => {});
  await w.setFocus().catch(() => {});
}

/** "Geri al": close the window; the pane returns to this window once it is gone. */
export async function reattachLeader(agentId: string) {
  const w = await windowOf(agentId);
  if (w) {
    await w.close().catch(() => {});
    // The shell reports the destroyed window; polling is the fallback (no shell: e2e, a refused close).
    for (let i = 0; i < 20 && (await windowOf(agentId)); i++) await new Promise(r => setTimeout(r, 100));
  }
  patchPopout(agentId, { open: false });
}

/** Startup, after the teams are in: windows remembered open come back at their last place. */
export async function restorePopouts() {
  for (const [agentId, rec] of Object.entries(getPopouts())) {
    if (!rec.open) continue;
    void releasePty(agentId); // an online signal may have subscribed before the layout was read
    await openWindow(agentId, rec);
  }
}

/** The pop-out windows' reports: closed (shell), place and on-top switch (their page). */
export function listenPopouts(): () => void {
  const offs = [
    onEvent<string>("dex:lead-closed", ({ payload: label }) => {
      for (const [agentId, rec] of Object.entries(getPopouts())) {
        if (rec.open && popoutLabel(agentId) === label) patchPopout(agentId, { open: false });
      }
    }),
    onEvent<{ agentId?: unknown; bounds?: unknown }>("dex:lead-bounds", ({ payload }) => {
      const bounds = parseBounds(payload?.bounds);
      if (bounds && typeof payload.agentId === "string") patchPopout(payload.agentId, { bounds });
    }),
    onEvent<{ agentId?: unknown; onTop?: unknown }>("dex:lead-ontop", ({ payload }) => {
      if (typeof payload?.agentId === "string") patchPopout(payload.agentId, { onTop: payload.onTop === true });
    }),
  ];
  return () => offs.forEach(off => off());
}
