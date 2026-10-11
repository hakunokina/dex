// Modern §6: the bottom-right toast stack (at most 3, newest at the bottom). Hover or keyboard focus
// holds a toast; it fades out on its own afterwards. The region is always mounted so `aria-live` speaks.
import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { revealAgent } from "../layout/panels";
import { getTeamsSnapshot, selectTeam } from "../team/store";
import { reducedMotion } from "../motion";
import { toastLife } from "./queue";
import { dismissToast, useToasts, type Toast, type ToastCtx } from "./store";
import "./toast.css";

const LEAVE_MS = 320; // --dur-slow

export function Toasts({ onFocusAgent }: { onFocusAgent: (agentId: string) => void }) {
  const list = useToasts();
  const ctx: ToastCtx = {
    focusAgent: (id) => {
      const a = getTeamsSnapshot().agents.find((x) => x.id === id);
      if (!a) return;
      selectTeam(a.teamId);
      onFocusAgent(id);
      void revealAgent(a.teamId, id);
    },
  };
  return (
    <section className="toasts" aria-label="Bildirimler" aria-live="polite" aria-relevant="additions">
      {list.map((t) => (
        <ToastItem key={t.id} t={t} ctx={ctx} />
      ))}
    </section>
  );
}

function ToastItem({ t, ctx }: { t: Toast; ctx: ToastCtx }) {
  const [leaving, setLeaving] = useState(false);
  const [held, setHeld] = useState(false);
  const left = useRef(toastLife(t));

  // Counts down only while nobody holds it; what remains carries over to the next run.
  useEffect(() => {
    if (held || leaving) return;
    const t0 = Date.now();
    const timer = setTimeout(() => setLeaving(true), left.current);
    return () => {
      clearTimeout(timer);
      left.current -= Date.now() - t0;
    };
  }, [held, leaving]);

  useEffect(() => {
    if (!leaving) return;
    const timer = setTimeout(() => dismissToast(t.id), reducedMotion() ? 0 : LEAVE_MS);
    return () => clearTimeout(timer);
  }, [leaving, t.id]);

  return (
    <div
      className={`toast ${t.tone}${leaving ? " leaving" : ""}`}
      onMouseEnter={() => setHeld(true)}
      onMouseLeave={() => setHeld(false)}
      onFocus={() => setHeld(true)}
      onBlur={(e) => !e.currentTarget.contains(e.relatedTarget) && setHeld(false)}
    >
      <div className="toast-head">
        <b className="toast-title">{t.title}</b>
        <button className="toast-close" onClick={() => dismissToast(t.id)} aria-label="Bildirimi kapat" title="Kapat">
          <X />
        </button>
      </div>
      {t.body && <div className="toast-body">{t.body}</div>}
      {!!t.actions?.length && (
        <div className="toast-acts">
          {t.actions.map((a) => (
            <button
              key={a.label}
              onClick={() => {
                dismissToast(t.id);
                a.run(ctx);
              }}
            >
              {a.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
