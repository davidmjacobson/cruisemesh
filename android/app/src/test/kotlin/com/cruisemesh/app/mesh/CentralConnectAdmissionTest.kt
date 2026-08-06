package com.cruisemesh.app.mesh

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CentralConnectAdmissionTest {
    @Test
    fun pendingConnectionsConsumeCapacityBeforeFrameworkCallsStart() {
        val admission = CentralConnectAdmission(maxActive = 2)
        admission.startSession()

        val first = admission.tryReserve("a").reservation
        val second = admission.tryReserve("b").reservation
        val denied = admission.tryReserve("c")

        assertNotNull(first)
        assertNotNull(second)
        assertNull(denied.reservation)
        assertTrue(denied.atCapacity)
        assertEquals(2, denied.activeCount)

        admission.cancel(second!!)
        assertNotNull(admission.tryReserve("c").reservation)
    }

    @Test
    fun stopInvalidatesQueuedAndInFlightReservations() {
        val admission = CentralConnectAdmission(maxActive = 2)
        admission.startSession()
        val queued = admission.tryReserve("a").reservation!!
        val inFlight = admission.tryReserve("b").reservation!!
        assertTrue(admission.beginConnect(inFlight))

        admission.stopSession()

        assertFalse(admission.beginConnect(queued))
        assertFalse(admission.completeConnect(inFlight))
        admission.startSession()
        assertNotNull(admission.tryReserve("a").reservation)
    }

    @Test
    fun concurrentScanCallbacksCannotOverReserveTheCap() {
        val admission = CentralConnectAdmission(maxActive = 5)
        admission.startSession()
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(16)
        try {
            val futures = (0 until 64).map { index ->
                pool.submit<CentralConnectAdmission.Reservation?> {
                    start.await()
                    admission.tryReserve("peer-$index").reservation
                }
            }
            start.countDown()
            assertEquals(5, futures.count { it.get() != null })
        } finally {
            pool.shutdownNow()
        }
    }
}
