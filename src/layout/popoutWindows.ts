// #2258: the main window's side of a leader pop-out. Ownership lives here and in the layout store:
// a popped-out leader has no view in this window (host.tsx), this window drops its PTY
// subscription and takes no new one (pty.ts policy), and the dock panel shows a placeholder with
// the way back (Workspace.tsx). The pop-out window itself is the same bundle routed by
// `?pane=lead` (src/lead/LeadWindow.tsx); it is opened here with `WebviewWindow` — no shell
// command, only the create permission (src-tauri/capabilities/default.json). The shell reports a
// destroyed pop-out (`dex:lead-closed`) and closes them all with the main window (popout.rs).
// #2260: the living windows, not layout.json, decide who is popped out. Before any pane mounts the
// main window asks the shell for its `lead-*` windows and brings the records in line (`adoptPopouts`,
// run by initTeams before `ready`); it owns no PTY until then. A reload inside the save debounce
// thus finds its pop-out alive and leaves it the PTY instead of subscribing a second view.
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { onEvent } from "../api/hosts";
import { kept } from "../hot";
import { releasePty, setPtyPolicy } from "../pty";
import { getTeam, getTeamsSnapshot, membersOf } from "../team/store";
import { getPopout, getPopouts, patchPopout, removePopout, setPopout, setPopouts } from "./store";
import { detachAction, isPopoutLabel, mainWindowOwns, parseBounds, popoutLabel, popoutUrl, popoutWindowOptions, reconcilePopouts, type PopoutRec } from "./popout";

/** `lead-*` labels alive at startup (null = not asked yet) and whether the records were brought in line with them. */
const probe = kept(import.meta.hot, "probe", () => ({ live: null as Set<string> | null, reconciled: false }));

setPtyPolicy(id => mainWindowOwns(probe.reconciled, getPopouts(), id));

/** Which pop-out windows live. No shell (browser, e2e without the mock) → none: the records decide. */
async function liveLabels(): Promise<Set<string>> {
  try {
    return new Set((await WebviewWindow.getAll()).map(w => w.label).filter(isPopoutLabel));
  } catch {
    return new Set();
  }
}

/** Startup, teams loaded, before `ready`: a leader whose window lives is popped out whatever the file said. */
export async function adoptPopouts() {
  probe.live ??= await liveLabels();
  setPopouts(reconcilePopouts(getPopouts(), probe.live, getTeamsSnapshot().agents));
  probe.reconciled = true; // the policy opens: panes may subscribe from here, the popped-out ones excepted
}

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

/** Startup, after `ready`: windows remembered open but not alive come back at their last place. */
export async function restorePopouts() {
  if (!probe.reconciled) await adoptPopouts();
  const live = probe.live ?? new Set<string>();
  probe.live = null; // consumed: from here the records own the answer (a closed window reports itself)
  for (const [agentId, rec] of Object.entries(getPopouts())) {
    if (!rec.open) continue;
    void releasePty(agentId); // an online signal may have subscribed before the layout was read
    if (live.has(popoutLabel(agentId))) continue;
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
