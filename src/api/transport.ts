// S2: the app's only door to the backend. Nothing else imports `invoke`/`listen`/`Channel`.
// - TauriTransport: the core runs inside this app; Tauri commands + events + Channels.
// - WsTransport: a `dexd` server (docs/protocol.md) — same method names, params and events.
// - LocalTransport: this app's core — #150 `dexd --local` over its loopback endpoint (a
//   WsTransport) plus Tauri for what only the shell can do (src/api/clientOnly.ts).
// S4: one of each per host, side by side (src/api/hosts.ts owns them and routes every call).
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isClientOnly, isShellEvent } from "./clientOnly";
import { PtySnapshotQueue } from "./ptySnapshotQueue";
import { normalizeUrl } from "./connectionPolicy";

/** Same shape as a Tauri event, so handlers read `e.payload` either way. */
export interface DexEvent<T> {
  event: string;
  payload: T;
}
export type Unlisten = () => void;

export interface PtyHandlers {
  /** Identity of the attached process, before its snapshot or any queued output. */
  onGeneration?: (generation?: number) => void;
  onData: (bytes: Uint8Array) => void;
  /** The process ended (or, remote, can no longer be reached). #206 `lost`: the socket came back
   *  and the core no longer has it — it went with the core (a restart), nothing on screen is live. */
  onExit: (lost?: boolean) => void;
  /** #62: the PTY's size (the snapshot's, then every change), in order with the bytes. */
  onSize?: (cols: number, rows: number) => void;
  /** #206: after a reconnect the view was repainted from the core's snapshot — the core may have
   *  restarted and this be a new session under the same id (its own size, nobody's focus yet). */
  onReattach?: () => void;
}

export interface PtyInfo {
  id: string;
  kind: "agent" | "shell";
  cols: number;
  rows: number;
  alive: boolean;
  startedAt: number;
  generation?: number;
}

interface PtySnapshot {
  scrollback: number[];
  ansi: number[];
  cursor: [number, number];
  rows: number;
  cols: number;
  alternateScreen: boolean;
}

interface PtyAttachment {
  subscriber: number;
  snapshot: PtySnapshot;
  generation?: number;
}

export type DetachPty = () => Promise<void>;

/** A PTY channel's message in order: output bytes or (#62) a new size. */
type PtyItem = Uint8Array | { cols: number; rows: number };
function deliver(h: PtyHandlers, item: PtyItem) {
  if (item instanceof Uint8Array) h.onData(item);
  else h.onSize?.(item.cols, item.rows);
}
/** Tauri data channel: raw bytes, or JSON — a byte array, or (#62) the PTY's new size. */
function tauriItem(m: ArrayBuffer | number[] | { cols: number; rows: number }): PtyItem {
  if (m instanceof ArrayBuffer) return new Uint8Array(m);
  return Array.isArray(m) ? Uint8Array.from(m) : m;
}

function paintSnapshot(snapshot: PtySnapshot, h: PtyHandlers) {
  h.onSize?.(snapshot.cols, snapshot.rows);
  if (snapshot.scrollback.length) h.onData(Uint8Array.from(snapshot.scrollback));
  if (snapshot.ansi.length) h.onData(Uint8Array.from(snapshot.ansi));
}

export interface Transport {
  /** Talking to a dexd: paths are the server's, the local folder picker does not apply. */
  readonly remote: boolean;
  call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T>;
  on<T = unknown>(event: string, fn: (e: DexEvent<T>) => void): Promise<Unlisten>;
  /** `pty_spawn`; `h` gets every byte from the first one on. */
  openPty(spawn: Record<string, unknown>, h: PtyHandlers): Promise<void>;
  listPtys(): Promise<PtyInfo[]>;
  attachPty(id: string, h: PtyHandlers): Promise<DetachPty>;
  writePty(id: string, data: string): Promise<void>;
}

