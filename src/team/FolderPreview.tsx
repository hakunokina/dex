import { useEffect, useState } from "react";
import { hostSupports } from "../api/hosts";
import { teamFolderPreview, type TeamFolderPreview } from "./api";
import { isUnknownMethod } from "../project/model";

export function FolderPreview({ host, name }: { host: string; name: string }) {
  const [result, setResult] = useState<{ key: string; preview?: TeamFolderPreview; error?: string; unsupported?: boolean } | null>(null);
  const key = `${host}\n${name}`;
  const supported = hostSupports(host, "team_folder_preview");
  useEffect(() => {
    if (!supported || !name.trim()) return;
    let live = true;
    const timer = setTimeout(() => {
      void teamFolderPreview(host, name).then(
        preview => { if (live) setResult({ key, preview }); },
        error => {
          if (live) setResult({ key, unsupported: isUnknownMethod(error, "team_folder_preview"), error: String(error) });
        },
      );
    }, 250);
    return () => { live = false; clearTimeout(timer); };
  }, [host, name, key, supported]);
  const current = result?.key === key ? result : null;
  if (!supported || current?.unsupported) return <p className="team-note">Klasör sunucuda &lt;Ad&gt;/&lt;Ad&gt;Main olarak oluşturulur.</p>;
  if (!name.trim()) return <p className="team-note">Ad girince kullanıcı dizininde takım klasörü ve içinde Main klasörü oluşturulur.</p>;
  if (current?.error) return <p className="team-err" role="alert">Klasör önizlemesi alınamadı: {current.error}</p>;
  return <p className="team-note team-folder-preview" role="status">
    {current?.preview ? <>Varsayılan klasör: <code>{current.preview.mainRoot}</code>
      {current.preview.owner ? " — bu klasör başka bir takıma bağlı." : current.preview.exists.main ? " — mevcut klasör kullanılır." : " — oluşturulacak."}</> : "Klasör önizlemesi yükleniyor…"}
  </p>;
}
