export interface GatewayAccount {
  id: string;
  label: string;
  email: string | null;
  signedIn: boolean;
  plan?: string;
  /** Epoch milliseconds; only metadata crosses IPC. */
  expiresAt?: number;
}
export interface GatewayModel { id: string; label: string }
export interface GatewayStatus {
  state: "signed_out" | "signing_in" | "ready" | "error";
  activeAccountId: string | null;
  accounts: GatewayAccount[];
  models: GatewayModel[];
  error: string | null;
}
// Never reflect an arbitrary RPC/upstream response into the auth UI.
export function gatewayError(error: unknown): string {
  const code = error instanceof Error ? error.message : String(error);
  switch (code) {
    case "gateway_login_required":
    case "gateway_account_not_found":
    case "gateway_account_inactive": return "Giriş yok veya oturum süresi doldu. ChatGPT ile yeniden giriş yapın.";
    case "gateway_auth_rate_limited": return "Kota veya istek sınırına ulaşıldı. Daha sonra yeniden deneyin.";
    case "gateway_account_ineligible":
    case "gateway_auth_rejected":
    case "gateway_scope_missing": return "Bu hesabın model erişim izni yok. Hesabı ve ChatGPT izinlerini kontrol edin.";
    case "gateway_auth_denied": return "ChatGPT giriş izni reddedildi. Yeniden giriş yapabilirsiniz.";
    case "gateway_auth_expired": return "Giriş bağlantısının süresi doldu. Yeniden giriş başlatın.";
    case "gateway_auth_cancelled": return "Giriş iptal edildi. Yeniden giriş yapabilirsiniz.";
    case "gateway_auth_transport":
    case "gateway_auth_unavailable": return "ChatGPT bağlantısı kurulamadı. Bağlantınızı kontrol edip yeniden deneyin.";
  }
  return "ChatGPT bağlantısı tamamlanamadı. Yeniden deneyin.";
}
