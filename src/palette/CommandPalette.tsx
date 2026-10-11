// Command palette (DEX-6). Ctrl+K anywhere except inside a terminal, where Ctrl+K is Claude Code's
// kill-to-end-of-line; Ctrl+Shift+P works everywhere, terminal included (Cmd+K too on macOS).
import { useEffect, useRef, useState } from "react";
import { useTeams } from "../team/store";
import { buildCommands, quickTask, type Bridge, type Command } from "./commands";
import { sections } from "./rank";
import { setPaletteOpen, usePaletteOpen } from "./store";
import "./palette.css";

// ---- open/close store lives in ./store (the global shortcut must not pull this module) ----
const setOpen = setPaletteOpen;
const useOpen = usePaletteOpen;

// ---- recents (per-machine convenience; storage failures only lose the ordering) ----
const RECENT_KEY = "dex.palette.recent";
const MAX_RECENT = 8;
function loadRecent(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}
function pushRecent(id: string) {
  const next = [id, ...loadRecent().filter((x) => x !== id)].slice(0, MAX_RECENT);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {}
}

export function CommandPalette(bridge: Bridge) {
  const open = useOpen();
  return open ? <Panel bridge={bridge} /> : null;
}

function Panel({ bridge }: { bridge: Bridge }) {
  useTeams(); // re-render on team/member changes
  const [, tick] = useState(0);
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [recent] = useState(loadRecent);
  const listRef = useRef<HTMLDivElement>(null);

  // Start/stop entries follow live run state.
  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 400);
    return () => clearInterval(t);
  }, []);

  const quick = quickTask(query);
  const groups = quick ? [{ label: quick.group, items: [quick] }] : sections(buildCommands(bridge), query, recent);
  const items = groups.flatMap((g) => g.items);
  const cur = Math.min(sel, Math.max(items.length - 1, 0));

  useEffect(() => {
    listRef.current?.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  }, [cur, query]);

  async function run(c: Command | undefined) {
    if (!c) return;
    setError(null);
    try {
      await c.run();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }
    if (!c.transient) pushRecent(c.id);
    setOpen(false, !!c.restoreFocus);
  }

  function onInputKey(e: React.KeyboardEvent) {
    const n = items.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (n) setSel((cur + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      void run(items[cur]);
    }
  }

  return (
    <div className="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && setOpen(false, true)}>
      <div className="palette" role="dialog" aria-modal="true" aria-label="Komut paleti">
        <input
          className="palette-input"
          autoFocus
          spellCheck={false}
          value={query}
          placeholder="Komut ara — takım, ajan, eylem…  ya da  yeni görev: <metin>"
          onChange={(e) => {
            setQuery(e.target.value);
            setSel(0);
            setError(null);
          }}
          onKeyDown={onInputKey}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={items.length ? `palette-opt-${cur}` : undefined}
        />
        <div className="palette-list" id="palette-list" role="listbox" ref={listRef}>
          {groups.map((g, gi) => (
            <div key={g.label} role="group" aria-labelledby={`palette-g${gi}`}>
              <div className="palette-group" id={`palette-g${gi}`} role="presentation">{g.label}</div>
              {g.items.map((c) => {
                const i = items.indexOf(c);
                return (
                  <div
                    key={c.id}
                    id={`palette-opt-${i}`}
                    role="option"
                    aria-selected={i === cur}
                    className={`palette-item${i === cur ? " sel" : ""}`}
                    onMouseMove={() => i !== cur && setSel(i)}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => void run(c)}
                  >
                    <span className="palette-title">{c.title}</span>
                    <span className="palette-hint">{c.hint}</span>
                  </div>
                );
              })}
            </div>
          ))}
          {items.length === 0 && <div className="palette-empty">Eşleşen komut yok</div>}
        </div>
        {error && <div className="palette-error" role="alert">{error}</div>}
        <div className="palette-foot">
          <span><kbd>↑</kbd><kbd>↓</kbd> gezin</span>
          <span><kbd>↵</kbd> çalıştır</span>
          <span><kbd>Esc</kbd> kapat</span>
          <span className="spacer" />
          <span><kbd>Ctrl K</kbd> · terminalde <kbd>Ctrl ⇧ P</kbd></span>
        </div>
      </div>
    </div>
  );
}
