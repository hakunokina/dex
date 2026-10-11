// #411: the red circuit trace from a Kart chip to the pane it opened — pure geometry (viewport px).

export interface Pt { x: number; y: number }
export interface Box { left: number; top: number; width: number; height: number }

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/**
 * Right-angled route from the chip's edge to the nearest point of the target's edge: out along the
 * longer axis, one jog halfway, in. [] = the chip is inside the target (nothing to cross: pulse only).
 */
export function circuitPath(from: Box, to: Box): Pt[] {
  const c = { x: from.left + from.width / 2, y: from.top + from.height / 2 };
  const end = {
    x: clamp(c.x, to.left, to.left + to.width),
    y: clamp(c.y, to.top, to.top + to.height),
  };
  const dx = end.x - c.x;
  const dy = end.y - c.y;
  if (!dx && !dy) return [];
  const across = Math.abs(dx) >= Math.abs(dy);
  const start = across
    ? { x: c.x + Math.sign(dx) * from.width / 2, y: c.y }
    : { x: c.x, y: c.y + Math.sign(dy) * from.height / 2 };
  // The target edge is inside the chip: a straight hop from its centre.
  if (across ? Math.sign(end.x - start.x) !== Math.sign(dx) : Math.sign(end.y - start.y) !== Math.sign(dy)) return [c, end];
  const mid = across ? (start.x + end.x) / 2 : (start.y + end.y) / 2;
  const points = across
    ? [start, { x: mid, y: start.y }, { x: mid, y: end.y }, end]
    : [start, { x: start.x, y: mid }, { x: end.x, y: mid }, end];
  // Aligned: the jog folds flat — drop repeated and straight-through corners.
  const out: Pt[] = [];
  for (const p of points) {
    const a = out[out.length - 2];
    const b = out[out.length - 1];
    if (b && b.x === p.x && b.y === p.y) continue;
    if (a && b && ((a.x === b.x && b.x === p.x) || (a.y === b.y && b.y === p.y))) out.pop();
    out.push(p);
  }
  return out;
}

/** Sum of the segment lengths (the dash animation's length). */
export function pathLength(points: Pt[]): number {
  let length = 0;
  for (let i = 1; i < points.length; i++) length += Math.hypot(points[i].x - points[i - 1].x, points[i].y - points[i - 1].y);
  return length;
}
