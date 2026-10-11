// "Klasör seç…" for a team or project folder: this machine's dialog, or for a remote host its
// folder browser in place (RemoteFolderPicker). Moved out of TeamBar so the project list shares it.
import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { LOCAL } from "../api/hosts";
import { RemoteFolderPicker } from "../team/RemoteFolder";

/** This machine's folder dialog; `start`: where it opens (e.g. the team's category folder). */
async function pickLocalFolder(start?: string | null): Promise<string | null> {
  const dir = await open({ directory: true, multiple: false, title: "Proje klasörü", defaultPath: start ?? undefined });
  return typeof dir === "string" ? dir : null;
}

export function FolderButton({ host, teamId, label, disabled, title, start, onPick }: {
  host: string; teamId?: string; label: string; disabled?: boolean; title?: string; start?: string | null; onPick: (dir: string) => void;
}) {
  const [browsing, setBrowsing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  return (
    <>
      <button type="button" disabled={disabled || picking} title={title}
        onClick={async () => {
          if (host !== LOCAL) return setBrowsing(true);
          setPicking(true);
          setError(null);
          try {
            const dir = await pickLocalFolder(start);
            if (dir) onPick(dir);
          } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
          finally { setPicking(false); }
        }}>
        {label}
      </button>
      {error && <span className="team-err" role="alert">{error}</span>}
      {browsing && (
        <RemoteFolderPicker host={host} teamId={teamId} start={start} onCancel={() => setBrowsing(false)} onPick={(dir) => { setBrowsing(false); onPick(dir); }} />
      )}
    </>
  );
}
