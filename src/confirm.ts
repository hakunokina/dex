// #2051: the yes/no before a destructive act is awaited — in Tauri window.confirm returns a truthy Promise.
// #2145: the question is Dex's own dialog (ConfirmDialog, themed), not the dialog plugin's native Windows
// box; it renders into its own root over everything, so any caller (in or out of React) can await it.
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { ConfirmDialog } from "./settings/kit/ConfirmDialog";
import { confirmParts, serial } from "./confirmModel";

export type ConfirmOpts = {
  title?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Destructive (default): the primary is red and focus opens on İptal. */
  danger?: boolean;
};

const queue = serial();

/** True only when the patron chose Tamam; Esc, İptal and a click outside are false. Calls queue up. */
export function askConfirm(text: string, opts: ConfirmOpts = {}): Promise<boolean> {
  return queue(() => show(text, opts));
}

function show(text: string, opts: ConfirmOpts): Promise<boolean> {
  const { title, body } = confirmParts(text, opts.title);
  const back = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  return new Promise((resolve) => {
    let done = false;
    const close = (ok: boolean) => {
      if (done) return;
      done = true;
      root.unmount();
      host.remove();
      if (back?.isConnected) back.focus();
      resolve(ok);
    };
    root.render(createElement(ConfirmDialog, {
      title,
      body,
      confirmLabel: opts.confirmLabel ?? "Tamam",
      cancelLabel: opts.cancelLabel ?? "İptal",
      danger: opts.danger ?? true,
      onConfirm: () => close(true),
      onCancel: () => close(false),
    }));
  });
}
