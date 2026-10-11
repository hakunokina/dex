// S4: every backend this window talks to, side by side. `local` = this machine's core (#150:
// `dexd --local` over its loopback endpoint, `LocalTransport`; it can be down like any host);
// each paired `dexd` is a host with its own WebSocket. A team lives on exactly one host and every
// call about it goes to that host's transport.
//
// Ids: what a host returns is qualified here as `<host>:<id>` (teams, agents, PTYs, note scopes),
// so two hosts never collide in client state (layout.json, dock slots, status maps). Local ids stay
// bare — pre-S4 layouts and sessions keep working. Only this client sees qualified ids; a host is
// always called with its own bare ids (`splitId`).
//
// Secrets: the host list (name, address, device id) is in localStorage; each device token is in
// the OS keyring (`host_token_*` Tauri commands, src-tauri/src/hosts.rs).
import { useSyncExternalStore } from "react";
import { WEB, WEB_TOKEN_STORAGE } from "./platform";
import { normalizeUrl } from "./connectionPolicy";
import { webTokenCommand } from "./webTokens";
import { localMethods } from "./clientOnly";
import { readServerInfo, supports, type ServerInfo } from "./serverInfo";
import { LocalTransport, pairDevice, WsTransport, type DexEvent, type HostStatus, type Transport, type Unlisten } from "./transport";

export type { HostStatus };
export const LOCAL = "local";

export interface HostRec {
  id: string;
  name: string;
  url: string;
  /** dexd's device id for this window (`dexd devices revoke <id>`). */
  device: string;
}

export interface HostView {
  rec: HostRec;
  /** `no-token`: the keyring has no token for it (pair again). */
  status: HostStatus | "no-token";
  /** #142: from its last `auth.hello`; null until it connected once. */
  info: ServerInfo | null;
}

type Handler = (e: DexEvent<unknown>, host: string) => void;
interface Listener {
  event: string;
  fn: Handler;
  offs: Map<string, Promise<Unlisten | void>>;
}
interface Remote {
  rec: HostRec;
  t: WsTransport | null;
  status: HostView["status"];
}

const hot = import.meta.hot;
interface Registry {
  local: LocalTransport;
  remotes: Map<string, Remote>;
  listeners: Set<Listener>;
  up: Set<(host: string) => void>;
  subs: Set<() => void>;
  view: HostView[];
  started: Promise<void> | null;
  /** #142: the in-app core's `server_info` (null: mobile / browser — no local core). */
  localInfo: ServerInfo | null;
  /** #150: the local core's connection; optimistic until its socket says otherwise. */
  localStatus: HostStatus;
  /** #203: why the local core is down (the shell's last start failure), for the band. */
  localDown: string | null;
}
// One per page load: a vite hot update must not open second sockets.
const reg: Registry = hot
  ? ((hot.data.hosts as Registry | undefined) ??= fresh())
  : fresh();

function fresh(): Registry {
  // #146: back from a drop / missed events on the local endpoint → what it shows is fetched again.
  const refetch = () => {
    for (const fn of reg.up) fn(LOCAL);
  };
  const status = (s: HostStatus) => {
    // The first "connecting" keeps the optimistic start (calls wait on the socket anyway).
    if (s === "connecting" || reg.localStatus === s) return;
    reg.localStatus = s;
    changed();
  };
  const down = (why: string) => {
    if (reg.localDown === why) return;
    reg.localDown = why;
    changed();
  };
  return { local: new LocalTransport(refetch, status, down), remotes: new Map(), listeners: new Set(), up: new Set(), subs: new Set(), view: [], started: null, localInfo: null, localStatus: "connected", localDown: null };
}

// ---- ids ----

export const qid = (host: string, raw: string) => (host === LOCAL ? raw : `${host}:${raw}`);

/** `[host, bare id]`. Local ids never contain `:` (slug-xxxx, sh-<uuid>, "shared"). */
export function splitId(id: string): [string, string] {
  const i = id.indexOf(":");
  return i > 0 ? [id.slice(0, i), id.slice(i + 1)] : [LOCAL, id];
}
export const hostOf = (id: string) => splitId(id)[0];
export const rawId = (id: string) => splitId(id)[1];
/** A host-given id (or null) → this client's id. */
export const qOpt = (host: string, raw: string | null | undefined) => (raw ? qid(host, raw) : null);

// ---- calls & events ----

