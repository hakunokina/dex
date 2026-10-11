// DEX-37: canvas/xterm can't read CSS vars on their own — read the design tokens from :root.
const root = () => getComputedStyle(document.documentElement);

export const token = (name: string) => root().getPropertyValue(name).trim();

export const termFont = () => token("--term-font") || "monospace";
export const termFontSize = () => parseFloat(token("--term-size")) || 13;
export const termFontSizeSm = () => parseFloat(token("--term-size-sm")) || 11;
export const termLineHeight = () => parseFloat(token("--term-line")) || 1.15;

const ANSI = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"] as const;
const camel = (c: string) => c[0].toUpperCase() + c.slice(1);

/** xterm theme from App.css's --term-* tokens; `get` resolves one custom property (pure for tests). */
export function termThemeFrom(get: (name: string) => string) {
  const ansi: Record<string, string> = {};
  for (const c of ANSI) {
    ansi[c] = get(`--term-${c}`);
    ansi[`bright${camel(c)}`] = get(`--term-bright-${c}`);
  }
  return {
    background: get("--term-bg"),
    foreground: get("--text"),
    cursor: get("--red"),
    cursorAccent: get("--term-bg"),
    selectionBackground: get("--term-selection"),
    ...ansi,
  };
}

export const termTheme = () => termThemeFrom(token);
