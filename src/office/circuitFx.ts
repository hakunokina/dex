// #411: one fixed overlay layer over the whole window (pointer-events: none, no layout) that draws the
// Kart → pane circuit trace and the target's edge flash, then removes itself.
import { circuitPath, type Box } from "./circuit";
import { TOUCH_MS, reachAt, tentacleContour } from "./symbiote";

const NS = "http://www.w3.org/2000/svg";
const PULSE_MS = 420;

let layer: HTMLDivElement | null = null;

function layerEl() {
  if (!layer) {
    layer = document.createElement("div");
    layer.className = "office-fx";
    layer.setAttribute("aria-hidden", "true");
    document.body.appendChild(layer);
  }
  return layer;
}

function svgEl<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number>) {
  const el = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  return el;
}

/** From the chip (viewport box) to the pane / tab; `pulseOnly`: the pane was already on screen. */
export function playCircuit(from: Box, to: Box, pulseOnly: boolean) {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const host = layerEl();
  const svg = svgEl("svg", { width: "100%", height: "100%" });
  host.appendChild(svg);
  const points = pulseOnly ? [] : circuitPath(from, to);
  const edge = svgEl("rect", { x: to.left + 1, y: to.top + 1, width: Math.max(0, to.width - 2), height: Math.max(0, to.height - 2), rx: 6, class: "office-fx-edge" });
  svg.appendChild(edge);
  const done: Promise<unknown>[] = [];
  let delay = 0;
  if (points.length > 1) {
    const line = svgEl("polyline", { class: "office-fx-trace" });
    svg.appendChild(line);
    done.push(new Promise<void>(resolve => {
      const start = performance.now();
      const frame = (now: number) => {
        const age = now - start;
        if (age >= TOUCH_MS || !line.isConnected || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return resolve();
        const contour = tentacleContour(points, reachAt(age), age);
        line.setAttribute("points", [...contour, ...contour.slice(0, 1)].map(p => `${p.x},${p.y}`).join(" "));
        requestAnimationFrame(frame);
      };
      requestAnimationFrame(frame);
    }));
    delay = 600;
  }
  done.push(edge.animate([{ opacity: 0 }, { opacity: 1, offset: 0.25 }, { opacity: 0 }], { duration: PULSE_MS, delay, fill: "both" }).finished);
  void Promise.allSettled(done).then(() => {
    svg.remove();
    if (layer && !layer.childElementCount) { layer.remove(); layer = null; }
  });
}
