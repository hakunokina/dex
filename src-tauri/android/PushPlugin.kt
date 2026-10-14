// #1719 Echo cevapları — bildirim köprüsü (Rust: src-tauri/src/mobile_hosts.rs `push_*`, eklenti
// `dex-push`). Üç iş:
//   - `token`: FCM jetonu. google-services.json'suz derlemede FirebaseApp yoktur → `{}` (uygulama
//     çalışır, bildirim gelmez). Jeton yenilenince (DexMessagingService.onNewToken) WebView'a
//     "dex-push-token" olayı gider; JS jetonu yeniden okuyup hostlara `push_register` eder.
//   - `take`: bildirime dokunma → MainActivity `dex://reply/<id>` verisiyle açılır (kapalıysa
//     load(), açıksa singleTask onNewIntent); bağlantı JS'e bir kez verilir, olay "dex-open-reply".
//   - `openUrl`: cevaptaki http/https bağlantıyı sistem tarayıcısında açar (başka şema reddedilir).
// Jeton ve içerik loglanmaz.

package com.hakunokina.dex.hosttokens

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.firebase.messaging.FirebaseMessaging

@InvokeArg
class UrlArgs {
    var url: String = ""
}

@TauriPlugin
class PushPlugin(private val activity: Activity) : Plugin(activity) {

    private var webView: WebView? = null
    private var pending: String? = null

    override fun load(webView: WebView) {
        this.webView = webView
        live = this
        capture(activity.intent)
    }

    override fun onNewIntent(intent: Intent) {
        if (capture(intent)) dispatch("dex-open-reply")
    }

    @Command
    fun token(invoke: Invoke) {
        try {
            FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
                val out = JSObject()
                if (task.isSuccessful) out.put("token", task.result)
                invoke.resolve(out)
            }
        } catch (e: Exception) {
            // FirebaseApp yok (google-services.json'suz APK) ya da Play hizmetleri yok.
            invoke.resolve(JSObject())
        }
    }

    /** Bekleyen bildirim bağlantısı (`dex://reply/<id>`), bir kez; yoksa `{}`. */
    @Command
    fun take(invoke: Invoke) {
        val uri = synchronized(this) { pending.also { pending = null } }
        invoke.resolve(if (uri != null) JSObject().put("uri", uri) else JSObject())
    }

    @Command
    fun openUrl(invoke: Invoke) {
        val url = try {
            invoke.parseArgs(UrlArgs::class.java).url
        } catch (e: Exception) {
            ""
        }
        val uri = Uri.parse(url)
        if (uri.scheme != "http" && uri.scheme != "https") {
            invoke.reject("yalnız http/https bağlantı açılır")
            return
        }
        try {
            activity.startActivity(Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE))
            invoke.resolve(JSObject())
        } catch (e: Exception) {
            invoke.reject("tarayıcı açılamadı")
        }
    }

    private fun capture(intent: Intent?): Boolean {
        if (intent == null || intent.action != ACTION_OPEN_REPLY) return false
        val uri = intent.dataString ?: return false
        if (!uri.startsWith(REPLY_PREFIX)) return false
        synchronized(this) { pending = uri }
        // Aynı intent yeniden okunmasın (döndürme / aktivite yeniden yaratma).
        activity.intent = Intent(Intent.ACTION_MAIN)
        return true
    }

    private fun dispatch(event: String) {
        activity.runOnUiThread {
            webView?.evaluateJavascript("window.dispatchEvent(new Event('$event'))", null)
        }
    }

    companion object {
        const val ACTION_OPEN_REPLY = "com.hakunokina.dex.OPEN_REPLY"
        const val REPLY_PREFIX = "dex://reply/"

        /** Yaşayan örnek (servis jeton yenilemesini WebView'a duyurur); uygulama kapalıysa null. */
        @Volatile
        private var live: PushPlugin? = null

        fun tokenChanged() {
            live?.dispatch("dex-push-token")
        }
    }
}
