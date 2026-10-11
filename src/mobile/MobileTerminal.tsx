// S5-A: an agent's terminal on a phone. Same contract as the desktop pane (S3/S4): attach to the
// server-owned PTY, paint the snapshot, stream bytes; `agent_start` when it isn't running. On top of
// the desktop flow: the extra-keys bar (Esc Tab Ctrl arrows Enter / and copy/paste — a phone has
// none of them), and keyboard-aware resizing through `visualViewport` (fitting the xterm and
// reporting `pty_resize` makes the TUI redraw). #62: the phone takes the PTY's size only while the
// patron types or focuses the input line here; otherwise it draws the desktop's size as it is.
//
// #58: a phone keyboard never types into xterm. Android IMEs (Gboard, Samsung; Turkish with
// suggestions) send every key as keyCode 229 and xterm diffs its hidden textarea to guess what
// changed (CompositionHelper._handleAnyTextareaChanges): once Backspace reshapes the composed word
// the diff misses and whole fragments go out again ("ö m im ö m im …"). The phone types into its own
// input line and sends it as one paste; xterm's textarea is read-only (no soft keyboard there), a
// hardware keyboard still reaches it through keydown.
import { useEffect, useRef, useState } from "react";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { AGENT_ERROR_EVENT, getAgentError, getStatus, markRunning, noteDraftLine, noteInput, setAgentError } from "../agents";
import { attachPty, coreAnswered, detachPty, ptyFit, writePty, type PtySink } from "../pty";
import { callHost, splitId } from "../api/hosts";
import { termFont, termFontSize, termLineHeight, termTheme } from "../theme";
import { ArrowDown, ArrowDownToLine, ArrowLeft, ArrowRight, ArrowUp, ClipboardPaste, Copy, CornerDownLeft, Delete, SendHorizontal, Slash } from "lucide-react";

