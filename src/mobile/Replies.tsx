// #1719 Echo cevapları: liste (eşlenmiş hostlardaki asistanlı takımların son cevapları, canlı
// `inbox-reply` ile tazelenir) ve tek cevap ekranı (bildirim ya da listeden). Markdown masaüstü
// sohbetin `chat/md.ts`'iyle, ham HTML'siz; bağlantılar yalnız http/https ve sistem tarayıcısında.
// "Terminale geç" Echo'nun AgentScreen'ini terminal görünümünde açar. İçerik loglanmaz.
import { useEffect, useMemo, useState } from "react";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { hostSupports, initHosts, isHostUp, onEvent, onHostUp, qid, rawId, remoteHosts, whenHostUp } from "../api/hosts";
import { openExternal } from "../api/push";
import { getReply, INBOX_REPLY_EVENT, listReplies, type Reply, type ReplyId, type ReplySummary } from "../api/replies";
import { listAgents, listTeams } from "../team/api";
import { assistantTeams } from "./shareLogic";
import { isWebUrl, mergeReplies, replyTaskLabel, replyTime, replyView } from "./replyLogic";
import { useBackToList } from "./AgentScreen";
import { Bell, ChevronLeft, ChevronRight, Copy, TerminalSquare } from "lucide-react";
import "../chat/chat.css";

type Row = ReplySummary & { host: string };

const CONNECT_MS = 8000;

function within<T>(p: Promise<T>, ms: number, why: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(why), ms);
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}

async function loadRows(): Promise<Row[]> {
  await initHosts();
  const hosts = remoteHosts().map((h) => h.id).filter((h) => isHostUp(h) && hostSupports(h, "inbox_replies"));
  const lists = await Promise.all(
    hosts.map(async (host) => {
      const [teams, agents] = await Promise.all([listTeams(host), listAgents(host)]);
      const per = await Promise.all(
        assistantTeams(teams, agents).map((t) => listReplies(host, rawId(t.id)).then((rs) => rs.map((r) => ({ ...r, host })))),
      );
      return per.flat();
    }),
  );
  return mergeReplies(lists);
}

/** Bildirimden gelen cevabın hostu bilinmez (FCM verisi yalnız id taşır): bağlanan hostlarda aranır. */
async function findReply(id: ReplyId): Promise<{ host: string; reply: Reply }> {
  await initHosts();
  let why = "Cevap bulunamadı";
  for (const { id: host } of remoteHosts()) {
    if (!(await within(whenHostUp(host), CONNECT_MS, "").then(() => true, () => false))) {
      why = "Sunucuya bağlanılamadı";
      continue;
    }
    if (!hostSupports(host, "inbox_reply_get")) continue;
    try {
      return { host, reply: await getReply(host, id) };
    } catch (e) {
      why = String(e);
    }
  }
  throw why;
}

function NotifyAsk() {
  const [granted, setGranted] = useState(true);
  useEffect(() => {
    void isPermissionGranted().then(setGranted, () => {});
  }, []);
  if (granted) return null;
  return (
    <button
      className="m-reply-notify"
      onClick={() => void requestPermission().then((p) => setGranted(p === "granted"), () => {})}
    >
      <Bell size={16} /> Cevap gelince bildirim almak için izin ver
    </button>
  );
}

