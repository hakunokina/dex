import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { parseGlyph } from "./svg.ts";

test("parseGlyph: path, circle, ellipse and rounded rect become path data; fill is kept", () => {
  const parts = parseGlyph(
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M4 17h16"/>' +
    '<circle cx="12" cy="12" r="1" fill="currentColor"/><ellipse cx="12" cy="6" rx="7" ry="2.6"/>' +
    '<rect x="4" y="4" width="16" height="6" rx="1.5"/><rect x="1" y="1" width="2" height="2"/></svg>',
  );
  assert.deepEqual(parts.map((p) => p.fill), [false, true, false, false, false]);
  assert.equal(parts[0].d, "M4 17h16");
  assert.equal(parts[1].d, "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z");
  assert.equal(parts[2].d, "M5 6a7 2.6 0 1 0 14 0a7 2.6 0 1 0 -14 0Z");
  assert.match(parts[3].d, /^M5\.5 4h13a1\.5 1\.5 0 0 1 1\.5 1\.5v3/);
  assert.equal(parts[4].d, "M1 1h2v2h-2Z");
});

test("every catalog glyph parses to drawable parts", () => {
  const dir = new URL(".", import.meta.url);
  const files = readdirSync(dir).filter((f) => f.endsWith(".svg"));
  assert.equal(files.length, 20);
  for (const file of files) {
    const parts = parseGlyph(readFileSync(new URL(file, dir), "utf8"));
    assert.ok(parts.length > 0 && parts.every((p) => p.d.startsWith("M")), file);
  }
});
