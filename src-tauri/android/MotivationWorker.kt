// #1859 Günlük motivasyon: her sabah 04:45'te (uygulama kapalıyken de; WorkManager) eşli dexd
// host'larından o günün üç cümlesini (`motivation_today`) çeker, JezLauncher'a ContentProvider
// `call` ile yazar, sonucu aynı host'a `motivation_result` ile bildirir. Bildirim yok: PC'ye
// erişilemiyorsa metin de yoktur. #2090: dexd cümleleri kaydedince FCM data mesajı (`kind: motivation`)
// bugünün işini hemen koşturur (DexMessagingService → MotivationWork.pushed); 04:45 işi yedektir. Zamanlama/karar: MotivationPlan.kt. Host adresi + jeton:
// HostTokensPlugin (Keystore'lu şifreli tercihler). Manifest (izin + queries): motivation.xml,
// build-apk.sh ekler. Jeton ve cümleler loglanmaz.

package com.hakunokina.dex.hosttokens

import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.util.Log
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import androidx.work.workDataOf
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
 *  en çok 250 karakter. Dönüş boş Bundle = kabul; geçersiz set IllegalArgumentException, izin yoksa
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

    /** Worker girdisi: push ile kurulan iş pencere ve "bugün yazıldı" denetimini atlar (kayıt yeni). */
    const val PUSHED = "pushed"

    /** Yarınki (ya da bugünkü, 04:45 öncesiyse) 04:45 işi; 04:45–12:00 arası ve bugün yazılmadıysa hemen.
     *  `afterRun`: koşan işten çağrı — sonraki iş hep ertesi gün (MotivationPlan.nextAfterRun). */
    fun ensure(context: Context, afterRun: Boolean = false) {
        val now = System.currentTimeMillis()
        val tz = TimeZone.getDefault()
        val next = if (afterRun) MotivationPlan.nextAfterRun(now, tz) else MotivationPlan.nextRun(now, tz)
        enqueue(context, MotivationPlan.day(next, tz), next - now)
        val today = MotivationPlan.day(now, tz)
        if (!MotivationPlan.inWindow(now, tz) || lastWritten(context) == today) return
        // Koşan iş bugünün işidir; REPLACE onu (kendini) iptal ederdi.
        if (afterRun) enqueue(context, today, 0) else replaceUnlessRunning(context, today)
    }

    /** #2090: dexd bugünün cümlelerini kaydetti (FCM). Backoff'ta bekleyen ya da süren işin yerine hemen
     *  koşar; süren iş kaydı henüz görmemiş olabilir. */
    fun pushed(context: Context, date: String) {
        if (!MotivationPlan.isToday(date, System.currentTimeMillis(), TimeZone.getDefault())) return
        enqueue(context, date, 0, ExistingWorkPolicy.REPLACE, pushed = true)
    }

    /** #2090 uygulama açılışı: backoff'ta (15 dk × n) bekleyen bugünün işi hemen denenir; süren iş kalır.
     *  Bekleyen/bitmiş iş için REPLACE'in KEEP'ten farkı yalnız backoff beklemesidir. */
    private fun replaceUnlessRunning(context: Context, day: String) {
        val infos = WorkManager.getInstance(context).getWorkInfosForUniqueWork(name(day))
        infos.addListener({
            val running = try {
                infos.get().any { it.state == WorkInfo.State.RUNNING }
            } catch (e: Exception) {
                false
            }
            if (!running) enqueue(context, day, 0, ExistingWorkPolicy.REPLACE)
        }, { it.run() })
    }

    private fun name(day: String) = "$TAG-$day"

    private fun enqueue(
        context: Context,
        day: String,
        delayMs: Long,
        policy: ExistingWorkPolicy = ExistingWorkPolicy.KEEP,
        pushed: Boolean = false,
    ) {
        val b = OneTimeWorkRequest.Builder(MotivationWorker::class.java)
            .setInitialDelay(delayMs, TimeUnit.MILLISECONDS)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setBackoffCriteria(BackoffPolicy.LINEAR, MotivationPlan.BACKOFF_MINUTES, TimeUnit.MINUTES)
            .addTag(TAG)
        if (pushed) {
            b.setInputData(workDataOf(PUSHED to true))
            // Expedited: high-priority FCM'nin verdiği kısa izinde Doze'a takılmadan koşar. Android 12
            // öncesinde ön plan servisi + bildirim ister (getForegroundInfo); orada normal iş.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                b.setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            }
        }
        // KEEP (varsayılan): bekleyen/süren aynı günün işi (backoff'ta olsa da) olduğu gibi kalır.
        WorkManager.getInstance(context).enqueueUniqueWork(name(day), policy, b.build())
    }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun lastWritten(context: Context): String? = prefs(context).getString(LAST, null)

    fun markWritten(context: Context, date: String) {
        prefs(context).edit().putString(LAST, date).apply()
    }
}

