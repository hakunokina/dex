import type { SoundKind } from "./model";

/** Short envelopes avoid clicks; every source stops and disconnects itself. No audio assets. */
export function synthesize(ctx: AudioContext, kind: SoundKind, setting: number) {
  // The levels below were too quiet at 100 %: tripled. Overlapping peaks stay under 1 (no clipping).
  const volume = setting * 3;
  const start = ctx.currentTime + 0.01;
  const tone = (from: number, to: number, offset: number, duration: number, level: number, type: OscillatorType = "sine") => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    const at = start + offset;
    osc.type = type;
    osc.frequency.setValueAtTime(from, at);
    osc.frequency.exponentialRampToValueAtTime(to, at + duration);
    gain.gain.setValueAtTime(0, at);
    gain.gain.linearRampToValueAtTime(volume * level, at + 0.012);
    gain.gain.exponentialRampToValueAtTime(0.0001, at + duration);
    osc.connect(gain).connect(ctx.destination);
    osc.onended = () => { osc.disconnect(); gain.disconnect(); };
    osc.start(at);
    osc.stop(at + duration);
  };
  switch (kind) {
    case "complete":
      tone(523, 523, 0, 0.22, 0.16);
      tone(659, 659, 0.14, 0.32, 0.13);
      break;
    case "attention":
      tone(880, 880, 0, 0.12, 0.18);
      tone(1175, 1175, 0.15, 0.16, 0.16);
      break;
    case "warning":
      tone(220, 165, 0, 0.32, 0.18, "triangle");
      break;
    case "worker":
      tone(1200, 600, 0, 0.065, 0.10);
      break;
    case "delegation": {
      tone(350, 1700, 0, 0.24, 0.08);
      // Airy, rising "siuv": band-pass white noise swept upward with a soft envelope.
      const duration = 0.24;
      const buffer = ctx.createBuffer(1, Math.ceil(ctx.sampleRate * duration), ctx.sampleRate);
      const data = buffer.getChannelData(0);
      for (let i = 0; i < data.length; i++) data[i] = Math.random() * 2 - 1;
      const noise = ctx.createBufferSource();
      const filter = ctx.createBiquadFilter();
      const gain = ctx.createGain();
      noise.buffer = buffer;
      filter.type = "bandpass";
      filter.Q.value = 1.4;
      filter.frequency.setValueAtTime(500, start);
      filter.frequency.exponentialRampToValueAtTime(4200, start + duration);
      gain.gain.setValueAtTime(0, start);
      gain.gain.linearRampToValueAtTime(volume * 0.22, start + 0.055);
      gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
      noise.connect(filter).connect(gain).connect(ctx.destination);
      noise.onended = () => { noise.disconnect(); filter.disconnect(); gain.disconnect(); };
      noise.start(start);
      noise.stop(start + duration);
      break;
    }
  }
}
