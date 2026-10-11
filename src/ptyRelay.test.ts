import { test } from "node:test";
import assert from "node:assert/strict";
import { PtyRelay } from "./ptyRelay.ts";

test("output and exit before a delayed view attach are delivered in order", () => {
  const relay = new PtyRelay();
  relay.onSize(80, 24);
  relay.onData(new TextEncoder().encode("final output"));
  relay.onExit();
  const received: unknown[] = [];
  relay.attach({
    onSize: (cols, rows) => received.push([cols, rows]),
    onData: bytes => received.push(new TextDecoder().decode(bytes)),
    onExit: () => received.push("exit"),
  });
  assert.deepEqual(received, [[80, 24], "final output", "exit"]);
  assert.equal(relay.ended, true);
});

test("an existing view's relay can detach without retaining unseen output", () => {
  const relay = new PtyRelay(false);
  const sink = { onData: () => {}, onExit: () => {} };
  relay.attach(sink);
  relay.detach(sink);
  relay.onData(Uint8Array.of(65));
  const received: number[] = [];
  relay.attach({ onData: bytes => received.push(...bytes), onExit: () => {} });
  assert.deepEqual(received, []);
});

test("a detached view receives buffered bytes once and keeps the PTY size", () => {
  const relay = new PtyRelay();
  const first = { onData: () => {}, onExit: () => {} };
  relay.attach(first);
  relay.onSize(100, 30);
  relay.detach(first);
  relay.onData(Uint8Array.of(65));
  const received: unknown[] = [];
  const second = {
    onSize: (cols: number, rows: number) => received.push([cols, rows]),
    onData: (bytes: Uint8Array) => received.push([...bytes]),
    onExit: () => {},
  };
  relay.attach(second);
  assert.deepEqual(received, [[100, 30], [65]]);
  relay.detach(first); // an old pane's cleanup cannot detach its replacement
  relay.onData(Uint8Array.of(66));
  assert.deepEqual(received, [[100, 30], [65], [66]]);
});
