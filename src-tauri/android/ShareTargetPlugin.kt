// Android "Paylaş" hedefi: ACTION_SEND (text/plain, image/*) intent'ini yakalar ve JS'e bırakır.
// Manifest intent-filter'ı build-apk.sh ekler (gen/ commit'li değil). Akış:
//   - uygulama kapalı: aktivite SEND intent'iyle açılır → load() activity.intent'i okur;
//   - uygulama açık (singleTask): TauriActivity.onNewIntent → burası → WebView'a "dex-share" olayı.
// Yük (metin + görsel) JS'e olayla değil `take` komutuyla (Rust: mobile_hosts.rs `share_take`) bir kez
// verilir; olay yalnız "bekleyen var" der. İçerik loglanmaz.

package com.hakunokina.dex.hosttokens

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.util.Base64
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@TauriPlugin
class ShareTargetPlugin(private val activity: Activity) : Plugin(activity) {

    private var webView: WebView? = null
    private var pending: JSObject? = null

    override fun load(webView: WebView) {
        this.webView = webView
        capture(activity.intent)
    }

    override fun onNewIntent(intent: Intent) {
        if (capture(intent)) {
            activity.runOnUiThread {
                webView?.evaluateJavascript("window.dispatchEvent(new Event('dex-share'))", null)
            }
        }
    }

    /** Bekleyen paylaşımı bir kez verir; yoksa `{}`. */
    @Command
    fun take(invoke: Invoke) {
        val p = synchronized(this) { pending.also { pending = null } }
        invoke.resolve(p ?: JSObject())
    }

    private fun capture(intent: Intent?): Boolean {
        if (intent == null || intent.action != Intent.ACTION_SEND) return false
        val out = JSObject()
        intent.getStringExtra(Intent.EXTRA_TEXT)?.let { out.put("text", it) }
        intent.getStringExtra(Intent.EXTRA_SUBJECT)?.let { out.put("subject", it) }
        val type = intent.type.orEmpty()
        if (type.startsWith("image/")) {
            @Suppress("DEPRECATION")
            val uri = intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)
            if (uri != null) {
                try {
                    out.put("image", readImage(uri, type))
                } catch (e: TooBig) {
                    out.put("error", "Görsel çok büyük")
                } catch (e: Exception) {
                    out.put("error", "Görsel okunamadı")
                }
            }
        }
        // Aynı intent yeniden okunmasın (döndürme / aktivite yeniden yaratma).
        activity.intent = Intent(Intent.ACTION_MAIN)
        if (out.length() == 0) return false
        synchronized(this) { pending = out }
        return true
    }

    private class TooBig : Exception()

    private fun readImage(uri: Uri, type: String): JSObject {
        val mime = activity.contentResolver.getType(uri) ?: type
        val bytes = activity.contentResolver.openInputStream(uri)?.use { input ->
            val buf = java.io.ByteArrayOutputStream()
            val chunk = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(chunk)
                if (n < 0) break
                if (buf.size() + n > MAX_READ) throw TooBig()
                buf.write(chunk, 0, n)
            }
            buf.toByteArray()
        } ?: throw java.io.IOException("açılamadı")
        return JSObject().put("mime", mime).put("base64", Base64.encodeToString(bytes, Base64.NO_WRAP))
    }

    companion object {
        // JNI/WebView köprüsünü şişirmemek için ham okuma üst sınırı; istemci zaten ~700 KB'a küçültür.
        private const val MAX_READ = 20 * 1024 * 1024
    }
}
