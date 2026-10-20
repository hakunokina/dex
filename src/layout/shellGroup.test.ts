import { test } from "node:test";
import assert from "node:assert/strict";
import { removesEmptyGroup, resolveShellGroup } from "./shellGroup.ts";

const grid = [
  { id: "1", panels: ["office", "settings"] },
  { id: "2", panels: ["agent:L1", "notes"] },
  { id: "3", panels: ["shell:a", "procs"] },
];

test("hatırlanan grup yaşıyorsa odur — içinde terminal kalmasa da (yalnız Ayarlar) ve boşken de", () => {
  assert.equal(resolveShellGroup(grid, "3"), "3");
  assert.equal(resolveShellGroup([{ id: "1", panels: ["office"] }, { id: "3", panels: ["settings"] }], "3"), "3");
  assert.equal(resolveShellGroup([{ id: "1", panels: ["office"] }, { id: "3", panels: [] }], "3"), "3");
});

test("hatırlanan yoksa (ya da gitmişse) terminalin grubu; o da yoksa boş grup; hiçbiri yoksa null (bir kez kurulur)", () => {
  assert.equal(resolveShellGroup(grid, undefined), "3");
  assert.equal(resolveShellGroup(grid, "9"), "3");
  assert.equal(resolveShellGroup([{ id: "1", panels: ["office"] }, { id: "4", panels: ["procs"] }], undefined), "4");
  assert.equal(resolveShellGroup([{ id: "1", panels: ["office"] }, { id: "5", panels: [] }], undefined), "5");
  assert.equal(resolveShellGroup([{ id: "1", panels: ["office"] }, { id: "2", panels: ["agent:L1"] }], undefined), null);
});

test("son sekme kapanınca yalnız sabit grup kalır, diğer gruplar gider", () => {
  assert.equal(removesEmptyGroup("3", "3"), false);
  assert.equal(removesEmptyGroup("3", "2"), true);
  assert.equal(removesEmptyGroup(null, "3"), true);
});
