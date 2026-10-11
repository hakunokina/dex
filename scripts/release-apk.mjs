// #215 `pnpm release:apk`: yerel HEAD'den Android APK. Keystore ve NDK yalnız derleme sunucusunda (deploy.conf DEX_DEPLOY_HOST):
// git archive → sunucu ~/dex-build-<sha> (temiz) → pnpm install + ~/bin/build-apk → imza
// parmak izi = release anahtarı → .dex/release/android/<sha>.apk → `adb install -r` (cihaz yoksa atlar).
// Commit'lenmemiş değişiklikler APK'ya girmez (HEAD arşivlenir).
// com.hakunokina.dex ayrı paket: eski paketin APK/verisi kalır; yeni uygulamada bir kez eşleyin.
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { deployConfig } from "./deploy-config.mjs";

// Güncelleme APK'sı aynı release anahtarıyla imzalanmalı; beklenen SHA-256 yerel ayardan gelir.
const { host, apkCert: CERT } = deployConfig();
if (!CERT) throw new Error("DEX_APK_CERT is required: set it in local deploy.conf or the environment");
const root = resolve(import.meta.dirname, "..");
const git = (...a) => execFileSync("git", a, { cwd: root, encoding: "utf8" }).trim();
const ssh = (script, opts = {}) =>
  execFileSync("ssh", [host, "bash -s"], { input: script, encoding: "utf8", stdio: ["pipe", "pipe", "inherit"], ...opts });

const sha = git("rev-parse", "--short=10", "HEAD");
const time = git("log", "-1", "--format=%ct");
if (git("status", "--porcelain")) console.warn(`uyarı: commit'lenmemiş değişiklikler var; APK HEAD'den (${sha})`);
const dir = `dex-build-${sha}`;

console.log(`release:apk ${sha} → ${host}:~/${dir}`);
const busy = ssh(`pgrep -fa "build-apk|tauri android" | grep -v pgrep || true`).trim();
if (busy) throw new Error(`${host}'de başka bir APK derlemesi sürüyor:\n${busy}`);
execFileSync("ssh", [host, `rm -rf ~/${dir} && mkdir -p ~/${dir} && tar -x -C ~/${dir}`], {
  input: execFileSync("git", ["archive", "--format=tar", "HEAD"], { cwd: root, maxBuffer: 1 << 30 }),
  stdio: ["pipe", "inherit", "inherit"],
});

// .git yok: commit damgası DEX_BUILD_COMMIT ile (crates/dex-core/build_commit.rs).
// build-apk hep ~/apk/dex-<sürüm>.apk'ya yazar; sha'lı kopyası alınır.
const ver = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
const apk = `apk/dex-${ver}-${sha}.apk`; // ssh kullanıcısının evine göre
const remote = `set -euo pipefail
cd ~/${dir}
export DEX_BUILD_COMMIT=${sha} DEX_BUILD_COMMIT_TIME=${time}
pnpm install --frozen-lockfile
DEX_SRC=$PWD ~/bin/build-apk
cp ~/apk/dex-${ver}.apk ~/${apk}
`;
const build = spawnSync("ssh", [host, `set -o pipefail; bash -s 2>&1 | tee ~/apk/build-${sha}.log`], {
  input: remote,
  stdio: ["pipe", "inherit", "inherit"],
});
if (build.status !== 0) throw new Error(`build-apk başarısız; log ${host}:~/apk/build-${sha}.log, dizin ~/${dir} incelemeye bırakıldı`);

const v = ssh(`~/android-sdk/build-tools/34.0.0/apksigner verify --print-certs ~/${apk}; sha256sum ~/${apk}`);
const cert = /Signer #1 certificate SHA-256 digest: (\w+)/.exec(v)?.[1]?.toLowerCase();
if (cert !== CERT) throw new Error(`imza release anahtarı değil: ${cert ?? "yok"}\n${v}`);
const remoteSum = /^(\w{64})\s/m.exec(v)?.[1];

const outDir = join(root, ".dex", "release", "android");
mkdirSync(outDir, { recursive: true });
const out = join(outDir, `${sha}.apk`);
execFileSync("scp", ["-q", `${host}:${apk}`, out], { stdio: "inherit" });
const localSum = createHash("sha256").update(readFileSync(out)).digest("hex");
if (localSum !== remoteSum) throw new Error(`indirilen APK bozuk: ${localSum} ≠ ${remoteSum}`);
console.log(`APK: ${out} (imza ${CERT.slice(0, 8)}…, sha256 ${localSum.slice(0, 12)}…)`);
ssh(`rm -rf ~/${dir}`);

const sdkAdb = join(process.env.LOCALAPPDATA || "", "Android", "Sdk", "platform-tools", "adb.exe");
const adb = process.env.ADB || (existsSync(sdkAdb) ? sdkAdb : "adb");
const devices = (() => {
  try {
    return execFileSync(adb, ["devices"], { encoding: "utf8" })
      .split("\n")
      .map((l) => l.trim().split(/\s+/))
      .filter(([s, st]) => s && st === "device");
  } catch {
    return [];
  }
})();
// Kablosuz adb aynı telefonu mDNS adıyla da listeler; seri numarası olanı seç.
const serial = (devices.find(([s]) => !s.includes("._adb-tls")) ?? devices[0])?.[0];
if (!serial) {
  console.log("adb: bağlı cihaz yok — kurulum atlandı (telefonda kablosuz hata ayıklamayı açıp tekrar: adb install -r)");
} else {
  execFileSync(adb, ["-s", serial, "install", "-r", out], { stdio: "inherit" });
  console.log(`kuruldu: ${serial}`);
}
