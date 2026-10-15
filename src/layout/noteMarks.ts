// #2040: what the notes tabs and the team note's append fields read — each note's body and each
// team's leader appends, by qualified id, kept fresh from `notes-changed` / `leader-prompt-changed`
// and fetched again when a host comes back. Light (no views): the dock tabs import it.
import { useEffect, useSyncExternalStore } from "react";
import { callHost, hostSupports, onEvent, onHostUp, qid, splitId } from "../api/hosts";
import { kept, latest } from "../hot";
import type { LeaderPrompt } from "./notePrompts";

interface NoteMsg { scope: string; body: string }

const hot = import.meta.hot;
const state = kept(hot, "noteMarks", () => ({
  notes: new Map<string, string>(),
  prompts: new Map<string, LeaderPrompt>(),
  /** Asked for once per page load (and again on a reconnect), so mounting a tab never re-fetches. */
  asked: new Set<string>(),
  subs: new Set<() => void>(),
}));

const changed = () => state.subs.forEach(f => f());
const subscribe = (f: () => void) => {
  state.subs.add(f);
  return () => void state.subs.delete(f);
};

export function setNoteBody(scope: string, body: string) {
  if (state.notes.get(scope) === body) return;
  state.notes.set(scope, body);
  changed();
}

/** Newer wins: an older echo (own save racing a remote write) never rolls the fields back. */
export function setLeaderPrompt(teamId: string, lp: LeaderPrompt) {
  const old = state.prompts.get(teamId);
  if (old && old.updatedAt > lp.updatedAt) return;
  state.prompts.set(teamId, lp);
  changed();
}

const onNote = latest(hot, "noteMarks:note", ({ payload: n }: { payload: NoteMsg }, host: string) => setNoteBody(qid(host, n.scope), n.body));
const onPrompt = latest(hot, "noteMarks:prompt", ({ payload: p }: { payload: LeaderPrompt }, host: string) => setLeaderPrompt(qid(host, p.teamId), p));
const onUp = latest(hot, "noteMarks:up", (host: string) => {
  for (const key of [...state.asked]) if (splitId(key.slice(key.indexOf("|") + 1))[0] === host) {
    state.asked.delete(key);
    fetchKey(key);
  }
});
kept(hot, "noteMarks:listening", () => [
  onEvent<NoteMsg>("notes-changed", onNote),
  onEvent<LeaderPrompt>("leader-prompt-changed", onPrompt),
  onHostUp(onUp),
]);

/** `note|<scope>` or `lp|<teamId>` (qualified). */
function fetchKey(key: string) {
  if (state.asked.has(key)) return;
  state.asked.add(key);
  const [kind, id] = [key.slice(0, key.indexOf("|")), key.slice(key.indexOf("|") + 1)];
  const [host, bare] = splitId(id);
  const forget = () => void state.asked.delete(key);
  if (kind === "note") {
    callHost<NoteMsg>(host, "note_get", { scope: bare }).then(n => setNoteBody(id, n.body), forget);
  } else if (hostSupports(host, "leader_prompt_get")) {
    callHost<LeaderPrompt>(host, "leader_prompt_get", { teamId: bare }).then(p => setLeaderPrompt(id, p), forget);
  } else forget();
}

/** The note's body (`""` until read); `scope` is "shared" or a qualified team id. */
export function useNoteBody(scope: string): string {
  useEffect(() => fetchKey(`note|${scope}`), [scope]);
  return useSyncExternalStore(subscribe, () => state.notes.get(scope) ?? "");
}

/** The team's leader appends; null until read or when its host predates them. */
export function useLeaderPrompt(teamId: string): LeaderPrompt | null {
  useEffect(() => fetchKey(`lp|${teamId}`), [teamId]);
  return useSyncExternalStore(subscribe, () => state.prompts.get(teamId) ?? null);
}
