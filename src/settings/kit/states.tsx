// Ortak kit: satır geri bildirimi + sayfa durumları
// (yükleniyor iskeleti, okuma hatası bandı, boş, sunucu çevrimdışı).
import { useEffect, useState, type ReactNode } from "react";

/** Anında kaydın başarısı: alanın yanında 2 sn görünür (`role=status`). */
export function SavedFlash({ stamp }: { stamp: number }) {
  const [show, setShow] = useState(stamp > 0);
  useEffect(() => {
    if (!stamp) return;
    setShow(true);
    const t = setTimeout(() => setShow(false), 2000);
    return () => clearTimeout(t);
  }, [stamp]);
  if (!show) return null;
  return <span className="set-saved" role="status">✓ Kaydedildi</span>;
}

export function FieldMsg({ ok, text }: { ok: boolean; text: string }) {
  return ok
    ? <p className="set-note" role="status">{text}</p>
    : <p className="set-err" role="alert">{text}</p>;
}

/** Yükleniyor: başlık ve etiketler hemen, kontrol yerinde iskelet. */
export function LoadingRows({ rows = 3 }: { rows?: number }) {
  return (
    <div className="set-pad" role="status" aria-label="Yükleniyor">
      <div className="set-skel-list">
        {Array.from({ length: rows }, (_, i) => (
          <span key={i} className="skel" style={{ height: 34 }} />
        ))}
      </div>
    </div>
  );
}

/** Okuma hatası: bant + Yeniden dene; kontroller devre dışı kalır. */
export function ErrorBand({ text, onRetry }: { text: string; onRetry: () => void }) {
  return (
    <div className="set-band" role="alert">
      <span>{text}</span>
      <button type="button" onClick={onRetry}>Yeniden dene</button>
    </div>
  );
}

export function EmptyNote({ children }: { children: ReactNode }) {
  return <div className="set-pad"><p className="set-note">{children}</p></div>;
}
