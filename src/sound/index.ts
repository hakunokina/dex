import { useSyncExternalStore } from "react";
import { MOBILE } from "../api/platform";
import { onEvent } from "../api/hosts";
import { kept } from "../hot";
import { SOUND_LABELS, createSoundGate, normalizeSoundSettings, soundForEvent, type SoundEvent, type SoundKind, type SoundSettings } from "./model";
import { synthesize } from "./synth";

const KEY = "dex.sounds";
const hot = import.meta.hot;
function readSettings() {
  try { return normalizeSoundSettings(JSON.parse(localStorage.getItem(KEY) ?? "null")); }
  catch { return normalizeSoundSettings(null); }
}
const box = kept(hot, "sound", () => ({
  view: { settings: readSettings(), error: null as string | null, audioError: null as string | null },
  context: null as AudioContext | null,
  accept: createSoundGate(),
  /** Custom sound files (public/sounds/<kind>.<ext>): buffer, null = none, absent = not loaded yet. */
  files: new Map<SoundKind, AudioBuffer | null>(),
  listeners: new Set<() => void>(),
}));
function publish(settings: SoundSettings, error: string | null, audioError = box.view.audioError) {
  box.view = { settings, error, audioError };
  for (const listener of box.listeners) listener();
}
function clearAudioError() {
  if (box.view.audioError) publish(box.view.settings, box.view.error, null);
}
const subscribe = (listener: () => void) => {
  box.listeners.add(listener);
  return () => void box.listeners.delete(listener);
};
export const useSoundSettings = () => useSyncExternalStore(subscribe, () => box.view);

export function setSoundSettings(value: SoundSettings) {
  const settings = normalizeSoundSettings(value);
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
    publish(settings, null);
  } catch {
    publish(box.view.settings, "Ses ayarları kaydedilemedi. Cihaz depolamasını kontrol edin.");
  }
}

function audio() {
  if (typeof AudioContext === "undefined") throw new Error("Bu cihazda ses oynatma desteklenmiyor.");
  if (!box.context || box.context.state === "closed") {
    const ctx = box.context = new AudioContext();
    box.files.clear();
    preloadFiles(ctx);
    ctx.onstatechange = () => {
      if (box.context === ctx && ctx.state === "running") clearAudioError();
    };
  }
  return box.context;
}
const SOUND_EXTS = ["opus", "mp3", "wav", "ogg"];
/** First playable `public/sounds/<kind>.<ext>`; Vite's SPA fallback (text/html) counts as missing. */
async function loadFile(ctx: AudioContext, kind: SoundKind): Promise<AudioBuffer | null> {
  for (const ext of SOUND_EXTS) {
    try {
      const res = await fetch(`/sounds/${kind}.${ext}`);
      if (!res.ok || (res.headers.get("content-type") ?? "").includes("text/html")) continue;
      return await ctx.decodeAudioData(await res.arrayBuffer());
    } catch { /* next extension */ }
  }
  return null;
}
function preloadFiles(ctx: AudioContext) {
  for (const kind of Object.keys(SOUND_LABELS) as SoundKind[]) {
    if (box.files.has(kind)) continue;
    box.files.set(kind, null);
    void loadFile(ctx, kind).then((buffer) => box.files.set(kind, buffer));
  }
}
/** A custom file if one is loaded, else the built-in synth. */
function play(ctx: AudioContext, kind: SoundKind, volume: number) {
  const buffer = box.files.get(kind);
  if (!buffer) return synthesize(ctx, kind, volume);
  const source = ctx.createBufferSource();
  const gain = ctx.createGain();
  source.buffer = buffer;
  gain.gain.value = volume;
  source.connect(gain).connect(ctx.destination);
  source.onended = () => { source.disconnect(); gain.disconnect(); };
  source.start();
}
const failed = (error: unknown) => publish(box.view.settings, box.view.error, error instanceof Error ? error.message : "Ses çalınamadı.");

/** Automatic events never queue old sounds behind an autoplay lock. Focus is irrelevant. */
export function soundEvent(event: SoundEvent) {
  if (MOBILE) return;
  const kind = soundForEvent(event);
  if (!kind || !box.accept(kind, box.view.settings, performance.now())) return;
  try {
    const ctx = audio();
    if (ctx.state !== "running") {
      void ctx.resume().catch(failed);
      throw new Error("Sesleri etkinleştirmek için Dex'te bir kez tıklayın veya bir tuşa basın.");
    }
    play(ctx, kind, box.view.settings.volume);
    clearAudioError();
  } catch (error) { failed(error); }
}

/** Listen ignores the switches and debounce so even a disabled event can be auditioned. */
export async function previewSound(kind: SoundKind) {
  if (MOBILE) throw new Error("Olay sesleri yalnız masaüstünde kullanılabilir.");
  const ctx = audio();
  if (ctx.state !== "running") {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        ctx.resume(),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("Ses açılamadı. Yeniden Dinle'ye basın.")), 1500); }),
      ]);
    } finally { clearTimeout(timer); }
  }
  if (ctx.state !== "running") throw new Error("Ses aygıtı hazır değil. Yeniden deneyin.");
  if (box.view.settings.volume === 0) throw new Error("Dinlemek için ses düzeyini artırın.");
  play(ctx, kind, box.view.settings.volume);
  clearAudioError();
}

/** Desktop shell lifetime: gesture unlock + host events, with no timers or polling. */
export function initSounds() {
  if (MOBILE) return;
  const unlock = () => {
    if (!box.view.settings.enabled) return;
    try {
      const ctx = audio();
      if (ctx.state !== "running") void ctx.resume().catch(failed);
    } catch (error) { failed(error); }
  };
  const storage = (event: StorageEvent) => {
    if (event.key === KEY || event.key === null) publish(readSettings(), null);
  };
  const off = onEvent<{ teamId: string; leaderId: string; timestamp: number }>("team_turn_done", () => soundEvent("team_turn_done"));
  window.addEventListener("pointerdown", unlock, true);
  window.addEventListener("keydown", unlock, true);
  window.addEventListener("storage", storage);
  unlock();
  return () => {
    off();
    window.removeEventListener("pointerdown", unlock, true);
    window.removeEventListener("keydown", unlock, true);
    window.removeEventListener("storage", storage);
  };
}
