// S4: a remote team's project folder is on its server — browse it there (`fs.list` / `fs.mkdir`,
// crates/dexd/src/fs.rs), only under the folders the server allows.
import { useEffect, useState } from "react";
import { ChevronUp, Folder, FolderPlus } from "lucide-react";
import { callHost, hostName, splitId } from "../api/hosts";

interface Listing {
  path: string | null;
  parent: string | null;
  entries: { name: string; path: string }[];
}

export function RemoteFolderPicker({ host, teamId, start, onPick, onCancel }: { host: string; teamId?: string; start?: string | null; onPick: (dir: string) => void; onCancel: () => void }) {
  const [at, setAt] = useState<Listing | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** "Yeni klasör" name being typed (null = field closed). */
  const [naming, setNaming] = useState<string | null>(null);
  const scope = teamId ? { teamId: splitId(teamId)[1] } : {};
  const go = (path: string | null) =>
    callHost<Listing>(host, "fs.list", { path, ...scope }).then(
      (l) => {
        setAt(l);
        setError(null);
      },
      (e) => setError(String(e)),
    );
  useEffect(() => {
    void callHost<Listing>(host, "fs.list", { path: start ?? null, ...scope }).then(
      // One allowed root: start inside it.
      (l) => (!l.path && l.entries.length === 1 ? go(l.entries[0].path) : setAt(l)),
      (e) => setError(String(e)),
    );
  }, [host, teamId, start]);

  async function mkdir() {
    const name = naming?.trim();
    if (!at?.path || !name) return;
    try {
      await go(await callHost<string>(host, "fs.mkdir", { parent: at.path, name, ...scope }));
      setNaming(null);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="remote-folder" role="dialog" aria-label={`${hostName(host)} klasörleri`}
      onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); onCancel(); } }}>
      <div className="remote-folder-head">
        <button type="button" className="icon-btn" disabled={!at?.path} title="Yukarı"
          onClick={() => void go(at?.parent ?? null)} aria-label="Üst klasör"><ChevronUp /></button>
        <code title={at?.path ?? ""}>{at?.path ?? `${hostName(host)}: izinli klasörler`}</code>
        <button type="button" className="icon-btn" disabled={!at?.path} title="Yeni klasör" onClick={() => setNaming("")} aria-label="Yeni klasör">
          <FolderPlus />
        </button>
      </div>
      {naming !== null && (
        <div className="remote-folder-new">
          <input autoFocus value={naming} placeholder="yeni klasörün adı" aria-label="Yeni klasörün adı"
            onChange={(e) => setNaming(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") { e.preventDefault(); void mkdir(); }
              if (e.key === "Escape") { e.stopPropagation(); setNaming(null); }
            }} />
          <button type="button" className="primary" disabled={!naming.trim()} onClick={() => void mkdir()}>Oluştur</button>
        </div>
      )}
      <ul className="remote-folder-list">
        {at?.entries.map((e) => (
          <li key={e.path}>
            <button type="button" onClick={() => void go(e.path)}><Folder /> {e.name}</button>
          </li>
        ))}
        {at && !at.entries.length && <li className="team-note">boş klasör</li>}
      </ul>
      {error && <p className="team-err" role="alert">{error}</p>}
      <div className="team-actions">
        <button type="button" onClick={onCancel}>Vazgeç</button>
        <button type="button" className="primary" disabled={!at?.path} onClick={() => at?.path && onPick(at.path)}>Bu klasörü seç</button>
      </div>
    </div>
  );
}
