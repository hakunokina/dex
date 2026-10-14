// Ortak kit: bölüm — başlık kartın DIŞINDA, alanlar kartın içinde.
// Sayfa › bölüm › alan hiyerarşisi: h2 › h3 › label.
import type { ReactNode } from "react";

export function Section({ label, description, danger, children }: {
  label: string;
  description?: ReactNode;
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <section className="set-sec" aria-label={label}>
      <h3>{label}</h3>
      {description && <p className="set-sec-desc">{description}</p>}
      <div className={`set-card${danger ? " set-danger" : ""}`}>{children}</div>
    </section>
  );
}
