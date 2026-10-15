// #2051: the Kart's frame loop stays alive. One throw in a frame used to end the rAF chain for good (no
// next frame was ever asked for) — the board froze until it was remounted (a team switch). Now a failing
// frame is logged once and the loop goes on; a once-a-second check revives a chain that went idle while
// the board is on screen (a missed IntersectionObserver change, or anything else that stops it).

export interface LoopState {
  stopped: boolean;
  /** A frame (rAF or timer) is already asked for. */
  pending: boolean;
  /** Office's `active` prop and the page: hidden → idle on purpose. */
  active: boolean;
  hidden: boolean;
  /** The observer's last word: the canvas intersects the viewport. */
  inView: boolean;
}

/**
 * What the health check does with an idle loop: `observe` = out of view by the observer's last word —
 * ask it again (re-observing delivers the current state); `schedule` = in view but nothing asked for
 * a frame: the chain died, restart it.
 */
export function revive(s: LoopState): "none" | "observe" | "schedule" {
  if (s.stopped || s.pending || !s.active || s.hidden) return "none";
  return s.inView ? "schedule" : "observe";
}

/** Logs each distinct failure once (a frame that throws every time must not flood the console). */
export function onceLog(log: (...args: unknown[]) => void = console.error) {
  const seen = new Set<string>();
  return (what: string, error?: unknown) => {
    const key = `${what}\n${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`;
    if (seen.has(key)) return;
    seen.add(key);
    log(`[Kart] ${what}`, ...(error === undefined ? [] : [error]));
  };
}
