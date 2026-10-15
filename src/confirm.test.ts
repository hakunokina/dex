// #2051: Tauri'de window.confirm bir Promise döndürür (dialog eklentisi) — `if (!confirm(..)) return`
// hiç durmaz, onaysız oturum kapatır / süreç öldürür. Masaüstü kodu askConfirm'ü await eder.
import { strict as assert } from "node:assert";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

const root = new URL(".", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const path = join(dir, e.name);
    if (e.isDirectory()) return e.name === "mobile" ? [] : sources(path); // Android: no plugin override
    return /\.tsx?$/.test(e.name) && !/\.test\.tsx?$/.test(e.name) && e.name !== "confirm.ts" ? [path] : [];
  });
}

describe("onay soruları", () => {
  it("masaüstü kodunda çıplak confirm() yok; askConfirm her yerde await edilir", () => {
    const raw: string[] = [];
    const unawaited: string[] = [];
    for (const file of sources(root)) {
      readFileSync(file, "utf8").split("\n").forEach((line, i) => {
        const at = `${file.slice(root.length)}:${i + 1}`;
        if (/^\s*(\/\/|\*)/.test(line)) return; // comments
        if (/(^|[^\w.])(window\.)?confirm\(/.test(line)) raw.push(at);
        if (/askConfirm\(/.test(line) && !/import/.test(line) && !/(await|void) askConfirm\(/.test(line)) unawaited.push(at);
      });
    }
    assert.deepEqual(raw, []);
    assert.deepEqual(unawaited, []);
  });
});
