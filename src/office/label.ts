// #411: the Kart's team words — the leader's label ("Ana Core") and the corner signature. Display only:
// the leader's real name stays as it is.

/** Longest team word put before the leader's name; longer ones are cut (the canvas still ellipsizes). */
export const PREFIX_MAX = 12;
/** Longest signature before the cut. */
export const SIGNATURE_MAX = 28;

const cut = (text: string, max: number) => ([...text].length > max ? `${[...text].slice(0, max - 1).join("")}…` : text);

/** "Ana Takım" + "Core" → "Ana Core"; no team word (or the same word) → the name alone. */
export function coreLabel(teamName: string | null | undefined, leaderName: string): string {
  const word = (teamName ?? "").trim().split(/\s+/)[0] ?? "";
  if (!word || word.toLocaleLowerCase("tr-TR") === leaderName.trim().toLocaleLowerCase("tr-TR")) return leaderName;
  return `${cut(word, PREFIX_MAX)} ${leaderName}`;
}

/** The team name as the board's signature: whitespace folded, cut when long; "" = none. */
export function teamSignature(teamName: string | null | undefined): string {
  return cut((teamName ?? "").trim().replace(/\s+/g, " "), SIGNATURE_MAX);
}

/** #451: an offline Core's name stays in the team colour, only dimmed — still ≥4.5:1 (red --team-hi on the off chip ≈4.8:1). */
export const OFFLINE_NAME_ALPHA = 0.88;

/** The chip name's colour by kind alone; alpha replaces the chip's own (null: keep it). */
export function chipNameStyle(leader: boolean, offline: boolean): { color: "teamHi" | "text"; alpha: number | null } {
  if (!leader) return { color: "text", alpha: null };
  return { color: "teamHi", alpha: offline ? OFFLINE_NAME_ALPHA : null };
}
