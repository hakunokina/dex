// S5-A: device-token bridge for the Android build. Same three commands as desktop `hosts.rs`
// (get/set/delete), but the token never touches localStorage or plaintext: it is wrapped with a
// Keystore-held AES-256-GCM master key and only ciphertext lands in the app-private prefs file.
// Rust side: src-tauri/src/mobile_hosts.rs (`dex-host-tokens` plugin, methods "get"/"set"/"delete").

package com.hakunokina.dex.hosttokens

import android.app.Activity
import android.content.SharedPreferences
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
}

@TauriPlugin
class HostTokensPlugin(activity: Activity) : Plugin(activity) {

    private val prefs: SharedPreferences by lazy {
        val masterKey = MasterKey.Builder(activity)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            activity,
            PREFS,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
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
        if (prefs.contains(args.id)) ret.put("value", prefs.getString(args.id, null)) else ret.put("value", null)
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
        invoke.resolve(JSObject())
    }

    @Command
    fun delete(invoke: Invoke) {
        val args = argsOrReject(invoke) ?: return
        prefs.edit().remove(args.id).apply()
        invoke.resolve(JSObject())
    }

    companion object {
        private const val PREFS = "dex-host-tokens"
    }
}
