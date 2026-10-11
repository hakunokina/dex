// Modern §7: each team's accent (one `--team` family, App.css `[data-accent]`). The team record carries
// none yet, so it is derived from the id: stable across restarts and devices, no storage.

/** Order = App.css `[data-accent]` blocks. Red first: the Dex default. */
export const TEAM_ACCENTS = ["red", "blue", "green", "violet", "cyan"] as const;
export type TeamAccent = (typeof TEAM_ACCENTS)[number];

const isAccent = (v: unknown): v is TeamAccent => TEAM_ACCENTS.includes(v as TeamAccent);

/** FNV-1a (32 bit) of the id → palette slot; a stored `accent` wins when it names one. */
export function teamAccent(team: { id: string; accent?: string | null }): TeamAccent {
  if (isAccent(team.accent)) return team.accent;
  if (team.id === "default") return "red"; // #311: the main team is Dex red (its hash gave cyan)
  let h = 0x811c9dc5;
  for (let i = 0; i < team.id.length; i++) {
    h ^= team.id.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return TEAM_ACCENTS[(h >>> 0) % TEAM_ACCENTS.length];
}
