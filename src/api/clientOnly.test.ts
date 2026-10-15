import { test } from "node:test";
import assert from "node:assert/strict";
import { CLIENT_ONLY, isClientOnly, isShellEvent, localMethods } from "./clientOnly.ts";
import { supports } from "./serverInfo.ts";

test("routing: the shell's own commands stay on Tauri, core calls go to the endpoint", () => {
  for (const m of ["host_token_get", "snip_start", "logs_open", "local_host", "local_start", "local_shutdown"]) assert.ok(isClientOnly(m), m);
  for (const m of ["team_list", "pty_spawn", "pty_resize", "layout_save", "core_status", "server_info", "memory_graph", "tailnet_set", "core_shutdown", "usage_get"]) assert.ok(!isClientOnly(m), m);
  assert.ok(!isShellEvent("usage-snapshot") && !isShellEvent("delegation-changed") && !isShellEvent("dexd.lagged"));
});

test("localMethods: advertised + shell commands; unknown stays unknown", () => {
  const m = localMethods(["team_list", "pty_spawn"])!;
  assert.ok(supports({ version: null, commit: null, commitTime: 1, methods: m }, "team_list"));
  assert.ok(supports({ version: null, commit: null, commitTime: 1, methods: m }, "snip_start"), "shell commands stay on");
  assert.ok(!supports({ version: null, commit: null, commitTime: 1, methods: m }, "memory_graph"), "not advertised → hidden");
  assert.equal(m.length, 2 + CLIENT_ONLY.length);
  assert.equal(localMethods(null), null);
  assert.equal(new Set(localMethods([...CLIENT_ONLY])).size, CLIENT_ONLY.length, "no duplicates");
});
