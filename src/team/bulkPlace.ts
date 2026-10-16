// #2134: "Tüm worker'lar" popover'ının pencere içindeki yeri. Düğmeye sağdan yaslanır ama
// pencere kenarından taşmaz; aşağıda yer yoksa yukarı açılır, iki yönde de sığmazsa yüksekliği
// büyük olan tarafa sınırlanır (içerik kaydırılır).
export interface Box { left: number; top: number; right: number; bottom: number }

export interface BulkSpot {
  left: number;
  top: number;
  maxHeight: number;
  above: boolean;
}

const EDGE = 8; // pencere kenarı boşluğu
const GAP = 6; // düğme ile popover arası

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(lo, v), Math.max(lo, hi));

export function bulkSpot(anchor: Box, pop: { w: number; h: number }, view: { w: number; h: number }): BulkSpot {
  const left = clamp(anchor.right - pop.w, EDGE, view.w - pop.w - EDGE);
  const belowTop = anchor.bottom + GAP;
  const belowSpace = Math.max(0, view.h - EDGE - belowTop);
  const aboveSpace = Math.max(0, anchor.top - GAP - EDGE);
  if (pop.h <= belowSpace || belowSpace >= aboveSpace) {
    return { left, top: belowTop, maxHeight: belowSpace, above: false };
  }
  const h = Math.min(pop.h, aboveSpace);
  return { left, top: anchor.top - GAP - h, maxHeight: aboveSpace, above: true };
}
