// #161 (C): `pnpm dexd:dev [--now] [--data-dir DIR]` — build `dexd` and have the running local core come back as
// that build: once it is drained (default: jobs wait in the queue until nothing runs) or now
// (`--now`: running jobs are cut). Either way the queue is kept and leaders reopen with --resume.
// `tauri dev` rebuilds only the app (its watcher never rebuilds dexd), so a core change lands
// through this. No core running: just the build (the app starts this one next time).
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const now = process.argv.includes("--now");
const root = resolve(import.meta.dirname, "..");
const target = process.env.CARGO_TARGET_DIR || join(root, "target");
execFileSync("cargo", ["build", "-p", "dexd", "--target-dir", target], { cwd: root, stdio: "inherit" });
const exe = join(target, "debug", `dexd${process.platform === "win32" ? ".exe" : ""}`);

// Ask the shared Rust resolver; the bundle identity may still use legacy desktop storage.
const identifier = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8")).identifier;
// `--data-dir DIR`: another core (a test's), not the app's.
const at = process.argv.indexOf("--data-dir");
const dataDir = at > 0
  ? process.argv[at + 1]
  : execFileSync(exe, ["--desktop-data-dir", identifier], { encoding: "utf8" }).trim();
const discovery = join(dataDir, "dexd.local.json");
if (!existsSync(discovery)) {
  console.log("dexd:dev: çalışan yerel çekirdek yok — derlendi; uygulama açılınca bu sürüm başlar.");
  process.exit(0);
}
const d = JSON.parse(readFileSync(discovery, "utf8"));
const token = readFileSync(d.tokenFile, "utf8").trim();

const ws = new WebSocket(d.url);
let next = 1;
const pending = new Map();
const call = (method, params = {}) =>
  new Promise((ok, fail) => {
    const id = next++;
    pending.set(id, { ok, fail });
    ws.send(JSON.stringify({ id, method, params }));
  });
ws.onmessage = (e) => {
  const m = JSON.parse(String(e.data));
  const p = pending.get(m.id);
  if (!p) return;
  pending.delete(m.id);
  m.error ? p.fail(new Error(m.error.message ?? m.error.code)) : p.ok(m.result);
};
ws.onerror = () => {
  console.error(`dexd:dev: çekirdeğe bağlanılamadı (${d.url}) — derlendi; uygulama açılınca bu sürüm başlar.`);
  process.exit(0);
};
ws.onopen = async () => {
  try {
    // `probe`: not a window — it does not hold the core's no-window grace.
    const hello = await call("auth.hello", { protocol: 1, token, probe: true });
    if (!hello.methods?.includes("core_drain_cancel")) {
      throw new Error("çalışan çekirdek bu güncellemeyi bilmiyor (C öncesi) — Ayarlar/tepsiden yeniden başlatın");
    }
    await call("core_restart", { exe, whenIdle: !now });
    console.log(
      now
        ? "dexd:dev: çekirdek şimdi yeni sürümle yeniden başlıyor."
        : "dexd:dev: çekirdek boşalınca yeni sürümle yeniden başlayacak (yeni işler kuyrukta bekler).",
    );
    process.exit(0);
  } catch (e) {
    console.error(`dexd:dev: ${e.message}`);
    process.exit(1);
  }
};
