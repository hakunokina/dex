// #1859 MotivationPlan birim testleri (JVM; build-apk.sh gen/ app/src/test altına kopyalar).
// Koşma: gen/android içinde `./gradlew :app:testDebugUnitTest --tests '*MotivationPlanTest'`.

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
    fun noRetryWhenEveryHostRefusedOrNoHost() {
        assertFalse(MotivationPlan.retry(listOf(Miss.SKIP, Miss.SKIP), false))
        assertFalse(MotivationPlan.retry(emptyList(), false))
    }

    @Test
    fun dateFormat() {
        assertTrue(MotivationPlan.validDate("2026-10-15"))
        assertFalse(MotivationPlan.validDate("2026-10-15T05:00"))
        assertFalse(MotivationPlan.validDate("15.10.2026"))
    }
}
