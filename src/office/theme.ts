// #1958: the Kart's colours, read at the canvas — its team stage's [data-accent] scope sets --team (Modern §7).
// The board itself is the lit membrane (symbioteCard LOOK); these are the status, label and Core tokens.
export interface OfficeTheme {
  text: string; text2: string; muted: string; faint: string;
  red: string; redHi: string; redGlow: string; green: string; amber: string;
  /** The team accent: active strands, the Core's name and the focus ring. Red stays for status and errors. */
  team: string; teamHi: string; teamGlow: string;
  /** Plates under badges and marks. */
  surface: string;
  /** Core's unchanged inputs: its rim and the board colour its drawing blends into. */
  edge: string; board: string;
  font: string; mono: string;
}

export function readOfficeTheme(el: Element = document.documentElement): OfficeTheme {
  const css = getComputedStyle(el);
  const token = (name: string) => css.getPropertyValue(name).trim();
  return {
    text: token("--text"), text2: token("--text-2"), muted: token("--muted"), faint: token("--faint"),
    red: token("--red"), redHi: token("--red-hi"), redGlow: token("--red-glow"), green: token("--green"), amber: token("--amber"),
    team: token("--team"), teamHi: token("--team-hi"), teamGlow: token("--team-glow"),
    surface: token("--surface-1"), edge: token("--pcb-chip-edge"), board: token("--pcb-board"),
    font: token("--font-ui"), mono: token("--font-mono"),
  };
}
