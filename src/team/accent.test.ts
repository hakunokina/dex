import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { TEAM_ACCENTS, teamAccent } from "./accent.ts";

test("teamAccent: stable per id, a stored accent wins, unknown names fall back", () => {
  assert.equal(teamAccent({ id: "kargo-1a2b3c4d5e6f" }), teamAccent({ id: "kargo-1a2b3c4d5e6f" }));
  assert.equal(teamAccent({ id: "x", accent: "violet" }), "violet");
  assert.equal(teamAccent({ id: "x", accent: "pink" }), teamAccent({ id: "x" }));
  assert.equal(teamAccent({ id: "x", accent: null }), teamAccent({ id: "x" }));
  assert.equal(teamAccent({ id: "default" }), "red");
  assert.equal(teamAccent({ id: "default", accent: "blue" }), "blue");
});

test("teamAccent: spreads ids over the whole palette", () => {
  const seen = new Set(Array.from({ length: 200 }, (_, i) => teamAccent({ id: `team-${i}` })));
  assert.deepEqual([...seen].sort(), [...TEAM_ACCENTS].sort());
});

test("every non-default accent has its App.css block with the whole --team family", () => {
  const css = readFileSync(new URL("../App.css", import.meta.url), "utf8");
  for (const name of TEAM_ACCENTS.slice(1)) {
    const block = css.match(new RegExp(String.raw`\[data-accent="${name}"\]\s*\{([^}]*)\}`))?.[1];
    assert.ok(block, name);
    for (const v of ["--team:", "--team-hi:", "--team-soft:", "--team-line:", "--team-glow:"]) assert.ok(block.includes(v), `${name} ${v}`);
  }
});
