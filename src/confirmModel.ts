// #2145: askConfirm's text → the in-app dialog's parts, and its queue. Pure: unit-tested in node.

/** First paragraph is the title unless one is given; `\n\n` splits paragraphs, a single `\n` stays a line break. */
export function confirmParts(text: string, title?: string): { title: string; body: string[] } {
  const paras = text.split(/\n\s*\n/).map((p) => p.trim()).filter(Boolean);
  if (title) return { title, body: paras };
  return { title: paras[0] ?? "", body: paras.slice(1) };
}

/** A key inside an open confirm belongs to it: earlier window-capture Esc handlers (board, palette,
 *  overlays) step aside instead of closing what is under the question. */
export const inConfirm = (t: EventTarget | null) => !!(t as Element | null)?.closest?.('[role="alertdialog"]');

/** Runs one at a time, in call order: a second question waits until the first is answered. */
export function serial() {
  let tail: Promise<unknown> = Promise.resolve();
  return <T>(run: () => Promise<T>): Promise<T> => {
    const next = tail.then(run);
    tail = next.catch(() => {});
    return next;
  };
}
