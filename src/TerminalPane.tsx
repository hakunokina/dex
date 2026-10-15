import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { AGENT_ERROR_EVENT, AgentStatus, dismissAgentAlert, getAgentError, getStatus, isKeyInput, markRunning, noteInput, setAgentError } from "./agents";
import { useTeams } from "./team/store";
import { patronWritable } from "./team/rolePolicy";
import { kept } from "./hot";
import { attachPty, coreAnswered, detachPty, killPty, ptyFit, writePty, type PtySink } from "./pty";
import { FOCUS_EVENT, START_EVENT, startAgent, type StartRequest } from "./launcher";
import { callHost, onEvent, splitId, useHosts } from "./api/hosts";
import { chatSupported } from "./chat/api";
import { cancelTurn, retryTurn, slowReply, useSlowReply } from "./firstReply";
import { Lock, MessageSquareText, PencilLine, Play, RotateCcw, TerminalSquare, X } from "lucide-react";
import { termFont, termLineHeight, termTheme } from "./theme";
import { bindZoom, termSize, zoomKey } from "./termZoom";
import { clipboardKey } from "./termClipboard";
import { getDelegations, isActive, useDelegations } from "./delegations";
import { blockedJob, leaderProjectMismatch, projectErrorCode, projectErrorText, projectName, workerProjectTeam } from "./project/model";
import { agentProjectName, getTeamProjects, refreshSessions, selectedProject, useSessions, useTeamProjects } from "./project/store";
import "./project/project.css";
import { useAgentContext } from "./usage/contextStore";
import { contextLevel } from "./usage/contextFormat";
import { ContextMeter } from "./usage/ContextMeter";

// S5-B: the chat mounts only when its tab opens (the terminal stays mounted underneath).
const ChatView = lazy(() => import("./chat/ChatView"));

// #27: leaders started on their team's first opening — once per app run, so a leader the patron
// stopped is not started again by switching tabs.
const autoStarted = kept(import.meta.hot, "autoStarted", () => new Set<string>());

/** Görev #51: core `leader-reset` payload; `phase: null` = finished, cancelled or failed (`error`). */
interface ResetView {
  agentId: string;
  phase: "writing" | "timeout" | "restarting" | null;
  sent: boolean;
  error: string | null;
}

interface Props {
  teamId: string;
  /** DEX-36: one view per agent, pinned for its lifetime (src/layout/host.tsx keeps it mounted). */
  agentId: string;
  focused: boolean;
  onFocus: () => void;
}

