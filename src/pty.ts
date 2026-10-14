// S4: a PTY id is qualified (`<host>:<id>`); each call goes to that host with the bare id.
import { callHost, isHostUp, splitId, transportOf, whenHostUp } from "./api/hosts";
import { kept } from "./hot";
import { PtyRelay } from "./ptyRelay";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";

export interface SpawnOptions {
  id: string;
  cmd: string;
  args?: string[];
  cwd?: string;
  /** Team memory root (`shared/` + `agents/<id>/`); omitted → memory off for this session. */
  memoryDir?: string;
  /** DEX-13: provider id (`glm`/`muse`); null/`anthropic` → env untouched. */
  provider?: string | null;
  cols: number;
  rows: number;
  /** The session ended (not replaced by a newer spawn), whether or not a view is attached. */
  onExit: () => void;
}

/** The view showing a session: a TerminalPane's xterm. It can come and go while the process lives. */
export interface PtySink {
  onData: (bytes: Uint8Array) => void;
  /** #206 `lost`: the session went with the core (restart) — what the view shows is not live. */
  onExit: (lost?: boolean) => void;
  /** #62: the PTY's size; the view draws its terminal at it. */
  onSize?: (cols: number, rows: number) => void;
  /** #206: repainted after a reconnect (maybe a new session under the same id): report size, focus. */
  onReattach?: () => void;
}

/** RIS: a new subscription's snapshot paints on a clean terminal (no screen or modes of an older session). */
const RESET = new TextEncoder().encode("c");

// DEX-18: a PTY outlives the pane that shows it (remount, HMR); its channels write to whatever sink is
// attached now. The newest spawn per id is current; a replaced session's late output/exit is dropped.
interface Live {
  relay: PtyRelay;
  generation?: number;
  sink?: PtySink;
  attachment?: Promise<void>;
  remoteDetach?: () => Promise<void>;
}
function handOver(me: Live, sink: PtySink) {
  me.sink = sink;
  me.relay.attach(sink);
  return !me.relay.ended;
}
const live = kept(import.meta.hot, "live", () => new Map<string, Live>());

export async function spawnPty(o: SpawnOptions, sink: PtySink) {
  const me: Live = { relay: new PtyRelay(false) };
  handOver(me, sink);
  live.set(o.id, me);
  const onExit = (lost?: boolean) => {
    if (live.get(o.id) !== me) return;
    live.delete(o.id);
    o.onExit();
    me.relay.onExit(lost);
  };
  try {
    const [host, raw] = splitId(o.id);
    const spawn = {
      id: raw,
      cmd: o.cmd,
      args: o.args ?? [],
      cwd: o.cwd ?? null,
      memoryDir: o.memoryDir ?? null,
      provider: o.provider ?? null,
      cols: o.cols,
      rows: o.rows,
    };
    await transportOf(host).openPty(spawn, {
      onData: b => { if (live.get(o.id) === me) me.relay.onData(b); },
      onExit,
      onSize: (cols, rows) => { if (live.get(o.id) === me) me.relay.onSize(cols, rows); },
      onReattach: () => { if (live.get(o.id) === me) me.relay.onReattach(); },
    });
  } catch (e) {
    if (live.get(o.id) === me) live.delete(o.id);
    throw e;
  }
}

/** Subscribe on the backend's online signal, before React or xterm is ready. */
export async function capturePty(id: string, generation?: number) {
  const current = live.get(id);
  if (current && !current.relay.ended && (generation === undefined || current.generation === generation)) return current.attachment;
  const me = connectPty(id, current?.sink, generation);
  return me.attachment;
}

function connectPty(id: string, sink?: PtySink, generation?: number): Live {
  const [host, raw] = splitId(id);
  const me: Live = { relay: new PtyRelay(!sink), generation };
  if (sink) handOver(me, sink);
  live.set(id, me);
  me.relay.onData(RESET);
  me.attachment = transportOf(host).attachPty(raw, {
    onGeneration: generation => { if (live.get(id) === me) me.generation = generation; },
    onData: bytes => { if (live.get(id) === me) me.relay.onData(bytes); },
    onSize: (cols, rows) => { if (live.get(id) === me) me.relay.onSize(cols, rows); },
    onReattach: () => { if (live.get(id) === me) me.relay.onReattach(); },
    onExit: lost => {
      if (live.get(id) !== me) return;
      me.relay.onExit(lost);
      // An ended session without a view still owns the bytes waiting for that view.
      if (me.relay.hasSink) live.delete(id);
    },
  }).then(detach => {
    me.remoteDetach = detach;
    if (sink && live.get(id) === me && !me.relay.ended) me.relay.onReattach();
  }).catch(error => {
    if (live.get(id) === me) live.delete(id);
    throw error;
  });
  return me;
}

