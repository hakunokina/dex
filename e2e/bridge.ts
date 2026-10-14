// Runs in the page before the app (page.addInitScript): stands in for the Tauri runtime so the
// DESKTOP shell (src/App.tsx) runs in a plain browser, with a real dexd as its "local" core (#150:
// the app's own socket to it, as with `dexd --local`; or, `localWs: false`, through this IPC
// bridge — every core command then goes to dexd over the bridge's WebSocket)
// (docs/protocol.md: same names, same arguments, same results). Tauri-only bits (window, dialog,
// notification, host keyring) get inert answers. Self-contained: serialized into the page.

export interface BridgeConfig {
  url: string;
  token: string;
  /** The folder the native picker answers with (`plugin:dialog|open`). */
  pick: string | null;
  /**
   * #150 (default, as the app now runs): `local_host` hands the app this dexd, so the app's own
   * LocalTransport talks to it over WebSocket and only the shell's commands come through here
   * (its requests are recorded too, below). `false` (`DEX_LOCAL_WS=0`): `local_host` is null —
   * an `inproc-core` shell — and every core call is bridged through `invoke`.
   */
  localWs?: boolean;
}

/**
 * An older dexd, faked over the real one (set before the app loads: `setOldServer`). Works the same
 * whether core calls come through `invoke` or the app's own socket (#146), so a spec never depends
 * on `DEX_LOCAL_WS`. `hide`: left out of `server_info` / `auth.hello` methods (#142 list without
 * them). `unknown`: answered as dexd answers a method it lacks (methods.rs `unknown_method`).
 */
export interface OldServer {
  hide?: string[];
  unknown?: string[];
  /** #151: another build in `auth.hello` / `server_info` (the "Çekirdek güncel değil" band). */
  commit?: string;
  commitTime?: number;
}

/** #151: the local core is not running while this is set (survives a reload); `local_start` clears it.
 *  #203: a value other than "1" is the shell's reason (`local_host`'s `down`). */
export const LOCAL_DOWN_KEY = "dex.e2e.localDown";
/** #203: `local_host` answers after this many ms (the shell finding / starting the core). */
export const LOCAL_SLOW_KEY = "dex.e2e.localSlowMs";
/** #151: dexd's `core_life` as `dexd --local` answers it (the worker's dexd is `serve`: local_only). */
export const FAKE_LIFE = { pid: 4242, commit: "e2e0000000", commitTime: 1700000000, startedAt: 1700000100, clients: 2, graceSecs: 120, shutdownInSecs: null, agents: 1 };

export interface RecordedCall {
  cmd: string;
  args: Record<string, unknown>;
}

declare global {
  interface Window {
    /** Every command the app invoked, in order (channels dropped) — for assertions. */
    __dexCalls: RecordedCall[];
    __dexOld?: OldServer;
    /** The app's own sockets to dexd (#206: a spec closes them as a core restart would). */
    __dexSockets?: WebSocket[];
    /** #209: canned results by method (set before the app loads) — data a spec cannot make dexd produce. */
    __dexFake?: Record<string, unknown>;
  }
}

