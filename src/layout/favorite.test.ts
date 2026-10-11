import { test } from "node:test";
import assert from "node:assert/strict";
import { remapDock, type FavoriteRec } from "./favorite.ts";

type Panel = { id: string; title?: string; params?: Record<string, string> };
// Minimal dockview JSON: panels + grid leaf'lerindeki views.
const dockOf = (panels: Record<string, Panel>, views: string[][]) => ({
  grid: {
    orientation: "VERTICAL",
    root: { type: "branch", data: views.map((v, i) => ({ type: "leaf", data: { id: `g${i}`, views: v, activeView: v[0] } })) },
  },
  panels,
  activeGroup: "g0",
}) as unknown as FavoriteRec["dock"];

const fav: FavoriteRec = {
  dock: dockOf(
    {
      office: { id: "office" },
      "agent:L1": { id: "agent:L1", title: "Core", params: { agentId: "L1" } },
      "agent:W1": { id: "agent:W1", title: "Pixel-1", params: { agentId: "W1" } },
      "agent:W2": { id: "agent:W2", title: "Pixel-2", params: { agentId: "W2" } },
      "shell:S1": { id: "shell:S1", params: { shellId: "S1" } },
    },
    [["office"], ["agent:L1"], ["agent:W1", "agent:W2"], ["shell:S1"]],
  ),
  leaders: ["L1"],
  workers: ["W1", "W2"],
  shells: ["S1"],
  from: "t1",
};

const target = {
  leaders: [{ id: "L9", name: "Core-9" }],
  workers: [
    { id: "W7", name: "Pixel-7" },
    { id: "W8", name: "Pixel-8" },
  ],
  shells: ["S9"],
};

test("lider/worker/shell yuvaları hedef takımın ajanlarıyla dolar, ofis aynen kalır", () => {
  const { dock, dropped } = remapDock(fav.dock, fav, target);
  const d = dock as unknown as { panels: Record<string, Panel>; grid: { root: { data: { data: { views: string[] } }[] } } };
  assert.deepEqual(dropped, []);
  assert.ok(d.panels["office"]);
  assert.ok(!d.panels["agent:L1"] && d.panels["agent:L9"].title === "Core-9");
  assert.equal(d.panels["agent:L9"].params?.agentId, "L9");
  assert.ok(d.panels["agent:W7"] && d.panels["agent:W8"]);
  assert.ok(!d.panels["agent:W1"] && !d.panels["agent:W2"]);
  assert.ok(d.panels["shell:S9"] && !d.panels["shell:S1"]);
  const views = d.grid.root.data.flatMap(g => g.data.views);
  assert.ok(views.includes("agent:L9") && views.includes("agent:W7") && views.includes("shell:S9"));
  assert.ok(!views.some(v => v === "agent:L1" || v === "agent:W1" || v === "shell:S1"));
});

test("fazla kaynak yuva düşer, fazla hedef ajan panelsiz kalır", () => {
  const { dock, dropped } = remapDock(fav.dock, fav, {
    leaders: [{ id: "L9", name: "Core-9" }],
    workers: [{ id: "W7", name: "Pixel-7" }],
    shells: [],
  });
  const d = dock as unknown as { panels: Record<string, Panel>; grid: { root: { data: { data: { views: string[]; activeView?: string } }[] } } };
  assert.deepEqual([...dropped].sort(), ["agent:W2", "shell:S1"]);
  assert.ok(d.panels["agent:W7"] && !d.panels["agent:W2"]);
  for (const g of d.grid.root.data) {
    assert.ok(!g.data.views.includes("agent:W2"));
    if (g.data.activeView) assert.ok(g.data.views.includes(g.data.activeView));
  }
});

test("girdi değişmez", () => {
  const before = JSON.stringify(fav.dock);
  remapDock(fav.dock, fav, target);
  assert.equal(JSON.stringify(fav.dock), before);
});
