// #401: the office artwork (sources: .dex/design/office-assets/final, converted to WebP at ~2× their largest
// on-screen size). An image is handed out only once decoded; until then — or if it fails — world.ts keeps
// the procedural drawing. The office's frame loop redraws on its own, so no load callback is needed.
import pcbUrl from "./pcb.webp";
import workerUrl from "./worker.webp";

const URLS = { pcb: pcbUrl, worker: workerUrl };
type Key = keyof typeof URLS;

const ready = new Map<Key, HTMLImageElement>();
let started = false;

function start() {
  started = true;
  for (const [key, url] of Object.entries(URLS) as [Key, string][]) {
    const img = new Image();
    img.src = url;
    img.decode().then(() => ready.set(key, img), () => { /* failed: the procedural drawing stays */ });
  }
}

function get(key: Key): HTMLImageElement | null {
  if (!started) start();
  return ready.get(key) ?? null;
}

export const pcbImage = () => get("pcb");
export const workerImage = () => get("worker");
