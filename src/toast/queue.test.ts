import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_TOASTS, pushToast, toastLife, type Toast } from "./queue.ts";

const t = (id: number, key?: string): Toast => ({ id, tone: "info", title: `t${id}`, key });

test("toasts: newest last, at most 3 — the oldest drops", () => {
  let list: Toast[] = [];
  for (let i = 1; i <= 5; i++) list = pushToast(list, t(i));
  assert.equal(MAX_TOASTS, 3);
  assert.deepEqual(list.map((x) => x.id), [3, 4, 5]);
});

test("toasts: the same key replaces the older toast and moves to the end", () => {
  let list = pushToast(pushToast(pushToast([], t(1, "a")), t(2, "b")), t(3, "a"));
  assert.deepEqual(list.map((x) => x.id), [2, 3]);
  list = pushToast(list, t(4));
  assert.deepEqual(list.map((x) => x.id), [2, 3, 4]);
});

test("toasts: one with actions stays longer", () => {
  assert.ok(toastLife({ ...t(1), actions: [{ label: "Aç", run: () => {} }] }) > toastLife(t(2)));
});
