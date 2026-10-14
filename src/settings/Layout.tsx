// Bu cihaz › Genel › Pano düzeni: açık takımın düzeni + tüm takımlara
// etki eden favori. "Favoriyi tüm takımlara uygula" onay ister.
import { useState, useSyncExternalStore } from "react";
import { useTeams } from "../team/store";
import {
  applyFavoriteLayout, applyFavoriteToAll, resetLayout, saveFavoriteLayout,
} from "../layout/panels";
import { getSnapshot as getLayoutSnapshot, subscribe as subscribeLayout } from "../layout/store";
import { Row } from "./kit/Row";
import { Pad } from "./kit/Row";
import { Section } from "./kit/Section";
import { ConfirmDialog } from "./kit/ConfirmDialog";
import { FieldMsg } from "./kit/states";

export function LayoutSection({ teamId }: { teamId: string }) {
  const { teams } = useTeams();
  const team = teams.find((t) => t.id === teamId);
  const fav = useSyncExternalStore(subscribeLayout, () => getLayoutSnapshot().favorite !== null);
  const [confirmAll, setConfirmAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const act = (f: () => string) => {
    setBusy(true);
    try {
      setMsg({ ok: true, text: f() });
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section label={`Pano düzeni${team ? ` · açık takım: ${team.name}` : ""}`}>
      <Row
        id="layout-favorite"
        label={fav ? "Favori düzen: kayıtlı" : "Favori düzen: yok"}
        description="Yeni takımlar bununla açılır."
        control={
          <span className="set-field-btns">
            <button
              type="button"
              disabled={busy}
              onClick={() => act(() => {
                if (!saveFavoriteLayout(teamId)) throw new Error("Panel henüz hazır değil — takım açılınca tekrar deneyin");
                return "Bu takımın düzeni favori yapıldı.";
              })}
              title="Bu takımın düzenini favori yap (oturumlar sürer)"
            >
              {fav ? "Düzeni favori güncelle" : "Bu takımın düzenini favori yap"}
            </button>
            {fav && (
              <button
                type="button"
                disabled={busy}
                onClick={() => act(() => {
                  if (!applyFavoriteLayout(teamId)) throw new Error("Favori uygulanamadı");
                  return "Favori bu takıma uygulandı.";
                })}
                title="Favori düzeni bu takıma uygula (oturumlar sürer)"
              >
                Favoriyi bu takıma uygula
              </button>
            )}
            {fav && <button type="button" disabled={busy} onClick={() => setConfirmAll(true)}>Favoriyi tüm takımlara uygula…</button>}
          </span>
        }
        feedback={msg && <FieldMsg ok={msg.ok} text={msg.text} />}
      />
      <Row
        id="layout-reset"
        label="Açık takımın düzenini sıfırla"
        description="Paneller varsayılan yerleşime döner; oturumlar sürer."
        control={<button type="button" disabled={busy} onClick={() => resetLayout(teamId)}>Sıfırla</button>}
      />
      {confirmAll && (
        <Pad>
          <ConfirmDialog
            title="Favori tüm takımlara uygulansın mı?"
            lines={[
              `${teams.length} takımın panelleri favori düzene döner.`,
              "Oturumlar sürer; her takım kendi ajanlarıyla açılır.",
            ]}
            confirmLabel="Tümüne uygula"
            busy={busy}
            onCancel={() => setConfirmAll(false)}
            onConfirm={() => {
              act(() => {
                const n = applyFavoriteToAll(teams.map((t) => t.id));
                if (!n) throw new Error("Favori uygulanamadı");
                return `Favori ${n} takıma uygulandı.`;
              });
              setConfirmAll(false);
            }}
          />
        </Pad>
      )}
    </Section>
  );
}
