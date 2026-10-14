// Android "Paylaş" hedefi köprüsü: Kotlin `ShareTargetPlugin` bekleyen paylaşımı bir kez verir
// (Rust: src-tauri/src/mobile_hosts.rs `share_take`; kabuk komutu, çekirdeğe gitmez). Yalnız Android'de var.
import { TauriTransport } from "./transport";

export interface RawShare {
  text?: string;
  subject?: string;
  image?: { mime: string; base64: string };
  error?: string;
}

/** Uygulamaya yeni paylaşım geldiğinde (uygulama açıkken) Kotlin'in WebView'a gönderdiği olay. */
export const SHARE_EVENT = "dex-share";

const tauri = new TauriTransport();

/** Bekleyen paylaşım; yoksa `{}`. */
export const takeShare = () => tauri.call<RawShare>("share_take");
