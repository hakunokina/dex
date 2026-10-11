import { callHost } from "../api/hosts";
import type { GatewayModel, GatewayStatus } from "./gatewayView";

export const gatewayStatus = (host: string) => callHost<GatewayStatus>(host, "gateway_auth_status");
export const gatewayStart = (host: string, accountId?: string) =>
  callHost<{ authorizationUrl: string; expiresAt: number }>(host, "gateway_auth_start", { accountId: accountId ?? null });
export const gatewayLogout = (host: string, accountId: string) =>
  callHost<{ remoteRevoked: boolean }>(host, "gateway_auth_logout", { accountId });
const CATALOG_EVENT = "dex:gateway-catalog-changed";
export async function gatewayModels(host: string) {
  try { return await callHost<GatewayModel[]>(host, "gateway_models"); }
  finally { window.dispatchEvent(new CustomEvent(CATALOG_EVENT, { detail: host })); }
}
/** Core auth events precede the explicit catalog fetch; wake already open pickers afterwards. */
export function onGatewayCatalogChanged(refresh: (host: string) => void) {
  const listener = (event: Event) => refresh((event as CustomEvent<string>).detail);
  window.addEventListener(CATALOG_EVENT, listener);
  return () => window.removeEventListener(CATALOG_EVENT, listener);
}
