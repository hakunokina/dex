// Modern §2: shared motion helpers. The CSS side (pane fade, card pop, skeleton) lives in App.css.
import { useEffect, useRef, useState } from "react";

export const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

export const reducedMotion = () => typeof matchMedia === "function" && matchMedia(REDUCED_MOTION).matches;

/** The number shown at `k` (0…1) of a count from `from` to `to` — ease-out cubic, whole numbers. */
export function countAt(from: number, to: number, k: number): number {
  const t = Math.min(1, Math.max(0, k));
  return Math.round(from + (to - from) * (1 - (1 - t) ** 3));
}

/** A number that counts to its new value over `ms` instead of jumping (reduced motion: jumps). */
export function useCountUp(value: number, ms = 300): number {
  const [shown, setShown] = useState(value);
  const at = useRef(value);
  useEffect(() => {
    const from = at.current;
    if (from === value || reducedMotion()) {
      at.current = value;
      setShown(value);
      return;
    }
    const t0 = performance.now();
    let raf = requestAnimationFrame(function step(now) {
      const k = (now - t0) / ms;
      at.current = countAt(from, value, k);
      setShown(at.current);
      if (k < 1) raf = requestAnimationFrame(step);
    });
    return () => cancelAnimationFrame(raf);
  }, [value, ms]);
  return shown;
}
