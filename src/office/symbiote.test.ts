import { test } from "node:test";
import assert from "node:assert/strict";
import { symbioteParams, bodyContour, tentacleContour, reachAt, DelegationTouches, fragmentPaths, FragmentMotion, fragmentContour, FRAGMENTS, sideways } from "./symbiote.ts";
import * as symbiote from "./symbiote.ts";

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

test("moving body keeps the stance contact fixed in world coordinates", () => {
  const state = symbiote.createFootState();
  const paths = fragmentPaths(body, [trace], [], bounds);
  const first = symbiote.plantedFeet(paths, body, 0, false, state)[0];
  const contact = { x: first.x, y: first.y };
  const moved = { ...body, x: 210 };
  const held = symbiote.plantedFeet(fragmentPaths(moved, [trace], [], bounds), moved, 600, false, state)[0];
  assert.equal(held.lift, 0);
  assert.deepEqual({ x: held.x, y: held.y }, contact);
  const movedPaths = fragmentPaths(moved, [trace], [], bounds);
  const swing = symbiote.plantedFeet(movedPaths, moved, 960, false, state)[0];
  assert.ok(swing.lift > 0);
  const landed = symbiote.plantedFeet(movedPaths, moved, 1200, false, state)[0];
  assert.equal(landed.lift, 0);
  assert.ok(Math.abs(landed.x - 290) < 1e-6);
  assert.ok(Math.abs(landed.y - 223.32) < 1e-6);
  assert.notDeepEqual({ x: landed.x, y: landed.y }, contact);
});

test("a changed route starts an early step without jumping the contact", () => {
  const state = symbiote.createFootState();
  const first = symbiote.plantedFeet(fragmentPaths(body, [trace], [], bounds), body, 0, false, state)[0];
  const contact = { x: first.x, y: first.y };
  const paths = fragmentPaths(body, [[{ x: 200, y: 140 }, { x: 200, y: 70 }]], [], bounds);
  const start = symbiote.plantedFeet(paths, body, 100, false, state)[0];
  assert.deepEqual({ x: start.x, y: start.y }, contact);
  const swing = symbiote.plantedFeet(paths, body, 250, false, state)[0];
  assert.ok(swing.lift > 0);
  const landed = symbiote.plantedFeet(paths, body, 400, false, state)[0];
  assert.equal(landed.lift, 0);
  assert.equal(landed.x, 200);
  assert.ok(landed.y < 140);
});

test("excessive reach regrows from under the body instead of teleporting to a new contact", () => {
  const state = symbiote.createFootState();
  symbiote.plantedFeet(fragmentPaths(body, [trace], [], bounds), body, 0, false, state);
  symbiote.plantedFeet(fragmentPaths(body, [trace], [], bounds), body, 960, false, state);
  const moved = { ...body, x: 800, y: 500 };
  const paths = fragmentPaths(moved, [], [], { x: 0, y: 0, w: 1200, h: 900 });
  const first = symbiote.plantedFeet(paths, moved, 970, false, state)[0];
  assert.deepEqual({ x: first.x, y: first.y }, { x: moved.x, y: moved.y });
  for (let t = 980; t <= 1400; t += 10) {
    for (const foot of symbiote.plantedFeet(paths, moved, t, false, state)) {
      assert.ok(Math.hypot(foot.x - moved.x, foot.y - moved.y) <= moved.s * 1.05);
    }
  }
  const landed = state.feet[0];
  assert.equal(landed.lift, 0);
  assert.ok(Math.hypot(landed.x - moved.x, landed.y - moved.y) > moved.s / 2);
});

test("a route removed during swing retargets continuously and missing feet release their state", () => {
  const state = symbiote.createFootState();
  const paths = fragmentPaths(body, [trace], [], bounds);
  symbiote.plantedFeet(paths, body, 0, false, state);
  const swing = symbiote.plantedFeet(paths, body, 960, false, state)[0];
  const contact = { x: swing.x, y: swing.y };
  const lift = swing.lift;
  const changed = fragmentPaths(body, [[{ x: 200, y: 140 }, { x: 200, y: 70 }]], [], bounds);
  const redirected = symbiote.plantedFeet(changed, body, 970, false, state)[0];
  assert.deepEqual({ x: redirected.x, y: redirected.y }, contact);
  assert.equal(redirected.lift, lift);
  const landed = symbiote.plantedFeet(changed, body, 1270, false, state)[0];
  assert.equal(landed.x, 200);
  assert.ok(landed.y < 150);
  const result = state.feet, slots = [...state.slots];
  assert.equal(symbiote.plantedFeet([], body, 1300, false, state), result);
  assert.equal(result.length, 0);
  symbiote.plantedFeet(changed, body, 1400, false, state);
  assert.equal(result[0], slots[0]);
  assert.equal(result[0].x, 200);
  assert.equal(result[0].y, body.y);
  assert.equal(result[0].lift, 0);
  symbiote.plantedFeet(changed, body, 1550, false, state);
  assert.ok(result[0].lift > 0);
  symbiote.plantedFeet(changed, body, 1700, false, state);
  assert.equal(result[0].lift, 0);
  assert.ok(result[0].y < 150);
});

