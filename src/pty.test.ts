import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { PtyRelay } from "./ptyRelay.ts";

// Exercise the actual subscription code with an in-memory host; no browser/Tauri is needed.
function fixture(holdFirst = false) {
  const subscriptions: { onData: (b: Uint8Array) => void; onExit: () => void; onGeneration?: (g?: number) => void }[] = [];
  const detached: number[] = [];
  let generation = 0;
  let failFirst!: (error: string) => void;
  const first = holdFirst ? new Promise<void>((_resolve, reject) => { failFirst = reject; }) : Promise.resolve();
  const hosts = {
    splitId: (id: string) => ["local", id], isHostUp: () => true,
    transportOf: () => ({
      attachPty: async (_id: string, handlers: typeof subscriptions[number]) => {
        const index = subscriptions.push(handlers) - 1;
        if (index === 0) await first;
        handlers.onGeneration?.(generation);
        return async () => { detached.push(index); };
      },
    }),
  };
  const source = readFileSync(new URL("./pty.ts", import.meta.url), "utf8").replaceAll("import.meta.hot", "undefined");
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
  const exports = {} as typeof import("./pty.ts");
  runInNewContext(code, { exports, TextEncoder, require: (id: string) => {
    if (id === "./api/hosts") return hosts;
    if (id === "./hot") return { kept: (_hot: unknown, _key: string, init: () => unknown) => init() };
    if (id === "./ptyRelay") return { PtyRelay };
    throw new Error(id);
  } });
  return { api: exports, subscriptions, detached, failFirst, setGeneration: (g: number) => { generation = g; } };
}

test("same-id restart attaches the new generation and ignores the old stream's late exit", async () => {
  const f = fixture();
  const output: string[] = [];
  let exits = 0;
  const sink = { onData: (b: Uint8Array) => output.push(new TextDecoder().decode(b)), onExit: () => { exits++; } };
  await f.api.capturePty("kaan", 0);
  await f.api.capturePty("kaan", 0);
  assert.equal(f.subscriptions.length, 1, "generation zero is idempotent");
  assert.equal(await f.api.attachPty("kaan", sink), true);
  f.subscriptions[0].onData(new TextEncoder().encode("OLD"));
  // kill returns while the old reader is held; a new online signal arrives before its exit.
  f.setGeneration(1);
  await f.api.capturePty("kaan", 1);
  assert.equal(f.subscriptions.length, 2, "the replacement must open a new subscription");
  f.subscriptions[1].onData(new TextEncoder().encode("NEW"));
  f.subscriptions[0].onData(new TextEncoder().encode("LATE_OLD"));
  f.subscriptions[0].onExit();
  assert.ok(output.includes("NEW"));
  assert.ok(!output.includes("LATE_OLD"));
  assert.equal(exits, 0, "old exit must not mark the new pane offline");
  assert.equal(await f.api.attachPty("kaan", sink), true);
  await f.api.capturePty("kaan", 1);
  assert.equal(f.subscriptions.length, 2, "same generation is idempotent, including zero");
  assert.deepEqual(f.detached, [], "retired channels finish with their own reader's exit");
  f.subscriptions[1].onExit();
  assert.equal(exits, 1);
});

test("attach snapshot identity prevents a redundant subscription on the next online signal", async () => {
  const f = fixture();
  f.setGeneration(7);
  const sink = { onData: () => {}, onExit: () => {} };
  assert.equal(await f.api.attachPty("kaan", sink), true);
  await f.api.capturePty("kaan", 7);
  assert.equal(f.subscriptions.length, 1);
});

test("online signals without generation keep the legacy subscription behavior", async () => {
  const f = fixture();
  await f.api.capturePty("kaan");
  await f.api.capturePty("kaan");
  assert.equal(f.subscriptions.length, 1);
});

test("an in-flight old attach failure follows the replacement subscription", async () => {
  const f = fixture(true);
  const sink = { onData: () => {}, onExit: () => {} };
  const attaching = f.api.attachPty("kaan", sink);
  f.setGeneration(1);
  await f.api.capturePty("kaan", 1);
  f.failFirst("no session");
  assert.equal(await attaching, true, "an obsolete attachment failure cannot hide a live replacement");
  assert.equal(f.subscriptions.length, 2);
});
