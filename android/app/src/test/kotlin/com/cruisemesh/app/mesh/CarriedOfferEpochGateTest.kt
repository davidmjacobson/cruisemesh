package com.cruisemesh.app.mesh

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class CarriedOfferEpochGateTest {
    @Test
    fun concurrentDigestsAtomicallyReserveOnlyTwoOffers() {
        val gate = CarriedOfferEpochGate(epochMs = 5_000)
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(16)
        try {
            val futures = (0 until 64).map {
                pool.submit<CarriedOfferEpochGate.Reservation?> {
                    start.await()
                    gate.tryReserve(nowMs = 1_000)
                }
            }
            start.countDown()
            assertEquals(2, futures.count { it.get() != null })
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun emptyPlanReleasesButCommittedOfferCountsUntilNextEpoch() {
        val gate = CarriedOfferEpochGate(epochMs = 100)
        val empty = gate.tryReserve(nowMs = 1_000)!!
        val sent = gate.tryReserve(nowMs = 1_000)!!
        assertNull(gate.tryReserve(nowMs = 1_000))

        gate.release(empty)
        val replacement = gate.tryReserve(nowMs = 1_000)
        assertNotNull(replacement)
        gate.commit(sent)
        gate.commit(replacement!!)
        assertNull(gate.tryReserve(nowMs = 1_099))
        assertNotNull(gate.tryReserve(nowMs = 1_100))
    }
}
