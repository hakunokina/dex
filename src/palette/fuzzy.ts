// Palette matching: Turkish-aware normalization + word-anchored subsequence score.
const norm = (s: string) =>
  s
    .toLocaleLowerCase("tr")
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/ı/g, "i")
    .replace(/['’]/g, "");

const isStart = (t: string, i: number) => i === 0 || /[\s:·/-]/.test(t[i - 1]);

/**
 * Higher is better; null = no match. Each query letter must continue the previous match or start a
 * word ("gpan" → "Görev panosunu", "odak ada" → "Odaklan: Ada"), so scattered letters don't match.
 */
export function fuzzyScore(query: string, text: string): number | null {
  const q = norm(query.trim());
  if (!q) return 0;
  const t = norm(text);
  const at = t.indexOf(q);
  if (at >= 0) return 1000 - at - (isStart(t, at) ? 0 : 50);

  let score = 0;
  let prev = -2;
  for (const ch of q) {
    if (ch === " ") continue;
    let i = t.indexOf(ch, prev + 1);
    while (i >= 0 && i !== prev + 1 && !isStart(t, i)) i = t.indexOf(ch, i + 1);
    if (i < 0) return null;
    score += i === prev + 1 ? 15 : 10;
    prev = i;
  }
  return score - prev * 0.5;
}