export function transportOf(host: string): Transport {
  if (host === LOCAL) return reg.local;
  const r = reg.remotes.get(host);
  if (!r) throw "bu sunucu kaldırılmış";
  if (!r.t) throw `${r.rec.name}: cihaz jetonu yok — Ayarlar → Sunucular'dan yeniden eşleyin`;
  return r.t;
}

export async function callHost<T = unknown>(host: string, method: string, params?: Record<string, unknown>): Promise<T> {
  return transportOf(host).call<T>(method, params);
}
/** The core inside this app (layout, host tokens). */
export const callLocal = <T = unknown>(method: string, params?: Record<string, unknown>) =>
  WEB ? webTokenCommand<T>(method, params ?? {}, localStorage, import.meta.env.DEV, window.location.origin) : reg.local.call<T>(method, params);

function attach(l: Listener, host: string, t: Transport) {
  l.offs.set(host, t.on(l.event, (e) => l.fn(e, host)).catch(() => {}));
}

/** `event` from every host, present and future; the handler learns which host sent it. */
export function onEvent<T = unknown>(event: string, fn: (e: DexEvent<T>, host: string) => void): Unlisten {
  const l: Listener = { event, fn: fn as Handler, offs: new Map() };
  reg.listeners.add(l);
  attach(l, LOCAL, reg.local);
  for (const [id, r] of reg.remotes) if (r.t) attach(l, id, r.t);
  return () => {
    reg.listeners.delete(l);
    for (const off of l.offs.values()) void off.then((f) => f?.());
  };
}

/** Runs for the local core now and for a remote host each time it (re)connects — refetch its lists here. */
export function onHostUp(fn: (host: string) => void): Unlisten {
  reg.up.add(fn);
  queueMicrotask(() => {
    fn(LOCAL);
    for (const [id, r] of reg.remotes) if (r.status === "connected") fn(id);
  });
  return () => void reg.up.delete(fn);
}

/** Resolves once `host` is connected (now, or at its next (re)connect / re-pair). */
export function whenHostUp(host: string): Promise<void> {
  if (isHostUp(host)) return Promise.resolve();
  return new Promise((resolve) => {
    const fn = (h: string) => {
      if (h !== host) return;
      reg.up.delete(fn);
      resolve();
    };
    reg.up.add(fn);
  });
}

// ---- state for the UI ----

export const hostStatus = (host: string): HostView["status"] => (host === LOCAL ? reg.localStatus : reg.remotes.get(host)?.status ?? "unauthorized");
export const isHostUp = (host: string) => hostStatus(host) === "connected";
export const hostName = (host: string) => (host === LOCAL ? "Yerel" : reg.remotes.get(host)?.rec.name ?? host);
export const remoteHosts = () => [...reg.remotes.values()].map((r) => r.rec);

/** #142: the host's build; local = this machine's core (#150: its last hello — a restarted core
 *  may be another build). null = not known (yet). */
export const hostInfo = (host: string): ServerInfo | null => {
  if (host !== LOCAL) return readServerInfo(reg.remotes.get(host)?.t?.hello);
  const ws = reg.local.ws?.hello;
  if (!ws) return reg.localInfo;
  const info = readServerInfo(ws);
  return info && { ...info, methods: localMethods(info.methods) };
};
/**
 * Can `host` answer `method`? false only when its `auth.hello` listed its methods without it — hide
 * the feature then. Unknown (pre-#142 dexd, not connected yet) → true: call it, and the
 * "bilinmeyen metot" fallback catches an older host.
 */
export const hostSupports = (host: string, method: string) => supports(hostInfo(host), method);

function changed() {
  reg.view = [...reg.remotes.values()].map((r) => ({ rec: r.rec, status: r.status, info: readServerInfo(r.t?.hello) }));
  for (const s of reg.subs) s();
}
export function subscribeHosts(fn: () => void): Unlisten {
  reg.subs.add(fn);
  return () => void reg.subs.delete(fn);
}
export const useHosts = () => useSyncExternalStore(subscribeHosts, () => reg.view);
/** #150: the local core's connection — `down` = not running (the band, "Çekirdeği başlat"). */
export const useLocalStatus = () => useSyncExternalStore(subscribeHosts, () => reg.localStatus);
/** #203: why it is down — e.g. "yerel çekirdek açılırken çıktı (kod 2): …" (null: not told). */
export const useLocalDownReason = () => useSyncExternalStore(subscribeHosts, () => reg.localDown);
/** #150: "Çekirdeği başlat" — finds or starts `dexd --local`, then connects right away. */
export const startLocalCore = () => reg.local.start();