export function RepliesList({ onOpen, onClose }: { onOpen: (host: string, id: ReplyId) => void; onClose: () => void }) {
  useBackToList(onClose);
  const [rows, setRows] = useState<Row[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let live = true;
    loadRows().then(
      (r) => live && (setRows(r), setErr(null)),
      (e) => live && (setRows((old) => old ?? []), setErr(String(e))),
    );
    return () => void (live = false);
  }, [tick]);
  useEffect(() => {
    const again = () => setTick((n) => n + 1);
    const offUp = onHostUp((h) => h !== "local" && again());
    const offEv = onEvent(INBOX_REPLY_EVENT, again);
    return () => (offUp(), offEv());
  }, []);
  return (
    <div className="m-agent-screen m-reply" role="dialog" aria-label="Echo cevapları">
      <header className="m-agent-top">
        <button className="m-back" onClick={onClose} aria-label="Geri">
          <ChevronLeft />
        </button>
        <div className="m-agent-title">
          <b>Echo cevapları</b>
          <span>Paylaştıklarına gelen son cevaplar</span>
        </div>
      </header>
      <div className="m-reply-scroll">
        <NotifyAsk />
        {err && <p className="m-err" role="alert">{err}</p>}
        {rows === null ? (
          <p className="m-hint">Yükleniyor…</p>
        ) : rows.length === 0 ? (
          <p className="m-hint">Henüz cevap yok. Bir şeyi Dex'e paylaşınca Echo'nun cevabı burada görünür.</p>
        ) : (
          <ul className="m-reply-list">
            {rows.map((r) => (
              <li key={`${r.host}:${r.id}`}>
                <button className="m-reply-row" onClick={() => onOpen(r.host, r.id)}>
                  <span className="m-reply-row-main">
                    <span className="m-reply-row-top">
                      {replyTaskLabel(r.task) && <i className="m-reply-chip">{replyTaskLabel(r.task)}</i>}
                      <b>{r.agentName}</b>
                      <time>{replyTime(r.createdAt)}</time>
                    </span>
                    <span className="m-reply-row-preview">{r.preview || "—"}</span>
                  </span>
                  <ChevronRight size={16} className="m-chev-agent" />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

export function ReplyScreen({
  host: knownHost,
  id,
  onClose,
  onTerminal,
}: {
  /** null: bildirimden geldi, host aranır. */
  host: string | null;
  id: ReplyId;
  onClose: () => void;
  /** Echo'nun ekranı (nitelikli takım ve ajan kimliği). */
  onTerminal: (teamId: string, agentId: string) => void;
}) {
  useBackToList(onClose);
  const [state, setState] = useState<{ host: string; reply: Reply } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [copied, setCopied] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);

  useEffect(() => {
    let live = true;
    setErr(null);
    const load = knownHost
      ? within(whenHostUp(knownHost), CONNECT_MS, "Sunucuya bağlanılamadı").then(() => getReply(knownHost, id)).then((reply) => ({ host: knownHost, reply }))
      : findReply(id);
    load.then(
      (s) => live && setState(s),
      (e) => live && setErr(String(e)),
    );
    return () => void (live = false);
  }, [knownHost, id, attempt]);

  const reply = state?.reply ?? null;
  const view = useMemo(() => (reply ? replyView(reply.markdown) : null), [reply]);
  const task = replyTaskLabel(reply?.task);

  const copy = (text: string, label = "✓ Kopyalandı") =>
    Promise.resolve()
      .then(() => navigator.clipboard.writeText(text))
      .then(() => setCopied(label), () => setCopied("Kopyalanamadı"))
      .then(() => setTimeout(() => setCopied(null), 1400));

  // Bağlantı → sistem tarayıcısı (yalnız http/https); kod bloğu "Kopyala" düğmesi.
  const onBodyClick = (e: React.MouseEvent<HTMLElement>) => {
    const el = e.target as HTMLElement;
    const a = el.closest<HTMLAnchorElement>("a[href]");
    if (a) {
      e.preventDefault();
      const href = a.getAttribute("href");
      if (isWebUrl(href)) void openExternal(href!).catch(() => window.open(href!, "_blank", "noopener"));
      return;
    }
    const btn = el.closest<HTMLElement>("[data-copy]");
    if (btn) void copy(btn.parentElement?.querySelector("code")?.textContent ?? "", "✓ Kod kopyalandı");
  };

  async function toTerminal() {
    if (!state) return;
    setOpening(true);
    try {
      const agentId = qid(state.host, state.reply.agentId);
      const agent = (await listAgents(state.host)).find((a) => a.id === agentId);
      if (!agent) throw `${state.reply.agentName} bu sunucuda bulunamadı`;
      onTerminal(agent.teamId, agent.id);
    } catch (e) {
      setErr(String(e));
      setOpening(false);
    }
  }

  return (
    <div className="m-agent-screen m-reply" role="dialog" aria-label="Echo cevabı">
      <header className="m-agent-top">
        <button className="m-back" onClick={onClose} aria-label="Geri">
          <ChevronLeft />
        </button>
        <div className="m-agent-title">
          <b>{reply ? `${reply.agentName}${task ? ` · ${task}` : ""}` : "Echo cevabı"}</b>
          <span>{reply ? replyTime(reply.createdAt) : "—"}</span>
        </div>
      </header>
      <div className="m-reply-scroll" onClick={onBodyClick}>
        {!reply ? (
          err ? (
            <>
              <p className="m-err" role="alert">{err}</p>
              <button className="m-share-cancel" onClick={() => setAttempt((n) => n + 1)}>Yeniden dene</button>
            </>
          ) : (
            <p className="m-hint">Yükleniyor…</p>
          )
        ) : (
          <>
            {reply.preview && <p className="m-reply-preview">{reply.preview}</p>}
            {view?.lead && <div className="m-reply-lead chat-md" dangerouslySetInnerHTML={{ __html: view.lead }} />}
            {view?.body && <article className="m-reply-md chat-md" dangerouslySetInnerHTML={{ __html: view.body }} />}
            {err && <p className="m-err" role="alert">{err}</p>}
          </>
        )}
      </div>
      {reply && (
        <footer className="m-reply-actions">
          <button className="m-primary" onClick={() => void toTerminal()} disabled={opening}>
            <TerminalSquare size={18} /> Terminale geç
          </button>
          <button className="m-reply-copy" onClick={() => void copy(reply.markdown)} aria-label="Cevabı kopyala">
            <Copy size={18} /> {copied ?? "Kopyala"}
          </button>
        </footer>
      )}
    </div>
  );
}
