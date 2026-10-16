// S5-A: device-token bridge for the Android build. Same three commands as desktop `hosts.rs`
// (get/set/delete), but the token never touches localStorage or plaintext: it is wrapped with a
// Keystore-held AES-256-GCM master key and only ciphertext lands in the app-private prefs file.
// Rust side: src-tauri/src/mobile_hosts.rs (`dex-host-tokens` plugin, methods "get"/"set"/"delete").
// #1859: get/set may also carry the host's address (`url`, already policy-checked by JS); it is kept
// next to the token (own encrypted file) so MotivationWorker can reach the host with the app closed.

package com.hakunokina.dex.hosttokens

import android.app.Activity
import android.content.Context
import android.content.SharedPreferences
import android.webkit.WebView
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class TokenArgs {
    lateinit var id: String
    var token: String? = null
    var url: String? = null
}

@TauriPlugin
class HostTokensPlugin(private val activity: Activity) : Plugin(activity) {

    private val prefs: SharedPreferences by lazy { tokens(activity) }
    private val urls: SharedPreferences by lazy { urls(activity) }

    override fun load(webView: WebView) {
        // #1859: the morning job is (re)scheduled on every start; a WorkManager failure must not
        // take pairing down with it.
        ensureMotivation()
    }

    // #2095: arka planda canlı süreç öne gelince load() koşmaz; bugünün backoff'taki işi burada da
    // kırılır (bugün yazıldıysa ya da iş koşuyorsa ensure dokunmaz). Soğuk başlatmada load'la çift
    // çağrı zararsız: ikinci REPLACE henüz koşmayan işi aynısıyla değiştirir.
    override fun onResume() {
        ensureMotivation()
    }

    private fun ensureMotivation() {
        try {
            MotivationWork.ensure(activity.applicationContext)
        } catch (e: Exception) {
            // Sabah işi kurulamadı; jeton köprüsü çalışmaya devam eder.
        }
    }

    private fun keepUrl(args: TokenArgs) {
        val url = args.url?.trim() ?: return
        if (!validUrl(url) || urls.getString(args.id, null) == url) return
        urls.edit().putString(args.id, url).apply()
        // #1877: açılışta load()'un kurduğu iş adres yazılmadan koşmuş olabilir; yeni adresle yeniden kur.
        ensureMotivation()
    }

    private fun argsOrReject(invoke: Invoke): TokenArgs? {
        val args = try {
            invoke.parseArgs(TokenArgs::class.java)
        } catch (e: Exception) {
            invoke.reject("eksik argüman: $e")
            return null
        }
        val id = args.id
        if (id.isEmpty() || id.length > 32 || !id.all { it.isLetterOrDigit() || it == '-' }) {
            invoke.reject("geçersiz sunucu kimliği: $id")
            return null
        }
        return args
    }

    @Command
    fun get(invoke: Invoke) {
        val args = argsOrReject(invoke) ?: return
        val ret = JSObject()
        if (prefs.contains(args.id)) {
            keepUrl(args)
            ret.put("value", prefs.getString(args.id, null))
        } else {
            ret.put("value", null)
        }
        invoke.resolve(ret)
    }

    @Command
    fun set(invoke: Invoke) {
        val args = argsOrReject(invoke) ?: return
        val token = (args.token ?: "").trim()
        if (token.isEmpty()) {
            invoke.reject("boş jeton")
            return
        }
        prefs.edit().putString(args.id, token).apply()
        keepUrl(args)
        invoke.resolve(JSObject())
    }

    @Command
    fun delete(invoke: Invoke) {
        val args = argsOrReject(invoke) ?: return
        prefs.edit().remove(args.id).apply()
        urls.edit().remove(args.id).apply()
        invoke.resolve(JSObject())
    }

    companion object {
        private const val PREFS = "dex-host-tokens"
        private const val URLS = "dex-host-urls"

        private fun encrypted(context: Context, name: String): SharedPreferences {
            val masterKey = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            return EncryptedSharedPreferences.create(
                context,
                name,
                masterKey,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        }

        // #1877: plugin ve MotivationWorker aynı örneği kullanır. İki ayrı EncryptedSharedPreferences
        // ilk açılışta aynı dosyanın keyset'ini yarışarak üretebilir; kaybedenin yazdığı girdi çözülemez.
        private var tokensPrefs: SharedPreferences? = null
        private var urlsPrefs: SharedPreferences? = null

        @Synchronized
        fun tokens(context: Context): SharedPreferences =
            tokensPrefs ?: encrypted(context.applicationContext, PREFS).also { tokensPrefs = it }

        @Synchronized
        fun urls(context: Context): SharedPreferences =
            urlsPrefs ?: encrypted(context.applicationContext, URLS).also { urlsPrefs = it }

        fun validUrl(url: String): Boolean =
            url.length <= 512 && (url.startsWith("ws://") || url.startsWith("wss://"))

        /** #1859: every paired host the native side can reach — (address, device token). */
        fun hosts(context: Context): List<Pair<String, String>> {
            val tokens = tokens(context)
            return urls(context).all.mapNotNull { (id, url) ->
                val token = tokens.getString(id, null)
                if (url is String && validUrl(url) && !token.isNullOrEmpty()) url to token else null
            }.sortedBy { it.first }
        }
    }
}
