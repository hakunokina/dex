// #2055: a running job's progress (its worker's own task list): the board card row and the pane tab chip.
import { useEffect, useState } from "react";
import { percent, progressLine, type Progress } from "./progressModel";
import "./progress.css";

/** "X dk önce" stays true without a new event. */
function useMinute() {
  const [now, set] = useState(Date.now);
  useEffect(() => {
    const t = setInterval(() => set(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}

function Bar({ p }: { p: Progress }) {
  return (
    <span className="progress-bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent(p)}>
      <i style={{ width: `${percent(p)}%` }} />
    </span>
  );
}

/** Board card: bar + "%43 · 3/7 · süren adım · 2 dk önce". */
export function ProgressRow({ p }: { p: Progress }) {
  const line = progressLine(p, useMinute());
  return (
    <div className="progress-row" title={line} data-testid="job-progress">
      <Bar p={p} />
      <span className="progress-text">{line}</span>
    </div>
  );
}

/** Pane tab: "%43" with a mini bar; the line in the title. */
export function ProgressChip({ p }: { p: Progress }) {
  const line = progressLine(p, useMinute());
  return (
    <span className="progress-chip" title={`İlerleme: ${line}`} data-testid="tab-progress">
      <Bar p={p} />%{percent(p)}
    </span>
  );
}
