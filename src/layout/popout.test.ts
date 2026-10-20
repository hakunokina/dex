import { test } from "node:test";
import assert from "node:assert/strict";
import { detachAction, parseLeadQuery, parsePopouts, popoutLabel, popoutUrl, popoutWindowOptions, POPOUT_SIZE } from "./popout.ts";

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
