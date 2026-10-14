// Android "Paylaş" → Dex: paylaşılan metin/görseli seçili sunucudaki asistana gönderir
// (`inbox_submit`, mevcut kimlik doğrulamalı dexd bağlantısı). Düzenlenebilir metin, görsel önizleme,
// sunucu ve (birden çok asistanlı takım varsa) takım seçici. İçerik loglanmaz.
// #1717: hızlı gönderim olmadıysa açılır; görev (Paylaş girişi) seçili gelir ve değiştirilebilir,
// hızlı denemenin clientId'si ve hatası taşınır (yeniden gönderim çift teslim etmez).
import { useEffect, useMemo, useRef, useState } from "react";
import { callHost, hostSupports, onHostUp, rawId, useHosts } from "../api/hosts";
import type { RawShare } from "../api/shareTarget";
import { listAgents, listTeams } from "../team/api";
import { previewUrl, prepareImage } from "./shareImage";
import {
  assistantTeams,
  buildSubmit,
  initialText,
  resolveShareHost,
  resultText,
  SHARE_TASKS,
  shouldReloadTargets,
  taskText,
  validateSubmit,
  type ShareTask,
  type SubmitImage,
} from "./shareLogic";
import { ChevronLeft } from "lucide-react";

interface Target {
  id: string;
  name: string;
}

