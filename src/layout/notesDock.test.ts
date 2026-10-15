import { test } from "node:test";
import assert from "node:assert/strict";
import { migrateNotesDock, NOTES_ID } from "./notesDock.ts";

type Leaf = { type: "leaf"; data: { id: string; views: string[]; activeView?: string }; size?: number };
type Branch = { type: "branch"; data: (Leaf | Branch)[]; size?: number };
type Dock = {
  grid: { root: Branch; orientation: string };
  panels: Record<string, { id: string; title?: string; params?: Record<string, string>; contentComponent?: string }>;
  activeGroup?: string;
  floatingGroups?: { data: Leaf["data"]; position: Record<string, number> }[];
};

const leaf = (id: string, views: string[], activeView = views[0]): Leaf => ({ type: "leaf", data: { id, views, activeView } });
const panel = (id: string) => ({ id });
const OLD = {
  "notes:shared": { id: "notes:shared", contentComponent: "notes", title: "Ortak not", params: { page: "shared" } },
  "notes:team": { id: "notes:team", contentComponent: "notes", title: "Takım notu", params: { page: "team" } },
};
const dock = (root: Branch, extra: Partial<Dock> = {}): Dock => ({
  grid: { root, orientation: "HORIZONTAL" },
  panels: { office: panel("office"), "agent:core": panel("agent:core"), "agent:echo": panel("agent:echo"), ...OLD },
  activeGroup: "g1",
  ...extra,
});
const migrate = (d: Dock) => migrateNotesDock(d as never) as unknown as Dock;
const views = (d: Dock) => {
  const out: string[][] = [];
  const walk = (n: Leaf | Branch) => (n.type === "branch" ? n.data.forEach(walk) : out.push(n.data.views));
  walk(d.grid.root);
  return out;
};

test("lider grubunda iki sayfa: tek Notlar paneli Ortak not'un yerinde (Core | Notlar | Echo)", () => {
  const d = dock({ type: "branch", data: [leaf("g0", ["office"]), leaf("g1", ["agent:core", "notes:shared", "notes:team", "agent:echo"], "agent:core")] });
  const m = migrate(d);
  assert.deepEqual(views(m), [["office"], ["agent:core", NOTES_ID, "agent:echo"]]);
  assert.equal(m.panels["notes:shared"], undefined);
  assert.equal(m.panels["notes:team"], undefined);
  assert.deepEqual(m.panels[NOTES_ID], { id: NOTES_ID, contentComponent: "notes", tabComponent: "notes", title: "Notlar" });
  assert.equal((m.grid.root.data[1] as Leaf).data.activeView, "agent:core");
  assert.ok(d.panels["notes:shared"], "girdi değişmez");
});

test("etkin sayfa Takım notu idiyse etkin görünüm Notlar olur", () => {
  const m = migrate(dock({ type: "branch", data: [leaf("g1", ["agent:core", "notes:shared", "notes:team"], "notes:team")] }));
  assert.equal((m.grid.root.data[0] as Leaf).data.activeView, NOTES_ID);
});

test("ayrı gruplar: ilk sayfa kalır, yalnız ikinci sayfayı taşıyan grup budanır, etkin grup düşerse Notlar'a geçer", () => {
  const d = dock(
    { type: "branch", data: [leaf("g0", ["office"]), { type: "branch", data: [leaf("g1", ["agent:core"]), leaf("g2", ["notes:team"]), leaf("g3", ["notes:shared"])] }] },
    { activeGroup: "g3" },
  );
  const m = migrate(d);
  assert.deepEqual(views(m), [["office"], ["agent:core"], [NOTES_ID]]);
  assert.equal(m.activeGroup, "g2");
});

test("yalnız Takım notu açıksa o Notlar olur", () => {
  const d = dock({ type: "branch", data: [leaf("g1", ["agent:core", "notes:team"])] });
  delete (d.panels as Record<string, unknown>)["notes:shared"];
  const m = migrate(d);
  assert.deepEqual(views(m), [["agent:core", NOTES_ID]]);
  assert.ok(m.panels[NOTES_ID]);
});

test("yüzen grup: tek sayfalı yüzen grup kalkar, ızgaradaki sayfa Notlar olur", () => {
  const d = dock(
    { type: "branch", data: [leaf("g1", ["agent:core", "notes:shared"])] },
    { floatingGroups: [{ data: { id: "f1", views: ["notes:team"], activeView: "notes:team" }, position: { top: 0, left: 0, width: 300, height: 200 } }] },
  );
  const m = migrate(d);
  assert.deepEqual(views(m), [["agent:core", NOTES_ID]]);
  assert.deepEqual(m.floatingGroups, []);
});

test("Notlar zaten varsa eski sayfalar yalnız düşer; eski sayfa yoksa düzen aynı nesnedir", () => {
  const d = dock({ type: "branch", data: [leaf("g1", ["agent:core", NOTES_ID]), leaf("g2", ["notes:team"])] });
  d.panels[NOTES_ID] = { id: NOTES_ID, title: "Notlar" };
  const m = migrate(d);
  assert.deepEqual(views(m), [["agent:core", NOTES_ID]]);
  assert.equal(m.panels["notes:team"], undefined);
  assert.equal(migrate(m), m);
});

test("kapalı sayfalar yalnız panels'ta kaldıysa temizlenir, Notlar eklenmez", () => {
  const d = dock({ type: "branch", data: [leaf("g1", ["agent:core"])] });
  const m = migrate(d);
  assert.deepEqual(views(m), [["agent:core"]]);
  assert.deepEqual(Object.keys(m.panels).sort(), ["agent:core", "agent:echo", "office"]);
});
