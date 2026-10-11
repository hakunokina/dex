import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Modern §5: blur (backdrop-filter) only on fixed layers — never on scrolling content (cost), and never on
// the usage strip itself (it would become its fixed hover card's containing block: zemin ::before'da).
const ALLOWED = [".usage-strip::before"];

function cssFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? cssFiles(join(dir, e.name)) : e.name.endsWith(".css") ? [join(dir, e.name)] : []);
}

test("backdrop-filter appears only on the allowed fixed layers", () => {
  const root = fileURLToPath(new URL(".", import.meta.url));
  const found: string[] = [];
  for (const file of cssFiles(root)) {
    const css = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      if (/(^|[;\s])(-webkit-)?backdrop-filter\s*:/.test(m[2])) found.push(m[1].trim());
    }
  }
  assert.deepEqual(found.filter((sel) => !ALLOWED.includes(sel)), []);
});
