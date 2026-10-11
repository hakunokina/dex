import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";

const keys = ["DEX_DEPLOY_HOST", "DEPLOY_USER", "DEPLOY_GROUP", "USER_HOME", "DEPLOY_HOME", "REPO", "SRC", "BIN", "DATA", "SERVICE", "PORT", "HEALTH_TIMEOUT", "KEEP_LOGS"];

for (const scenario of ["plain", "override", "invalid"]) {
  test(`Node installer transfers validated config: ${scenario}`, async (t) => {
    const saved = { ...process.env };
    const scratch = resolve(import.meta.dirname, "../.dex/tmp");
    mkdirSync(scratch, { recursive: true });
    const dir = mkdtempSync(resolve(scratch, "node-install-test-"));
    t.after(() => { process.env = saved; rmSync(dir, { recursive: true, force: true }); });
    for (const key of keys) delete process.env[key];
    const source = readFileSync(new URL("./deploy/deploy.conf.example", import.meta.url), "utf8");
    process.env.DEX_DEPLOY_CONF = resolve(dir, "deploy.conf");
    writeFileSync(process.env.DEX_DEPLOY_CONF, source);
    const expected = Object.fromEntries(source.split(/\r?\n/).filter((line) => line && !line.startsWith("#")).map((line) => line.split("=")));
    if (scenario === "override") {
      Object.assign(process.env, { DEX_DEPLOY_HOST: "admin@other.example.com", DEPLOY_HOME: "/srv/resolved", BIN: "/srv/bin/dexd", DATA: "/srv/data", SERVICE: "resolved.service" });
      for (const key of keys) if (process.env[key]) expected[key] = process.env[key];
    }
    if (scenario === "invalid") process.env.DEPLOY_HOME = "/srv/../bad";
    const calls = [];
    t.mock.module("node:child_process", { namedExports: { execFileSync: (...args) => { calls.push(args); return Buffer.alloc(0); } } });
    const run = () => import(`./deploy-install.mjs?${scenario}`);
    if (scenario === "invalid") {
      await assert.rejects(run(), /Invalid DEPLOY_HOME/);
      assert.equal(calls.length, 0);
      return;
    }
    await run();
    assert.deepEqual(calls.map(([command]) => command), ["tar", "ssh"]);
    assert.equal(calls[1][1][0], expected.DEX_DEPLOY_HOST);
    const match = /printf '%s' '([A-Za-z0-9+/=]+)' \| base64 -d/.exec(calls[1][1][1]);
    assert.ok(match, "selected config is included in SSH installer command");
    const installed = Object.fromEntries(Buffer.from(match[1], "base64").toString("utf8").split(/\r?\n/).filter((line) => line && !line.startsWith("#")).map((line) => line.split("=")));
    assert.deepEqual(installed, expected);
  });
}
