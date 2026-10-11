import { test } from "node:test";
import assert from "node:assert/strict";
import { sortAgents, type Orderable } from "./order.ts";

const a = (name: string, roleSlug: string | null, kind = "worker"): Orderable => ({ name, kind, roleSlug });
const names = (list: Orderable[]) => list.map((x) => x.name);
const CATALOG = ["backend", "frontend", "mobile", "lead", "qa"];

test("leader first, then role groups in catalog order", () => {
  const mixed = [
    a("Pixel-1", "frontend"), a("Kernel-1", "backend"), a("droid", "frontend"), a("Droid-1", "mobile"),
    a("Core", "lead", "leader"), a("Droid-2", "mobile"), a("Kernel-2", "backend"), a("Pixel-2", "frontend"),
  ];
  assert.deepEqual(names(sortAgents(mixed, CATALOG)),
    ["Core", "Kernel-1", "Kernel-2", "Pixel-1", "Pixel-2", "droid", "Droid-1", "Droid-2"]);
});

test("numbers sort naturally inside a group", () => {
  const list = [a("Pixel-10", "frontend"), a("Pixel-2", "frontend"), a("Pixel-1", "frontend")];
  assert.deepEqual(names(sortAgents(list, CATALOG)), ["Pixel-1", "Pixel-2", "Pixel-10"]);
});

test("unnumbered names go last in their group, alphabetical", () => {
  const list = [a("zeta", "qa"), a("Probe-1", "qa"), a("alfa", "qa")];
  assert.deepEqual(names(sortAgents(list, CATALOG)), ["Probe-1", "alfa", "zeta"]);
});

test("roles outside the catalog follow it alphabetically; no role last", () => {
  const list = [a("n", null), a("s", "seo"), a("d", "design"), a("q", "qa")];
  assert.deepEqual(names(sortAgents(list, CATALOG)), ["q", "d", "s", "n"]);
  assert.deepEqual(names(sortAgents(list, [])), ["d", "q", "s", "n"]);
});

test("several leaders keep natural order and stay ahead", () => {
  const list = [a("Kernel-1", "backend"), a("Lead-2", "lead", "leader"), a("Lead-1", "lead", "leader")];
  assert.deepEqual(names(sortAgents(list, CATALOG)), ["Lead-1", "Lead-2", "Kernel-1"]);
});

test("input is not mutated", () => {
  const list = [a("Pixel-2", "frontend"), a("Pixel-1", "frontend")];
  sortAgents(list, CATALOG);
  assert.deepEqual(names(list), ["Pixel-2", "Pixel-1"]);
});
