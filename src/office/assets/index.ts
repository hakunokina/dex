// #1958: the Kart's A · Doku maps (symbiote-cards/HANDOFF.md; rebuilt with tools/symbiote-cards). Decoded
// as raw ImageBitmaps — no premultiply, no colour conversion: normals and data channels stay exact. The
// nine are handed out only together (the card switches to the lit look at once); until then, or if one
// fails, world.ts keeps the 2D fallback. The office's frame loop redraws on its own: no load callback.
import bgN from "./symbiote-cards/A-bg-n.webp";
import bgC from "./symbiote-cards/A-bg-c.webp";
import bgX from "./symbiote-cards/A-bg-x.webp";
import workerN from "./symbiote-cards/A-worker-n.webp";
import workerC from "./symbiote-cards/A-worker-c.webp";
import workerX from "./symbiote-cards/A-worker-x.webp";
import echoN from "./symbiote-cards/A-echo-n.webp";
import echoC from "./symbiote-cards/A-echo-c.webp";
import echoX from "./symbiote-cards/A-echo-x.webp";
import meta from "./symbiote-cards/meta.json";
import { footprint } from "../symbioteCard";
import type { CardMaps, MapSet } from "../litLayer";

/** The bodies' coverage boxes (sprite size and centre, HANDOFF §7). */
export const WORKER_FOOT = footprint(meta["A-worker"]);
export const ECHO_FOOT = footprint(meta["A-echo"]);

let maps: CardMaps | null = null;
let started = false;

async function bitmap(url: string) {
  const blob = await (await fetch(url)).blob();
  return createImageBitmap(blob, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
}

async function mapSet(n: string, c: string, x: string): Promise<MapSet> {
  const [bn, bc, bx] = await Promise.all([bitmap(n), bitmap(c), bitmap(x)]);
  return { n: bn, c: bc, x: bx };
}

function start() {
  started = true;
  Promise.all([mapSet(bgN, bgC, bgX), mapSet(workerN, workerC, workerX), mapSet(echoN, echoC, echoX)])
    .then(([bg, worker, echo]) => { maps = { bg, worker, echo }; }, () => { /* failed: the 2D fallback stays */ });
}

/** All nine maps, or null while they load (or after a failure). */
export function cardMaps(): CardMaps | null {
  if (!started) start();
  return maps;
}
