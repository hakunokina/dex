import { isLocalOrTailnet } from "./connectionPolicy.ts";

export function browserTokensAllowed(dev: boolean, origin: string): boolean {
  if (dev) return true;
  try {
    const url = new URL(origin);
    return (url.protocol === "http:" || url.protocol === "https:") && isLocalOrTailnet(url.hostname);
  } catch {
    return false;
  }
}

/** Browser fallback only; native clients keep the OS secret store. */
export async function webTokenCommand<T>(method: string, p: Record<string, unknown>, storage: Storage, dev: boolean, origin: string): Promise<T> {
  const key = `dex.webtoken.${String(p.id)}`;
  // Deletion stays available so an unsupported origin can remove old credentials.
  if (method === "host_token_delete") return void storage.removeItem(key) as T;
  const allowed = browserTokensAllowed(dev, origin);
  if (method === "host_token_get") return (allowed ? storage.getItem(key) : null) as T;
  if (method === "host_token_set" && !allowed) {
    throw "tarayıcıda cihaz jetonu saklama yalnız geliştirme veya yerel/Tailscale origin'inde kullanılabilir";
  }
  if (method === "host_token_set") return void storage.setItem(key, String(p.token)) as T;
  throw `tarayıcı modunda yerel komut yok: ${method}`;
}