export function TerminalPane({ teamId, agentId, focused, onFocus }: Props) {
  const { agents, activeId } = useTeams();
  const agent = agents.find(a => a.id === agentId) ?? null;
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const pendingFocusRef = useRef(false);
  const runningRef = useRef(false);
  const attachingRef = useRef(false);
  const sinkRef = useRef<PtySink | null>(null);
  const sizeRef = useRef<ReturnType<typeof ptyFit> | null>(null);
  // Görev #51: the core is replacing the leader's session — no "closed" line, the new one paints clean.
  const freshRef = useRef(false);
  const [status, setStatus] = useState<AgentStatus>("offline");
  // DEX-13: provider/API failure as a DOM strip — writing into xterm would fight Claude's TUI redraw.
  const [alert, setAlert] = useState<string | null>(null);
  // DEX-18: shown for a few seconds after the pane picks up a still-running session (same reason: DOM, not xterm).
  const [reattached, setReattached] = useState(false);
  // #27: the backend answered whether a session already runs (no PTY there = truly offline).
  const [probed, setProbed] = useState(false);
  // #206: the session went with the core (a restart): a clean terminal, this note and ▶.
  const [lost, setLost] = useState(false);
  // #838: the last start was refused with project_unavailable — offer a clean session.
  const [stale, setStale] = useState(false);
  // #22: job workers open watch-only; the patron talks directly to leaders and the assistant.
  const [writable, setWritable] = useState(false);
  const watch = !!agent && !patronWritable(agent) && !writable;
  const watchRef = useRef(watch);
  watchRef.current = watch;
  useEffect(() => {
    if (!agent) return;
    setAlert(getAgentError(agent.id));
    const onError = (e: Event) => {
      if ((e as CustomEvent<string>).detail === agent.id) setAlert(getAgentError(agent.id));
    };
    window.addEventListener(AGENT_ERROR_EVENT, onError);
    return () => window.removeEventListener(AGENT_ERROR_EVENT, onError);
  }, [agent?.id]);

  useEffect(() => {
    if (!agent) return;
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
      const size = ptyFit(agent.id, term, fit, () => hostRef.current, () => runningRef.current);
      sizeRef.current = size;
      size.refit();
      termRef.current = term;
      if (pendingFocusRef.current) {
        pendingFocusRef.current = false;
        term.focus();
      }

      let fitRaf = 0;
      const ro = new ResizeObserver(() => {
        cancelAnimationFrame(fitRaf);
        fitRaf = requestAnimationFrame(() => size.refit());
      });
      ro.observe(hostRef.current!);
      const unzoom = bindZoom(term, hostRef.current!, () => size.refit());
      // #62: focusing this pane (a click, the window coming back) takes the PTY's size back.
      const onFocusIn = () => size.claim(true);
      term.textarea?.addEventListener("focus", onFocusIn);
      // #22: a watched worker pane swallows the patron's keys, Esc and pastes. xterm's own replies to
      // Claude's terminal queries still go through (#71: but not those the core answers itself);
      // Dex writes to the PTY in the backend, not here.
      // Copying a selection works in both modes (#38).
      term.attachCustomKeyEventHandler((e) => !zoomKey(e) && !clipboardKey(term!, e));
      const onData = term.onData((d) => {
        if (!runningRef.current || coreAnswered(d) || (watchRef.current && isKeyInput(d))) return;
        if (isKeyInput(d)) size.claim();
        const activity = noteInput(agent.id, d);
        if (activity) void callHost(agent.host, "agent_input", { id: splitId(agent.id)[1], draft: activity.draft }).catch(() => {});
        writePty(agent.id, d).catch(() => {});
      });

      const sink: PtySink = {
        onData: (bytes) => term!.write(bytes),
        onSize: size.onSize,
        onExit: (gone) => {
          runningRef.current = false;
          markRunning(agent.id, false);
          // #206: the old session's last frame (its cursor on the status line) is not a session.
          if (gone) {
            term!.reset();
            setLost(true);
          } else if (!freshRef.current) term!.write("\r\n\x1b[90m— oturum kapandı —\x1b[0m\r\n");
        },
        onReattach: () => {
          runningRef.current = true;
          resumed(term!, size);
        },
      };
      sinkRef.current = sink;
      // Unmount only detaches the view; after a full reload the backend snapshot paints first.
      let noteTimer = 0;
      void attachPty(agent.id, sink)
        .then((attached) => {
          if (disposed) {
            detachPty(agent.id, sink);
            return;
          }
          setProbed(true);
          if (!attached) return;
          runningRef.current = true;
          setReattached(true);
          noteTimer = window.setTimeout(() => setReattached(false), 5000);
          size.refit();
        })
        .catch((error) => !disposed && setAlert(String(error)));

      cleanup = () => {
        cancelAnimationFrame(fitRaf);
        ro.disconnect();
        unzoom();
        term!.textarea?.removeEventListener("focus", onFocusIn);
        onData.dispose();
        window.clearTimeout(noteTimer);
        detachPty(agent.id, sink);
        sinkRef.current = null;
        sizeRef.current = null;
        runningRef.current = false;
        term!.dispose();
        if (termRef.current === term) termRef.current = null;
      };
    })();

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, [agent?.id]);

  /**
   * #206: a session (re)attached after a drop or a restart may be a new one at the core's size: this
   * view reports its own, and when the patron is in this pane (or nowhere) it takes the size and the keys.
   */
  function resumed(term: Terminal, size: ReturnType<typeof ptyFit>) {
    setLost(false);
    size.refit();
    const at = document.activeElement;
    if (!focusedRef.current || (at && at !== document.body && !hostRef.current?.contains(at))) return;
    size.claim(true);
    term.focus();
  }

  useEffect(() => {
    const t = setInterval(() => {
      if (!agent) return;
      const next = getStatus(agent.id);
      setStatus(next);
      const sink = sinkRef.current;
      if (next === "offline" || runningRef.current || attachingRef.current || !sink) return;
      attachingRef.current = true;
      if (freshRef.current) {
        freshRef.current = false;
        termRef.current?.reset();
      }
      void attachPty(agent.id, sink)
        .then((attached) => {
          if (!attached) return;
          runningRef.current = true;
          if (termRef.current && sizeRef.current) resumed(termRef.current, sizeRef.current);
        })
        .catch((error) => setAlert(String(error)))
        .finally(() => { attachingRef.current = false; });
    }, 400);
    return () => clearInterval(t);
  }, [agent?.id]);

  useEffect(() => {
    setStatus("offline");
  }, [agent?.id]);

  const focusedRef = useRef(focused);
  focusedRef.current = focused;
  useEffect(() => {
    if (focused) termRef.current?.focus();
  }, [focused]);

  // #27: opening a team starts its offline leader, the same way the button does (resume included).
  // A running one was just reattached above, so no second session; a failed start brings the button
  // and the error strip back (launch).
  const shown = activeId === teamId;
  useEffect(() => {
    if (!agent || agent.kind !== "leader" || !probed || !shown || runningRef.current || autoStarted.has(agent.id)) return;
    autoStarted.add(agent.id);
    void launch(false, true);
  }, [agent?.id, agent?.kind, probed, shown]);

  // Office slap → start this agent's session (no deps: always sees the latest `start`/sessionId).
  useEffect(() => {
    const onStart = (e: Event) => {
      const req = (e as CustomEvent<StartRequest>).detail;
      if (!agent || req.agentId !== agent.id || !termRef.current) return;
      req.handled = true;
      if (!runningRef.current) start();
    };
    window.addEventListener(START_EVENT, onStart);
    return () => window.removeEventListener(START_EVENT, onStart);
  });

  // Rack/palette/office → focus this pane and flash it so the eye finds it (DEX-10b).
  const [flash, setFlash] = useState(0);
  useEffect(() => {
    if (!agent) return;
    const onFocusReq = (e: Event) => {
      if ((e as CustomEvent<string>).detail !== agent.id) return;
      pendingFocusRef.current = !termRef.current;
      termRef.current?.focus();
      setFlash(n => n + 1);
    };
    window.addEventListener(FOCUS_EVENT, onFocusReq);
    return () => window.removeEventListener(FOCUS_EVENT, onFocusReq);
  }, [agent?.id]);

  function start() {
    return launch();
  }

  /** `fresh`: a new session instead of the saved one (#838: its project is gone). */
  async function launch(fresh = false, automatic = false) {
    if (!agent) return;
    const term = termRef.current!;
    term.reset();
    setLost(false);
    setStale(false);
    runningRef.current = true;
    markRunning(agent.id, true);
    // #42: drop the launch overlay now, not at the next 400 ms status poll (it sat over the new session).
    setStatus(getStatus(agent.id));
    try {
      const options = { cols: term.cols, rows: term.rows, ...(fresh ? { fresh } : {}) };
      await startAgent(agent.id, options);
      if (!(await attachPty(agent.id, sinkRef.current!))) throw new Error("PTY başlatıldı ancak bağlanamadı");
      sizeRef.current?.claim(true); // #62: started here, sized here
      // Automatic starts preserve keys in a team tab, menu or form, even if focus moved while awaiting the PTY.
      const at = document.activeElement;
      if (!automatic || !at || at === document.body || hostRef.current?.closest(".pane")?.contains(at)) {
        onFocus();
        term.focus();
      }
    } catch (e) {
      runningRef.current = false;
      markRunning(agent.id, false);
      setStatus(getStatus(agent.id));
      term.write(`\x1b[31mBaşlatılamadı: ${projectErrorText(e)}\x1b[0m\r\n`);
      // #910: the saved session is bound to an archived / missing project — only a clean one starts.
      if (projectErrorCode(e) === "project_unavailable") setStale(true);
      else setAgentError(agent.id, `Başlatılamadı: ${e}`);
    }
  }

  // Görev #32 → #51: the patron's clean start for a leader. The core first asks the leader for a
  // hand-over note in its memory (`reset_ready` when done), then closes the PTY and starts it without
  // --resume. A job still out with a worker is named in the warning, not blocked.
  const [reset, setReset] = useState<ResetView | null>(null);
  const [asking, setAsking] = useState(false);
  useEffect(() => {
    if (!agent || agent.kind !== "leader") return;
    const [host, raw] = splitId(agent.id);
    const apply = (v: ResetView | null) => {
      if (v?.phase === "restarting") freshRef.current = true;
      setReset(v?.phase ? v : null);
      if (v?.error) {
        freshRef.current = false;
        setAgentError(agent.id, `Sıfırlanamadı: ${v.error}`);
      }
    };
    void callHost<ResetView | null>(host, "leader_reset", { id: raw, action: "get" }).then(apply).catch(() => {});
    return onEvent<ResetView>("leader-reset", (e, h) => {
      if (h === host && e.payload.agentId === raw) apply(e.payload);
    });
  }, [agent?.id, agent?.kind]);

  const resetAct = (action: "begin" | "force" | "cancel") => {
    const [host, raw] = splitId(agent!.id);
    return callHost<ResetView | null>(host, "leader_reset", { id: raw, action });
  };

  async function resetSession() {
    if (!agent || reset || asking) return;
    const out = getDelegations().filter((d) => isActive(d) && d.fromAgent === agent.id).length;
    const text = "Lider önce açık konuları ve kararları hafızasına devir notu olarak yazacak, sonra oturumu temiz açılacak; " +
      "konuşma geçmişi yeni oturuma taşınmaz." +
      (out ? `\n\nLider ${out} devretmenin sonucunu bekliyor; sonuçlar yeni oturuma teslim edilir ama bağlamı kaybolur.` : "") +
      "\n\nDevam edilsin mi?";
    if (!confirm(text)) return;
    setAsking(true);
    // No session: the core starts the fresh one at once.
    if (!runningRef.current) freshRef.current = true;
    try {
      const v = await resetAct("begin");
      if (v?.phase) setReset(v);
    } catch (e) {
      setAgentError(agent.id, `Sıfırlanamadı: ${e}`);
    } finally {
      setAsking(false);
    }
  }

  // No note in time: the patron resets anyway or keeps the session.
  useEffect(() => {
    if (!agent || reset?.phase !== "timeout") return;
    const force = confirm("Devir notu gelmedi.\n\nYine de sıfırlansın mı? (İptal: sıfırlama iptal edilir, oturum sürer)");
    void resetAct(force ? "force" : "cancel").catch((e) => setAgentError(agent.id, `Sıfırlanamadı: ${e}`));
  }, [reset?.phase]);

  // FIX-3: first reply late (backend watchdog) — one line + retry / cancel; the PTY stays.
  useSlowReply();
  const slow = agent ? slowReply(agent.id) : null;
  const [acting, setActing] = useState(false);
  const act = (f: (id: string) => Promise<unknown>) => {
    if (!agent) return;
    setActing(true);
    void f(agent.id)
      .catch((e) => setAgentError(agent.id, String(e)))
      .finally(() => setActing(false));
  };

  const offline = status === "offline";
  // #838: the header names the session's project; a session starting/ending re-reads the bindings.
  const delegations = useDelegations();
  const sessions = useSessions();
  const projectTeam = agent?.kind === "worker" ? workerProjectTeam(sessions[agent.id], delegations, agent.id, teamId) : teamId;
  useTeamProjects(projectTeam);
  useEffect(() => {
    if (agent) refreshSessions(agent.id);
  }, [agent?.id, offline]);
  const project = agent ? agentProjectName(agent, delegations) : null;
  const leaderMismatch = agent?.kind === "leader" && leaderProjectMismatch(sessions[agent.id], selectedProject(teamId)?.id ?? null, !offline);
  const leaderUnknown = agent?.kind === "leader" && !offline && !sessions[agent.id] && getTeamProjects(teamId).list.filter(p => p.archivedAt === null).length > 1;
  const blocked = agent?.kind === "worker" ? blockedJob(sessions[agent.id], delegations, agent.id, !offline) : null;
  const projectNameOf = (id: string | null | undefined) => projectName(getTeamProjects(projectTeam).list, id) ?? "başka proje";
  // S5-B: Terminal ↔ Sohbet. The terminal never unmounts (CSS-hide only) so the xterm
  // session survives tab switches; the chat mounts on first open.
  const [picked, setView] = useState<"term" | "chat">("term");
  // #142: an older dexd without chat → terminal only, no Sohbet tab (useHosts: re-render on its hello).
  useHosts();
  const chat = chatSupported(agentId);
  // #212: güncel context doluluğu; eski dexd'de null (gizli). Kırmızı eşikte "Temiz oturum" vurgulanır.
  const ctx = useAgentContext(teamId, agentId);
  const level = ctx && ctx.updatedAt !== null ? contextLevel(ctx.tokens) : "ok";
  const urgent = level === "critical";
  const view = chat ? picked : "term";
  const [chatOpen, setChatOpen] = useState(false);
  const showChat = (v: "term" | "chat") => {
    setView(v);
    if (v === "chat") setChatOpen(true);
  };
  const toggle = (
    <div className="pane-view-toggle" role="tablist" aria-label="Görünüm">
      <button role="tab" aria-selected={view === "term"} className={view === "term" ? "on" : ""} onClick={() => showChat("term")}>
        <TerminalSquare /> Terminal
      </button>
      {chat && (
        <button role="tab" aria-selected={view === "chat"} className={view === "chat" ? "on" : ""} onClick={() => showChat("chat")}>
          <MessageSquareText /> Sohbet
        </button>
      )}
      {project && (
        <span className="project-chip" title="Bu oturumun projesi">{project}</span>
      )}
      {ctx && agent && <ContextMeter ctx={ctx} compact={agent.kind === "worker"} />}
      {agent && !patronWritable(agent) && view === "term" && (
        <button
          className={`pane-write ${writable ? "on" : ""}`}
          aria-pressed={writable}
          onClick={() => {
            setWritable(w => !w);
            termRef.current?.focus();
          }}
          title={writable ? "Salt izlemeye dön: tuşlar ajana gitmez" : "Yazma modunu aç: tuşlar ajana gider"}
        >
          {writable ? <><PencilLine /> Yazma</> : <><Lock /> İzleme</>}
        </button>
      )}
      {agent?.kind === "leader" && view === "term" && (
        <button className={`pane-reset ctx-${level}${urgent ? " urgent" : ""}`} disabled={!!reset || asking} onClick={() => void resetSession()}
          title="Lider devir notunu hafızasına yazar, sonra temiz oturumla yeniden başlar (konuşma geçmişi taşınmaz)">
          <RotateCcw /> Temiz oturum
        </button>
      )}
    </div>
  );
  return (
    <section className={`pane ${focused ? "focused" : ""}`} onMouseDown={onFocus}>
      {flash > 0 && <span key={flash} className="pane-flash" />}
      {reattached && <span className="pane-note" title="Çalışan oturumun ekranı geri yüklendi">↻ yeniden bağlandı</span>}
      {lost && offline && <span className="pane-note" role="status">Çekirdek yeniden başladı — oturum kapandı</span>}
      {alert && (
        <div className="pane-alert" role="alert">
          <span>⚠ {alert}</span>
          <button className="icon-btn" onClick={() => agent && dismissAgentAlert(agent.id)} title="Uyarıyı kapat"><X /></button>
        </div>
      )}
      {stale && offline && (
        <div className="pane-alert pane-slow" role="alert">
          <span>⚠ Kayıtlı oturumun projesi kullanılamıyor (arşivde ya da klasörü yok).</span>
          <button className="pane-act" onClick={() => void launch(true)} title="Kayıtlı oturum sürdürülmez; yeni, bağsız oturum açılır">Temiz oturumla başlat</button>
        </div>
      )}
      {leaderMismatch && <div className="pane-alert pane-slow" role="status">
        <span>Lider {project ?? "takım"} oturumunda — yeni projede çalışmak için lideri yeniden başlat.</span>
        <button className="pane-act" disabled={!!reset || asking} onClick={() => void resetSession()}>Yeni projede temiz oturum</button>
      </div>}
      {leaderUnknown && <div className="pane-alert pane-slow" role="status">
        <span>Liderin proje bağı bilinmiyor.</span>
        <button className="pane-act" disabled={!!reset || asking} onClick={() => void resetSession()}>Temiz oturum aç</button>
      </div>}
      {blocked && agent && (
        <div className="pane-alert pane-slow" role="status">
          <span>⏸ Oturum {projectNameOf(sessions[agent.id]?.projectId)} projesinde; #{blocked.id} ({projectNameOf(blocked.projectId)}) işi bekliyor.</span>
          <button className="pane-act" title="Bu oturum kapanır; bekleyen iş kendi projesinde yeni oturumla başlar"
            onClick={() => {
              if (confirm("Oturum kapatılsın mı?\n\nBekleyen iş kendi projesinde yeni bir oturumla başlar; bu konuşma yeni oturuma taşınmaz.")) void killPty(agent.id);
            }}>
            Oturumu kapat
          </button>
        </div>
      )}
      {slow && (
        <div className="pane-alert pane-slow" role="status">
          <span>⏳ {slow}</span>
          <button className="pane-act" disabled={acting} onClick={() => act(retryTurn)} title="Turu kes ve aynı istemi yeniden gönder">Yeniden dene</button>
          <button className="pane-act" disabled={acting} onClick={() => act(cancelTurn)} title="Turu kes; istem giriş kutusunda kalır">İptal</button>
        </div>
      )}
      {reset && (
        <div className="pane-alert pane-slow pane-handover" role="status">
          {reset.phase === "restarting" ? (
            <span>↻ Oturum temiz açılıyor…</span>
          ) : (
            <>
              <span>✍ {reset.sent ? "Devir notu yazılıyor…" : "Devir notu istemi sırada (lider meşgul ya da yazıyorsun)…"}</span>
              <button className="pane-act" onClick={() => void resetAct("force").catch((e) => setAgentError(agent!.id, `Sıfırlanamadı: ${e}`))}
                title="Devir notunu beklemeden oturumu kapat ve temiz aç">Şimdi sıfırla</button>
              <button className="pane-act" onClick={() => void resetAct("cancel").catch((e) => setAgentError(agent!.id, String(e)))}
                title="Sıfırlamayı iptal et; oturum sürer">İptal</button>
            </>
          )}
        </div>
      )}
      {toggle}
      <div className="pane-body">
        {agent && (
          <>
            <div ref={hostRef} className="term-host" style={view === "chat" ? { display: "none" } : undefined} />
            {view === "chat" && chatOpen && (
              <div className="chat-host">
                <Suspense fallback={<p className="pane-note">Sohbet yükleniyor…</p>}>
                  <ChatView agentId={agentId} />
                </Suspense>
              </div>
            )}
            {offline && view === "term" && (
              <button className="launch" onClick={start}>
                <span><Play /></span> Claude'u başlat
              </button>
            )}
          </>
        )}
      </div>
    </section>
  );
}

/**
 * DEX-36: fit only a terminal that is laid out. A hidden dockview tab or a detached slot measures 0,
 * and fitting that would shrink the live PTY to 2×1 — the TUI would redraw into nothing.
 */
export const STATUS_LABEL: Record<AgentStatus, string> = {
  offline: "kapalı",
  idle: "boşta",
  working: "çalışıyor",
  waiting: "bekliyor",
};
