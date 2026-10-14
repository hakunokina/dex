// S5-A: the phone shell. A client only — every team lives on a `dexd` host over WebSocket; there is
// no local core and no docks. First open with no host → the pairing screen; then: team picker on
// top, one of four views (Kart / Ajanlar / Pano / Notlar), an agent screen over everything, and the
// extra-keys bar under the terminal. Z Flip8: the inner screen is a tall portrait, the cover screen
// a narrow one (`mobile.css` tightens the same layout under ~360px); folding/unfolding is a resize,
// which the PCB canvas and the terminal both follow (ResizeObserver / visualViewport).
import { useEffect, useState } from "react";
import { LOCAL, onEvent, useHosts } from "../api/hosts";
import { SHARE_EVENT, shareDone, shareOpaque, takeShare, type RawShare } from "../api/shareTarget";
import { REPLY_LINK_EVENT, takeReplyLink } from "../api/push";
import { INBOX_REPLY_EVENT, type ReplyId } from "../api/replies";
import { initTeams, selectTeam, useTeams } from "../team/store";
import { Office } from "../office/Office";
import { START_EVENT, type StartRequest } from "../launcher";
import { AddServer } from "./AddServer";
import { AgentsView } from "./AgentsView";
import { AgentScreen } from "./AgentScreen";
import { ShareScreen } from "./ShareScreen";
import { hasShare, isShareFastMode, shareTask, type ShareTask } from "./shareLogic";
import { tryFastShare } from "./shareFast";
import { RepliesList, ReplyScreen } from "./Replies";
import { parseReplyLink } from "./replyLogic";
import { askNotifyOnce, startPush } from "./push";
import { BoardView } from "../board/Board";
import { NotesEditor } from "../layout/Notes";
import { ChevronLeft, CircuitBoard, Inbox, KanbanSquare, NotebookPen, Server, Users } from "lucide-react";
import "../App.css";
import "./mobile.css";

type Tab = "card" | "agents" | "board" | "notes";
const TABS: { id: Tab; label: string; icon: typeof CircuitBoard }[] = [
  { id: "card", label: "Kart", icon: CircuitBoard },
  { id: "agents", label: "Ajanlar", icon: Users },
  { id: "board", label: "Pano", icon: KanbanSquare },
  { id: "notes", label: "Notlar", icon: NotebookPen },
];

