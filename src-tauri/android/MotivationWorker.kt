// #1859 Günlük motivasyon: her sabah 04:45'te (uygulama kapalıyken de; WorkManager) eşli dexd
// host'larından o günün üç cümlesini (`motivation_today`) çeker, JezLauncher'a ContentProvider
// `call` ile yazar, sonucu aynı host'a `motivation_result` ile bildirir. Bildirim/FCM yok: PC'ye
// erişilemiyorsa metin de yoktur. Zamanlama/karar: MotivationPlan.kt. Host adresi + jeton:
// HostTokensPlugin (Keystore'lu şifreli tercihler). Manifest (izin + queries): motivation.xml,
// build-apk.sh ekler. Jeton ve cümleler loglanmaz.

package com.hakunokina.dex.hosttokens

import android.content.Context
import android.net.Uri
import android.os.Bundle
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import java.io.Closeable
import java.util.TimeZone
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** JezLauncher sözleşmesi. Değişirse burası + motivation.xml (build-apk.sh ikisini eşler).
 *  Bundle: date (yyyy-MM-dd, telefonun yerel günü), morning, noon, evening — dördü zorunlu, boş değil,
 *  en çok 200 karakter. Dönüş boş Bundle = kabul; geçersiz set IllegalArgumentException, izin yoksa
 *  SecurityException. Launcher son 7 günü saklar; aynı tarih üzerine yazılır. */
object JezLauncher {
    const val AUTHORITY = "dev.jez.launcher.motivation"
    const val PERMISSION = "dev.jez.launcher.permission.SET_MOTIVATION"

    /** Launcher'ın debug sürümü; release bulunmazsa ona yazılır (ikisi de manifestte). */
    const val DEBUG_AUTHORITY = "dev.jez.launcher.debug.motivation"
    const val DEBUG_PERMISSION = "dev.jez.launcher.debug.permission.SET_MOTIVATION"
    const val METHOD = "setMotivation"

    /** Sırayla denenecek authority'ler: önce release, sonra debug. */
    val AUTHORITIES = listOf(AUTHORITY, DEBUG_AUTHORITY)
}

/** Sabah işini kurar; her gün kendi adını taşır (`dex-motivation-<gün>`), böylece kayma birikmez. */
object MotivationWork {
    private const val TAG = "dex-motivation"
    private const val PREFS = "dex-motivation"
    private const val LAST = "last-written"

    /** Yarınki (ya da bugünkü, 04:45 öncesiyse) 04:45 işi; 04:45–12:00 arası ve bugün yazılmadıysa hemen. */
    fun ensure(context: Context) {
        val now = System.currentTimeMillis()
        val tz = TimeZone.getDefault()
        val next = MotivationPlan.nextRun(now, tz)
        enqueue(context, MotivationPlan.day(next, tz), next - now)
        val today = MotivationPlan.day(now, tz)
        if (MotivationPlan.inWindow(now, tz) && lastWritten(context) != today) enqueue(context, today, 0)
    }

    private fun enqueue(context: Context, day: String, delayMs: Long) {
        val req = OneTimeWorkRequest.Builder(MotivationWorker::class.java)
            .setInitialDelay(delayMs, TimeUnit.MILLISECONDS)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setBackoffCriteria(BackoffPolicy.LINEAR, MotivationPlan.BACKOFF_MINUTES, TimeUnit.MINUTES)
            .addTag(TAG)
            .build()
        // KEEP: bekleyen/süren aynı günün işi (backoff'ta olsa da) olduğu gibi kalır.
        WorkManager.getInstance(context).enqueueUniqueWork("$TAG-$day", ExistingWorkPolicy.KEEP, req)
    }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun lastWritten(context: Context): String? = prefs(context).getString(LAST, null)

    fun markWritten(context: Context, date: String) {
        prefs(context).edit().putString(LAST, date).apply()
    }
}

class MotivationWorker(context: Context, params: WorkerParameters) : Worker(context, params) {

    private class Lines(val date: String, val morning: String, val noon: String, val evening: String)

    override fun doWork(): Result {
        val ctx = applicationContext
        val now = System.currentTimeMillis()
        val tz = TimeZone.getDefault()
        // Önce ertesi günün işi: bu koşu nasıl biterse bitsin zincir kopmaz.
        try {
            MotivationWork.ensure(ctx)
        } catch (e: Exception) {
            // Uygulamanın bir sonraki açılışında yeniden kurulur.
        }
        val today = MotivationPlan.day(now, tz)
        if (MotivationWork.lastWritten(ctx) == today) return Result.success()
        val misses = mutableListOf<MotivationPlan.Miss>()
        val hosts = try {
            HostTokensPlugin.hosts(ctx)
        } catch (e: Exception) {
            return Result.success() // Keystore/şifreli tercihler açılamadı; yarın yeniden.
        }
        for ((url, token) in hosts) {
            val miss = fromHost(ctx, url, token, today) ?: return Result.success()
            misses += miss
        }
        return if (MotivationPlan.retry(misses, MotivationPlan.pastWindow(now, tz))) Result.retry() else Result.success()
    }

