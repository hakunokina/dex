// DEX-29: the patron's own shell (pwsh / powershell.exe, picked by dex-core). Not an agent: no status
// hooks, office character, memory or CPU job — just a PTY in an xterm. The PTY outlives the pane
// (HMR, panel moves) exactly like an agent's (DEX-18); only closing it (✕ here or its tab) kills it.
import { useEffect, useRef, useState } from "react";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { attachPty, detachPty, ptyFit, spawnPty, writePty, type PtySink } from "./pty";
import { termFont, termLineHeight, termTheme } from "./theme";
import { bindZoom, termSize, zoomKey } from "./termZoom";
import { clipboardKey } from "./termClipboard";
import { isKeyInput } from "./agents";

interface Props {
  /** PTY id (`shellPtyId(...)`). */
  ptyId: string;
  cwd: string | null;
  focused?: boolean;
  onFocus?: () => void;
  /** #2019: another agent-less command dex-core resolves (`claude-login` + its account), not the shell. */
  cmd?: string;
  provider?: string;
  /** The session ended (shown in the terminal too). */
  onExit?: () => void;
}

const runningShells = new Set<string>();
const runningSubs = new Set<() => void>();
const shellStarters = new Map<string, () => void>();
const setShellRunning = (id: string, running: boolean) => {
  if (running) runningShells.add(id);
  else runningShells.delete(id);
  runningSubs.forEach((notify) => notify());
};
export const isShellRunning = (id: string) => runningShells.has(id);
export const subscribeShellRunning = (notify: () => void) => (runningSubs.add(notify), () => runningSubs.delete(notify));
export const restartShell = (id: string) => shellStarters.get(id)?.();

export function ShellPane({ ptyId, cwd, focused = false, onFocus, cmd = "shell", provider, onExit }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const onExitRef = useRef(onExit);
  onExitRef.current = onExit;
  const termRef = useRef<Terminal | null>(null);
  const sinkRef = useRef<PtySink | null>(null);
  const sizeRef = useRef<ReturnType<typeof ptyFit> | null>(null);
  const aliveRef = useRef(false);
  const [error, setError] = useState<string | null>(null);

  async function launch() {
    const term = termRef.current;
    const sink = sinkRef.current;
    if (!term || !sink) return;
    term.reset();
    setError(null);
    try {
      await spawnPty({ id: ptyId, cmd, provider, cwd: cwd ?? undefined, cols: term.cols, rows: term.rows, onExit: () => {} }, sink);
      aliveRef.current = true;
      setShellRunning(ptyId, true);
      sizeRef.current?.claim(true); // #62: started here, sized here
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    shellStarters.set(ptyId, () => void launch());
    return () => {
      shellStarters.delete(ptyId);
    };
  }, [ptyId]);

  useEffect(() => {
    // FIX-5: xterm loads after first paint (its own chunk), never in the initial bundle.
    let disposed = false;
    let term: Terminal | null = null;
    let cleanup: (() => void) | null = null;
    void (async () => {
      const [{ Terminal: XTerm }, { FitAddon: XFit }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]);
      if (disposed || !hostRef.current) return;
      term = new XTerm({
        fontFamily: termFont(),
        fontSize: termSize(),
        lineHeight: termLineHeight(),
        cursorBlink: true,
        theme: termTheme(),
        scrollback: 5000,
      });
      const fit = new XFit();
      term.loadAddon(fit);
      term.open(hostRef.current!);
      const size = ptyFit(ptyId, term, fit, () => hostRef.current, () => aliveRef.current);
      sizeRef.current = size;
      size.refit();
      termRef.current = term;

      let fitRaf = 0;
      const ro = new ResizeObserver(() => {
        cancelAnimationFrame(fitRaf);
        fitRaf = requestAnimationFrame(() => size.refit());
      });
      ro.observe(hostRef.current!);
      const unzoom = bindZoom(term, hostRef.current!, () => size.refit());
      term.attachCustomKeyEventHandler((e) => !zoomKey(e) && !clipboardKey(term!, e));
      // #62: focusing or typing here takes the PTY's size back (see ptyFit).
      const onFocusIn = () => size.claim(true);
      term.textarea?.addEventListener("focus", onFocusIn);
      const onData = term.onData((d) => {
        if (!aliveRef.current) return;
        if (isKeyInput(d)) size.claim();
        writePty(ptyId, d).catch(() => {});
      });
      const sink: PtySink = {
        onData: (bytes) => term!.write(bytes),
        onSize: size.onSize,
        onExit: () => {
          aliveRef.current = false;
          setShellRunning(ptyId, false);
          term!.write("\r\n\x1b[90m— kabuk kapandı —\x1b[0m\r\n");
          onExitRef.current?.();
        },
      };
      sinkRef.current = sink;
      void attachPty(ptyId, sink)
        .then((attached) => {
          if (disposed) {
            detachPty(ptyId, sink);
            return;
          }
          if (!attached) return launch();
          aliveRef.current = true;
          setShellRunning(ptyId, true);
          size.refit();
        })
        .catch((error) => !disposed && setError(String(error)));

      cleanup = () => {
        cancelAnimationFrame(fitRaf);
        ro.disconnect();
        unzoom();
        term!.textarea?.removeEventListener("focus", onFocusIn);
        onData.dispose();
        detachPty(ptyId, sink);
        aliveRef.current = false;
        setShellRunning(ptyId, false);
        sinkRef.current = null;
        sizeRef.current = null;
        termRef.current = null;
        term!.dispose();
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, [ptyId]);

  useEffect(() => {
    if (focused) termRef.current?.focus();
  }, [focused]);

  return (
    <section className={`pane shell ${focused ? "focused" : ""}`} onMouseDown={onFocus}>
      {error && (
        <div className="pane-alert" role="alert">
          <span>⚠ Kabuk başlatılamadı: {error}</span>
        </div>
      )}
      <div className="pane-body">
        <div ref={hostRef} className="term-host" />
      </div>
    </section>
  );
}
