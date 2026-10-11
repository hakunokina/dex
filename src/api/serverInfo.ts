// #142: what a host's build is and can do — dexd's `auth.hello` / `server_info`, the local core's
// `server_info` (crates/dexd/src/server.rs, src-tauri/src/hosts.rs). Pure: unit-tested.
//
// A pre-#142 dexd sends none of `commit` / `commitTime` / `methods`: its methods are unknown, so
// every call is tried and the "bilinmeyen metot" fallback stays the safety net.

export interface ServerInfo {
  version: string | null;
  commit: string | null;
  /** Unix secs of the build's commit; 0 = built without git; null = the host predates #142. */
  commitTime: number | null;
  /** null = unknown (older dexd) or "everything" (the local core is this very build). */
  methods: string[] | null;
}

/** Any host answer → ServerInfo; missing or mistyped fields are null (an older host never throws here). */
export function readServerInfo(v: unknown): ServerInfo | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const str = (x: unknown) => (typeof x === "string" && x ? x : null);
  return {
    version: str(o.version),
    commit: str(o.commit),
    commitTime: typeof o.commitTime === "number" ? o.commitTime : null,
    methods: Array.isArray(o.methods) ? o.methods.filter((m): m is string => typeof m === "string") : null,
  };
}

/** false only when the host published its method list and `method` is not on it. */
export const supports = (info: ServerInfo | null | undefined, method: string) => !info?.methods || info.methods.includes(method);

/**
 * The host's build is older than `ref` (this app's core). A host without `commitTime` predates
 * #142, so it is older than any build that has one; a build without git (0) can't be compared.
 */
export function isOlder(info: ServerInfo | null | undefined, ref: ServerInfo | null | undefined): boolean {
  if (!info || !ref?.commitTime) return false;
  if (info.commitTime === null) return true;
  return info.commitTime > 0 && info.commitTime < ref.commitTime;
}

/** #151: how the local core's build stands to this UI's: `older` / `newer` by commit time,
 *  `other` = another commit at the same time; null = the same, or one side can't tell. */
export type Drift = "older" | "newer" | "other";
export function coreDrift(core: ServerInfo | null | undefined, ui: { commit: string; commitTime: number }): Drift | null {
  if (!core || !ui.commitTime || core.commit === ui.commit) return null;
  if (core.commitTime === null) return "older";
  if (!core.commitTime) return null;
  return core.commitTime < ui.commitTime ? "older" : core.commitTime > ui.commitTime ? "newer" : "other";
}

/** "dexd 0.1.0 · 1b2c553a1f" — whatever the host told. */
export function describe(info: ServerInfo | null | undefined): string | null {
  if (!info) return null;
  const parts = [info.version && `dexd ${info.version}`, info.commit].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
}
