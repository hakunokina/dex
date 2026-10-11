import { useEffect, useRef, useSyncExternalStore } from "react";
import type { AgentStatus } from "../agents";
import { reducedMotion } from "../motion";
import { heartbeatView } from "./heartbeat";
import { heartbeatNow, heartbeatOf, subscribeHeartbeat } from "./heartbeatStore";

export function Heartbeat({ id, status }: { id: string; status: AgentStatus }) {
  const pulse = useSyncExternalStore(subscribeHeartbeat, () => heartbeatOf(id));
  const now = useSyncExternalStore(subscribeHeartbeat, heartbeatNow);
  const dot = useRef<HTMLSpanElement>(null);
  const last = useRef(pulse);
  const animation = useRef<Animation | null>(null);
  useEffect(() => () => animation.current?.cancel(), []);
  useEffect(() => {
    if (status === "offline" || !pulse) {
      animation.current?.cancel();
      animation.current = null;
      return;
    }
    const previous = last.current;
    if (!dot.current || !pulse.sequence || previous === pulse) return;
    // Consume only visible actions; status polling must not shorten their animation.
    last.current = pulse;
    animation.current?.cancel();
    animation.current = dot.current.animate(reducedMotion()
      ? [{ background: "var(--red-hi)" }, { background: "var(--red-hi)", offset: 0.6 }, { background: "var(--red)" }]
      : [{ transform: "scale(1)", boxShadow: "0 0 0 var(--red-glow)" },
        { transform: "scale(1.7)", boxShadow: "0 0 9px 3px var(--red-glow)", offset: 0.3 },
        { transform: "scale(1)", boxShadow: "0 0 0 var(--red-glow)" }],
    { duration: 400, easing: "ease-out" });
  }, [pulse, status]);
  const view = heartbeatView(pulse?.at ?? null, status, now);
  const label = pulse ? `Son nabız ${view?.label} önce${view?.quiet ? " · çalışan ajan sessiz" : ""}` : "Henüz nabız alınmadı";
  return <>
    <span ref={dot} className={`dot dex-heartbeat ${status}`} aria-hidden="true" />
    {view && <span className={`dex-heartbeat-age${view.quiet ? " quiet" : ""}`} title={label} aria-label={label}>{view.label}</span>}
  </>;
}
