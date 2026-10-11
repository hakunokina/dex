// #430: the Core's name + role (+ cost pill) under its body, with the plate under them on the PCB texture.
// Kept out of world.ts (which pulls in the artwork) so the drawing order is unit-testable.

export interface CoreNameSpec {
  x: number; nameY: number; roleY: number; maxW: number;
  /** Already the display label (#411: "Ana Core"). */
  name: string; role: string; cost: string | null;
  /** The plate's fill (#401: the board colour) when the PCB texture is drawn; null: no plate. */
  plate: string | null;
  nameColor: string; roleColor: string; font: string; mono: string;
}

/** The cost pill, drawn by the caller between minX and maxX on the role line. */
export type DrawPill = (text: string, minX: number, maxX: number, cy: number) => void;

/** The plate first, then the name, role and pill over it — the text never sits under the plate. */
export function drawCoreName(ctx: CanvasRenderingContext2D, s: CoreNameSpec, pill: DrawPill) {
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.font = `600 14px ${s.font}`;
  const name = ellipsis(ctx, s.name, s.maxW);
  const nameW = ctx.measureText(name).width;
  ctx.font = `600 9px ${s.mono}`;
  const roleText = s.role ? ellipsis(ctx, s.role, s.maxW * (s.cost ? 0.5 : 1)) : "";
  const roleW = roleText ? ctx.measureText(roleText).width : 0;
  ctx.font = `600 10px ${s.mono}`;
  const pillW = s.cost ? Math.min(s.maxW * 0.5, ctx.measureText(s.cost).width + 10) : 0;
  const gap = roleText && s.cost ? 6 : 0;
  const left = s.x - (roleW + gap + pillW) / 2;
  if (s.plate) {
    // #401 handoff: no text on the texture without a solid backing.
    const plateW = Math.max(nameW, roleW + gap + pillW) + 12;
    roundRect(ctx, s.x - plateW / 2, s.nameY - 10, plateW, s.roleY - s.nameY + 18, 4);
    ctx.save(); ctx.globalAlpha *= 0.85; ctx.fillStyle = s.plate; ctx.fill(); ctx.restore();
  }
  ctx.fillStyle = s.nameColor;
  ctx.font = `600 14px ${s.font}`;
  ctx.fillText(name, s.x, s.nameY);
  if (roleText) {
    ctx.globalAlpha *= 0.85;
    ctx.fillStyle = s.roleColor;
    ctx.font = `600 9px ${s.mono}`;
    ctx.textAlign = "left";
    ctx.fillText(roleText, left, s.roleY);
    ctx.globalAlpha /= 0.85;
  }
  if (s.cost) pill(s.cost, left + roleW + gap, left + roleW + gap + pillW, s.roleY);
}

export function ellipsis(ctx: CanvasRenderingContext2D, text: string, max: number) {
  if (ctx.measureText(text).width <= max) return text; let value = text;
  while (value.length > 1 && ctx.measureText(`${value}…`).width > max) value = value.slice(0, -1);
  return `${value}…`;
}

export function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  const radius = Math.min(r, w / 2, h / 2); ctx.beginPath(); ctx.moveTo(x + radius, y); ctx.lineTo(x + w - radius, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + radius); ctx.lineTo(x + w, y + h - radius); ctx.quadraticCurveTo(x + w, y + h, x + w - radius, y + h);
  ctx.lineTo(x + radius, y + h); ctx.quadraticCurveTo(x, y + h, x, y + h - radius); ctx.lineTo(x, y + radius); ctx.quadraticCurveTo(x, y, x + radius, y); ctx.closePath();
}
