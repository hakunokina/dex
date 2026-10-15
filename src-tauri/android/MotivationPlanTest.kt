// #1859 MotivationPlan birim testleri (JVM; build-apk.sh gen/ app/src/test altına kopyalar).
// Koşma: gen/android içinde `./gradlew :app:testArmDebugUnitTest --tests '*MotivationPlanTest'`.

package com.hakunokina.dex.hosttokens

import com.hakunokina.dex.hosttokens.MotivationPlan.Miss
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Calendar
import java.util.TimeZone

class MotivationPlanTest {
    private val tz = TimeZone.getTimeZone("Europe/Istanbul")

    private fun at(y: Int, mo: Int, d: Int, h: Int, mi: Int = 0): Long {
        val c = Calendar.getInstance(tz)
        c.clear()
        c.set(y, mo - 1, d, h, mi, 0)
        return c.timeInMillis
    }

    @Test
    fun nextRunIsTodayBefore0445() {
        assertEquals(at(2026, 10, 15, 4, 45), MotivationPlan.nextRun(at(2026, 10, 15, 4, 44), tz))
        assertEquals(at(2026, 10, 15, 4, 45), MotivationPlan.nextRun(at(2026, 10, 15, 0, 1), tz))
    }

    @Test
    fun nextRunIsTomorrowFrom0445On() {
        assertEquals(at(2026, 10, 16, 4, 45), MotivationPlan.nextRun(at(2026, 10, 15, 4, 45), tz))
        assertEquals(at(2026, 10, 16, 4, 45), MotivationPlan.nextRun(at(2026, 10, 15, 23, 30), tz))
        assertEquals(at(2027, 1, 1, 4, 45), MotivationPlan.nextRun(at(2026, 12, 31, 13), tz))
    }

    @Test
    fun dayIsLocal() {
        assertEquals("2026-10-15", MotivationPlan.day(at(2026, 10, 15, 0, 30), tz))
        assertEquals("2026-01-05", MotivationPlan.day(at(2026, 1, 5, 23, 59), tz))
    }

    @Test
    fun windowIs0445ToNoon() {
        assertFalse(MotivationPlan.inWindow(at(2026, 10, 15, 4, 44), tz))
        assertTrue(MotivationPlan.inWindow(at(2026, 10, 15, 4, 45), tz))
        assertTrue(MotivationPlan.inWindow(at(2026, 10, 15, 11, 59), tz))
        assertFalse(MotivationPlan.inWindow(at(2026, 10, 15, 12), tz))
        assertTrue(MotivationPlan.pastWindow(at(2026, 10, 15, 12), tz))
        assertFalse(MotivationPlan.pastWindow(at(2026, 10, 15, 11, 59), tz))
    }

    @Test
    fun retriesEmptyOrUnreachableUntilNoon() {
        assertTrue(MotivationPlan.retry(listOf(Miss.EMPTY), false))
        assertTrue(MotivationPlan.retry(listOf(Miss.SKIP, Miss.UNREACHABLE), false))
        assertFalse(MotivationPlan.retry(listOf(Miss.EMPTY, Miss.UNREACHABLE), true))
    }

    @Test
    fun noRetryWhenEveryHostRefused() {
        assertFalse(MotivationPlan.retry(listOf(Miss.SKIP, Miss.SKIP), false))
    }

    // #1877: güncelleme sonrası ilk açılışta adresler native depoya yazılmadan iş koşar.
    @Test
    fun retriesWhenNoHostYetUntilNoon() {
        assertTrue(MotivationPlan.retry(emptyList(), false))
        assertFalse(MotivationPlan.retry(emptyList(), true))
    }

    @Test
    fun fetchOnlyBeforeNoonAndWhenNotWritten() {
        assertTrue(MotivationPlan.shouldFetch(null, at(2026, 10, 15, 9, 58), tz))
        assertTrue(MotivationPlan.shouldFetch("2026-10-14", at(2026, 10, 15, 11, 59), tz))
        assertFalse(MotivationPlan.shouldFetch("2026-10-15", at(2026, 10, 15, 9, 58), tz))
        // Geç kalan backoff denemesi (13:45): ağa çıkmadan biter.
        assertFalse(MotivationPlan.shouldFetch(null, at(2026, 10, 15, 13, 45), tz))
        assertFalse(MotivationPlan.shouldFetch("2026-10-14", at(2026, 10, 15, 12), tz))
    }

    @Test
    fun nextAfterRunIsAlwaysTomorrow() {
        // Saat geri alınıp 04:45'ten önce koşsa da bugünü (koşan iş) değil yarını kurar.
        assertEquals(at(2026, 10, 16, 4, 45), MotivationPlan.nextAfterRun(at(2026, 10, 15, 3, 50), tz))
        assertEquals(at(2026, 10, 16, 4, 45), MotivationPlan.nextAfterRun(at(2026, 10, 15, 4, 45), tz))
        assertEquals(at(2026, 10, 16, 4, 45), MotivationPlan.nextAfterRun(at(2026, 10, 15, 10, 0), tz))
        assertEquals(at(2027, 1, 1, 4, 45), MotivationPlan.nextAfterRun(at(2026, 12, 31, 23, 59), tz))
    }

    @Test
    fun dateFormat() {
        assertTrue(MotivationPlan.validDate("2026-10-15"))
        assertFalse(MotivationPlan.validDate("2026-10-15T05:00"))
        assertFalse(MotivationPlan.validDate("15.10.2026"))
    }
}
