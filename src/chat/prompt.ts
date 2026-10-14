// Waiting agent → structured prompt, parsed from the PTY screen text (`chat_screen`). Pure.
// Claude draws numbered select menus ("❯ 1. Yes") and y/n questions.

export interface PromptOption {
  label: string;
  /** Keystrokes that answer with this option. */
  keys: string;
}

export interface PendingPrompt {
  question: string;
  options: PromptOption[];
  kind: "choice" | "yesno" | "text";
}

const BOX = /[│┃║╭╮╰╯─━┌┐└┘├┤]/g;
const MARK = "❯›▸►>*";
const OPTION = new RegExp(`^\\s*([${MARK}])?\\s*(\\d{1,2})[.)]\\s+(.+?)\\s*$`);
const YESNO = /\(\s*y\s*\/\s*n\s*\)|\[\s*y\s*\/\s*n\s*\]|\[Y\/n\]|\[y\/N\]/i;

function clean(screen: string): string[] {
  return screen
    .split("\n")
    .map((l) => l.replace(BOX, " ").replace(/\s+$/, ""))
    .filter((l) => l.trim() !== "");
}

/** The last numbered run that starts at 1 and counts up — the live menu, not older output. */
function lastMenu(lines: string[]): { start: number; items: { n: number; label: string; marked: boolean }[] } | null {
  let best: ReturnType<typeof lastMenu> = null;
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(OPTION);
    if (!m || Number(m[2]) !== 1) continue;
    const items = [{ n: 1, label: m[3], marked: !!m[1] && m[1] !== "*" }];
    for (let j = i + 1; j < lines.length; j++) {
      const o = lines[j].match(OPTION);
      if (o && Number(o[2]) === items.length + 1) items.push({ n: items.length + 1, label: o[3], marked: !!o[1] && o[1] !== "*" });
      else if (/^\s{2,}\S/.test(lines[j]) && !o) continue; // wrapped label continuation
      else break;
    }
    if (items.length >= 2) best = { start: i, items };
  }
  return best;
}

export function parsePrompt(screen: string | null | undefined): PendingPrompt | null {
  if (!screen || !screen.trim()) return null;
  const lines = clean(screen);
  const menu = lastMenu(lines);
  if (menu) {
    const question = lines.slice(Math.max(0, menu.start - 6), menu.start).join("\n").trim();
    // A highlighted "❯ n." means a select list: the digit picks it on the spot. Without a marker
    // it is a text prompt, so confirm with Enter.
    const select = menu.items.some((o) => o.marked);
    return {
      question,
      kind: "choice",
      options: menu.items.slice(0, 8).map((o) => ({ label: o.label, keys: select ? String(o.n) : `${o.n}\r` })),
    };
  }
  const tail = lines.slice(-6);
  const q = [...tail].reverse().find((l) => YESNO.test(l));
  if (q) {
    return {
      question: tail.join("\n").trim(),
      kind: "yesno",
      options: [
        { label: "Evet", keys: "y\r" },
        { label: "Hayır", keys: "n\r" },
      ],
    };
  }
  return { question: lines.slice(-8).join("\n").trim(), kind: "text", options: [] };
}

/** Short label for a button; the full text stays in the title. */
export function shortLabel(s: string, max = 28): string {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}
