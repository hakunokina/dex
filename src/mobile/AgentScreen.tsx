// S5-A: one agent, two views — Sohbet (default, S5-B `src/chat/ChatView.tsx`) and Terminal (S3 attach
// over the phone's own PTY channel). The terminal is mounted only after the user picks it: the chat
// reads the session transcript over RPC and never attaches to / sizes the PTY (pty-size-owner #62),
// so opening an agent whose pane is open on the PC no longer fights it over the PTY size. The one
// exception is an office slap on a sleeping agent (`autoStart`): starting it needs the terminal.
// `agentId` is the qualified id — host included — so ChatView needs no separate `hostId` prop.
import { Suspense, lazy, useEffect, useRef, useState } from "react";
import { getStatus, type AgentStatus } from "../agents";
import { getQuestions, workerQuestion } from "../questions";
import { chatSupported } from "../chat/api";
import { useTeams } from "../team/store";
import { MobileTerminal } from "./MobileTerminal";
import { PanelBoundary } from "../layout/PanelBoundary";
import { ChevronLeft, TerminalSquare, MessageSquareText } from "lucide-react";

const ChatView = lazy(() => import("../chat/ChatView"));

const STATUS_TEXT: Record<AgentStatus, string> = {
  working: "çalışıyor",
  waiting: "bekliyor",
  idle: "boşta",
  offline: "kapalı",
};

/** Follow the visual viewport so the screen shrinks above the on-screen keyboard (the layout viewport doesn't on Android). */
function useVisualViewport(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const vv = window.visualViewport;
    const el = ref.current;
    if (!vv || !el) return;
    const fit = () => {
      el.style.top = `${vv.offsetTop}px`;
      el.style.height = `${vv.height}px`;
      el.style.bottom = "auto";
    };
    fit();
    vv.addEventListener("resize", fit);
    vv.addEventListener("scroll", fit);
    return () => {
      vv.removeEventListener("resize", fit);
      vv.removeEventListener("scroll", fit);
    };
  }, [ref]);
}

/** Our own `history.back()` (UI close) fires a popstate that must not close the next screen. */
let selfBack = 0;
/** UI close is deferred a tick so a StrictMode remount (or a quick re-open) reuses the pushed entry. */
let pendingBack: ReturnType<typeof setTimeout> | null = null;

/** Android back / back gesture closes the screen (→ agent list) instead of leaving the app. */
export function useBackToList(onClose: () => void) {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  useEffect(() => {
    let popped = false;
    if (pendingBack) {
      clearTimeout(pendingBack);
      pendingBack = null;
    } else {
      history.pushState({ dexAgentScreen: true }, "");
    }
    const onPop = () => {
      if (selfBack > 0) {
        selfBack--;
        return;
      }
      popped = true;
      onCloseRef.current();
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      // Closed by the UI (not by back): drop the entry we pushed.
      if (!popped) {
        pendingBack = setTimeout(() => {
          pendingBack = null;
          selfBack++;
          history.back();
        }, 0);
      }
    };
  }, []);
}

export function AgentScreen({
  teamId,
  agentId,
  autoStart,
  initialView,
  onClose,
  onStarted,
}: {
  teamId: string;
  agentId: string;
  autoStart: boolean;
  /** #1719: Echo cevabından "Terminale geç" → terminal görünümünde açılır. */
  initialView?: "term" | "chat";
  onClose: () => void;
  onStarted: () => void;
}) {
  const { agents } = useTeams();
  const agent = agents.find((a) => a.id === agentId);
  // #142: an older dexd without chat → terminal only.
  const chat = chatSupported(agentId);
  const [view, setView] = useState<"term" | "chat">(autoStart || !chat ? "term" : initialView ?? "chat");
  const [, setTick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setTick((n) => n + 1), 500);
    return () => clearInterval(t);
  }, []);
  const status = getStatus(agentId);
  const question = workerQuestion(getQuestions(), agentId);
  const root = useRef<HTMLDivElement>(null);
  useVisualViewport(root);
  useBackToList(onClose);
  return (
    <div className="m-agent-screen" ref={root}>
      <header className="m-agent-top">
        <button className="m-back" onClick={onClose} aria-label="Geri">
          <ChevronLeft />
        </button>
        <div className="m-agent-title">
          <b>{agent?.name ?? agentId}</b>
          <span className="m-agent-state">
            <i className={`m-dot ${status}`} />
            {STATUS_TEXT[status]}
            {question && <i className="mark ask" title="Sorusu var">? soru bekliyor</i>}
          </span>
        </div>
        {view === "chat" ? (
          <button className="m-view-btn" onClick={() => setView("term")} aria-label="Terminal">
            <TerminalSquare />
          </button>
        ) : chat ? (
          <button className="m-view-btn" onClick={() => setView("chat")} aria-label="Sohbet">
            <MessageSquareText />
          </button>
        ) : null}
      </header>
      {view === "term" ? (
        <MobileTerminal teamId={teamId} agentId={agentId} autoStart={autoStart} onStarted={onStarted} />
      ) : (
        <div className="m-chat-slot">
          <PanelBoundary key={agentId}>
            <Suspense fallback={<p className="m-hint">Sohbet yükleniyor…</p>}>
              <ChatView agentId={agentId} onNeedTerminal={() => setView("term")} />
            </Suspense>
          </PanelBoundary>
        </div>
      )}
    </div>
  );
}
