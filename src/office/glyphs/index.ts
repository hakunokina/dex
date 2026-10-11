import { parseGlyph, type GlyphPart } from "./svg";

const files = import.meta.glob<string>("./*.svg", { query: "?raw", import: "default", eager: true });

/** Role catalog slug → glyph parts (#229 set). Roles outside the catalog have none (patron: no fallback glyph). */
export const GLYPHS = new Map<string, GlyphPart[]>(
  Object.entries(files).map(([path, svg]) => [path.slice(2, -4), parseGlyph(svg)]),
);
