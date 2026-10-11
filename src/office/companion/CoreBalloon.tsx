// #243 → #252: Core's decision balloon, shared by the leader pane's strip and the office's big Core.
// It carries Dex's own decision cards (Proposals/Questions `Card`, same proposal_decide /
// question_answer paths — no new decision route). One card at a time, `n/m`; Esc defers.
import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type Ref } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useQuestions } from "../../questions";
import { Card as ProposalCard, useProposals } from "../../team/Proposals";
import { Card as QuestionCard } from "../../team/Questions";
import { balloonKey } from "./coreState";
import { isDeferred, setDeferred, useDeferredMap } from "./coreShown";
import "./core.css";

export type DecisionItem = { kind: "proposal"; key: string; at: number; p: ReturnType<typeof useProposals>[number] } | {
  kind: "question"; key: string; at: number; q: ReturnType<typeof useQuestions>[number];
};

/** Patron decisions of this leader, oldest first (the balloon's queue), and whether Esc shut it. */
export function useDecisions(agentId: string): { items: DecisionItem[]; deferred: boolean } {
  const questions = useQuestions();
  const proposals = useProposals();
  const items = useMemo<DecisionItem[]>(() => {
    const list: DecisionItem[] = [
      ...proposals.filter((p) => p.leader === agentId && p.status === "pending").map((p) => ({ kind: "proposal" as const, key: `p${p.key}`, at: p.createdAt, p })),
      ...questions.filter((q) => q.leader === agentId && q.status === "escalated").map((q) => ({ kind: "question" as const, key: `q${q.key}`, at: q.createdAt, q })),
    ];
    return list.sort((a, b) => a.at - b.at);
  }, [proposals, questions, agentId]);
  const map = useDeferredMap();
  const keys = items.map((i) => i.key);
  const shut = isDeferred(agentId, keys);
  // A card that was not on hand at Esc opens the balloon again (and keeps it open).
  const stale = map.has(agentId) && !shut;
  useEffect(() => {
    if (stale) setDeferred(agentId, null);
  }, [stale, agentId]);
  return { items, deferred: shut };
}

export const deferDecisions = (agentId: string, items: DecisionItem[]) => setDeferred(agentId, items.map((i) => i.key));

interface Props {
  agentId: string;
  items: DecisionItem[];
  /** A card's decision/answer failed (Core shows `!`). */
  onFail: (text: string) => void;
  /** After Esc: where the focus goes back to. */
  onDefer: () => void;
  className?: string;
  style?: CSSProperties;
  ref?: Ref<HTMLDivElement>;
}

export function CoreBalloon({ agentId, items, onFail, onDefer, className, style, ref }: Props) {
  const [index, setIndex] = useState(0);
  const at = Math.min(index, Math.max(0, items.length - 1));
  const item = items[at];
  const balloonRef = useRef<HTMLDivElement>(null);
  const setRefs = (el: HTMLDivElement | null) => {
    balloonRef.current = el;
    if (typeof ref === "function") ref(el);
    else if (ref) ref.current = el;
  };

  // A card opened while nobody types anywhere (or the patron is in the balloon) takes the focus —
  // never from the terminal or another field.
  useEffect(() => {
    if (!item) return;
    const a = document.activeElement;
    if (a && a !== document.body && !balloonRef.current?.contains(a)) return;
    balloonRef.current?.querySelector<HTMLElement>(".core-card button:not(:disabled), .core-card input")?.focus();
  }, [item?.key]);

  const defer = () => {
    deferDecisions(agentId, items);
    onDefer();
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!item) return;
    const t = e.target as HTMLElement;
    const action = balloonKey(
      { key: e.key, ctrlKey: e.ctrlKey, altKey: e.altKey, metaKey: e.metaKey, inText: t.tagName === "INPUT" || t.tagName === "TEXTAREA", onButton: t.tagName === "BUTTON" },
      item.kind === "proposal" ? { kind: "proposal", options: 0 } : { kind: "question", options: Math.min(9, item.q.options.length) },
    );
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    if (action === "defer") return defer();
    balloonRef.current?.querySelector<HTMLButtonElement>(`.core-card [data-key="${action.click}"]:not(:disabled)`)?.click();
  };

  if (!item) return null;
  return (
    <div ref={setRefs} className={`core-balloon${className ? ` ${className}` : ""}`} style={style} role="group"
      aria-label="Core: karar bekleyenler" onKeyDown={onKeyDown}>
      <div className="core-balloon-body">
        {items.length > 1 && (
          <div className="core-pager">
            <button className="core-btn" disabled={at === 0} onClick={() => setIndex(at - 1)} aria-label="Önceki karar"><ChevronLeft /></button>
            <span>{at + 1}/{items.length}</span>
            <button className="core-btn" disabled={at === items.length - 1} onClick={() => setIndex(at + 1)} aria-label="Sonraki karar"><ChevronRight /></button>
          </div>
        )}
        <div className="core-card" key={item.key}>
          {item.kind === "proposal" ? (
            <ProposalCard p={item.p} onError={() => onFail("karar gönderilemedi")} />
          ) : (
            <QuestionCard q={item.q} onError={() => onFail("cevap gönderilemedi")} />
          )}
        </div>
        <p className="core-keys">
          {item.kind === "proposal" ? (
            <><kbd>E</kbd> {item.p.delegationId ? "onayla" : "ekle"} · <kbd>R</kbd> reddet</>
          ) : (
            <>{item.q.options.length > 0 && <><kbd>1</kbd>–<kbd>{Math.min(9, item.q.options.length)}</kbd> seçenek · </>}<kbd>↵</kbd> gönder</>
          )} · <kbd>Esc</kbd> ertele
        </p>
      </div>
    </div>
  );
}
