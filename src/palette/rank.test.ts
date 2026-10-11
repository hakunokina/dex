import { test } from "node:test";
import assert from "node:assert/strict";
import { RECENT_GROUP, sections, type Group, type Rankable } from "./rank.ts";

const c = (id: string, title: string, group: Group): Rankable => ({ id, title, hint: "", group });
const cmds = [
  c("board", "Görev panosunu aç", "Eylemler"),
  c("team:a", "Takıma geç: Kargo", "Takımlar"),
  c("focus:w1", "Odaklan: Ada", "Ajanlar"),
  c("settings", "Ayarlar", "Eylemler"),
  c("team.new", "Yeni takım", "Takımlar"),
];
const shape = (q: string, recent: string[] = []) => sections(cmds, q, recent).map((s) => [s.label, s.items.map((i) => i.id)]);

test("palette: empty query lists Takımlar → Ajanlar → Eylemler, recents on top without duplicates", () => {
  assert.deepEqual(shape(""), [
    ["Takımlar", ["team:a", "team.new"]],
    ["Ajanlar", ["focus:w1"]],
    ["Eylemler", ["board", "settings"]],
  ]);
  assert.deepEqual(shape("", ["settings", "gone"]), [
    [RECENT_GROUP, ["settings"]],
    ["Takımlar", ["team:a", "team.new"]],
    ["Ajanlar", ["focus:w1"]],
    ["Eylemler", ["board"]],
  ]);
});

test("palette: with a query the best hit's group leads, so Enter still runs the best match", () => {
  const s = sections(cmds, "ayar", []);
  assert.equal(s[0].label, "Eylemler");
  assert.equal(s[0].items[0].id, "settings");
  assert.deepEqual(shape("zzz"), []);
});

test("palette: a group appears once even when its hits are not adjacent in rank", () => {
  const labels = sections(cmds, "a", []).map((s) => s.label);
  assert.equal(new Set(labels).size, labels.length);
});
