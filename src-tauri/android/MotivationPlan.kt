// #1859 Günlük motivasyon — saf zamanlama/karar mantığı (Android API'si yok; MotivationPlanTest).
// İş her gün 04:45'te koşar (dexd 04:30'da üretir; launcher 05:00'ten önce ister); kayıt yoksa ya da
// host'a erişilemezse 15 dk lineer backoff ile (05:00, 05:30, 06:15, 07:15, 08:30, 10:00, 11:45)
// tekrar denenir, 12:00'den sonra o gün bırakılır. Henüz host adresi yoksa (güncelleme sonrası ilk
// açılışta iş, JS adresleri native depoya yazmadan koşabilir) da tekrar denenir.

package com.hakunokina.dex.hosttokens

import java.util.Calendar
import java.util.Locale
import java.util.TimeZone

object MotivationPlan {
    const val RUN_HOUR = 4
    const val RUN_MINUTE = 45
    const val GIVE_UP_HOUR = 12
    const val BACKOFF_MINUTES = 15L

    /** Bir host'tan kayıt alınamadığında ne olduğu. */
    enum class Miss {
        /** `motivation_today` null (PC o günün metnini henüz üretmemiş) ya da başka günün kaydı. */
        EMPTY,

        /** Bağlantı kurulamadı / koptu / zaman aşımı. */
        UNREACHABLE,

        /** Cihaz tanınmıyor (4001/4003), metot yok ya da RPC hatası: tekrar denemek düzeltmez. */
        SKIP,
    }

    /** `now`dan sonraki ilk 04:45 (tam 04:45'te: ertesi gün). */
    fun nextRun(now: Long, tz: TimeZone): Long {
        val c = Calendar.getInstance(tz)
        c.timeInMillis = now
        c.set(Calendar.HOUR_OF_DAY, RUN_HOUR)
        c.set(Calendar.MINUTE, RUN_MINUTE)
        c.set(Calendar.SECOND, 0)
        c.set(Calendar.MILLISECOND, 0)
        if (c.timeInMillis <= now) c.add(Calendar.DAY_OF_MONTH, 1)
        return c.timeInMillis
    }

    /** Koşan işin kurduğu sonraki iş: 04:45'ten önce koşsa da (saat geri alındı, saat dilimi değişti)
     *  bugünü değil ertesi günü kurar; koşan iş KEEP'le korunduğundan aksi halde zincir kopar. */
    fun nextAfterRun(now: Long, tz: TimeZone): Long {
        val c = Calendar.getInstance(tz)
        c.timeInMillis = now
        c.set(Calendar.HOUR_OF_DAY, RUN_HOUR)
        c.set(Calendar.MINUTE, RUN_MINUTE)
        c.set(Calendar.SECOND, 0)
        c.set(Calendar.MILLISECOND, 0)
        return nextRun(maxOf(now, c.timeInMillis), tz)
    }

    /** yyyy-MM-dd (yerel gün). */
    fun day(t: Long, tz: TimeZone): String {
        val c = Calendar.getInstance(tz)
        c.timeInMillis = t
        return String.format(
            Locale.ROOT,
            "%04d-%02d-%02d",
            c.get(Calendar.YEAR),
            c.get(Calendar.MONTH) + 1,
            c.get(Calendar.DAY_OF_MONTH),
        )
    }

    private fun minuteOfDay(t: Long, tz: TimeZone): Int {
        val c = Calendar.getInstance(tz)
        c.timeInMillis = t
        return c.get(Calendar.HOUR_OF_DAY) * 60 + c.get(Calendar.MINUTE)
    }

    /** 04:45–12:00 arası: bugünün işi henüz bitmediyse hemen denenir (uygulama açılışı). */
    fun inWindow(t: Long, tz: TimeZone): Boolean =
        minuteOfDay(t, tz) in RUN_HOUR * 60 + RUN_MINUTE until GIVE_UP_HOUR * 60

    fun pastWindow(t: Long, tz: TimeZone): Boolean = minuteOfDay(t, tz) >= GIVE_UP_HOUR * 60

    /** Ağa çıkmadan önce: bugün yazıldıysa ya da 12:00 geçtiyse (geç kalan backoff denemesi) bırak. */
    fun shouldFetch(lastWritten: String?, now: Long, tz: TimeZone): Boolean =
        lastWritten != day(now, tz) && !pastWindow(now, tz)

    /** Hiçbir host kayıt vermedi: 12:00 geçmediyse ve host yoksa (adresler henüz yazılmadı) ya da en az
     *  biri boş/erişilemez ise tekrar dene. Hepsi reddettiyse (SKIP) tekrar denemek düzeltmez. */
    fun retry(misses: List<Miss>, pastWindow: Boolean): Boolean =
        !pastWindow && (misses.isEmpty() || misses.any { it != Miss.SKIP })

    private val DATE = Regex("^\\d{4}-\\d{2}-\\d{2}$")

    fun validDate(date: String): Boolean = DATE.matches(date)
}