test("repeated swing redirects preserve height without accumulating lift", () => {
  const state = symbiote.createFootState();
  const paths = fragmentPaths(body, [trace], [], bounds);
  const up = fragmentPaths(body, [[{ x: 200, y: 140 }, { x: 200, y: 70 }]], [], bounds);
  symbiote.plantedFeet(paths, body, 0, false, state);
  symbiote.plantedFeet(paths, body, 960, false, state);
  let t = 970;
  for (let i = 0; i < 8; i++) {
    const route = i % 2 ? paths : up;
    symbiote.plantedFeet(route, body, t, false, state);
    const foot = symbiote.plantedFeet(route, body, t + 150, false, state)[0];
    assert.ok(foot.lift >= 0 && foot.lift <= body.s / 40);
    t += 160;
  }
});

test("feet plant on a circuit corner, hold, lift, then land on the next trace segment", () => {
  assert.equal(typeof symbiote.plantedFeet, "function");
  const paths = fragmentPaths(body, [trace], [], bounds);
  const state = symbiote.createFootState();
  const feet = (t: number) => symbiote.plantedFeet(paths, body, t, false, state);
  const first = { ...feet(0)[0] };
  assert.deepEqual(first, feet(600)[0]);
  assert.equal(first.lift, 0);
  const swing = feet(960)[0];
  assert.ok(swing.lift > 0);
  const landed = feet(1200)[0];
  assert.equal(landed.lift, 0);
  assert.notDeepEqual(landed, first);
  const loop = symbiote.createFootState();
  for (let t = 0; t < 5000; t += 17) {
    for (const foot of symbiote.plantedFeet(paths, body, t, false, loop)) {
      assert.ok(Number.isFinite(foot.x) && Number.isFinite(foot.y));
      // Both the planted point and the swing's board projection follow the existing polyline.
      assert.ok(Math.abs(foot.y - 200) < 1e-6 || Math.abs(foot.x - 290) < 1e-6);
      assert.ok(Math.hypot(foot.x - body.x, foot.y - body.y) <= paths[0].length);
    }
  }
  assert.deepEqual(symbiote.plantedFeet([], body, 900, false, state), []);
  const reduced = symbiote.createFootState();
  const staticFoot = { ...symbiote.plantedFeet(paths, body, 0, true, reduced)[0] };
  assert.deepEqual(staticFoot, symbiote.plantedFeet(paths, body, 9000, true, reduced)[0]);
  assert.ok(symbiote.plantedFeet(paths, body, 9000, true, reduced).every(f => f.lift === 0));
});

test("weight transfer follows the stride with a delay, freezes and stays inside the hover bounds", () => {
  assert.equal(typeof symbiote.bodyWeight, "function");
  assert.deepEqual(symbiote.bodyWeight(0, true), { x: 0, y: 0 });
  assert.deepEqual(symbiote.bodyWeight(9000, true), { x: 0, y: 0 });
  assert.notDeepEqual(symbiote.bodyWeight(0, false), symbiote.bodyWeight(500, false));
  for (const state of ["idle", "tool", "error", "sleep"] as const) {
    for (let t = 0; t < 5000; t += 31) {
      const weight = symbiote.bodyWeight(t, false);
      for (const p of bodyContour(state, t, t, false)) {
        assert.ok(Math.abs(p.x * 1.025 + weight.x) <= 60);
        assert.ok(Math.abs(p.y * 1.025 + weight.y) <= 60);
      }
    }
  }
});

test("body lighting has a fixed upper-left light, bounded sphere normals and an illuminated rim", () => {
  assert.equal(typeof symbiote.bodyLighting, "function");
  const lit = symbiote.bodyLighting(-0.4, -0.55), dark = symbiote.bodyLighting(0.4, 0.55);
  assert.ok(lit.diffuse > dark.diffuse);
  assert.ok(lit.specular > dark.specular);
  assert.ok(symbiote.bodyLighting(-0.7, -0.7).rim > symbiote.bodyLighting(0, 0).rim);
  for (const [x, y] of [[0, 0], [-0.4, -0.55], [0.9, 0.9], [3, -4]]) {
    const light = symbiote.bodyLighting(x, y);
    for (const v of [light.diffuse, light.specular, light.rim]) assert.ok(Number.isFinite(v) && v >= 0 && v <= 1);
    assert.ok(Math.abs(Math.hypot(light.normal.x, light.normal.y, light.normal.z) - 1) < 1e-6);
  }
});

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
