import { test } from "node:test";
import assert from "node:assert/strict";
import { symbioteParams, bodyContour, tentacleContour, reachAt, DelegationTouches, fragmentPaths, FragmentMotion, fragmentContour, FRAGMENTS, sideways } from "./symbiote.ts";

test("Core states distinguish breathing, agitation, spikes and settling", () => {
  const idle = symbioteParams("idle", 1000);
  assert.ok(symbioteParams("thinking", 1000).speed > idle.speed);
  assert.ok(symbioteParams("tool", 1000).amplitude > idle.amplitude);
  assert.ok(symbioteParams("error", 1000).spikes > idle.spikes);
  assert.ok(symbioteParams("done", 0).amplitude > symbioteParams("done", 5000).amplitude);
});

test("tentacle extends, touches, then retracts to its origin", () => {
  assert.equal(reachAt(0), 0);
  assert.equal(reachAt(600), 1);
  assert.equal(reachAt(800), 1);
  assert.equal(reachAt(1400), 0);
  const route = [{ x: 0, y: 0 }, { x: 100, y: 0 }, { x: 100, y: 100 }];
  const contour = tentacleContour(route, 1, 300);
  assert.deepEqual(contour[24], route[2]);
  assert.ok(contour.every(p => Number.isFinite(p.x) && Number.isFinite(p.y)));
  assert.deepEqual(tentacleContour([], 1, 0), []);
  assert.deepEqual(tentacleContour(route, 0, 0), []);
  assert.deepEqual(tentacleContour([{ x: 1, y: 1 }, { x: 1, y: 1 }], 1, 0), []);
});

test("delegation contact only fires for new active jobs, never loaded history", () => {
  const touches = new DelegationTouches();
  const job = { id: 1, key: "local:1", fromAgent: "core", toAgent: "worker", status: "running" };
  assert.deepEqual(touches.observe([job]), []);
  assert.deepEqual(touches.observe([job]), []);
  assert.deepEqual(touches.observe([job, { ...job, id: 2, key: "local:2" }]), [{ leader: "core", worker: "worker" }]);
  assert.deepEqual(touches.observe([job, { ...job, id: 3, key: "local:3", status: "done" }]), []);
});

test("delegation contact fires for the same id on a different host only once", () => {
  const touches = new DelegationTouches();
  const local = { id: 42, key: "local:42", fromAgent: "local-core", toAgent: "local-worker", status: "running" };
  const remote = { ...local, key: "remote:42", fromAgent: "remote-core", toAgent: "remote-worker" };
  assert.deepEqual(touches.observe([local]), []);
  assert.deepEqual(touches.observe([local, remote]), [{ leader: "remote-core", worker: "remote-worker" }]);
  assert.deepEqual(touches.observe([local, remote]), []);
});

test("organic silhouette stays inside the body square and freezes under reduced motion", () => {
  const first = bodyContour("error", 100, 1000, true);
  assert.deepEqual(first, bodyContour("error", 9000, 1000, true));
  assert.equal(first.length, 64);
  assert.ok(first.every(p => Number.isFinite(p.x) && Number.isFinite(p.y) && Math.abs(p.x) <= 60 && Math.abs(p.y) <= 60));
  assert.notDeepEqual(bodyContour("idle", 0, 1000, false), bodyContour("idle", 1000, 1000, false));
  for (const state of ["idle", "thinking", "tool", "delegated", "ask", "done", "error", "sleep"] as const) {
    for (let time = 0; time < 20_000; time += 137) {
      assert.ok(bodyContour(state, time, time, false).every(p => Math.abs(p.x) <= 60 && Math.abs(p.y) <= 60), `${state} at ${time}`);
    }
  }
});

const body = { x: 200, y: 200, s: 120 };
const bounds = { x: 0, y: 0, w: 400, h: 400 };
const trace = [{ x: 260, y: 200 }, { x: 290, y: 200 }, { x: 290, y: 300 }];

