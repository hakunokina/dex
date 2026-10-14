// #1719 FCM jetonunu eşlenmiş her hosta kaydeder (`push_register`): açılışta, host (yeniden)
// bağlandığında ve jeton yenilendiğinde. Kural saf (`pushRegisterNeeded`, birim testli); host metodu
// ilan etmediyse ya da Firebase'siz APK'da (jeton yok) sessizce atlanır. Jeton loglanmaz.
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { hostInfo, isHostUp, onHostUp, remoteHosts } from "../api/hosts";
import { PUSH_TOKEN_EVENT, pushToken } from "../api/push";
import { pushRegister } from "../api/replies";
import { pushRegisterNeeded } from "./replyLogic";

export function startPush(): () => void {
  let token: string | null = null;
  const sent = new Map<string, string>();
  const sync = (host: string) => {
    if (!token || !pushRegisterNeeded({ host, token, methods: hostInfo(host)?.methods ?? null, sentToken: sent.get(host) })) return;
    const t = token;
    sent.set(host, t);
    // Hata (eski dexd, kopan bağlantı): bir sonraki bağlanışta yeniden denenir.
    pushRegister(host, t).catch(() => sent.get(host) === t && sent.delete(host));
  };
  const pull = () =>
    void pushToken().then(
      (t) => {
        token = t;
        for (const h of remoteHosts()) if (isHostUp(h.id)) sync(h.id);
      },
      () => {}, // köprü yok (masaüstü önizleme / eski APK)
    );
  const off = onHostUp(sync);
  window.addEventListener(PUSH_TOKEN_EVENT, pull);
  pull();
  return () => {
    off();
    window.removeEventListener(PUSH_TOKEN_EVENT, pull);
  };
}

const ASKED = "dex.notify.asked";

/** Android 13+ bildirim izni — ilk paylaşımda bir kez sorulur (sonra Echo cevapları ekranından). */
export async function askNotifyOnce(): Promise<void> {
  try {
    if (localStorage.getItem(ASKED)) return;
    localStorage.setItem(ASKED, "1");
  } catch {
    return;
  }
  if (!(await isPermissionGranted().catch(() => true))) await requestPermission().catch(() => {});
}
