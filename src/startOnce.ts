const pending = new Map<string, Promise<void>>();

/** A pane mounting during a hidden start must join that start, never issue another one. */
export function startOnce(id: string, start: () => Promise<unknown>): Promise<void> {
  const current = pending.get(id);
  if (current) return current;
  const next = Promise.resolve().then(start).then(() => {}).finally(() => pending.delete(id));
  pending.set(id, next);
  return next;
}