export class TauriTransport implements Transport {
  readonly remote = false;
  call<T>(method: string, params?: Record<string, unknown>) {
    return invoke<T>(method, params);
  }
  on<T>(event: string, fn: (e: DexEvent<T>) => void) {
    return listen<T>(event, fn);
  }
  async openPty(spawn: Record<string, unknown>, h: PtyHandlers) {
    const onData = new Channel<ArrayBuffer | number[] | { cols: number; rows: number }>();
    onData.onmessage = (m) => deliver(h, tauriItem(m));
    const onExit = new Channel<null>();
    onExit.onmessage = () => h.onExit();
    await invoke("pty_spawn", { ...spawn, onData, onExit });
  }
  listPtys() {
    return invoke<PtyInfo[]>("pty_list");
  }
  async attachPty(id: string, h: PtyHandlers) {
    const queue = new PtySnapshotQueue();
    const onData = new Channel<ArrayBuffer | number[] | { cols: number; rows: number }>();
    onData.onmessage = (m) => {
      const item = tauriItem(m);
      queue.deliver(() => deliver(h, item));
    };
    const onExit = new Channel<null>();
    onExit.onmessage = () => queue.deliver(() => h.onExit());
    const attachment = await invoke<PtyAttachment>("pty_attach", { id, onData, onExit });
    h.onGeneration?.(attachment.generation);
    queue.flush(() => paintSnapshot(attachment.snapshot, h));
    return async () => void (await invoke("pty_detach", { id, subscriber: attachment.subscriber }));
  }
  async writePty(id: string, data: string) {
    await invoke("pty_write", { id, data });
  }
}

const PROTOCOL = 1;
/** RIS: xterm clears screen and scrollback. */
const RESET = new TextEncoder().encode("\x1bc");

interface Pending {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
}

/** Opens a socket and does one request on it (pairing: no token yet). */
function once(url: string, method: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    let ws: WebSocket;
    try {
      ws = new WebSocket(normalizeUrl(url));
    } catch (e) {
      // e.g. a secure page refusing ws:// — synchronous, so it would otherwise never settle
      return reject(`${url} açılamadı: ${e instanceof Error ? e.message : String(e)}`);
    }
    // A socket that never opens (dropped packets, blocked route) fires neither error nor close for
    // a long time; without this the pairing screen spins forever.
    const timer = setTimeout(() => {
      reject(`${url} 10 sn içinde cevap vermedi (durum: ${["bağlanıyor", "açık", "kapanıyor", "kapalı"][ws.readyState]}) — Tailscale bu cihazda açık mı?`);
      ws.close();
    }, 10_000);
    const done = () => clearTimeout(timer);
    ws.onopen = () => ws.send(JSON.stringify({ id: 1, method, params }));
    ws.onerror = () => { done(); reject(`${url} adresine bağlanılamadı`); };
    ws.onclose = (e) => { done(); reject(`sunucu cevap vermeden bağlantıyı kapattı (kod ${e.code})`); };
    ws.onmessage = (m) => {
      done();
      const r = JSON.parse(String(m.data));
      ws.close();
      if (r.error) reject(r.error.message);
      else resolve(r.result);
    };
  });
}

/** S4: shown per host (Settings → Sunucular, team tab, the grey banner). #150 `down`: the local
 *  core is not running (no discovery / no answer) — it does not come back by itself. */
export type HostStatus = "connecting" | "connected" | "reconnecting" | "unauthorized" | "blocked" | "down";

/** #150: the shell's `local_host` — the local core's endpoint, or why there is none. */
export type LocalAnswer = { url: string; token: string } | { down: string };

export class WsTransport implements Transport {
  private ws: WebSocket | null = null;
  private ready!: Promise<void>;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private listeners = new Map<string, Set<(e: DexEvent<unknown>) => void>>();
  private nextChannel = 1;
  /** Channel → PTY; `byId` = the newest channel of each PTY (where keystrokes go). */
  private chans = new Map<number, { id: string; h: PtyHandlers; queue: PtySnapshotQueue }>();
  private byId = new Map<string, number>();
  private retry = 0;
  private dead = false;
  /** `close()`d: the host was removed — no reconnect. */
  private closed = false;
  /** #142: the last `auth.hello` result (the host's build and methods; src/api/serverInfo.ts). */
  hello: unknown = null;
  private timer: ReturnType<typeof setTimeout> | undefined;