export function MobileTerminal({
  agentId,
  autoStart,
  onStarted,
}: {
  teamId: string;
  agentId: string;
  autoStart: boolean;
  onStarted: () => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const runningRef = useRef(false);
  const ctrlRef = useRef(false);
  const launchRef = useRef<() => Promise<void>>(() => Promise.resolve());
  const [offline, setOffline] = useState(true);
  const [alert, setAlert] = useState<string | null>(null);
  const [ctrl, setCtrl] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const sizeRef = useRef<ReturnType<typeof ptyFit> | null>(null);
  const [line, setLine] = useState("");
  const lineRef = useRef("");
  /** The view is scrolled up into the scrollback (shows "En alta"). */
  const [scrolled, setScrolled] = useState(false);

  /** A patron key or paste into the PTY: #18 draft / DEX-19 typing reach the core as on the desktop. */
  const send = (d: string) => {
    if (!runningRef.current) return;
    sizeRef.current?.claim();
    let activity = noteInput(agentId, d);
    if (activity && lineRef.current) activity = noteDraftLine(agentId, true);
    if (activity) void callHost(splitId(agentId)[0], "agent_input", { id: splitId(agentId)[1], draft: activity.draft }).catch(() => {});
    writePty(agentId, d).catch(() => {});
  };
  const sendRef = useRef(send);
  sendRef.current = send;

  useEffect(() => setAlert(getAgentError(agentId)), [agentId]);
  useEffect(() => {
    const onError = (e: Event) => {
      if ((e as CustomEvent<string>).detail === agentId) setAlert(getAgentError(agentId));
    };
    window.addEventListener(AGENT_ERROR_EVENT, onError);
    return () => window.removeEventListener(AGENT_ERROR_EVENT, onError);
  }, [agentId]);

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
        fontSize: termFontSize(),
        lineHeight: termLineHeight(),
        cursorBlink: true,
        theme: termTheme(),
        scrollback: 5000,
      });
      const fit = new XFit();
      term.loadAddon(fit);
      term.open(hostRef.current!);
      term.textarea!.readOnly = true;
      const size = ptyFit(agentId, term, fit, () => hostRef.current, () => runningRef.current);
      sizeRef.current = size;
      size.refit();
      termRef.current = term;

      // #58: xterm 6 scrolls on the wheel only (its viewport has no touch handling), and a synthetic
      // WheelEvent does not help (its wheelDeltaY is 0, which xterm reads first). A one-finger drag
      // scrolls by whole rows: the scrollback in the normal buffer; in the alternate buffer arrow
      // keys, as xterm does for the wheel there.
      const host = hostRef.current!;
      const screen = host.querySelector<HTMLElement>(".xterm-screen")!;
      let lastY: number | null = null;
      let rest = 0;
      const onTouchStart = (e: TouchEvent) => {
        lastY = e.touches.length === 1 ? e.touches[0].clientY : null;
        rest = 0;
      };
      const onTouchMove = (e: TouchEvent) => {
        if (lastY === null || e.touches.length !== 1) return;
        const y = e.touches[0].clientY;
        rest += lastY - y;
        lastY = y;
        e.preventDefault();
        const cell = screen.clientHeight / term!.rows;
        const lines = Math.trunc(rest / cell);
        if (!lines) return;
        rest -= lines * cell;
        if (term!.buffer.active.type === "normal") return term!.scrollLines(lines);
        const arrow = (term!.modes.applicationCursorKeysMode ? "\x1bO" : "\x1b[") + (lines < 0 ? "A" : "B");
        sendRef.current(arrow.repeat(Math.abs(lines)));
      };
      host.addEventListener("touchstart", onTouchStart, { passive: true });
      host.addEventListener("touchmove", onTouchMove, { passive: false });
      const onScroll = term.onScroll(() => setScrolled(term!.buffer.active.viewportY < term!.buffer.active.baseY));

      let fitRaf = 0;
      const refit = () => {
        cancelAnimationFrame(fitRaf);
        fitRaf = requestAnimationFrame(() => size.refit());
      };
      // Element size (fold/unfold, tab chrome) and the on-screen keyboard (visualViewport) both refit.
      const ro = new ResizeObserver(refit);
      ro.observe(hostRef.current!);
      const vv = window.visualViewport;
      vv?.addEventListener("resize", refit);

      const onData = term.onData((d) => {
        if (!runningRef.current || coreAnswered(d)) return;
        // Ctrl is latched on the extra bar: the next letter goes out as ctrl+letter (C-c, C-u …).
        if (ctrlRef.current && /^[a-z]$/i.test(d)) {
          ctrlRef.current = false;
          setCtrl(false);
          sendRef.current(ctrlOf(d));
          return;
        }
        sendRef.current(d);
      });

      const sink: PtySink = {
        onData: (bytes) => term!.write(bytes),
        onSize: size.onSize,
        onExit: () => {
          runningRef.current = false;
          markRunning(agentId, false);
          setOffline(true);
          term!.write("\r\n\x1b[90m— oturum kapandı —\x1b[0m\r\n");
        },
      };

      const launch = async () => {
        term!.reset();
        runningRef.current = true;
        setOffline(false);
        markRunning(agentId, true);
        try {
          await callHost(splitId(agentId)[0], "agent_start", { id: splitId(agentId)[1], options: { cols: term!.cols, rows: term!.rows } });
          if (!(await attachPty(agentId, sink))) throw new Error("PTY başlatıldı ancak bağlanamadı");
          size.claim(true); // #62: started here, sized here
          inputRef.current?.focus();
        } catch (e) {
          runningRef.current = false;
          markRunning(agentId, false);
          setOffline(true);
          term!.write(`\x1b[31mBaşlatılamadı: ${e}\x1b[0m\r\n`);
          setAgentError(agentId, `Başlatılamadı: ${e}`);
        }
      };
      launchRef.current = launch;

      let autoStartLeft = autoStart;
      void (async () => {
        // Wait for the host (dropped/reconnecting) before answering "no session" (S4).
        const attached = await attachPty(agentId, sink).catch((e) => {
          if (!disposed) setAlert(String(e));
          return false;
        });
        if (disposed) return;
        if (attached) {
          runningRef.current = true;
          setOffline(false);
          size.refit();
          return;
        }
        setOffline(true);
        if (autoStartLeft) {
          autoStartLeft = false;
          await launch();
          onStarted();
        }
      })();

      cleanup = () => {
        cancelAnimationFrame(fitRaf);
        ro.disconnect();
        vv?.removeEventListener("resize", refit);
        host.removeEventListener("touchstart", onTouchStart);
        host.removeEventListener("touchmove", onTouchMove);
        onScroll.dispose();
        onData.dispose();
        detachPty(agentId, sink);
        runningRef.current = false;
        sizeRef.current = null;
        term!.dispose();
        if (termRef.current === term) termRef.current = null;
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
    // autoStart/onStarted are consumed once at mount; agentId alone re-runs this effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentId]);

  // A session that ends while mounted (or starts elsewhere) flips the launch button back.
  useEffect(() => {
    const t = setInterval(() => setOffline(getStatus(agentId) === "offline"), 500);
    return () => clearInterval(t);
  }, [agentId]);

  const key = (data: string) => () => send(data);

  /** The input line goes out as one paste (bracketed when the TUI asked for it), Enter after it. */
  function submit() {
    const text = lineRef.current;
    const term = termRef.current;
    if (!runningRef.current || !term) return;
    setLine("");
    lineRef.current = "";
    if (!text) return send("\r");
    term.paste(text); // → onData → send
    // Same gap as a Dex ping (pastePing): Enter in its own write is not part of the paste.
    setTimeout(() => send("\r"), 150);
  }

  function onLine(v: string) {
    const prev = lineRef.current;
    // A latched Ctrl takes the next letter typed into the line (C-c, C-u …), not the line.
    if (ctrlRef.current && v.length === prev.length + 1 && v.startsWith(prev) && /^[a-z]$/i.test(v.slice(-1))) {
      ctrlRef.current = false;
      setCtrl(false);
      send(ctrlOf(v.slice(-1)));
      return;
    }
    setLine(v);
    lineRef.current = v;
    if (!runningRef.current) return;
    const activity = noteDraftLine(agentId, v !== "");
    void callHost(splitId(agentId)[0], "agent_input", { id: splitId(agentId)[1], draft: activity.draft }).catch(() => {});
  }

  async function copy() {
    const sel = termRef.current?.getSelection();
    if (!sel) return;
    try {
      await navigator.clipboard.writeText(sel);
      termRef.current?.clearSelection();
    } catch {
      setAlert("Panoya yazılamadı.");
    }
  }
  async function paste() {
    try {
      const text = await navigator.clipboard.readText();
      if (text) termRef.current?.paste(text);
    } catch {
      setAlert("Pano okunamadı — izin gerekli.");
    }
  }

  return (
    <div className="m-term">
      {alert && (
        <div className="pane-alert" role="alert">
          <span>⚠ {alert}</span>
          <button className="icon-btn" onClick={() => setAgentError(agentId, null)} title="Uyarıyı kapat">×</button>
        </div>
      )}
      <div className="m-term-view">
        <div ref={hostRef} className="m-term-host" />
        {scrolled && (
          <button className="m-term-bottom" onClick={() => termRef.current?.scrollToBottom()} aria-label="En alta">
            <ArrowDownToLine size={18} />
          </button>
        )}
      </div>
      {offline && (
        <button className="m-launch" onClick={() => void launchRef.current()}>
          Başlat
        </button>
      )}
      <form
        className="m-line"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <input
          ref={inputRef}
          onFocus={() => sizeRef.current?.claim(true)}
          value={line}
          onChange={(e) => onLine(e.target.value)}
          placeholder={offline ? "Oturum kapalı" : "Mesaj yaz…"}
          aria-label="Terminale yaz"
          enterKeyHint="send"
          autoComplete="off"
          disabled={offline}
        />
        <button type="submit" aria-label="Gönder" disabled={offline}>
          <SendHorizontal size={18} />
        </button>
      </form>
      {/* Tapping a key keeps the focus (and the soft keyboard) where it is. */}
      <div className="m-keys" role="toolbar" aria-label="Terminal tuşları" onMouseDown={(e) => e.preventDefault()}>
        <button onClick={key("\x1b")} aria-label="Esc">Esc</button>
        <button onClick={key("\t")} aria-label="Tab">Tab</button>
        <button
          className={ctrl ? "on" : ""}
          aria-pressed={ctrl}
          aria-label="Ctrl (bir sonraki harfle birleşir)"
          onClick={() => {
            ctrlRef.current = !ctrlRef.current;
            setCtrl(ctrlRef.current);
          }}
        >
          Ctrl
        </button>
        <button onClick={key("\x1b[A")} aria-label="Yukarı"><ArrowUp size={16} /></button>
        <button onClick={key("\x1b[B")} aria-label="Aşağı"><ArrowDown size={16} /></button>
        <button onClick={key("\x1b[D")} aria-label="Sol"><ArrowLeft size={16} /></button>
        <button onClick={key("\x1b[C")} aria-label="Sağ"><ArrowRight size={16} /></button>
        <button onClick={key("\x7f")} aria-label="Geri sil"><Delete size={16} /></button>
        <button onClick={key("\r")} aria-label="Enter"><CornerDownLeft size={16} /></button>
        <button onClick={key("/")} aria-label="Bölü"><Slash size={15} /></button>
        <button onClick={() => void copy()} aria-label="Kopyala (seçimi)"><Copy size={15} /></button>
        <button onClick={() => void paste()} aria-label="Yapıştır"><ClipboardPaste size={15} /></button>
      </div>
    </div>
  );
}

const ctrlOf = (letter: string) => String.fromCharCode(letter.toUpperCase().charCodeAt(0) & 0x1f);
