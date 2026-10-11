// Modern §6: empty screens with Core, so "nothing here" never reads as "broken". SVGs from the mock.
import type { ReactNode } from "react";
import "./empty.css";

type Mood = "board" | "team";

function CoreArt({ mood }: { mood: Mood }) {
  if (mood === "board")
    return (
      <svg width="72" height="72" viewBox="0 0 72 72" aria-hidden="true">
        <rect x="8" y="14" width="56" height="40" rx="8" fill="none" stroke="var(--empty-dash)" strokeDasharray="5 4" />
        <circle cx="36" cy="52" r="14" fill="var(--surface-3)" stroke="var(--red)" strokeWidth="2" />
        <circle cx="31" cy="50" r="1.8" fill="var(--text)" />
        <circle cx="41" cy="50" r="1.8" fill="var(--text)" />
        <path d="M31 56c1.5-1.2 8.5-1.2 10 0" stroke="var(--muted)" fill="none" />
      </svg>
    );
  return (
    <svg width="72" height="72" viewBox="0 0 72 72" aria-hidden="true">
      <circle cx="36" cy="36" r="15" fill="var(--surface-3)" stroke="var(--empty-line)" strokeWidth="2" />
      <path d="M29 35h4M39 35h4" stroke="var(--muted)" strokeWidth="2" strokeLinecap="round" />
      <path d="M46 22l4-5M50 30l6-2" stroke="var(--empty-dash)" strokeWidth="1.5" strokeLinecap="round" />
      <text x="36" y="62" textAnchor="middle" fill="var(--empty-dash)" fontSize="10" fontFamily="monospace">z z</text>
    </svg>
  );
}

export function EmptyState({ mood, title, children, actions, className = "" }: {
  mood: Mood;
  title: string;
  children: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <div className={`empty-state ${className}`} role="status">
      <CoreArt mood={mood} />
      <div className="empty-text">
        <b>{title}</b>
        <p>{children}</p>
        {actions && <div className="empty-acts">{actions}</div>}
      </div>
    </div>
  );
}
