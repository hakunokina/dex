// #150 (B): the local core is `dexd --local`, its own process; the webview talks to it over its
// loopback endpoint like to any host. What stays with the desktop shell (Tauri `invoke` / Tauri
// events): finding/starting/stopping that core, this machine's screen, keyring, log folder. The tailnet listener moved into dexd (its local-only methods). Pure: unit-tested;
// crates/dexd/src/methods.rs `tauri_registers_only_client_only_commands` keeps both lists equal.

export const CLIENT_ONLY: readonly string[] = [
  "local_host",
  "local_start",
  "local_proof",
  "local_status",
  "local_shutdown",
  "local_restart",
  "local_drain_cancel",
  "logs_open",
  "host_token_get",
  "host_token_set",
  "host_token_delete",
  "snip_start",
  "snip_finish",
  "snip_cancel",
  "snip_hotkey_get",
  "snip_hotkey_set",
];

/** Events the shell emits itself, not the core. #2258: a leader pop-out window's closing (src-tauri/src/popout.rs)
 *  and what that window's page reports to the main one (its place, its on-top switch; src/lead/LeadWindow.tsx). */
export const SHELL_EVENTS: readonly string[] = ["dex:lead-closed", "dex:lead-bounds", "dex:lead-ontop"];

export const isClientOnly = (method: string) => CLIENT_ONLY.includes(method);
export const isShellEvent = (event: string) => SHELL_EVENTS.includes(event);

/**
 * The local host's methods: what its endpoint advertised plus what the shell answers. Unknown
 * (null) stays unknown — `supports` then tries every call.
 */
export const localMethods = (advertised: string[] | null): string[] | null =>
  advertised && [...new Set([...advertised, ...CLIENT_ONLY])];
