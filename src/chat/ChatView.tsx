// S5-B: the agent's session transcript as a chat. Mounts anywhere:
// desktop `<ChatView agentId />`, mobile `<ChatView agentId onNeedTerminal={...} />`.
// Dynamic-import friendly (plain default export, no side effects on import).
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, Brain, ChevronDown, ChevronRight, Loader2, Send, Square } from "lucide-react";
import { chatHistory, chatScreen, chatSend, onChatMessage, onChatQueue, onChatReset, type ChatMessage } from "./api";
import { getStatus, noteInput } from "../agents";
import { writePty } from "../pty";
import { renderMarkdown } from "./md";
import { buildFeed, currentTool, dayLabel, reconcileOutgoing, timeLabel, type FeedItem, type Outgoing } from "./group";
import { parsePrompt, shortLabel } from "./prompt";
import { appendLabel } from "../layout/notePrompts";
import "./chat.css";

interface Props {
  agentId: string;
  /** Waiting card's "Terminale geç" action (mobile switches tabs, desktop focuses the pane). */
  onNeedTerminal?: () => void;
}

const FOLD_LINES = 8;

const Markdown = memo(function Markdown({ text }: { text: string }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  const onClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>("[data-copy]");
    if (!btn) return;
    const code = btn.parentElement?.querySelector("code")?.textContent ?? "";
    const say = (label: string) => {
      btn.textContent = label;
      setTimeout(() => (btn.textContent = "Kopyala"), 1200);
    };
    Promise.resolve()
      .then(() => navigator.clipboard.writeText(code))
      .then(() => say("✓ Kopyalandı"), () => say("Kopyalanamadı"));
  };
  return <div className="chat-md" onClick={onClick} dangerouslySetInnerHTML={{ __html: html }} />;
});

function Stamp({ ts }: { ts?: number | null }) {
  return ts ? <time className="chat-time">{timeLabel(ts)}</time> : null;
}

