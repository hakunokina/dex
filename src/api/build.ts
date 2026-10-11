// #151: this UI's build (vite.config.ts), for the "Çekirdek güncel değil" band.
export const UI_BUILD = {
  commit: import.meta.env.DEX_COMMIT ?? "unknown",
  commitTime: Number(import.meta.env.DEX_COMMIT_TIME ?? 0),
};
