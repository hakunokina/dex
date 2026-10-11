import { test } from "node:test";
import assert from "node:assert/strict";
import { rackOrder, rackVisible } from "./rackOrder.ts";

const ws = ["Bot-1", "Canvas-1", "Kernel-1", "Kernel-2", "Kernel-3", "Kernel-4", "Pixel-1"].map(id => ({ id }));
const ids = (xs: { id: string }[]) => xs.map(x => x.id);

test("aktifler (working/waiting) öne, iki grup da kendi sırasını korur", () => {
  const order = rackOrder(ws, { "Kernel-2": "working", "Kernel-4": "waiting", "Pixel-1": "idle", "Bot-1": "offline" });
  assert.deepEqual(ids(order), ["Kernel-2", "Kernel-4", "Bot-1", "Canvas-1", "Kernel-1", "Kernel-3", "Pixel-1"]);
});

test("aktif kalmayınca eski yerine döner", () => {
  assert.deepEqual(ids(rackOrder(ws, { "Kernel-2": "idle", "Kernel-4": "offline" })), ids(ws));
  assert.deepEqual(ids(rackOrder(ws, {})), ids(ws));
});

test("idle/offline aktif sayılmaz", () => {
  assert.deepEqual(ids(rackOrder(ws, { "Canvas-1": "idle", "Pixel-1": "offline" })), ids(ws));
});

test("üst çubukta yalnız kırmızılar: working/waiting ya da iş sayacı olan", () => {
  const jobs = (id: string) => (id === "Pixel-1" ? 2 : 0);
  const shown = rackVisible(ws, { "Kernel-2": "working", "Kernel-4": "waiting", "Canvas-1": "idle", "Bot-1": "offline", "Pixel-1": "idle" }, jobs);
  assert.deepEqual(ids(shown), ["Kernel-2", "Kernel-4", "Pixel-1"]);
  assert.deepEqual(rackVisible(ws, {}, () => 0), []);
});