function ToolCardView({ m }: { m: ChatMessage }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState(false);
  const t = m.tool!;
  const mark = t.ok == null ? <Loader2 className="spin" /> : t.ok ? "✓" : "✗";
  const lines = t.result ? t.result.split("\n") : [];
  const long = lines.length > FOLD_LINES;
  const shown = long && !all ? lines.slice(0, FOLD_LINES).join("\n") : t.result;
  return (
    <div className={`chat-tool ${t.ok == null ? "run" : t.ok ? "ok" : "err"}`}>
      <button className="chat-tool-head" onClick={() => setOpen((v) => !v)} title={t.input}>
        {open ? <ChevronDown /> : <ChevronRight />}
        <b>{t.name}</b>
        <span className="chat-tool-input">{t.input}</span>
        <span className="chat-tool-mark">{mark}</span>
      </button>
      {open && (
        <div className="chat-tool-body">
          {t.input && <pre className="chat-tool-cmd">{t.input}</pre>}
          {shown != null && shown !== "" && <pre className="chat-tool-result">{shown}</pre>}
          {long && (
            <button className="chat-more" onClick={() => setAll((v) => !v)}>
              {all ? "Daralt" : `Tümünü göster (${lines.length} satır)`}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function Activity({ item }: { item: Extract<FeedItem, { kind: "activity" }> }) {
  const [open, setOpen] = useState(false);
  const { tools, thoughts } = item;
  if (tools.length === 1 && thoughts.length === 0) return <ToolCardView m={tools[0]} />;
  const parts = [tools.length ? `${tools.length} araç` : "", thoughts.length ? "düşünce" : ""].filter(Boolean).join(" · ");
  return (
    <div className={`chat-group ${item.running ? "run" : item.failed ? "err" : ""}`}>
      <button className="chat-group-head" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        {open ? <ChevronDown /> : <ChevronRight />}
        {tools.length === 0 ? <Brain /> : null}
        <b>{parts}</b>
        {item.failed > 0 && <span className="chat-group-fail">{item.failed} hata</span>}
        <span className="chat-tool-mark">{item.running ? <Loader2 className="spin" /> : null}</span>
      </button>
      {open && (
        <div className="chat-group-body">
          {thoughts.map((m) => (
            <p key={m.id} className="chat-think">
              {m.text}
            </p>
          ))}
          {tools.map((m) => (
            <ToolCardView key={m.id} m={m} />
          ))}
        </div>
      )}
    </div>
  );
}

function Bubble({ role, text, ts, note, tone, append }: {
  role: string; text: string; ts?: number | null; note?: string; tone?: string; append?: string | null;
}) {
  return (
    <div className={`chat-row ${role === "user" ? "me" : "them"} ${tone ?? ""}`}>
      <div className="chat-bubble">
        <Markdown text={text} />
        {append && <p className="chat-append" title={append}>{appendLabel(append, 160)}</p>}
        <div className="chat-meta">
          <Stamp ts={ts} />
          {note && <span className="chat-note">{note}</span>}
        </div>
      </div>
    </div>
  );
}

const OUT_NOTE: Record<Outgoing["state"], string> = {
  sending: "gönderiliyor…",
  sent: "gönderildi",
  started: "ajan başlatıldı",
  failed: "gönderilemedi",
};

export default function ChatView({ agentId, onNeedTerminal }: Props) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [moreLoading, setMoreLoading] = useState(false);
  const [empty, setEmpty] = useState(false);
  const [draft, setDraft] = useState("");
  const [outgoing, setOutgoing] = useState<Outgoing[]>([]);
  const [queued, setQueued] = useState<string[]>([]);
  const [status, setStatus] = useState(() => getStatus(agentId));
  const [screen, setScreen] = useState<string | null>(null);
  const [reply, setReply] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [unseen, setUnseen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const boxRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const idRef = useRef(agentId);
  const seqRef = useRef(0);
  const outRef = useRef(0);
  idRef.current = agentId;
  seqRef.current = messages.length ? messages[messages.length - 1].seq : 0;

  // Full reload on agent switch.
  useEffect(() => {
    setMessages([]);
    setCursor(null);
    setHasMore(false);
    setLoading(true);
    setEmpty(false);
    setError(null);
    setScreen(null);
    setOutgoing([]);
    setQueued([]);
    setUnseen(false);
    stickRef.current = true;
    chatHistory(agentId, undefined, 50).then(
      (p) => {
        if (idRef.current !== agentId) return;
        setMessages(mergeMessages([], p.messages));
        setCursor(p.cursor ?? null);
        setHasMore(p.hasMore);
        setEmpty(p.messages.length === 0);
        setLoading(false);
      },
      (e) => {
        if (idRef.current !== agentId) return;
        setError(String(e));
        setLoading(false);
      },
    );
  }, [agentId]);

  // Live rows + queue + resets for this agent.
  useEffect(() => {
    const offs = [
      onChatMessage(({ agentId: id, message }) => {
        if (id !== idRef.current) return;
        setEmpty(false);
        setMessages((ms) => mergeMessages(ms, [message]));
        if (!stickRef.current && !message.thinking && message.role !== "user") setUnseen(true);
      }),
      onChatQueue((id, msgs) => {
        if (id !== idRef.current) return;
        setQueued(msgs ?? []);
      }),
      onChatReset((id) => {
        if (id !== idRef.current) return;
        chatHistory(id, undefined, 50).then(
          (p) => {
            if (idRef.current !== id) return;
            setMessages(mergeMessages([], p.messages));
            setCursor(p.cursor ?? null);
            setHasMore(p.hasMore);
            setEmpty(p.messages.length === 0);
          },
          () => {},
        );
      }),
    ];
    return () => void offs.forEach((off) => off());
  }, []);

  // Backend status; a waiting agent's screen is re-read every 2 s (the menu can advance).
  useEffect(() => {
    let tick = 0;
    const readScreen = () => void chatScreen(agentId).then(setScreen, () => setScreen(null));
    const t = setInterval(() => {
      const next = getStatus(agentId);
      tick++;
      setStatus((prev) => {
        if (prev !== next && next === "waiting") readScreen();
        if (next !== "waiting" && prev === "waiting") setScreen(null);
        return next;
      });
      if (next === "waiting" && tick % 4 === 0) readScreen();
      setNow(Date.now());
    }, 500);
    return () => clearInterval(t);
  }, [agentId]);

  // Optimistic bubbles leave as soon as the transcript echoes them.
  useEffect(() => {
    setOutgoing((o) => {
      const next = reconcileOutgoing(o, messages, now);
      return next.length === o.length ? o : next;
    });
  }, [messages, now]);

  const feed = useMemo(() => buildFeed(messages), [messages]);

  // Autoscroll on new rows — never while the user reads above.
  useEffect(() => {
    const box = boxRef.current;
    if (box && stickRef.current) box.scrollTop = box.scrollHeight;
  }, [feed, outgoing, queued, status, screen]);

  const toBottom = () => {
    const box = boxRef.current;
    if (!box) return;
    stickRef.current = true;
    setUnseen(false);
    box.scrollTo({ top: box.scrollHeight, behavior: "smooth" });
  };

  const onScroll = () => {
    const box = boxRef.current;
    if (!box) return;
    stickRef.current = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    if (stickRef.current) setUnseen(false);
    if (box.scrollTop < 120 && hasMore && !moreLoading && cursor != null) {
      setMoreLoading(true);
      const prevHeight = box.scrollHeight;
      chatHistory(agentId, cursor, 50).then(
        (p) => {
          if (idRef.current !== agentId) return;
          setMessages((ms) => mergeMessages(p.messages, ms));
          setCursor(p.cursor ?? null);
          setHasMore(p.hasMore);
          requestAnimationFrame(() => {
            if (boxRef.current) boxRef.current.scrollTop = boxRef.current.scrollHeight - prevHeight;
          });
          setMoreLoading(false);
        },
        () => setMoreLoading(false),
      );
    }
  };

  const deliver = (o: Outgoing) => {
    chatSend(agentId, o.text).then(
      (r) => {
        if (idRef.current !== agentId) return;
        if (r === "queued") {
          // The queue card (chat-queue) is the source of truth; keep it optimistic until it arrives.
          setQueued((q) => (q.includes(o.text) ? q : [...q, o.text]));
          setOutgoing((l) => l.filter((x) => x.id !== o.id));
        } else {
          setOutgoing((l) => l.map((x) => (x.id === o.id ? { ...x, state: r === "started" ? "started" : "sent", at: Date.now() } : x)));
        }
      },
      (e) => {
        setError(String(e));
        setOutgoing((l) => l.map((x) => (x.id === o.id ? { ...x, state: "failed" } : x)));
      },
    );
  };

  const send = () => {
    const text = draft.trim();
    if (!text) return;
    const o: Outgoing = { id: `out-${++outRef.current}`, text, afterSeq: seqRef.current, at: Date.now(), state: "sending" };
    setError(null);
    setDraft("");
    stickRef.current = true;
    setOutgoing((l) => [...l, o]);
    deliver(o);
  };

  const retry = (o: Outgoing) => {
    const fresh = { ...o, state: "sending" as const, at: Date.now(), afterSeq: seqRef.current };
    setOutgoing((l) => l.map((x) => (x.id === o.id ? fresh : x)));
    deliver(fresh);
  };

  /** A tap answer goes to the PTY as keystrokes. */
  const answer = (keys: string) => {
    writePty(agentId, keys).catch((e) => setError(String(e)));
    noteInput(agentId, keys);
  };
  const interrupt = () => answer("\x1b");

  const waiting = status === "waiting";
  const busy = status === "working" || waiting;
  const prompt = useMemo(() => (waiting ? parsePrompt(screen) : null), [waiting, screen]);
  const tool = useMemo(() => (busy ? currentTool(messages) : null), [busy, messages]);

  return (
    <div className="chat">
      <div className="chat-flow" ref={boxRef} onScroll={onScroll}>
        {loading && <p className="chat-hint">Yükleniyor…</p>}
        {error && <p className="chat-hint chat-err">{error}</p>}
        {moreLoading && <p className="chat-hint">Geçmiş yükleniyor…</p>}
        {!loading && hasMore && !moreLoading && <p className="chat-hint">Eski mesajlar için yukarı kaydırın</p>}
        {!loading && empty && !error && (
          <p className="chat-hint">Henüz sohbet yok — aşağıdan yazın, ajan cevaplayınca burada görünür.</p>
        )}
        {feed.map((it) => {
          if (it.kind === "day") return <p key={it.key} className="chat-day">{dayLabel(it.ts, now)}</p>;
          if (it.kind === "activity") return <Activity key={it.key} item={it} />;
          return <Bubble key={it.key} role={it.m.role} text={it.m.text} ts={it.m.ts} append={it.m.append} />;
        })}
        {outgoing.map((o) => (
          <div key={o.id} className={`chat-row me chat-pending ${o.state}`}>
            <div className="chat-bubble">
              <Markdown text={o.text} />
              <div className="chat-meta">
                <time className="chat-time">{timeLabel(o.at)}</time>
                <span className="chat-note">{OUT_NOTE[o.state]}</span>
                {o.state === "failed" && (
                  <button className="chat-retry" onClick={() => retry(o)}>
                    Tekrar dene
                  </button>
                )}
              </div>
            </div>
          </div>
        ))}
        {queued.map((q, i) => (
          <Bubble key={`q${i}`} role="user" text={q} note={`sırada (${i + 1}/${queued.length})`} tone="chat-queued" />
        ))}
        {busy && (
          <div className="chat-live" role="status">
            {waiting ? "⏳ girdi bekliyor" : <><Loader2 className="spin" /> çalışıyor</>}
            {tool && !waiting && (
              <span className="chat-live-tool" title={tool.input}>
                · <b>{tool.name}</b> {shortLabel(tool.input, 40)}
              </span>
            )}
          </div>
        )}
        {waiting && (
          <div className="chat-wait" role="status">
            <b>Ajan cevap bekliyor</b>
            {prompt?.question ? <pre>{prompt.question}</pre> : <p className="chat-hint">Ekran okunuyor…</p>}
            <div className="chat-wait-btns">
              {prompt?.options.map((o, i) => (
                <button key={i} className={i === 0 ? "primary" : ""} title={o.label} onClick={() => answer(o.keys)}>
                  {prompt.kind === "choice" ? `${i + 1}. ` : ""}
                  {shortLabel(o.label)}
                </button>
              ))}
              {(!prompt || prompt.options.length === 0) && <button onClick={() => answer("\r")}>Enter</button>}
              <button className="danger" onClick={interrupt}>
                Esc
              </button>
              {onNeedTerminal && (
                <button className="chat-term" onClick={onNeedTerminal}>
                  Terminale geç
                </button>
              )}
            </div>
            <form
              className="chat-reply"
              onSubmit={(e) => {
                e.preventDefault();
                if (!reply) return;
                answer(`${reply}\r`);
                setReply("");
              }}
            >
              <input value={reply} onChange={(e) => setReply(e.target.value)} placeholder="Başka cevap yaz…" aria-label="Bekleyen isteme cevap" />
              <button type="submit" disabled={!reply} aria-label="Cevabı gönder">
                <Send />
              </button>
            </form>
          </div>
        )}
      </div>
      {unseen && (
        <button className="chat-jump" onClick={toBottom}>
          <ArrowDown /> Yeni mesaj
        </button>
      )}
      <div className="chat-input">
        <textarea
          value={draft}
          rows={1}
          onChange={(e) => setDraft(e.target.value)}
          onInput={(e) => {
            const el = e.currentTarget;
            el.style.height = "auto";
            el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            }
          }}
          placeholder="Ajana yaz…"
          aria-label="Ajana mesaj gönder"
          enterKeyHint="send"
          maxLength={50000}
        />
        {busy && !waiting && (
          <button className="chat-stop" onClick={interrupt} aria-label="Ajanı durdur (Esc)" title="Ajanı durdur (Esc)">
            <Square />
          </button>
        )}
        <button onClick={send} disabled={!draft.trim()} aria-label="Gönder">
          <Send />
        </button>
      </div>
    </div>
  );
}

/** New rows appended, tool patches applied in place (same `id` replaces). */
function mergeMessages(old: ChatMessage[], fresh: ChatMessage[]): ChatMessage[] {
  if (!old.length) return [...fresh];
  const out = [...old];
  for (const m of fresh) {
    const i = out.findIndex((x) => x.id === m.id);
    if (i >= 0) out[i] = m;
    else out.push(m);
  }
  // Live `seq` keeps file order; history pages arrive sorted already.
  out.sort((a, b) => a.seq - b.seq);
  return out;
}