function setStatus(host: string, status: HostView["status"]) {
  const r = reg.remotes.get(host);
  if (!r || r.status === status) return;
  r.status = status;
  changed();
  if (status === "connected") for (const fn of reg.up) fn(host);
}

function open(rec: HostRec, token: string | null) {
  const r: Remote = { rec, t: null, status: token ? "connecting" : "no-token" };
  reg.remotes.set(rec.id, r);
  if (token) {
    r.t = new WsTransport(rec.url, token, (s) => setStatus(rec.id, s));
    for (const l of reg.listeners) attach(l, rec.id, r.t);
    // Missed events (slow connection): everything shown from this host is fetched again.
    void r.t.on("dexd.lagged", () => {
      for (const fn of reg.up) fn(rec.id);
    });
  }
  changed();
}

// ---- host list (localStorage: no secrets) ----

const KEY = "dex.hosts";
/** S2's single developer server (token in localStorage); moved into the list + keyring once. */
const LEGACY_KEY = "dex.server";

function saved(): HostRec[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((h) => h?.id && h?.url) : [];
  } catch {
    return [];
  }
}
function save() {
  localStorage.setItem(KEY, JSON.stringify(remoteHosts()));
}

const newHostId = () => `h${crypto.getRandomValues(new Uint32Array(1))[0].toString(16).padStart(8, "0").slice(0, 6)}`;
export { normalizeUrl } from "./connectionPolicy";

/** Opens every saved host (tokens from the keyring). Idempotent; never waits for a socket. */
export function initHosts(): Promise<void> {
  reg.started ??= (async () => {
    // `inproc-core` without the endpoint: its `server_info` (over the endpoint: its hello, `hostInfo`).
    reg.localInfo = await reg.local.info().then(readServerInfo, () => null);
    if (!WEB || WEB_TOKEN_STORAGE) await migrateLegacy();
    for (const rec of saved()) {
      if (reg.remotes.has(rec.id)) continue;
      const token = await callLocal<string | null>("host_token_get", { id: rec.id }).catch(() => null);
      open(rec, token);
    }
  })();
  return reg.started;
}

async function migrateLegacy() {
  let old: { url?: string; token?: string; device?: string } | null = null;
  try {
    old = JSON.parse(localStorage.getItem(LEGACY_KEY) ?? "null");
  } catch {
    /* unreadable: dropped below */
  }
  if (old?.url && old.token) {
    const rec: HostRec = { id: newHostId(), name: "Sunucu", url: old.url, device: old.device ?? "" };
    await callLocal("host_token_set", { id: rec.id, token: old.token });
    localStorage.setItem(KEY, JSON.stringify([...saved(), rec]));
  }
  localStorage.removeItem(LEGACY_KEY);
}

/** `dexd pair` code → device token (keyring) → the host is connected and listed. */
export async function addHost(name: string, address: string, code: string, deviceName: string): Promise<HostRec> {
  const url = normalizeUrl(address.trim());
  if (remoteHosts().some((h) => {
    try { return normalizeUrl(h.url) === url; }
    catch { return false; } // An invalid saved address stays listed as blocked.
  })) throw "bu adres zaten ekli";
  const { token, device } = await pairDevice(url, code, deviceName);
  const rec: HostRec = { id: newHostId(), name: name.trim() || url.replace(/^wss?:\/\//, ""), url, device };
  await callLocal("host_token_set", { id: rec.id, token });
  open(rec, token);
  save();
  return rec;
}

/** Pairs a listed host again (revoked / token lost); its teams and layout stay. */
export async function repairHost(id: string, code: string, deviceName: string) {
  const r = reg.remotes.get(id);
  if (!r) throw "bu sunucu kaldırılmış";
  const { token, device } = await pairDevice(r.rec.url, code, deviceName);
  await callLocal("host_token_set", { id, token });
  r.t?.close();
  for (const l of reg.listeners) void l.offs.get(id)?.then((f) => f?.());
  open({ ...r.rec, device }, token);
  save();
}

/** Drops the connection, the token and the entry. Its teams leave this window (they keep running on the server). */
export async function removeHost(id: string) {
  const r = reg.remotes.get(id);
  if (!r) return;
  r.t?.close();
  for (const l of reg.listeners) {
    void l.offs.get(id)?.then((f) => f?.());
    l.offs.delete(id);
  }
  reg.remotes.delete(id);
  save();
  changed();
  await callLocal("host_token_delete", { id }).catch(() => {});
}
