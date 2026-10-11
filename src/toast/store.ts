// Modern §6: the toast stack's store. Anything may push (delegations, agents); `Toasts` renders it.
import { useSyncExternalStore } from "react";
import { kept } from "../hot";
import { pushToast, type Toast } from "./queue";

export type { Toast, ToastAction, ToastCtx, ToastTone } from "./queue";

const hot = import.meta.hot;
const box = kept(hot, "toasts", () => ({ list: [] as Toast[], next: 1 }));
const listeners = kept(hot, "listeners", () => new Set<() => void>());
const emit = () => listeners.forEach((l) => l());

export function toast(t: Omit<Toast, "id">): number {
  const id = box.next++;
  box.list = pushToast(box.list, { ...t, id });
  emit();
  return id;
}

export function dismissToast(id: number) {
  if (!box.list.some((t) => t.id === id)) return;
  box.list = box.list.filter((t) => t.id !== id);
  emit();
}

export const useToasts = () =>
  useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => box.list,
  );
