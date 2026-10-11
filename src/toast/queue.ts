// Modern §6: in-app toasts — the pure part (stack rules), kept free of React for node:test.

/** At most this many on screen; a new one pushes the oldest out. */
export const MAX_TOASTS = 3;

export type ToastTone = "ok" | "err" | "info";

export interface ToastCtx {
  /** Bring the agent's team, panel and terminal forward (App's focus). */
  focusAgent: (agentId: string) => void;
}

export interface ToastAction {
  label: string;
  run: (ctx: ToastCtx) => void;
}

export interface Toast {
  id: number;
  tone: ToastTone;
  title: string;
  body?: string;
  actions?: ToastAction[];
  /** Same key → the newer toast replaces the older one (one toast per agent / event source). */
  key?: string;
}

/** Newest last; past `max` the oldest drops. */
export function pushToast(list: readonly Toast[], t: Toast, max = MAX_TOASTS): Toast[] {
  const rest = t.key ? list.filter((x) => x.key !== t.key) : list;
  return [...rest, t].slice(-max);
}

/** How long a toast stays: one with actions gives the patron time to reach them. */
export const toastLife = (t: Toast) => (t.actions?.length ? 10_000 : 6_000);
