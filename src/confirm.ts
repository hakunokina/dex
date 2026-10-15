// #2051: the yes/no before a destructive act. In Tauri the dialog plugin (2.7.x) replaces window.confirm
// with an async call to a command it no longer has ("dialog.confirm not allowed. Command not found"), and
// the Promise it returns is truthy: `if (!confirm(..)) return` went on without asking. The plugin's own
// `confirm` (its `message` command, `dialog:allow-message`) is awaited instead; a plain browser keeps the native one.
import { confirm } from "@tauri-apps/plugin-dialog";
import { WEB } from "./api/platform";

/** True only when the patron chose Tamam. */
export async function askConfirm(text: string): Promise<boolean> {
  if (WEB) return window.confirm(text);
  return confirm(text, { title: "Dex", kind: "warning", okLabel: "Tamam", cancelLabel: "İptal" });
}
