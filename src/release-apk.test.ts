import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmdirSync, unlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { deployConfig } from "../scripts/deploy-config.mjs";

const root = resolve(import.meta.dirname, "..");
const example = readFileSync(join(root, "scripts/deploy/deploy.conf.example"), "utf8");
const fingerprint = "ab".repeat(32);

function withConfig(contents: string, cert: string | undefined, run: () => void) {
  const scratch = join(root, ".dex/tmp");
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(join(scratch, "apk-config-"));
  const file = join(dir, "deploy.conf");
  writeFileSync(file, contents);
  const previous = { conf: process.env.DEX_DEPLOY_CONF, cert: process.env.DEX_APK_CERT, host: process.env.DEX_DEPLOY_HOST };
  process.env.DEX_DEPLOY_CONF = file;
  delete process.env.DEX_DEPLOY_HOST;
  if (cert === undefined) delete process.env.DEX_APK_CERT;
  else process.env.DEX_APK_CERT = cert;
  try {
    run();
  } finally {
    for (const [key, value] of [["DEX_DEPLOY_CONF", previous.conf], ["DEX_APK_CERT", previous.cert], ["DEX_DEPLOY_HOST", previous.host]] as const) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    unlinkSync(file);
    rmdirSync(dir);
  }
}

test("APK certificate is read from local deploy.conf", () => {
  withConfig(`${example}\nDEX_APK_CERT=${fingerprint}\n`, undefined, () => {
    const config = deployConfig();
    assert.equal(config.apkCert, fingerprint);
    assert.equal(config.host, "admin@vm.example.com");
    assert.doesNotMatch(config.contents, /DEX_APK_CERT/);
  });
});

test("APK certificate environment override is normalized", () => {
  withConfig(`${example}\nDEX_APK_CERT=${"cd".repeat(32)}\n`, fingerprint.toUpperCase(), () => {
    assert.equal(deployConfig().apkCert, fingerprint);
  });
});

test("deploy still accepts old config and the empty certificate placeholder", () => {
  for (const contents of [example, example.replace(/^DEX_APK_CERT=.*\r?\n/gm, "")]) {
    withConfig(contents, undefined, () => {
      assert.equal(deployConfig().apkCert || undefined, undefined);
      assert.equal(deployConfig().host, "admin@vm.example.com");
    });
  }
  withConfig(example.replace(/^DEX_APK_CERT=.*\r?\n/gm, ""), fingerprint, () => {
    assert.equal(deployConfig().apkCert, fingerprint);
  });
});

test("APK certificate rejects invalid fingerprints without echoing the value", () => {
  for (const cert of ["ab".repeat(31), "ab".repeat(33), "zz".repeat(32)]) {
    for (const env of [undefined, cert]) {
      withConfig(`${example}\nDEX_APK_CERT=${cert}\n`, env, () => {
        assert.throws(() => deployConfig(), (error: Error) => {
          assert.match(error.message, /Invalid DEX_APK_CERT/);
          assert.ok(!error.message.includes(cert));
          return true;
        });
      });
    }
  }
});

test("APK release requires a certificate before invoking external commands", () => {
  withConfig(example.replace(/^DEX_APK_CERT=.*\r?\n/gm, ""), undefined, () => {
    const result = spawnSync(process.execPath, [join(root, "scripts/release-apk.mjs")], {
      cwd: root, encoding: "utf8", env: { ...process.env, PATH: "" },
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /DEX_APK_CERT.*required/);
    assert.equal(result.stdout, "");
  });
});

test("APK release rejects invalid certificates before invoking external commands", () => {
  withConfig(example, "zz".repeat(32), () => {
    const result = spawnSync(process.execPath, [join(root, "scripts/release-apk.mjs")], {
      cwd: root, encoding: "utf8", env: { ...process.env, PATH: "" },
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Invalid DEX_APK_CERT/);
    assert.equal(result.stdout, "");
  });
});
