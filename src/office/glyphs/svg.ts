/** One drawable part of a 24×24 role glyph: SVG path data, stroked (and filled when the source says so). */
export interface GlyphPart { d: string; fill: boolean }

const num = (attrs: Record<string, string>, key: string) => parseFloat(attrs[key] ?? "0") || 0;

/**
 * #232: the role glyphs (#229) use only path/circle/ellipse/rect; canvas has no SVG DOM, so each
 * element becomes Path2D data. Colour is applied at draw time (the files say currentColor).
 */
export function parseGlyph(svg: string): GlyphPart[] {
  const parts: GlyphPart[] = [];
  for (const [, tag, body] of svg.matchAll(/<(path|circle|ellipse|rect)\b([^>]*)\/?>/g)) {
    const attrs = Object.fromEntries([...body.matchAll(/([\w-]+)="([^"]*)"/g)].map(([, k, v]) => [k, v]));
    const d = tag === "path" ? attrs.d ?? ""
      : tag === "circle" ? ellipse(num(attrs, "cx"), num(attrs, "cy"), num(attrs, "r"), num(attrs, "r"))
      : tag === "ellipse" ? ellipse(num(attrs, "cx"), num(attrs, "cy"), num(attrs, "rx"), num(attrs, "ry"))
      : rect(num(attrs, "x"), num(attrs, "y"), num(attrs, "width"), num(attrs, "height"), num(attrs, "rx"));
    if (d) parts.push({ d, fill: attrs.fill !== undefined && attrs.fill !== "none" });
  }
  return parts;
}

function ellipse(cx: number, cy: number, rx: number, ry: number) {
  return `M${cx - rx} ${cy}a${rx} ${ry} 0 1 0 ${rx * 2} 0a${rx} ${ry} 0 1 0 ${-rx * 2} 0Z`;
}

function rect(x: number, y: number, w: number, h: number, r: number) {
  const k = Math.min(r, w / 2, h / 2);
  if (!k) return `M${x} ${y}h${w}v${h}h${-w}Z`;
  return `M${x + k} ${y}h${w - 2 * k}a${k} ${k} 0 0 1 ${k} ${k}v${h - 2 * k}a${k} ${k} 0 0 1 ${-k} ${k}` +
    `h${-(w - 2 * k)}a${k} ${k} 0 0 1 ${-k} ${-k}v${-(h - 2 * k)}a${k} ${k} 0 0 1 ${k} ${-k}Z`;
}
