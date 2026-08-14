package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PeripheralRejectionScheduleTest {

    @Test
    fun `retries three times then waits a full disconnect grace before adoption`() {
        val schedule = PeripheralRejectionSchedule()

        assertEquals(
            PeripheralRejectionFollowUp.Retry(attempt = 2, delayMs = 4_000L),
            schedule.after(attempt = 1),
        )
        assertEquals(
            PeripheralRejectionFollowUp.Retry(attempt = 3, delayMs = 4_000L),
            schedule.after(attempt = 2),
        )
        assertEquals(
            PeripheralRejectionFollowUp.Adopt(delayMs = 12_000L),
            schedule.after(attempt = 3),
        )
    }

    @Test
    fun `default ladder stays bounded while outlasting observed callback latency`() {
        val totalMs =
            (PeripheralRejectionSchedule.MAX_ATTEMPTS - 1) * PeripheralRejectionSchedule.RETRY_DELAY_MS +
                PeripheralRejectionSchedule.FINAL_DISCONNECT_GRACE_MS

        assertEquals(20_000L, totalMs)
        assertTrue(PeripheralRejectionSchedule.FINAL_DISCONNECT_GRACE_MS > 7_879L)
    }

    @Test(expected = IllegalArgumentException::class)
    fun `cannot advance beyond the configured ladder`() {
        PeripheralRejectionSchedule().after(attempt = 4)
    }
}
