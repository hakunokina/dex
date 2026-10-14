// #1719 FCM: Echo bir paylaşıma cevap verince host data mesajı yollar — `{ type: "inbox_reply", id,
// task? }`, içerik YOK. Burada yerel bildirim kurulur ("Echo cevap verdi"; görev varsa "Echo · Repo
// incele"); dokunma MainActivity'yi `dex://reply/<id>` ile açar (PushPlugin.take → cevap ekranı).
// Manifest kaydı build-apk.sh (push-service.xml). Jeton ve içerik loglanmaz.

package com.hakunokina.dex.hosttokens

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

class DexMessagingService : FirebaseMessagingService() {

    override fun onNewToken(token: String) {
        // Jeton burada saklanmaz: JS `push_token` ile güncel jetonu okur ve hostlara kaydeder.
        PushPlugin.tokenChanged()
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val data = message.data
        if (data["type"] != "inbox_reply") return
        val id = data["id"]?.takeIf { ID.matches(it) } ?: return
        notify(id, TASKS[data["task"]])
    }

    private fun notify(id: String, task: String?) {
        val nm = NotificationManagerCompat.from(this)
        if (!nm.areNotificationsEnabled()) return // Android 13+ izni yok ya da kullanıcı kapattı
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(CHANNEL, "Echo cevapları", NotificationManager.IMPORTANCE_DEFAULT)
            ch.description = "Paylaştıklarına Echo cevap verince"
            getSystemService(NotificationManager::class.java).createNotificationChannel(ch)
        }
        val open = (packageManager.getLaunchIntentForPackage(packageName) ?: return).apply {
            action = PushPlugin.ACTION_OPEN_REPLY
            data = Uri.parse(PushPlugin.REPLY_PREFIX + id)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        val pi = PendingIntent.getActivity(
            this,
            id.hashCode(),
            open,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(smallIcon())
            .setContentTitle(if (task != null) "Echo · $task" else "Echo cevap verdi")
            .setContentText(if (task != null) "Echo cevap verdi — okumak için dokun" else "Okumak için dokun")
            .setAutoCancel(true)
            .setContentIntent(pi)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .build()
        try {
            nm.notify(TAG, id.hashCode(), n)
        } catch (e: SecurityException) {
            // İzin arada geri alındı.
        }
    }

    /** Durum çubuğu simgesi yalnız alfa kanalıyla çizilir: saydam zeminli ön plan katmanı (tauri
     *  icon üretir), yoksa uygulama simgesi. */
    @Suppress("DiscouragedApi")
    private fun smallIcon(): Int =
        resources.getIdentifier("ic_launcher_foreground", "mipmap", packageName).takeIf { it != 0 }
            ?: applicationInfo.icon

    companion object {
        private const val CHANNEL = "echo-replies"
        private const val TAG = "echo-reply"
        private val ID = Regex("^[A-Za-z0-9_-]{1,64}$")
        private val TASKS = mapOf("repo" to "Repo incele", "verify" to "Kaynak doğrula", "ask" to "Soru")
    }
}
