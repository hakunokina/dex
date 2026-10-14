// Merge öncesi kontrol kapısı: `pnpm gate`. Tüm adımları sırayla koşar (ilk hatada durmaz),
// sonda özet tablo basar; herhangi bir adım kaldıysa çıkış kodu 1. E2E bilerek dışarıda (QA işi).
import { spawnSync } from "node:child_process";

// Repo'nun varsayılan dalı `master` (`main` yok); worktree dalı başka tabana karşı ölçülecekse argüman ver.
const BASE = process.argv[2] ?? "master";

const STEPS = [
  ["tsc", "npx tsc --noEmit"],
  ["unit", "pnpm test"],
  ["core test", "cargo test -p dex-core --lib"],
  ["core clippy", "cargo clippy -p dex-core --lib -- -D warnings"],
  // #150: the shell no longer links dexd — its tests and the `inproc-core` way back on their own.
  // Whole crate (not --lib): tests/local_process.rs drives the real `dexd --local` the shell starts.
  ["dexd test", "cargo test -p dexd"],
  ["check dexd+dex", "cargo check -p dexd -p dex"],
  ["check dex inproc", "cargo check -p dex --features inproc-core"],
  // Android: NDK gerekmez (yalnız rustup hedefi); masaüstü-only modülün #[cfg(desktop)] kapısı
  // eksikse burada E0432/E0433 ile düşer — masaüstü derleme bunu yakalamaz.
  ["check android", "cargo check --target aarch64-linux-android -p dex"],
];

const results = [];
for (const [name, cmd] of STEPS) {
  console.log(`\n=== [gate] ${name}: ${cmd}`);
  const t0 = Date.now();
  const r = spawnSync(cmd, { shell: true, stdio: "inherit" });
  const ok = r.status === 0;
  results.push({ name, ok, secs: (Date.now() - t0) / 1000, note: ok ? "" : r.error?.message ?? `exit ${r.status}` });
}

// Merge'e giren değişiklik: BASE ile ortak atadan HEAD'e (commit'lenmemiş iş sayılmaz).
const diff = spawnSync(`git diff --name-only ${BASE}...HEAD`, { shell: true, encoding: "utf8" });
const changed = diff.status === 0 ? diff.stdout.split(/\r?\n/).filter(Boolean) : null;

const notes = [];
if (changed === null) {
  notes.push(`! git diff ${BASE}...HEAD koşmadı: ${diff.stderr.trim()}`);
} else {
  const policy = changed.filter((f) => /^crates\/dex-core\/src\/(permit|hooks|pty)\.rs$/.test(f));
  if (policy.length) {
    notes.push(
      [
        "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
        "!!  POLİTİKA DOSYASI DEĞİŞTİ — PATRON ONAYI GEREKİR        !!",
        "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
        ...policy.map((f) => `   - ${f}`),
      ].join("\n"),
    );
  }
  if (changed.some((f) => f === "package.json" || f === "pnpm-lock.yaml")) {
    notes.push("* package.json/pnpm-lock değişti → ana klasörde `pnpm install` gerekir.");
  }
  if (changed.some((f) => f.endsWith(".rs") || /(^|\/)Cargo\.(toml|lock)$/.test(f))) {
    notes.push("* Rust değişti → merge Dex'i yeniden başlatır + kullanılan uzak dexd sunucusunun güncellenmesi gerekir.");
  }
  if (!notes.length) notes.push(`* ${BASE}...HEAD: ${changed.length} dosya; özel not yok.`);
}

console.log("\n=== [gate] özet");
const w = Math.max(...results.map((r) => r.name.length));
for (const r of results) {
  console.log(`${r.ok ? "GEÇTİ" : "KALDI"}  ${r.name.padEnd(w)}  ${r.secs.toFixed(1).padStart(6)} sn  ${r.note}`);
}
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} geçti${failed ? `, ${failed} kaldı` : ""}.`);
console.log(notes.join("\n"));
process.exit(failed ? 1 : 0);