  /**
   * `remote = false`: #146's loopback endpoint of this app's own core (the local folder picker
   * applies). #150 `locate`: the local core's address is not fixed (a new `dexd --local` = a new
   * port and token) — asked again before every reconnect (and first, with no `url`); no answer =
   * `down`, retried with the same backoff.
   */
  constructor(private url: string, private token: string, private onStatus: (s: HostStatus) => void = () => {}, readonly remote = true, private locate?: () => Promise<LocalAnswer>) {
    this.connect();
  }

  close() {
    this.closed = this.dead = true;
    clearTimeout(this.timer);
    this.ws?.close();
  }

  /** #150: try now (the local core was just started), not at the next backoff tick. */
  wake() {
    const s = this.ws?.readyState;
    if (this.closed || s === WebSocket.OPEN || s === WebSocket.CONNECTING) return;
    clearTimeout(this.timer);
    this.connect();
  }

  private later() {
    this.retry++;
    this.timer = setTimeout(() => this.connect(), Math.min(10_000, 500 * 2 ** (this.retry - 1)));
  }

  private connect() {
    if (this.closed) return;
    let opened!: () => void, failed!: (e: unknown) => void;
    this.ready = new Promise((res, rej) => ((opened = res), (failed = rej)));
    this.ready.catch(() => {});
    if (!this.locate || (this.retry === 0 && this.url)) return this.open(opened, failed);
    const locate = this.locate;
    void locate()
      .then((h) => ("url" in h ? h : Promise.reject(h.down)))
      .then(
        (h) => {
          if (this.closed) return;
          this.url = h.url;
          this.token = h.token;
          this.open(opened, failed);
        },
        (why) => {
          failed(`yerel çekirdek çalışmıyor: ${why}`);
          if (this.closed) return;
          this.onStatus("down");
          this.later();
        },
      );
  }

  private open(opened: () => void, failed: (e: unknown) => void) {
    this.onStatus(this.retry === 0 ? "connecting" : "reconnecting");
    let ws: WebSocket;
    try {
      ws = new WebSocket(normalizeUrl(this.url));
    } catch (e) {
      failed(e instanceof Error ? e.message : String(e));
      this.onStatus("blocked");
      return;
    }
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => {
      const id = this.nextId++;
      this.pending.set(id, {
        resolve: (v) => {
          this.hello = v;
          this.retry = 0;
          opened();
          void this.reattach();
          this.onStatus("connected");
        },
        reject: failed,
      });
      ws.send(JSON.stringify({ id, method: "auth.hello", params: { protocol: PROTOCOL, token: this.token } }));
    };
    ws.onmessage = (m) => (typeof m.data === "string" ? this.onText(m.data) : this.onBinary(m.data as ArrayBuffer));
    ws.onclose = (e) => {
      const why = e.code === 4001 || e.code === 4003 ? "sunucu bu cihazı tanımıyor (eşleme iptal edilmiş olabilir)" : "sunucu bağlantısı koptu";
      for (const p of this.pending.values()) p.reject(why);
      this.pending.clear();
      failed(why);
      if (this.closed) return;
      // The local core's token is per start: a refusal means a new core — locate it again.
      if ((e.code === 4001 || e.code === 4003) && !this.locate) {
        this.dead = true;
        this.onStatus("unauthorized");
        return;
      }
      // PTYs live on in the server; after the reconnect their channels are attached again.
      this.onStatus("reconnecting");
      this.later();
    };
  }

  private onText(text: string) {
    const m = JSON.parse(text);
    if ("event" in m) {
      if (m.event === "pty.exit") return this.exited(m.payload.channel);
      if (m.event === "pty.size") return this.item(m.payload.channel, { cols: m.payload.cols, rows: m.payload.rows });
      return this.emit(m.event, m.payload);
    }
    const p = this.pending.get(m.id);
    if (!p) return;
    this.pending.delete(m.id);
    // Like Tauri's invoke: the rejection value is the core's error text.
    if (m.error) p.reject(m.error.message);
    else p.resolve(m.result);
  }

