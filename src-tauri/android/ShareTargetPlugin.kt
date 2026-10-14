// Android "Paylaş" hedefi: ACTION_SEND (text/plain, image/*) intent'ini yakalar ve JS'e bırakır.
// Manifest intent-filter'ı build-apk.sh ekler (gen/ commit'li değil). Akış (#1759 tramplen):
//   - Instagram → Paylaş → alias → WebView'sız ShareActivity → SEND MainActivity'ye iletilir;
//   - uygulama kapalı: MainActivity iletilen SEND ile açılır → load() okur (saydam pencere);
//   - uygulama açık (singleTask): MainActivity.onNewIntent → burası → WebView'a "dex-share" olayı.
// Yük (metin + görsel) JS'e olayla değil `take` komutuyla (Rust: mobile_hosts.rs `share_take`) bir kez
// verilir; olay yalnız "bekleyen var" der. İçerik loglanmaz.
// #1717: Paylaş menüsünde üç giriş (manifest activity-alias'ları, hedef ShareActivity): hangi alias'tan
// gelindiği iletide `dex.task` olur (repo | verify | ask; eski doğrudan MainActivity çağrılarına uyum
// için component adından türetme yedeği durur). Hızlı gönderim sonrası `done` kısa bir Toast
// gösterip Dex'i kapatır; kullanıcı paylaştığı uygulamaya döner.
// #1759 pencere: MainActivity teması translucent'tır. Paylaşımda pencere + WebView saydam tutulur
// (kullanıcı Instagram'ı görmeye devam eder); paylaşım değilse ilk kareden opak siyaha çekilir
// (translucent önizleme normal açılışta başlatıcıyı göstermesin). Neden `moveTaskToBack` değil:
// paylaşım Dex'in `singleTask` MainActivity'sine gelir; soğuk açılışta paylaşım için YENİ bir Dex
// görevi kurulur ve arkaya almak bu görevi başlatıcıda/önde bırakabilir, ayrıca görev son
// kullanılanlarda ayrı bir "paylaşım" kopyası olur. Soğuk paylaşımda `finishAndRemoveTask` görevi
// tamamen kaldırır (arkada Instagram belirir, kopya kalmaz). Uygulama zaten açıkken gelen paylaşımda
// (`onNewIntent`) mevcut görev kullanılır, o yüzden eski davranış (`moveTaskToBack`) korunur.
// Gönderim ekranı (ShareScreen) açılırsa Dex bilerek önde kalır (opak CSS örter).

package com.hakunokina.dex.hosttokens

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.net.Uri
import android.os.SystemClock
import android.util.Base64
import android.util.Log
import android.webkit.WebView
import android.widget.Toast
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class DoneArgs {
    var message: String = ""
}

@TauriPlugin
class ShareTargetPlugin(private val activity: Activity) : Plugin(activity) {

    private var webView: WebView? = null
    private var pending: JSObject? = null
    /** Soğuk açılış paylaşımı mı: `load()` paylaşımla açıldı, `onNewIntent` görmedi. `done` tüketir. */
    private var coldShare = false
    /** #1759 tramplen `onCreate` damgası (`dex.share_t0`); yoksa -1. Yalnız süre ölçümü. */
    private var shareT0 = -1L

    override fun load(webView: WebView) {
        this.webView = webView
        coldShare = capture(activity.intent)
        applyWindow(webView, coldShare)
    }

    override fun onNewIntent(intent: Intent) {
        if (capture(intent)) {
            coldShare = false
            webView?.let { applyWindow(it, true) }
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

    /** Hızlı gönderim bitti: kısa Toast, sonra Dex'i kapat (soğuk paylaşım) ya da arka plana al. */
    @Command
    fun done(invoke: Invoke) {
        val args = try {
            invoke.parseArgs(DoneArgs::class.java)
        } catch (e: Exception) {
            null
        }
        val msg = args?.message.orEmpty().take(MAX_TOAST)
        val cold = synchronized(this) { coldShare.also { coldShare = false } }
        val t0 = synchronized(this) { shareT0.also { shareT0 = -1L } }
        // #1759: tramplenden kapanışa geçen süre; yalnız süre, içerik yok.
        if (cold && t0 > 0) Log.i(TAG, "soguk paylasim gonderimi ${SystemClock.elapsedRealtime() - t0} ms")
        activity.runOnUiThread {
            if (msg.isNotEmpty()) Toast.makeText(activity.applicationContext, msg, Toast.LENGTH_SHORT).show()
            if (cold) {
                try {
                    activity.finishAndRemoveTask()
                } catch (e: Exception) {
                    activity.finish()
                }
            } else {
                activity.moveTaskToBack(true)
            }
        }
        invoke.resolve(JSObject())
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
        out.put("task", taskOf(intent))
        synchronized(this) {
            pending = out
            val t0 = intent.getLongExtra("dex.share_t0", -1L)
            if (t0 > 0) shareT0 = t0
        }
        return true
    }

    /**
     * #1759 pencere saydamlığı: paylaşımda pencere + WebView saydam (arkada Instagram görünür),
     * değilse opak siyah (translucent tema normal açılışta başlatıcıyı göstermesin; JS ilk karede
     * gerçek temayı çizer). Her zaman UI iş parçacığında çağrılmalıdır.
     */
    private fun applyWindow(view: WebView, shared: Boolean) {
        activity.runOnUiThread {
            if (shared) {
                view.setBackgroundColor(Color.TRANSPARENT)
                activity.window?.setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))
            } else {
                view.setBackgroundColor(Color.BLACK)
                activity.window?.setBackgroundDrawable(ColorDrawable(Color.BLACK))
            }
        }
    }

    /**
     * Gelinen Paylaş girişi → görev. #1759 tramplen `dex.task` extra'sını taşır (component
     * MainActivity'ye çevrildiği için alias adı kaybolur); extra yoksa eski component eşlemesi
     * (doğrudan MainActivity çağrıları).
     */
    private fun taskOf(intent: Intent): String {
        when (intent.getStringExtra("dex.task")) {
            "repo" -> return "repo"
            "verify" -> return "verify"
            "ask" -> return "ask"
        }
        return when (intent.component?.className?.substringAfterLast('.')) {
            "ShareRepo" -> "repo"
            "ShareVerify" -> "verify"
            else -> "ask"
        }
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
        private const val TAG = "DexShare"
        // JNI/WebView köprüsünü şişirmemek için ham okuma üst sınırı; istemci zaten ~700 KB'a küçültür.
        private const val MAX_READ = 20 * 1024 * 1024
        private const val MAX_TOAST = 80
    }
}
