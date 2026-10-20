import { test } from "node:test";
import assert from "node:assert/strict";
import { detachAction, mainWindowOwns, parseLeadQuery, parsePopouts, popoutLabel, popoutUrl, popoutWindowOptions, POPOUT_SIZE, reconcilePopouts } from "./popout.ts";

test("label: the agent id in Tauri's allowed set, host-qualified ids included", () => {
  assert.equal(popoutLabel("kaan"), "lead-kaan");
  assert.equal(popoutLabel("h1a2b3:a1b2-c3"), "lead-h1a2b3:a1b2-c3");
  assert.equal(popoutLabel("x y.z"), "lead-x_y_z");
});

test("same pane twice: the second detach focuses the open window", () => {
  const rec = { teamId: "t", open: true, onTop: false, bounds: null };
  assert.equal(detachAction({}, "kaan"), "open");
  assert.equal(detachAction({ kaan: rec }, "kaan"), "focus");
  assert.equal(detachAction({ kaan: { ...rec, open: false } }, "kaan"), "open", "a remembered place is not an open window");
  assert.equal(detachAction({ kaan: rec }, "ece"), "open", "another leader opens its own window");
});

test("url ↔ query round trip; a page without the route is not a pop-out", () => {
  const url = popoutUrl("team-1", "h1:kaan", true);
  assert.equal(url, "index.html?pane=lead&team=team-1&agent=h1%3Akaan&ontop=1");
  assert.deepEqual(parseLeadQuery(url.slice(url.indexOf("?"))), { teamId: "team-1", agentId: "h1:kaan", onTop: true });
  assert.deepEqual(parseLeadQuery("?pane=lead&team=t&agent=a"), { teamId: "t", agentId: "a", onTop: false });
  assert.equal(parseLeadQuery("?snip"), null);
  assert.equal(parseLeadQuery("?pane=lead&team=t"), null, "no agent → no pop-out");
});

test("persisted popouts: bad entries dropped, flags coerced, bounds validated", () => {
  const parsed = parsePopouts({
    kaan: { teamId: "t", open: true, onTop: "yes", bounds: { x: 10, y: 20, width: 500, height: 300 } },
    ece: { teamId: "t", open: "1", bounds: { x: 1, y: 2, width: 0, height: 9 } },
    bad: { open: true },
    worse: null,
  });
  assert.deepEqual(parsed, {
    kaan: { teamId: "t", open: true, onTop: false, bounds: { x: 10, y: 20, width: 500, height: 300 } },
    ece: { teamId: "t", open: false, onTop: false, bounds: null },
  });
  assert.deepEqual(parsePopouts(undefined), {});
  assert.deepEqual(parsePopouts("x"), {});
});

test("window options: the last place wins over the default size; on-top carried", () => {
  const base = { teamId: "t", open: true, onTop: true, bounds: null };
  const fresh = popoutWindowOptions(base, "Takım · Kaan", "index.html?pane=lead");
  assert.equal(fresh.width, POPOUT_SIZE.width);
  assert.equal(fresh.height, POPOUT_SIZE.height);
  assert.equal(fresh.alwaysOnTop, true);
  assert.equal(fresh.title, "Takım · Kaan");
  assert.ok(!("x" in fresh), "no remembered place → the OS picks");
  const placed = popoutWindowOptions({ ...base, bounds: { x: 5, y: 6, width: 400, height: 260 } }, "T", "u");
  assert.deepEqual([placed.x, placed.y, placed.width, placed.height], [5, 6, 400, 260]);
  assert.equal(placed.minWidth, POPOUT_SIZE.minWidth);
});

test("#2260: a living window makes the leader popped out even without a record; the policy waits for the answer", () => {
  const live = new Set(["main", "lead-kaan"]);
  const agents = [{ id: "kaan", teamId: "t" }, { id: "ece", teamId: "t" }];
  // Empty record (reload inside the save debounce) + living `lead-kaan` → kaan is out, ece stays here.
  const rec = reconcilePopouts({}, live, agents);
  assert.deepEqual(rec, { kaan: { teamId: "t", open: true, onTop: false, bounds: null } });
  assert.equal(mainWindowOwns(true, rec, "kaan"), false, "the main window does not subscribe to kaan's PTY");
  assert.equal(mainWindowOwns(true, rec, "ece"), true);
  // Before the reconciliation nothing is owned: no subscription ahead of it, whatever the file said.
  assert.equal(mainWindowOwns(false, {}, "ece"), false);
  assert.equal(mainWindowOwns(false, {}, "kaan"), false);
  assert.equal(mainWindowOwns(true, { kaan: { ...rec.kaan, open: false } }, "kaan"), true, "a closed window: the pane is back here");
});

test("#2260: reconciliation keeps a closed record's place and on-top, leaves open records and unknown labels alone", () => {
  const closed = { teamId: "t", open: false, onTop: true, bounds: { x: 1, y: 2, width: 400, height: 300 } };
  const open = { teamId: "t", open: true, onTop: false, bounds: null };
  const before = { kaan: closed, ece: open };
  const after = reconcilePopouts(before, new Set(["lead-kaan", "lead-ece", "lead-gone"]), [{ id: "kaan", teamId: "t" }, { id: "ece", teamId: "t" }]);
  assert.deepEqual(after, { kaan: { ...closed, open: true }, ece: open });
  assert.equal(reconcilePopouts(before, new Set(["main"]), [{ id: "kaan", teamId: "t" }]), before, "nothing alive: the same object, no save");
});