class MotivationWorker(context: Context, params: WorkerParameters) : Worker(context, params) {

    private companion object {
        /** İz: yalnız sabit kısa nedenler; jeton, adres ve cümleler loglanmaz. */
        const val LOG = "DexMotivation"
    }

    private class Lines(val date: String, val morning: String, val noon: String, val evening: String)

    override fun doWork(): Result {
        val ctx = applicationContext
        val now = System.currentTimeMillis()
        val tz = TimeZone.getDefault()
        // Önce ertesi günün işi: bu koşu nasıl biterse bitsin zincir kopmaz.
        try {
            MotivationWork.ensure(ctx, afterRun = true)
        } catch (e: Exception) {
            // Uygulamanın bir sonraki açılışında yeniden kurulur.
        }
        val today = MotivationPlan.day(now, tz)
        val pushed = inputData.getBoolean(MotivationWork.PUSHED, false)
        if (!pushed && !MotivationPlan.shouldFetch(MotivationWork.lastWritten(ctx), now, tz)) return Result.success()
        val misses = mutableListOf<MotivationPlan.Miss>()
        val hosts = try {
            HostTokensPlugin.hosts(ctx)
        } catch (e: Exception) {
            Log.w(LOG, "host listesi okunamadı: ${e.javaClass.simpleName}")
            return Result.success() // Keystore/şifreli tercihler açılamadı; yarın yeniden.
        }
        // Boş liste = adresler henüz yazılmadı (ilk açılış): retry; HostTokensPlugin.keepUrl da yeniden kurar.
        for ((url, token) in hosts) {
            val miss = fromHost(ctx, url, token, today, pushed) ?: return Result.success()
            misses += miss
        }
        val retry = MotivationPlan.retry(misses, MotivationPlan.pastWindow(System.currentTimeMillis(), tz))
        if (hosts.isEmpty()) Log.i(LOG, if (retry) "host adresi yok; tekrar denenecek" else "host adresi yok")
        return if (retry) Result.retry() else Result.success()
    }

    /** null: kayıt bu host'tan geldi ve yazma denendi (sonuç ne olursa olsun bugünün işi biter). */
    private fun fromHost(ctx: Context, url: String, token: String, today: String, pushed: Boolean): MotivationPlan.Miss? {
        try {
            HostRpc.open(url, token).use { rpc ->
                val lines = parse(rpc.call("motivation_today", JSONObject()))
                    ?.takeIf { it.date == today } ?: return MotivationPlan.Miss.EMPTY
                // Push: kayıt yeni (düzenlenmiş olabilir); launcher aynı tarihin üzerine yazar.
                if (!pushed && MotivationWork.lastWritten(ctx) == lines.date) return null
                val (ok, error) = write(ctx, lines)
                if (ok) MotivationWork.markWritten(ctx, lines.date)
                val result = JSONObject().put("date", lines.date).put("ok", ok)
                if (error != null) result.put("error", error)
                try {
                    rpc.call("motivation_result", result)
                } catch (e: HostRpc.Failure) {
                    // Yazma sonucu değişmez; bildirilemedi.
                    Log.w(LOG, "motivation_result bildirilemedi: ${e.message}")
                }
                return null
            }
        } catch (e: HostRpc.Unreachable) {
            return MotivationPlan.Miss.UNREACHABLE
        } catch (e: HostRpc.Refused) {
            Log.w(LOG, "host reddetti: ${e.message}")
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

    /** Host reddetti (jeton tanınmıyor, metot yok, RPC hatası) → tekrar denenmez. Mesaj sabit ve kısa
     *  (worker loglar): jeton/adres/içerik taşımaz. */
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
