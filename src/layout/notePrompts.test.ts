import { test } from "node:test";
import assert from "node:assert/strict";
import { audienceLabel, channelLabel, hasText, noteFilled, replacePart, type PromptPart } from "./notePrompts.ts";

test("hasText: boşluk ve satır sonu boş sayılır", () => {
  assert.equal(hasText(""), false);
  assert.equal(hasText("  \n\t"), false);
  assert.equal(hasText(null), false);
  assert.equal(hasText(undefined), false);
  assert.equal(hasText(" x "), true);
});

test("noteFilled: ortak not yalnız gövdeye bakar", () => {
  assert.equal(noteFilled(""), false);
  assert.equal(noteFilled("  \n"), false);
  assert.equal(noteFilled("# başlık"), true);
});

test("noteFilled: takım notu gövde ya da eklemelerden herhangi biri doluysa", () => {
  const empty = { systemAppend: "", messageAppend: " " };
  assert.equal(noteFilled("", empty), false);
  assert.equal(noteFilled("", null), false);
  assert.equal(noteFilled("", { systemAppend: "kısa yaz", messageAppend: "" }), true);
  assert.equal(noteFilled("", { systemAppend: "", messageAppend: "Türkçe" }), true);
  assert.equal(noteFilled("not", empty), true);
});

test("audienceLabel / channelLabel", () => {
  assert.equal(audienceLabel({ audience: "all" }), "Herkes");
  assert.equal(audienceLabel({ audience: "leader" }), "Lider");
  assert.equal(audienceLabel({ audience: "worker" }), "Worker");
  assert.equal(audienceLabel({ audience: "role", role: "frontend" }), "Rol: frontend");
  assert.equal(audienceLabel({ audience: "role" }), "Rol");
  assert.equal(channelLabel("system"), "sistem istemi");
  assert.equal(channelLabel("session-start"), "oturum başı");
});

test("replacePart: yalnız aynı id'yi değiştirir, sırayı korur", () => {
  const part = (id: string, text: string): PromptPart => ({
    id, title: id, audience: "all", channel: "system", editable: true, defaultText: "d", text, overridden: false, updatedAt: 0,
  });
  const list = [part("a", "1"), part("b", "2")];
  const out = replacePart(list, { ...part("b", "3"), overridden: true });
  assert.deepEqual(out.map(p => [p.id, p.text, p.overridden]), [["a", "1", false], ["b", "3", true]]);
  assert.deepEqual(replacePart(list, part("z", "9")), list);
});