export default function MobileApp() {
  const { ready, teams, activeId } = useTeams();
  const hosts = useHosts();
  const [tab, setTab] = useState<Tab>("card");
  const [serverSheet, setServerSheet] = useState(false);
  /** The agent screen shown over everything (its id), if any. */
  const [agent, setAgent] = useState<string | null>(null);
  /** #1719: Echo'nun cevabından "Terminale geç" → ajan ekranı terminal görünümünde. */
  const [agentView, setAgentView] = useState<"term" | "chat" | undefined>(undefined);
  /** #1719 Echo cevapları: liste ya da tek cevap (host null: bildirimden, aranır). Ekranlar tek tek
   *  açılır (her biri kendi geri-tuşu kaydını tutar). */
  const [replies, setReplies] = useState<
    { view: "list" } | { view: "reply"; host: string | null; id: ReplyId; fromList: boolean } | null
  >(null);
  const [unreadReply, setUnreadReply] = useState(false);
  // Office slap → nobody may have a pane mounted on the phone yet: open the screen and start there.
  const [pendingStart, setPendingStart] = useState<string | null>(null);
  useEffect(() => {
    const onStart = (e: Event) => {
      const req = (e as CustomEvent<StartRequest>).detail;
      req.handled = true;
      setAgent(req.agentId);
      setPendingStart(req.agentId);
    };
    window.addEventListener(START_EVENT, onStart);
    return () => window.removeEventListener(START_EVENT, onStart);
  }, []);

  useEffect(() => {
    void initTeams();
  }, []);

  // #1719: FCM jetonu → eşlenmiş hostlara `push_register`; bildirime dokunma → o cevabın ekranı.
  // #1759: push kaydı paylaşım çekişmesinden sonra başlar (aşağıdaki etki) — hızlı gönderimde arka
  // plan işi (push_register) hiç başlamaz; ikinci WebView olmadığı için çift kayıt da olmaz.
  useEffect(() => {
    const pull = () =>
      void takeReplyLink().then(
        (uri) => {
          const id = parseReplyLink(uri);
          if (id === null) return;
          setAgent(null);
          setAgentView(undefined);
          setReplies({ view: "reply", host: null, id, fromList: false });
        },
        () => {}, // köprü yok (eski APK / masaüstü önizleme)
      );
    pull();
    window.addEventListener(REPLY_LINK_EVENT, pull);
    return () => window.removeEventListener(REPLY_LINK_EVENT, pull);
  }, []);
  useEffect(() => onEvent(INBOX_REPLY_EVENT, () => setUnreadReply(true)), []);

  // Android Paylaş → Dex: uygulama kapalıyken açılışta bekleyen paylaşım alınır; açıkken Kotlin
  // `dex-share` olayını atar. `seq` her yeni paylaşımda ekranı sıfırdan kurar.
  // #1717: önce hızlı gönderim (`fast`); hedef belirsizse ya da hata olursa ShareScreen açılır.
  const [share, setShare] = useState<{
    raw: RawShare;
    seq: number;
    task: ShareTask;
    clientId: string;
    fast: boolean;
    error: string | null;
  } | null>(null);
  /** Açılıştaki ilk `takeShare` yanıtı geldi mi (yok / köprü yok dahil). */
  const [shareChecked, setShareChecked] = useState(false);
  useEffect(() => {
    let seq = 0;
    const pull = () =>
      void takeShare().then(
        (raw) => {
          setShareChecked(true);
          if (!hasShare(raw)) return;
          const mine = ++seq;
          const task = shareTask(raw.task);
          const clientId = crypto.randomUUID();
          setShare({ raw, seq: mine, task, clientId, fast: true, error: null });
          void tryFastShare(raw, task, clientId).then((r) => {
            if (mine !== seq) return; // arada yeni paylaşım geldi
            if (r.sent) {
              setShare(null);
              // İlk paylaşımda bildirim izni (Echo cevap verince haber vermek için), sonra arka plana.
              void askNotifyOnce().finally(() => shareDone(r.toast).catch(() => {}));
            } else {
              // #1762 geri düşüş: gönderim ekranı opak açılır (pencere + WebView opaklığa döner).
              void shareOpaque().catch(() => {});
              setShare((s) => (s?.seq === mine ? { ...s, fast: false, error: r.error } : s));
            }
          });
        },
        // paylaş köprüsü yok (eski APK / masaüstü önizleme): sessizce geç, push normal başlar
        () => setShareChecked(true),
      );
    pull();
    window.addEventListener(SHARE_EVENT, pull);
    return () => window.removeEventListener(SHARE_EVENT, pull);
  }, []);

  // #1759: push kaydı çekişmeden sonra; hızlı gönderimde (saydam kök) hiç başlamaz.
  const shareFast = isShareFastMode(share);
  useEffect(() => {
    if (!shareChecked || shareFast) return;
    return startPush();
  }, [shareChecked, shareFast]);
  // #1759: hızlı gönderimde kök saydam — kullanıcı paylaştığı uygulamayı görmeye devam eder.
  useEffect(() => {
    document.documentElement.classList.toggle("share-fast", shareFast);
    return () => document.documentElement.classList.remove("share-fast");
  }, [shareFast]);

  const remoteTeams = teams.filter((t) => t.host !== LOCAL);
  const team = remoteTeams.find((t) => t.id === activeId) ?? remoteTeams[0] ?? null;
  useEffect(() => {
    if (team && team.id !== activeId) selectTeam(team.id);
  }, [team, activeId]);

  const shareScreen = share && !share.fast && (
    <ShareScreen
      key={share.seq}
      share={share.raw}
      ready={ready}
      defaultHost={team?.host ?? null}
      task={share.task}
      clientId={share.clientId}
      error={share.error}
      onClose={() => setShare(null)}
    />
  );

  // No paired server yet → the first-open pairing screen (also reached from the top bar).
  // #1759: hızlı gönderim sürerken Dex hiç çizilmez — boş saydam kök; toast + kapanış Kotlin'de.
  // (`fast` bitince ya paylaşım silinir ya da gönderim ekranı (`fast: false`) açılır. Bütün kancalar
  // yukarıda koşulsuz çalıştı.)
  if (shareFast) return null;
  if (!serverSheet && hosts.length === 0) {
    return (
      <>
        <AddServer onDone={() => {}} first />
        {shareScreen}
      </>
    );
  }

  return (
    <div className="m-app">
      <header className="m-top">
        <span className="m-brand"><img src="/dex-icon.svg" alt="" />DEX</span>
        <TeamPicker
          teams={remoteTeams}
          activeId={team?.id ?? ""}
          onPick={(id) => {
            selectTeam(id);
            setAgent(null);
          }}
        />
        <button
          className="m-host-btn m-replies-btn"
          onClick={() => {
            setUnreadReply(false);
            setReplies({ view: "list" });
          }}
          aria-label="Echo cevapları"
        >
          <Inbox />
          {unreadReply && <span className="m-host-dot connected" />}
        </button>
        <button
          className="m-host-btn"
          onClick={() => setServerSheet(true)}
          aria-label="Sunucular"
        >
          <Server />
          <span className={`m-host-dot ${hosts.every((h) => h.status === "connected") ? "connected" : hosts[0]?.status ?? "unauthorized"}`} />
          <span className="m-host-label">Sunucular</span>
        </button>
      </header>

      <main className="m-body">
        {serverSheet ? (
          <>
            <button className="m-back" onClick={() => setServerSheet(false)}>
              <ChevronLeft /> Geri
            </button>
            <AddServer onDone={() => setServerSheet(false)} />
          </>
        ) : !ready ? (
          <p className="m-hint">Yükleniyor…</p>
        ) : !team ? (
          <p className="m-hint">
            Bu sunucuda takım yok. Takımları masaüstü Dex'ten ya da sunucudan oluşturun; telefon
            yalnız izler ve yönetir.
          </p>
        ) : tab === "card" ? (
          <div className="m-card" inert={agent !== null}>
            <Office
              teamId={team.id}
              focusedId={agent}
              active={agent === null}
              onSelect={(id) => {
                // Chip tap: awake → its screen; asleep → the slap starts it (pendingStart) and opens it.
                setAgent(id);
              }}
            />
          </div>
        ) : tab === "agents" ? (
          <AgentsView teamId={team.id} onOpen={setAgent} />
        ) : tab === "board" ? (
          <div className="m-board">
            <BoardView />
          </div>
        ) : (
          <NotesEditor scope={team.id} placeholder="Takım notları…" />
        )}
      </main>

      <nav className="m-tabs" role="tablist">
        {TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            role="tab"
            aria-label={label}
            aria-selected={tab === id}
            className={`m-tab${tab === id ? " on" : ""}`}
            onClick={() => {
              setTab(id);
              setServerSheet(false);
            }}
          >
            <Icon />
            <span>{label}</span>
          </button>
        ))}
      </nav>

      {team && agent && (
        <AgentScreen
          teamId={team.id}
          agentId={agent}
          autoStart={agent === pendingStart}
          initialView={agentView}
          onClose={() => {
            setAgent(null);
            setAgentView(undefined);
          }}
          onStarted={() => setPendingStart(null)}
        />
      )}
      {replies?.view === "list" && (
        <RepliesList
          onOpen={(host, id) => setReplies({ view: "reply", host, id, fromList: true })}
          onClose={() => setReplies(null)}
        />
      )}
      {replies?.view === "reply" && (
        <ReplyScreen
          key={`${replies.host}:${replies.id}`}
          host={replies.host}
          id={replies.id}
          onClose={() => setReplies(replies.fromList ? { view: "list" } : null)}
          onTerminal={(teamId, agentId) => {
            setReplies(null);
            selectTeam(teamId);
            setTab("card");
            setAgentView("term");
            setAgent(agentId);
          }}
        />
      )}
      {shareScreen}
    </div>
  );
}

function TeamPicker({ teams, activeId, onPick }: { teams: { id: string; name: string }[]; activeId: string; onPick: (id: string) => void }) {
  const [open, setOpen] = useState(false);
  const active = teams.find((t) => t.id === activeId);
  if (teams.length === 0) return <span className="m-team-name">—</span>;
  return (
    <div className="m-teams">
      <button className="m-team-btn" onClick={() => setOpen(!open)} aria-expanded={open}>
        {active?.name ?? "Takım seç"} <ChevronLeft className={`m-chev${open ? " up" : ""}`} size={14} />
      </button>
      {open && (
        <ul className="m-team-list">
          {teams.map((t) => (
            <li key={t.id}>
              <button
                className={t.id === activeId ? "on" : ""}
                onClick={() => {
                  onPick(t.id);
                  setOpen(false);
                }}
              >
                {t.name}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