  private onBinary(buf: ArrayBuffer) {
    this.item(new DataView(buf).getUint32(0), new Uint8Array(buf, 4));
  }

  private item(channel: number, item: PtyItem) {
    const c = this.chans.get(channel);
    if (!c) return;
    c.queue.deliver(() => deliver(c.h, item));
  }

  private emit(event: string, payload: unknown) {
    for (const fn of this.listeners.get(event) ?? []) fn({ event, payload });
  }

  private exited(channel: number, lost = false) {
    const c = this.chans.get(channel);
    if (!c) return;
    c.queue.deliver(() => {
      if (this.chans.get(channel) !== c) return;
      this.chans.delete(channel);
      if (this.byId.get(c.id) === channel) this.byId.delete(c.id);
      c.h.onExit(lost);
    });
  }

  /**
   * After a reconnect: the same channel numbers on the new socket. A PTY that died meanwhile exits.
   * S4: the view still holds what it showed before the drop; the snapshot replaces it (a full reset
   * first — painted on top, the whole scrollback would repeat after every drop).
   */
  private async reattach() {
    for (const [channel, c] of [...this.chans]) {
      c.queue = new PtySnapshotQueue();
      await this.request<PtyAttachment>("pty.attach", { id: c.id, channel })
        .then((attachment) => {
          c.h.onGeneration?.(attachment.generation);
          c.queue.flush(() => {
            c.h.onData(RESET);
            paintSnapshot(attachment.snapshot, c.h);
          });
          if (this.chans.get(channel) === c) c.h.onReattach?.();
        })
        .catch(() => { c.queue.flush(() => {}); this.exited(channel, true); });
    }
  }

  private request<T>(method: string, params?: Record<string, unknown>): Promise<T> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return Promise.reject("sunucuya bağlı değil");
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      ws.send(JSON.stringify({ id, method, params: params ?? {} }));
    });
  }

  async call<T>(method: string, params?: Record<string, unknown>) {
    if (this.dead) throw "sunucu bu cihazı tanımıyor — Ayarlar'dan yeniden eşleyin";
    await this.ready;
    return this.request<T>(method, params);
  }

  async on<T>(event: string, fn: (e: DexEvent<T>) => void) {
    const set = this.listeners.get(event) ?? new Set();
    this.listeners.set(event, set);
    const f = fn as (e: DexEvent<unknown>) => void;
    set.add(f);
    return () => void set.delete(f);
  }

  async openPty(spawn: Record<string, unknown>, h: PtyHandlers) {
    const channel = this.nextChannel++;
    const id = String(spawn.id);
    // Registered first: output can arrive before the reply.
    const queue = new PtySnapshotQueue();
    queue.flush(() => {});
    this.chans.set(channel, { id, h, queue });
    this.byId.set(id, channel);
    try {
      await this.call("pty_spawn", { ...spawn, channel });
    } catch (e) {
      this.chans.delete(channel);
      if (this.byId.get(id) === channel) this.byId.delete(id);
      throw e;
    }
  }

  listPtys() {
    return this.call<PtyInfo[]>("pty.list");
  }

  async attachPty(id: string, h: PtyHandlers) {
    const channel = this.nextChannel++;
    const c = { id, h, queue: new PtySnapshotQueue() };
    this.chans.set(channel, c);
    this.byId.set(id, channel);
    try {
      const attachment = await this.call<PtyAttachment>("pty.attach", { id, channel });
      h.onGeneration?.(attachment.generation);
      c.queue.flush(() => paintSnapshot(attachment.snapshot, h));
    } catch (e) {
      this.chans.delete(channel);
      if (this.byId.get(id) === channel) this.byId.delete(id);
      throw e;
    }
    return async () => {
      await this.call<void>("pty.detach", { channel });
      this.chans.delete(channel);
      if (this.byId.get(id) === channel) this.byId.delete(id);
    };
  }

  async writePty(id: string, data: string) {
    const channel = this.byId.get(id);
    if (channel === undefined || this.ws?.readyState !== WebSocket.OPEN) return this.call<void>("pty_write", { id, data });
    const bytes = new TextEncoder().encode(data);
    const frame = new Uint8Array(4 + bytes.length);
    new DataView(frame.buffer).setUint32(0, channel);
    frame.set(bytes, 4);
    this.ws.send(frame);
  }
}

