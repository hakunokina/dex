// Dev-only module state that outlives a vite hot update (DEX-18). Saving any file re-executes every
// module between it and the nearest React boundary, and vite runs `dispose` only for the boundary —
// so a store here would restart empty (team store `ready=false` → every pane unmounts) and each
// transport `on` would register once more. In a build `import.meta.hot` is undefined: plain init.
import { onEvent } from "./api/hosts";
import type { DexEvent } from "./api/transport";

type Hot = ImportMeta["hot"];

/** The previous instance's value for `key`, else `init()`. Reassigned values must be written back. */
export function kept<T>(hot: Hot, key: string, init: () => T): T {
  if (!hot) return init();
  return (hot.data[key] ??= init()) as T;
}

/** A stable function that always calls the newest instance's `fn` (for callbacks registered once). */
export function latest<A extends unknown[], R>(hot: Hot, key: string, fn: (...a: A) => R): (...a: A) => R {
  const slot = kept(hot, key, () => {
    const s = { fn, call: (...a: A) => s.fn(...a) };
    return s;
  });
  slot.fn = fn;
  return slot.call;
}

/** `listen` once per page load, on every host (S4); a hot-updated module only swaps the handler. */
export function listenKept<T>(hot: Hot, event: string, handler: (e: DexEvent<T>, host: string) => void) {
  const h = latest(hot, `listen:${event}`, handler);
  kept(hot, `listening:${event}`, () => onEvent<T>(event, h));
}
