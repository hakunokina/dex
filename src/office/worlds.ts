// One PCB scene per team. Keeping the World preserves chip movement and packet clocks across
// tab/panel switches, while hidden scenes do no background drawing or simulation work.
import { kept, latest, listenKept } from "../hot";
import { qid } from "../api/hosts";
import { getAgentError, getStatus } from "../agents";
import { getDelegations, isActive, unreadResult } from "../delegations";
import { requestStart } from "../launcher";
import { getQuestions, leaderPending, workerQuestion } from "../questions";
import { getProposals } from "../team/Proposals";
import { applyStep, type Step, type StepEvent } from "./companion/coreState";
import { stalledFor } from "../stall";
import { slowReply } from "../firstReply";
import { membersOf, subscribe, getTeamsSnapshot } from "../team/store";
import { agentToday } from "../usage/costStore";
import { costText } from "../usage/costFormat";
import { World, type WorldData } from "./world";

const STEP_S = 0.05;
const MAX_GAP_S = 1;
const WORLD_CACHE_VERSION = 3;

interface WorldEntry { version: number; world: World; last: number }

const hot = import.meta.hot;
const worlds = kept(hot, "worlds", () => new Map<string, WorldEntry>());

// #252: the leaders' running tools for the office's big Core (the strip keeps its own, #243).
const steps = kept(hot, "coreSteps", () => new Map<string, Step[]>());
listenKept<StepEvent>(hot, "agent.step", (e, host) => {
  const id = qid(host, e.payload.agentId);
  steps.set(id, applyStep(steps.get(id) ?? [], e.payload));
});
const NO_STEPS: Step[] = [];
const liveData: WorldData = {
  status: getStatus,
  delegations: getDelegations,
  active: isActive,
  result: (agentId) => unreadResult(getDelegations(), agentId),
  question: (agentId) => workerQuestion(getQuestions(), agentId),
  stalled: stalledFor,
  slow: (id) => slowReply(id) !== null,
  start: requestStart,
  cost: (id) => costText(agentToday(id)?.total, false),
  proposals: (leaderId) => getProposals().filter((p) => p.leader === leaderId),
  escalated: (leaderId) => getQuestions().filter((q) => q.leader === leaderId && q.status === "escalated").length,
  workerAsks: (leaderId) => leaderPending(getQuestions(), leaderId),
  steps: (id) => {
    // A turn that ended took its tools with it (an `end` lost on a reconnect must not pin "araç").
    const status = getStatus(id);
    if (status !== "working" && status !== "waiting") steps.delete(id);
    return steps.get(id) ?? NO_STEPS;
  },
  agentError: getAgentError,
};

function createEntry(teamId: string, now = performance.now()): WorldEntry {
  return { version: WORLD_CACHE_VERSION, world: new World(membersOf(teamId), liveData), last: now };
}

function currentEntry(entry: WorldEntry | undefined): entry is WorldEntry {
  return !!entry && entry.version === WORLD_CACHE_VERSION && entry.world instanceof World;
}

export function worldFor(teamId: string): World {
  let entry = worlds.get(teamId);
  if (!currentEntry(entry)) {
    entry = createEntry(teamId);
    worlds.set(teamId, entry);
  }
  return entry.world;
}

/** Advances only when a visible Office asks for a frame. */
export function advance(teamId: string, now: number) {
  let entry = worlds.get(teamId);
  if (!currentEntry(entry)) {
    entry = createEntry(teamId, now);
    worlds.set(teamId, entry);
  }
  let dt = Math.min(MAX_GAP_S, Math.max(0, (now - entry.last) / 1000));
  entry.last = now;
  while (dt > 0) {
    const step = Math.min(STEP_S, dt);
    entry.world.update(step);
    dt -= step;
  }
}

const onTeams = latest(hot, "onTeams", () => {
  const known = new Set(getTeamsSnapshot().teams.map((team) => team.id));
  for (const [id, entry] of worlds) {
    if (!known.has(id)) worlds.delete(id);
    else if (currentEntry(entry)) entry.world.sync(membersOf(id));
    else worlds.set(id, createEntry(id));
  }
});
kept(hot, "subscribed", () => subscribe(onTeams));