export function ShareScreen({ share, ready, defaultHost, task: initialTask, clientId: fastClientId, error, onClose }: {
  share: RawShare;
  ready: boolean;
  defaultHost: string | null;
  task: ShareTask;
  /** Hızlı denemenin id'si: aynı paylaşım aynı id ile gider. */
  clientId?: string;
  /** Hızlı denemenin hatası (zaman aşımı / RPC). */
  error?: string | null;
  onClose: () => void;
}) {
  const hosts = useHosts();
  const [task, setTask] = useState<ShareTask>(initialTask);
  const [text, setText] = useState(() => initialText(share));
  const [image, setImage] = useState<SubmitImage | null>(null);
  const [imageErr, setImageErr] = useState<string | null>(share.error ?? null);
  // Yalnız kullanıcının açık seçimi; yoksa sunucu her render güncel `defaultHost`'tan türer
  // (soğuk açılışta takımlar sonradan yüklenir).
  const [chosenHost, setChosenHost] = useState<string | null>(null);
  const [targets, setTargets] = useState<Target[] | null>(null);
  const [teamId, setTeamId] = useState<string | null>(null);
  const [lookupErr, setLookupErr] = useState<string | null>(null);
  // Hedef aramasını yeniden tetikler ("Yeniden dene" / sunucu bağlanınca).
  const [attempt, setAttempt] = useState(0);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(error ?? null);
  const [done, setDone] = useState<string | null>(null);
  // Paylaşım başına bir kez: yeniden denemede aynı id gider, asistan çift teslim almaz.
  const clientId = useRef(fastClientId ?? crypto.randomUUID());

  useEffect(() => {
    if (!share.image) return;
    let live = true;
    prepareImage(share.image).then(
      (img) => live && setImage(img),
      (e) => live && setImageErr(String(e)),
    );
    return () => void (live = false);
  }, [share.image]);

  // Sunucu: açık seçim > varsayılan (aktif takımın sunucusu) > ilk kayıtlı.
  const hostKey = resolveShareHost(hosts.map((h) => h.rec.id), chosenHost, defaultHost);
  const host = hosts.find((h) => h.rec.id === hostKey) ?? null;
  // onHostUp geri çağrısı güncel durumu okusun diye (aboneliği yeniden kurmadan).
  const lookupFailed = useRef(false);
  lookupFailed.current = !!lookupErr;

  // Bağlantı geç gelirse (ya da kopup dönerse) başarısız arama kendiliğinden yeniden denenir.
  useEffect(
    () => onHostUp((up) => shouldReloadTargets(up, hostKey, lookupFailed.current) && setAttempt((n) => n + 1)),
    [hostKey],
  );

  useEffect(() => {
    if (!hostKey) return;
    let live = true;
    setTargets(null);
    setTeamId(null);
    setLookupErr(null);
    if (!hostSupports(hostKey, "inbox_submit")) {
      setTargets([]);
      setLookupErr("Bu sunucu paylaşımı desteklemiyor (dexd'yi güncelleyin)");
      return;
    }
    Promise.all([listTeams(hostKey), listAgents(hostKey)]).then(
      ([teams, agents]) => {
        if (!live) return;
        const found = assistantTeams(teams, agents).map((t) => ({ id: t.id, name: t.name }));
        setTargets(found);
        setTeamId(found[0]?.id ?? null);
      },
      (e) => {
        if (!live) return;
        setTargets([]);
        setLookupErr(String(e));
      },
    );
    return () => void (live = false);
  }, [hostKey, attempt]);

  const problem = useMemo(() => validateSubmit(text, image, teamId, task), [text, image, teamId, task]);
  const waitingImage = !!share.image && !image && !imageErr;

  async function send() {
    if (!hostKey || !teamId || problem) return;
    setBusy(true);
    setErr(null);
    try {
      const r = await callHost<{ queued?: boolean }>(hostKey, "inbox_submit", { ...buildSubmit(rawId(teamId), taskText(task, text), image, clientId.current) });
      setDone(resultText(r));
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  const noHost = ready && !host;
  return (
    <div className="m-agent-screen m-share" role="dialog" aria-label="Asistana gönder">
      <header className="m-agent-top">
        <button className="m-back" onClick={onClose} aria-label="Vazgeç">
          <ChevronLeft />
        </button>
        <div className="m-agent-title">
          <b>Asistana gönder</b>
          <span>{host ? host.rec.name : "—"}</span>
        </div>
      </header>

      <div className="m-share-body">
        {!ready ? (
          <p className="m-hint">Yükleniyor…</p>
        ) : noHost ? (
          <p className="m-hint">Önce bir sunucu eşleyin (Sunucular), sonra yeniden paylaşın.</p>
        ) : done ? (
          <>
            <p className="m-share-done" role="status">{done}</p>
            <button className="m-primary" onClick={onClose}>Tamam</button>
          </>
        ) : (
          <>
            {hosts.length > 1 && (
              <label className="m-share-field">
                <span>Sunucu</span>
                <select value={hostKey ?? ""} onChange={(e) => setChosenHost(e.target.value)} disabled={busy}>
                  {hosts.map((h) => (
                    <option key={h.rec.id} value={h.rec.id}>{h.rec.name}</option>
                  ))}
                </select>
              </label>
            )}
            {targets && targets.length > 1 && (
              <label className="m-share-field">
                <span>Takım</span>
                <select value={teamId ?? ""} onChange={(e) => setTeamId(e.target.value)} disabled={busy}>
                  {targets.map((t) => (
                    <option key={t.id} value={t.id}>{t.name}</option>
                  ))}
                </select>
              </label>
            )}
            <label className="m-share-field">
              <span>Görev</span>
              <select value={task} onChange={(e) => setTask(e.target.value as ShareTask)} disabled={busy}>
                {SHARE_TASKS.map((t) => (
                  <option key={t.id} value={t.id}>{t.label}</option>
                ))}
              </select>
            </label>
            <label className="m-share-field">
              <span>Metin</span>
              <textarea value={text} onChange={(e) => setText(e.target.value)} rows={6} disabled={busy} placeholder="Metin / bağlantı" />
            </label>
            {image && <img className="m-share-img" src={previewUrl(image)} alt="Paylaşılan görsel önizlemesi" />}
            {waitingImage && <p className="m-hint">Görsel hazırlanıyor…</p>}
            {imageErr && <p className="m-err" role="alert">{imageErr}</p>}
            {targets === null && <p className="m-hint">Asistan aranıyor…</p>}
            {targets && targets.length === 0 && (
              <p className="m-err" role="alert">{lookupErr ?? "Bu hostta asistan yok"}</p>
            )}
            {targets && targets.length === 0 && (
              <button className="m-share-cancel" onClick={() => setAttempt((n) => n + 1)} disabled={busy}>Yeniden dene</button>
            )}
            {err && <p className="m-err" role="alert">{err}</p>}
            <button className="m-primary" disabled={busy || waitingImage || !!problem} onClick={() => void send()}>
              {busy ? "Gönderiliyor…" : "Gönder"}
            </button>
            {problem && targets && targets.length > 0 && <p className="m-hint">{problem}</p>}
            <button className="m-share-cancel" onClick={onClose} disabled={busy}>Vazgeç</button>
          </>
        )}
      </div>
    </div>
  );
}
