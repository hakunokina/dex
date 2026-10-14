// Ortak kit: satır — etiket + açıklama solda, kontrol sağda, tek hizalama.
// `id` derin bağlantı hedefidir (palet "Ayar: …" satırı odaklar).
import type { ReactNode } from "react";

export function Row({ id, label, description, control, feedback }: {
  id?: string;
  label: ReactNode;
  description?: ReactNode;
  control: ReactNode;
  feedback?: ReactNode;
}) {
  return (
    <div className="set-row" data-setting={id}>
      <div className="set-row-text">
        <span className="set-label">{label}</span>
        {description && <p className="set-desc">{description}</p>}
        {feedback}
      </div>
      <div className="set-row-ctl">{control}</div>
    </div>
  );
}

/** Kart içi serbest içerik (liste formu, düğme grubu): satırla aynı dolgu. */
export function Pad({ id, children }: { id?: string; children: ReactNode }) {
  return <div className="set-pad" data-setting={id}>{children}</div>;
}
