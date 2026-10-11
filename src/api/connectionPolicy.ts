/** URL.hostname (already canonicalized by the URL parser), never substring matching. */
export function isLocalOrTailnet(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/\.$/, "");
  if (host === "localhost" || host === "[::1]" || host.endsWith(".ts.net")) return true;
  const ip = host.split(".").map(Number);
  return /^\d+\.\d+\.\d+\.\d+$/.test(host) && ip.every((n) => n >= 0 && n <= 255)
    && (ip[0] === 127 || (ip[0] === 100 && ip[1] >= 64 && ip[1] <= 127));
}

export function normalizeUrl(address: string): string {
  const raw = address.trim();
  const explicit = /^[a-z][a-z\d+.-]*:\/\//i.test(raw);
  const url = new URL(explicit ? raw : `wss://${raw}`);
  if (!["ws:", "wss:"].includes(url.protocol) || url.username || url.password || url.hash) {
    throw new Error("Geçerli bir ws:// veya wss:// sunucu adresi girin (kullanıcı bilgisi ve # olmadan)");
  }
  if (!explicit && isLocalOrTailnet(url.hostname)) url.protocol = "ws:";
  if (url.protocol === "ws:" && !isLocalOrTailnet(url.hostname)) {
    throw new Error("şifresiz bağlantı yalnız yerel/Tailscale adreslerinde kullanılabilir; bu adres için wss:// girin");
  }
  return url.href;
}