/**
 * Show live output or a finished session's replay in `sink`. False when no session runs under `id`.
 * S4: a host that is not connected yet (starting up, dropped, revoked) is waited for — answering
 * "no session" would make a shell pane spawn a new one over the server's live session.
 */
export async function attachPty(id: string, sink: PtySink): Promise<boolean> {
  const [host] = splitId(id);
  if (!isHostUp(host)) await whenHostUp(host);
  const current = live.get(id);
  const me = current ?? connectPty(id, sink);
  try {
    // The live-only list cannot tell whether a finished agent still has replay.
    await me.attachment;
    // An online signal can replace the subscription while its snapshot is in flight.
    if (live.get(id) && live.get(id) !== me) return attachPty(id, sink);
  } catch (error) {
    if (live.get(id) && live.get(id) !== me) return attachPty(id, sink);
    if (String(error) === "no session") return false;
    throw error;
  }
  if (current) {
    const running = handOver(current, sink);
    if (!running && live.get(id) === current) live.delete(id);
    return running;
  }
  return !me.relay.ended;
}

/** The view goes away; the process keeps running. */
export function detachPty(id: string, sink: PtySink) {
  const s = live.get(id);
  s?.relay.detach(sink);
  if (s?.sink === sink) s.sink = undefined;
}

/**
 * #71: xterm's answer to a terminal query dex-core already answers for an agent PTY (`AgentSink` in
 * crates/dex-core/src/agents.rs: cursor position, DA1, OSC 10/11 colours). Sent as well, it is a
 * second answer the CLI reads as input: ConPTY's start-up `ESC[6n` answered twice types a "C" into
 * Claude's prompt. An agent view drops these; a shell (no core answer) still sends them.
 */
export const coreAnswered = (data: string) => CORE_ANSWERED.test(data);
const CORE_ANSWERED = /^(\x1b\[\d+;\d+R|\x1b\[\?[\d;]*c|\x1b\]1[01];[^\x07\x1b]*(\x07|\x1b\\))$/;

export async function writePty(id: string, data: string) {
  const [host, raw] = splitId(id);
  return transportOf(host).writePty(raw, data);
}
/** #62: this view's size; `claim` (the patron types or focuses here) makes it the PTY's size. */
export async function resizePty(id: string, cols: number, rows: number, claim = false) {
  const [host, raw] = splitId(id);
  return callHost(host, "pty_resize", { id: raw, cols, rows, claim });
}

/**
 * #62: one PTY has one size and several clients show it (desktop, phone). The view reports the
 * size it could show and draws its terminal at the PTY's size (`onSize`); the patron's typing or
 * focus `claim`s the PTY for this view's size. A view the PTY is narrower than draws it in its left
 * part, intact, instead of wrapping a TUI drawn for other columns. No session: the view just fits.
 */
export function ptyFit(id: string, term: Terminal, fit: FitAddon, host: () => HTMLElement | null, running: () => boolean) {
  let want: { cols: number; rows: number } | null = null;
  const at = (cols: number, rows: number) => term.cols === cols && term.rows === rows;
  return {
    /** The view's element or font changed size. */
    refit() {
      const el = host();
      const d = el && el.clientWidth > 0 && el.clientHeight > 0 ? fit.proposeDimensions() : undefined;
      if (!d || !(d.cols > 0 && d.rows > 0)) return;
      want = { cols: d.cols, rows: d.rows };
      if (running()) resizePty(id, d.cols, d.rows).catch(() => {});
      else if (!at(d.cols, d.rows)) term.resize(d.cols, d.rows);
    },
    /** The patron typed (`force` false: only when the PTY is not at this view's size) or focused here. */
    claim(force = false) {
      if (!running() || !want || (!force && at(want.cols, want.rows))) return;
      resizePty(id, want.cols, want.rows, true).catch(() => {});
    },
    onSize(cols: number, rows: number) {
      if (!at(cols, rows)) term.resize(cols, rows);
    },
  };
}
export async function killPty(id: string) {
  const [host, raw] = splitId(id);
  return callHost(host, "pty_kill", { id: raw });
}