export function installBridge(cfg: BridgeConfig) {
  type Cb = (data: unknown) => void;
  const callbacks = new Map<number, Cb>();
  let nextCb = 1;
  const transformCallback = (cb: Cb, once = false) => {
    const id = nextCb++;
    callbacks.set(id, (d) => {
      if (once) callbacks.delete(id);
      cb(d);
    });
    return id;
  };
  const fire = (id: number, data: unknown) => callbacks.get(id)?.(data);

  // Tauri channels deliver `{index, message}` in order; one counter per channel callback.
  const chanIndex = new Map<number, number>();
  const toChannel = (id: number, message: unknown) => {
    const index = chanIndex.get(id) ?? 0;
    chanIndex.set(id, index + 1);
    fire(id, { index, message });
  };
  const isChannel = (v: unknown): v is { id: number } =>
    !!v && typeof v === "object" && typeof (v as { id?: unknown }).id === "number" && "onmessage" in (v as object);

  // ---- dexd socket ----
  let ws!: WebSocket;
  let nextId = 1;
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>();
  const ptyChans = new Map<number, { data: number; exit: number }>();
  let nextPtyChan = 1;
  const listeners: { event: string; handler: number; eid: number }[] = [];
  let nextEid = 1;

  const emitLocal = (event: string, payload: unknown) => {
    for (const l of listeners.filter((l) => l.event === event)) fire(l.handler, { event, id: l.eid, payload });
  };

  const ready = new Promise<void>((resolve, reject) => {
    ws = new WebSocket(cfg.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      const id = nextId++;
      pending.set(id, { resolve: () => resolve(), reject });
      ws.send(JSON.stringify({ id, method: "auth.hello", params: { protocol: 1, token: cfg.token } }));
    };
    ws.onerror = () => reject("dexd bağlantısı kurulamadı");
    ws.onmessage = (m) => {
      if (typeof m.data !== "string") {
        const buf = m.data as ArrayBuffer;
        const c = ptyChans.get(new DataView(buf).getUint32(0));
        if (c) toChannel(c.data, buf.slice(4));
        return;
      }
      const msg = JSON.parse(m.data);
      if ("event" in msg) {
        if (msg.event === "pty.exit") {
          const c = ptyChans.get(msg.payload.channel);
          ptyChans.delete(msg.payload.channel);
          if (c) toChannel(c.exit, null);
          return;
        }
        // #62: the shell sends the PTY's new size on the data channel as JSON.
        if (msg.event === "pty.size") {
          const c = ptyChans.get(msg.payload.channel);
          if (c) toChannel(c.data, { cols: msg.payload.cols, rows: msg.payload.rows });
          return;
        }
        return emitLocal(msg.event, msg.payload);
      }
      const p = pending.get(msg.id);
      if (!p) return;
      pending.delete(msg.id);
      if (msg.error) p.reject(msg.error.message);
      else p.resolve(msg.result);
    };
  });

  // ---- an older dexd (OldServer), read per call: a spec sets it after this script ran ----
  const unknownError = (method: string) =>
    window.__dexOld?.unknown?.includes(method) ? { code: "unknown_method", message: `bilinmeyen metot: ${method}` } : null;
  const ageResult = (method: string, result: unknown) => {
    const old = window.__dexOld;
    if ((method !== "server_info" && method !== "auth.hello") || !old) return result;
    let r = result as { methods?: unknown };
    if (old.commit !== undefined) r = { ...r, commit: old.commit, commitTime: old.commitTime };
    const hide = old.hide;
    if (hide?.length && Array.isArray(r?.methods)) r = { ...r, methods: r.methods.filter((m: string) => !hide.includes(m)) };
    return r === result ? result : r;
  };
  // Serialized into the page: no imports (keep equal to FAKE_LIFE / LOCAL_DOWN_KEY above).
  const fakeLife = { pid: 4242, commit: "e2e0000000", commitTime: 1700000000, startedAt: 1700000100, clients: 2, graceSecs: 120, shutdownInSecs: null, agents: 1 };
  const downKey = "dex.e2e.localDown";
  const slowKey = "dex.e2e.localSlowMs";

  const request = async (method: string, params: Record<string, unknown>) => {
    const err = unknownError(method);
    if (err) throw err.message;
    if (window.__dexFake && method in window.__dexFake) return window.__dexFake[method];
    await ready;
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve: (v) => resolve(ageResult(method, v)), reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  };

  const openChan = (data: { id: number }, exit: { id: number }) => {
    const ch = nextPtyChan++;
    ptyChans.set(ch, { data: data.id, exit: exit.id });
    return ch;
  };

  window.__dexCalls = [];
  // #146: the app's own socket to dexd (LocalTransport) bypasses `invoke`; its requests are
  // recorded too (dexd names: `pty.attach`, not `pty_attach`), so `calls()` still sees them.
  // Keystrokes are binary frames there; they are recorded as the `pty_write` they replace.
  if (cfg.localWs) {
    const send = WebSocket.prototype.send;
    const chanPty = new Map<number, string>();
    // OldServer on the app's socket: requests by id (per socket), answers rewritten before the app sees them.
    const asked = new WeakMap<WebSocket, Map<number, string>>();
    const ours = new WeakSet<Event>();
    const answer = (sock: WebSocket, msg: unknown) => {
      const ev = new MessageEvent("message", { data: JSON.stringify(msg) });
      ours.add(ev);
      sock.dispatchEvent(ev);
    };
    const Native = window.WebSocket;
    // The transport normalizes URLs; compare browser-canonical endpoints (including path/query).
    const endpoint = new URL(cfg.url).href;
    window.WebSocket = class extends Native {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        if (this.url === endpoint) (window.__dexSockets ??= []).push(this);
        // Registered before the app sets `onmessage`, so it runs first.
        this.addEventListener("message", (ev) => {
          if (ours.has(ev) || typeof ev.data !== "string") return;
          const msg = JSON.parse(ev.data);
          const method = typeof msg.id === "number" ? asked.get(this)?.get(msg.id) : undefined;
          if (!method) return;
          asked.get(this)!.delete(msg.id);
          if (!("result" in msg)) return;
          const result = ageResult(method, msg.result);
          if (result === msg.result) return;
          ev.stopImmediatePropagation();
          answer(this, { ...msg, result });
        });
      }
    };
    WebSocket.prototype.send = function (this: WebSocket, data) {
      if (this !== ws && typeof data === "string") {
        const m = JSON.parse(data);
        if (typeof m.method === "string" && !m.method.startsWith("auth.")) window.__dexCalls.push({ cmd: m.method, args: m.params ?? {} });
        if (typeof m.params?.channel === "number" && typeof m.params?.id === "string") chanPty.set(m.params.channel, m.params.id);
        if (typeof m.method === "string" && typeof m.id === "number") {
          const err = unknownError(m.method);
          if (err) return void setTimeout(() => answer(this, { id: m.id, error: err }));
          if (m.method === "core_life") return void setTimeout(() => answer(this, { id: m.id, result: fakeLife }));
          const fake = window.__dexFake;
          // A Promise fake answers when it settles (a never-settling one keeps the call pending).
          if (fake && m.method in fake) return void setTimeout(() => void Promise.resolve(fake[m.method]).then(result => answer(this, { id: m.id, result })));
          if (!asked.has(this)) asked.set(this, new Map());
          asked.get(this)!.set(m.id, m.method);
        }
      } else if (this !== ws && data instanceof Uint8Array) {
        const id = chanPty.get(new DataView(data.buffer, data.byteOffset).getUint32(0));
        if (id) window.__dexCalls.push({ cmd: "pty_write", args: { id, data: new TextDecoder().decode(data.subarray(4)) } });
      }
      return send.call(this, data);
    };
  }
  const store = (id: unknown) => `dex.e2e.token.${String(id)}`;

  async function invoke(cmd: string, args: Record<string, unknown> = {}) {
    const plain = Object.fromEntries(Object.entries(args).filter(([, v]) => !isChannel(v)));
    window.__dexCalls.push({ cmd, args: plain });
    if (cmd === "plugin:event|listen") {
      const eid = nextEid++;
      listeners.push({ event: String(args.event), handler: args.handler as number, eid });
      return eid;
    }
    if (cmd === "plugin:event|unlisten") {
      const i = listeners.findIndex((l) => l.eid === args.eventId);
      if (i >= 0) listeners.splice(i, 1);
      return null;
    }
    if (cmd === "plugin:event|emit" || cmd === "plugin:event|emit_to") return emitLocal(String(args.event), args.payload), null;
    if (cmd === "plugin:notification|is_permission_granted") return false;
    if (cmd === "plugin:notification|request_permission") return "denied";
    if (cmd === "plugin:dialog|open") return cfg.pick;
    if (cmd.startsWith("plugin:window|")) return cmd.endsWith("|is_maximized") ? false : null;
    if (cmd.startsWith("plugin:")) return null;
    // #150: the core is the worker's dexd; #151: "not running" while LOCAL_DOWN_KEY is set.
    if (cmd === "local_host") {
      await new Promise((r) => setTimeout(r, Number(localStorage.getItem(slowKey)) || 0));
      const down = localStorage.getItem(downKey);
      return !cfg.localWs ? null : down ? { down: down === "1" ? "e2e: çekirdek kapalı" : down } : { url: cfg.url, token: cfg.token };
    }
    if (cmd === "local_start" || cmd === "local_restart") return localStorage.removeItem(downKey), localStorage.removeItem(slowKey), { url: cfg.url, token: cfg.token };
    if (cmd === "local_status") return { running: true, url: cfg.url, pid: null, commit: null, commitTime: null, startedAt: null, launched: null };
    if (cmd === "local_shutdown") return null;
    if (cmd === "core_life") return fakeLife;
    if (cmd === "host_token_get") return localStorage.getItem(store(args.id));
    if (cmd === "host_token_set") return localStorage.setItem(store(args.id), String(args.token)), null;
    if (cmd === "host_token_delete") return localStorage.removeItem(store(args.id)), null;
    if (cmd === "pty_spawn") {
      const channel = openChan(args.onData as { id: number }, args.onExit as { id: number });
      return request("pty_spawn", { ...plain, channel });
    }
    if (cmd === "pty_attach") {
      const channel = openChan(args.onData as { id: number }, args.onExit as { id: number });
      const r = (await request("pty.attach", { id: args.id, channel })) as { snapshot: unknown };
      // The subscriber the app hands back to pty_detach is our channel.
      return { subscriber: channel, snapshot: r.snapshot };
    }
    if (cmd === "pty_detach") {
      ptyChans.delete(args.subscriber as number);
      return request("pty.detach", { channel: args.subscriber });
    }
    if (cmd === "pty_list") return request("pty.list", {});
    return request(cmd, plain);
  }

  const internals = {
    invoke,
    transformCallback,
    unregisterCallback: (id: number) => void callbacks.delete(id),
    convertFileSrc: (p: string) => p,
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
    plugins: {},
  };
  Object.assign(window, {
    __TAURI_INTERNALS__: internals,
    __TAURI_EVENT_PLUGIN_INTERNALS__: {
      unregisterListener: (_event: string, eid: number) => {
        const i = listeners.findIndex((l) => l.eid === eid);
        if (i >= 0) listeners.splice(i, 1);
      },
    },
  });
}