/**
 * This app's core. #150 (B): `dexd --local`, its own process — `local_host` gives its loopback
 * endpoint (or why it is down), and every core call, event and PTY goes over that socket; only the
 * shell's own commands/events stay on Tauri (src/api/clientOnly.ts). The core outlives this page:
 * a reload or a new window attaches to the same PTYs. When it stops, the socket goes `down` and
 * keeps looking (`local_host` again, never starting it); `start()` = "Çekirdeği başlat".
 * `local_host` → null: an `inproc-core` shell without DEX_LOCAL_WS — the core is inside it,
 * TauriTransport as before A0. No such command (mobile): Tauri, for the shell's own commands.
 */
export class LocalTransport implements Transport {
  readonly remote = false;
  private tauri = new TauriTransport();
  /** The endpoint's socket, once `local_host` gave one. */
  ws: WsTransport | null = null;
  private picked: Promise<Transport>;

  /**
   * `refetch`: the socket came back after a drop / a new core, or missed events (`dexd.lagged`).
   * `onStatus`: the local core's connection (the band; src/api/hosts.ts). `onDown`: why it is
   * down, as `local_host` said (#203).
   */
  constructor(refetch: () => void = () => {}, onStatus: (s: HostStatus) => void = () => {}, onDown: (why: string) => void = () => {}) {
    let dropped = false;
    const status = (s: HostStatus) => {
      onStatus(s);
      if (s === "reconnecting" || s === "down") dropped = true;
      else if (s === "connected" && dropped) {
        dropped = false;
        refetch();
      }
    };
    const told = <T extends LocalAnswer | null>(h: T) => {
      if (h && "down" in h) onDown(h.down);
      return h;
    };
    const locate = () => invoke<LocalAnswer>("local_host").then(told);
    this.picked = invoke<LocalAnswer | null>("local_host").then(told).then(
      (h) => {
        if (!h) return this.tauri;
        const up = "url" in h ? h : { url: "", token: "" };
        const ws = new WsTransport(up.url, up.token, status, false, locate);
        void ws.on("dexd.lagged", refetch);
        return (this.ws = ws);
      },
      () => this.tauri,
    );
  }

  /** #150: start the local core (or find the running one) and connect to it now. */
  async start(): Promise<void> {
    await this.tauri.call("local_start");
    await this.picked;
    this.ws?.wake();
  }

  /** This core's build (`server_info`): from the endpoint when there is one. */
  async info(): Promise<unknown> {
    return (await this.picked).call("server_info");
  }
  async call<T>(method: string, params?: Record<string, unknown>) {
    if (isClientOnly(method)) return this.tauri.call<T>(method, params);
    return (await this.picked).call<T>(method, params);
  }
  async on<T>(event: string, fn: (e: DexEvent<T>) => void) {
    if (isShellEvent(event)) return this.tauri.on<T>(event, fn);
    return (await this.picked).on<T>(event, fn);
  }
  async openPty(spawn: Record<string, unknown>, h: PtyHandlers) {
    return (await this.picked).openPty(spawn, h);
  }
  async listPtys() {
    return (await this.picked).listPtys();
  }
  async attachPty(id: string, h: PtyHandlers) {
    return (await this.picked).attachPty(id, h);
  }
  async writePty(id: string, data: string) {
    return (await this.picked).writePty(id, data);
  }
}

/** Trades a `dexd pair` code for a device token (the socket closes right after). */
export async function pairDevice(url: string, code: string, name: string) {
  const r = await once(url, "auth.pair", { protocol: PROTOCOL, code, name });
  return { token: String(r.token), device: (r.device as { id: string }).id };
}
