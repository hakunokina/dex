import type { PcbTheme } from "./world";

/** Read at the canvas: its team stage's [data-accent] scope sets --team (Modern §7). */
export function readPcbTheme(el: Element = document.documentElement): PcbTheme {
  const css = getComputedStyle(el);
  const token = (name: string) => css.getPropertyValue(name).trim();
  return {
    outside: token("--pcb-outside"), board: token("--pcb-board"), boardEdge: token("--pcb-board-edge"),
    grid: token("--pcb-grid"), trace: token("--pcb-trace"), traceHot: token("--pcb-trace-hot"),
    chip: token("--pcb-chip"), chipOff: token("--pcb-chip-off"), chipEdge: token("--pcb-chip-edge"),
    pin: token("--pcb-pin"), via: token("--pcb-via"), text: token("--text"), muted: token("--muted"),
    faint: token("--faint"), red: token("--red"), redHi: token("--red-hi"), redGlow: token("--red-glow"),
    team: token("--team"), teamHi: token("--team-hi"), teamGlow: token("--team-glow"),
    amber: token("--amber"), green: token("--green"), provider: token("--pcb-provider"),
    font: token("--font-ui"), mono: token("--font-mono"), radius: parseFloat(token("--pcb-radius")) || 12,
  };
}
