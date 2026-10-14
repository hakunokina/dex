// #1719 Android bildirim köprüsü: Kotlin `PushPlugin` (Rust: src-tauri/src/mobile_hosts.rs `push_*`;
// kabuk komutları, çekirdeğe gitmez). Yalnız Android'de var; masaüstünde çağrılar reddedilir.
import { TauriTransport } from "./transport";

/** FCM jetonu yenilendi (Kotlin `DexMessagingService.onNewToken`) → `pushToken` yeniden okunur. */
export const PUSH_TOKEN_EVENT = "dex-push-token";
/** Uygulama açıkken bildirime dokunuldu → `takeReplyLink` bekleyen bağlantıyı verir. */
export const REPLY_LINK_EVENT = "dex-open-reply";

const tauri = new TauriTransport();

/** FCM jetonu; Firebase yapılandırılmadıysa (google-services.json'suz APK) `null`. */
export const pushToken = () => tauri.call<{ token?: string | null }>("push_token").then((r) => r?.token || null);

/** Bekleyen bildirim bağlantısı (`dex://reply/<id>`), bir kez; yoksa `null`. */
export const takeReplyLink = () => tauri.call<{ uri?: string | null }>("push_take").then((r) => r?.uri || null);

/** http/https bağlantıyı sistem tarayıcısında açar (Kotlin ACTION_VIEW; başka şema reddedilir). */
export const openExternal = (url: string) => tauri.call<void>("push_open_url", { url });