test("fragment routes follow nearby traces and stop before chips, labels and board edges", () => {
  const blocks = [{ x: 275, y: 245, w: 50, h: 50 }];
  const paths = fragmentPaths(body, [trace], blocks, bounds);
  assert.equal(paths.length, 1);
  assert.deepEqual(paths[0].points.slice(0, 3), [{ x: 200, y: 200 }, ...trace.slice(0, 2)]);
  const motion = new FragmentMotion();
  for (let i = 0; i < 1200; i++) {
    motion.step("thinking", 1 / 60, false);
    const pieces = motion.sample(paths, body, false);
    assert.equal(pieces.length, sideways(paths[0], body) ? 2 * FRAGMENTS : FRAGMENTS);
    for (const piece of pieces) {
      for (const p of fragmentContour(piece, motion.phase)) {
        assert.ok(Math.hypot(p.x - body.x, p.y - body.y) <= body.s * 1.2);
        assert.ok(p.x >= 0 && p.y >= 0 && p.x <= 400 && p.y <= 400);
        assert.ok(p.x < 275 || p.x > 325 || p.y < 245 || p.y > 295);
      }
    }
  }
});

test("fragment motion is continuous across state changes, faster while working, and returns on error", () => {
  const paths = fragmentPaths(body, [trace], [], bounds);
  const idle = new FragmentMotion(), busy = new FragmentMotion();
  idle.step("idle", 1, false);
  busy.step("tool", 1, false);
  assert.ok(busy.phase > idle.phase);
  const before = idle.sample(paths, body, false);
  idle.step("thinking", 0, false);
  assert.deepEqual(idle.sample(paths, body, false), before);
  idle.step("error", 0.25, false);
  const returning = idle.sample(paths, body, false);
  assert.ok(returning.every((p, i) => Math.hypot(p.x - body.x, p.y - body.y) < Math.hypot(before[i].x - body.x, before[i].y - body.y)));
  idle.step("error", 1, false);
  assert.ok(idle.sample(paths, body, false).every(p => Math.hypot(p.x - body.x, p.y - body.y) < 1));
  idle.step("idle", 1, false);
  assert.ok(idle.sample(paths, body, false).some(p => Math.hypot(p.x - body.x, p.y - body.y) > body.s / 2));
});

test("fragment contours stay inside a tight board at every size and avoid a crossed rectangle", () => {
  for (const s of [40, 60, 120]) {
    const core = { x: 50, y: 50, s };
    const board = { x: 0, y: 0, w: 100, h: 100 };
    const blocks = [{ x: 82, y: 30, w: 18, h: 40 }];
    const paths = fragmentPaths(core, [[{ x: 65, y: 50 }, { x: 110, y: 50 }]], blocks, board);
    const motion = new FragmentMotion();
    for (let i = 0; i < 300; i++) {
      motion.step("tool", 0.03, false);
      for (const piece of motion.sample(paths, core, false)) {
        for (const p of fragmentContour(piece, motion.phase)) {
          assert.ok(p.x >= 0 && p.y >= 0 && p.x <= 100 && p.y <= 100);
          assert.ok(p.x < 82 || p.y < 30 || p.y > 70);
        }
      }
    }
  }
});

test("fragments merge periodically; reduced motion hides them and freezes their clock", () => {
  const paths = fragmentPaths(body, [], [], bounds);
  assert.ok(paths.length >= 3);
  const motion = new FragmentMotion();
  let near = false, far = false;
  for (let i = 0; i < 1000; i++) {
    motion.step("idle", 0.02, false);
    const piece = motion.sample(paths, body, false)[0];
    const d = Math.hypot(piece.x - body.x, piece.y - body.y);
    near ||= d < body.s * 0.4;
    far ||= d > body.s * 0.7;
  }
  assert.ok(near && far);
  const phase = motion.phase;
  motion.step("tool", 10, true);
  assert.equal(motion.phase, phase);
  assert.deepEqual(motion.sample(paths, body, true), []);
  assert.deepEqual(fragmentPaths(body, [], [{ x: 0, y: 0, w: 400, h: 400 }], bounds), []);
  assert.deepEqual(motion.sample([], body, false), []);
});

test("side routes carry twice the pieces of upper ones", () => {
  const side = fragmentPaths(body, [[{ x: 260, y: 200 }, { x: 330, y: 200 }]], [], bounds);
  const up = fragmentPaths(body, [[{ x: 200, y: 140 }, { x: 200, y: 70 }]], [], bounds);
  assert.ok(sideways(side[0], body) && !sideways(up[0], body));
  const motion = new FragmentMotion();
  assert.equal(motion.sample(up, body, false).length, FRAGMENTS);
  assert.equal(motion.sample(side, body, false).length, 2 * FRAGMENTS);
  const both = motion.sample([...side, ...up], body, false);
  const onSide = both.filter(p => Math.abs(p.y - body.y) < 1).length;
  assert.equal(onSide, 2 * (both.length - onSide));
});
