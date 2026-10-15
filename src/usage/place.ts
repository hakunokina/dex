// #2034: kullanım şeridinin konumu (bu cihazın tercihi) ve hover kartının yeri. Sağ/sol: dikey şerit
// sahnenin kenarında, kart şeridin yanında, halkaya ortalı. Üst: başlıkta yatay şerit, kart halkanın
// altında, yatayda halkaya ortalı. Kart her durumda pencere içinde kalır.

export type StripPlace = "right" | "left" | "top";

export const STRIP_PLACES: readonly StripPlace[] = ["right", "left", "top"];

/** Saklanan değer; bilinmeyen/boş → sağ (bugünkü düzen). */
export const parsePlace = (v: string | null): StripPlace =>
  STRIP_PLACES.includes(v as StripPlace) ? (v as StripPlace) : "right";

export interface Box { left: number; top: number; right: number; bottom: number }

export interface CardSpot {
  left: number;
  top: number;
  /** Okun kartın hangi kenarında olduğu ve o kenar boyunca konumu (px). */
  arrow: "right" | "left" | "top";
  arrowAt: number;
  maxHeight: number;
}

const EDGE = 8; // pencere kenarı boşluğu
const GAP = 8; // şerit ile kart arası
const ARROW_MIN = 14; // ok kart köşesine girmesin

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(lo, v), Math.max(lo, hi));

export function cardSpot(place: StripPlace, anchor: Box, strip: Box, card: { w: number; h: number }, view: { w: number; h: number }): CardSpot {
  if (place === "top") {
    const top = strip.bottom + GAP;
    const mid = (anchor.left + anchor.right) / 2;
    const left = clamp(mid - card.w / 2, EDGE, view.w - card.w - EDGE);
    return { left, top, arrow: "top", arrowAt: clamp(mid - left, ARROW_MIN, card.w - ARROW_MIN), maxHeight: Math.max(0, view.h - top - EDGE) };
  }
  const mid = (anchor.top + anchor.bottom) / 2;
  const h = Math.min(card.h, view.h - 2 * EDGE);
  const top = clamp(mid - h / 2, EDGE, view.h - h - EDGE);
  const left = place === "right"
    ? Math.max(EDGE, strip.left - GAP - card.w)
    : Math.min(strip.right + GAP, Math.max(EDGE, view.w - card.w - EDGE));
  return { left, top, arrow: place === "right" ? "right" : "left", arrowAt: clamp(mid - top, ARROW_MIN, h - ARROW_MIN), maxHeight: view.h - 2 * EDGE };
}
