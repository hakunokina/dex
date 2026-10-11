import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { termThemeFrom } from "./theme.ts";

// App.css's first :root block, with var() resolved the way getComputedStyle would.
const css = readFileSync(new URL("./App.css", import.meta.url), "utf8");
const rootBlock = css.slice(css.indexOf(":root {"), css.indexOf("\n}", css.indexOf(":root {")));
const vars = new Map([...rootBlock.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
const resolve = (name: string): string => {
  const v = vars.get(name) ?? "";
  return v.replace(/var\((--[\w-]+)\)/g, (_, n: string) => resolve(n));
};

test("terminal theme: every xterm color comes from a defined App.css token", () => {
  const t = termThemeFrom(resolve);
  assert.equal(Object.keys(t).length, 5 + 16);
  for (const [k, v] of Object.entries(t)) assert.match(v, /^(#[0-9a-f]{6}|rgba?\([^)]+\))$/i, `${k} = "${v}"`);
});

test("terminal theme: state colors match the design tokens (mock §4)", () => {
  const t = termThemeFrom(resolve);
  assert.equal(t.green, resolve("--green")); // success
  assert.equal(t.brightRed, resolve("--red-hi")); // error
  assert.equal(t.yellow, resolve("--amber")); // warning
  assert.equal(t.brightBlue, "#8ab6ff"); // path / link
  assert.equal(t.brightBlack, resolve("--muted")); // dim echo
  assert.equal(t.background, resolve("--surface-1"));
});
