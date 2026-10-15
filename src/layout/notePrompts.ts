// #2040: the team note's leader appends and Dex's built-in prompt parts — wire types (dexd
// `leader_prompt_*`, `prompt_part*`) and the pure rules the notes UI shows. No DOM, unit-tested.

export interface LeaderPrompt {
  teamId: string;
  /** Appended to the leader's system prompt; applies on its next start. */
  systemAppend: string;
  /** Added to every patron message the leader gets; applies at once. */
  messageAppend: string;
  updatedAt: number;
  /** What the leader runs differs from the current appends/overrides: a restart applies it. */
  pendingRestart: boolean;
  appliedAt: number;
}

export type AppendField = "systemAppend" | "messageAppend";

export interface PromptPart {
  id: string;
  title: string;
  audience: "all" | "leader" | "worker" | "role";
  role?: string;
  channel: "system" | "session-start";
  editable: boolean;
  defaultText: string;
  /** In force: the override, else the default. */
  text: string;
  overridden: boolean;
  updatedAt: number;
  note?: string;
}

export interface PromptParts {
  parts: PromptPart[];
  policyNote: string;
}

export const hasText = (s: string | null | undefined) => !!s && s.trim() !== "";

/** A notes tab is marked when it holds something: the body, and for the team note its appends too. */
export const noteFilled = (body: string | null | undefined, lp?: Pick<LeaderPrompt, AppendField> | null) =>
  hasText(body) || hasText(lp?.systemAppend) || hasText(lp?.messageAppend);

export function audienceLabel(p: Pick<PromptPart, "audience" | "role">): string {
  switch (p.audience) {
    case "all": return "Herkes";
    case "leader": return "Lider";
    case "worker": return "Worker";
    case "role": return p.role ? `Rol: ${p.role}` : "Rol";
  }
}

export const channelLabel = (c: PromptPart["channel"]) => (c === "system" ? "sistem istemi" : "oturum başı");

/** Replaces one part in the list (a save, a reset or `prompt-parts-changed`); unknown ids are ignored. */
export const replacePart = (parts: PromptPart[], next: PromptPart) => parts.map(p => (p.id === next.id ? next : p));