    /** null: kayıt bu host'tan geldi ve yazma denendi (sonuç ne olursa olsun bugünün işi biter). */
    private fun fromHost(ctx: Context, url: String, token: String, today: String): MotivationPlan.Miss? {
        try {
            HostRpc.open(url, token).use { rpc ->
                val lines = parse(rpc.call("motivation_today", JSONObject()))
                    ?.takeIf { it.date == today } ?: return MotivationPlan.Miss.EMPTY
                if (MotivationWork.lastWritten(ctx) == lines.date) return null
                val (ok, error) = write(ctx, lines)
                if (ok) MotivationWork.markWritten(ctx, lines.date)
                val result = JSONObject().put("date", lines.date).put("ok", ok)
                if (error != null) result.put("error", error)
                try {
                    rpc.call("motivation_result", result)
                } catch (e: HostRpc.Failure) {
                    // Yazma sonucu değişmez; bildirilemedi.
                }
                return null
            }
        } catch (e: HostRpc.Unreachable) {
            return MotivationPlan.Miss.UNREACHABLE
        } catch (e: HostRpc.Refused) {
            return MotivationPlan.Miss.SKIP
        }
    }

    private fun parse(v: Any?): Lines? {
        val o = v as? JSONObject ?: return null
        fun s(k: String) = o.optString(k, "").takeIf { o.has(k) && !o.isNull(k) && it.isNotBlank() }
        val date = s("date")?.takeIf { MotivationPlan.validDate(it) } ?: return null
        return Lines(date, s("morning") ?: return null, s("noon") ?: return null, s("evening") ?: return null)
    }

    /** JezLauncher'a yazar → (ok, hata metni). */
    private fun write(ctx: Context, l: Lines): Pair<Boolean, String?> {
        val b = Bundle()
        b.putString("date", l.date)
        b.putString("morning", l.morning)
        b.putString("noon", l.noon)
        b.putString("evening", l.evening)
        // Bilinmeyen authority de IllegalArgumentException verir: "geçersiz set"ten ayırmak için önce bak.
        val authority = JezLauncher.AUTHORITIES.firstOrNull { ctx.packageManager.resolveContentProvider(it, 0) != null }
            ?: return false to "launcher yok"
        // Hata metinleri sabit ve kısa: istisna mesajı host'a gitmez.
        return try {
            ctx.contentResolver.call(Uri.parse("content://$authority"), JezLauncher.METHOD, null, b)
            true to null
        } catch (e: SecurityException) {
            false to "izin yok"
        } catch (e: IllegalArgumentException) {
            false to "geçersiz set"
        } catch (e: Exception) {
            false to "launcher hatası"
        }
    }
}

/** dexd'ye tek kullanımlık, eşzamanlı JSON-RPC (src/api/transport.ts WsTransport ile aynı çerçeve). */
class HostRpc private constructor(private val ws: WebSocket, private val inbox: LinkedBlockingQueue<Any>) :
    Closeable {

    open class Failure(why: String) : Exception(why)

    /** Ağ: bağlanamadı, koptu, zaman aşımı → tekrar denenir. */
    class Unreachable(why: String) : Failure(why)

    /** Host reddetti (jeton tanınmıyor, metot yok, RPC hatası) → tekrar denenmez. */
    class Refused(why: String) : Failure(why)

    private class Closed(val code: Int)

    private object Broken

    private var nextId = 1

    fun call(method: String, params: JSONObject): Any? {
        val id = nextId++
        if (!ws.send(JSONObject().put("id", id).put("method", method).put("params", params).toString())) {
            throw Unreachable("gönderilemedi")
        }
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(CALL_SECONDS)
        while (true) {
            val left = deadline - System.nanoTime()
            val m = (if (left > 0) inbox.poll(left, TimeUnit.NANOSECONDS) else null)
                ?: throw Unreachable("zaman aşımı")
            when (m) {
                is Closed -> throw if (m.code == 4001 || m.code == 4003) Refused("tanınmıyor") else Unreachable("koptu")
                Broken -> throw Unreachable("bağlantı hatası")
                is String -> {
                    val o = try {
                        JSONObject(m)
                    } catch (e: Exception) {
                        continue
                    }
                    if (o.has("event") || o.optInt("id", -1) != id) continue
                    if (o.has("error") && !o.isNull("error")) throw Refused("rpc hatası")
                    return if (o.isNull("result")) null else o.get("result")
                }
            }
        }
    }

    override fun close() {
        ws.close(1000, null)
    }

    companion object {
        private const val CALL_SECONDS = 15L
        private val client: OkHttpClient by lazy {
            OkHttpClient.Builder().connectTimeout(10, TimeUnit.SECONDS).build()
        }

        /** Bağlanır ve `auth.hello` ile kimlik doğrular. */
        fun open(url: String, token: String): HostRpc {
            val inbox = LinkedBlockingQueue<Any>()
            val request = try {
                Request.Builder().url(url).build()
            } catch (e: IllegalArgumentException) {
                throw Refused("geçersiz adres")
            }
            val ws = client.newWebSocket(request, object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    inbox.offer(text)
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    inbox.offer(Closed(code))
                    webSocket.close(code, null)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    inbox.offer(Closed(code))
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    inbox.offer(Broken)
                }
            })
            val rpc = HostRpc(ws, inbox)
            try {
                rpc.call("auth.hello", JSONObject().put("protocol", PROTOCOL).put("token", token))
            } catch (e: Failure) {
                ws.cancel()
                throw e
            }
            return rpc
        }

        private const val PROTOCOL = 1
    }
}
