package com.cruisemesh.app.mesh

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class InboundEnvelopeAdmissionTest {

    private fun msgId(tag: Int) = byteArrayOf(1, 2, 3, tag.toByte())

    @Test
    fun `the first claim of a fresh msg_id succeeds`() {
        val admission = InboundEnvelopeAdmission()
        assertTrue(admission.tryBegin(msgId(1)))
        assertEquals(1, admission.inFlightCount())
    }

    @Test
    fun `a second claim of the same msg_id while the first is still in flight is rejected`() {
        val admission = InboundEnvelopeAdmission()
        assertTrue(admission.tryBegin(msgId(1)))
        assertFalse(admission.tryBegin(msgId(1)))
        // Rejecting the duplicate must not disturb the original claim.
        assertEquals(1, admission.inFlightCount())
    }

    @Test
    fun `distinct msg_ids are claimed independently`() {
        val admission = InboundEnvelopeAdmission()
        assertTrue(admission.tryBegin(msgId(1)))
        assertTrue(admission.tryBegin(msgId(2)))
        assertEquals(2, admission.inFlightCount())
    }

    @Test
    fun `two ByteArray instances with equal content are treated as the same msg_id`() {
        // Regression: msg_id arrives as a fresh ByteArray on every transport
        // (BLE vs LAN), never the same object reference -- admission must key
        // on content, not identity.
        val admission = InboundEnvelopeAdmission()
        val first = byteArrayOf(9, 9, 9)
        val second = byteArrayOf(9, 9, 9)
        assertTrue(first !== second)
        assertTrue(admission.tryBegin(first))
        assertFalse(admission.tryBegin(second))
    }

    @Test
    fun `finish releases the claim so a later copy is admitted again`() {
        val admission = InboundEnvelopeAdmission()
        val id = msgId(1)
        assertTrue(admission.tryBegin(id))
        admission.finish(id, terminal = false)
        assertEquals(0, admission.inFlightCount())
        assertTrue(admission.tryBegin(id))
    }

    @Test
    fun `terminal finish runs onTerminal before releasing the claim`() {
        val admission = InboundEnvelopeAdmission()
        val id = msgId(1)
        var recorded = false
        admission.tryBegin(id)
        admission.finish(id, terminal = true) { recorded = true }
        assertTrue(recorded)
        assertEquals(0, admission.inFlightCount())
    }

    @Test
    fun `non-terminal finish does not invoke onTerminal -- matches the FAILED path leaving msg_id re-presentable`() {
        // Mirrors processInboundEnvelope's FAILED disposition (:2065 group
        // path, :2127 pairwise path): a transient store failure must not
        // record the msg_id into GossipState.seenIds, only release the
        // in-flight claim so a retry (or the next copy on any transport)
        // re-enters tryBegin as a fresh attempt.
        val admission = InboundEnvelopeAdmission()
        val id = msgId(1)
        var recorded = false
        admission.tryBegin(id)
        admission.finish(id, terminal = false) { recorded = true }
        assertFalse(recorded)
    }

    @Test
    fun `CARRIED's conditional record matches carryForeignEnvelope's success return exactly`() {
        // processInboundEnvelope's CARRIED branch (:2110-2113 in the pre-fix
        // code) only records when the durable carry actually persisted --
        // terminal is the carried boolean itself, not a constant.
        val admission = InboundEnvelopeAdmission()
        val carriedOk = msgId(1)
        val carriedFailed = msgId(2)
        var recordedOk = false
        var recordedFailed = false

        admission.tryBegin(carriedOk)
        admission.finish(carriedOk, terminal = true) { recordedOk = true }
        assertTrue(recordedOk)

        admission.tryBegin(carriedFailed)
        admission.finish(carriedFailed, terminal = false) { recordedFailed = true }
        assertFalse(recordedFailed)
    }

    @Test
    fun `a concurrent second copy of an in-flight msg_id is rejected under real thread contention`() {
        // The actual FA5 race: two receive-path threads (e.g. BLE central and
        // LAN reader) both call tryBegin for the identical msg_id at close to
        // the same instant. Exactly one must win.
        val admission = InboundEnvelopeAdmission()
        val id = msgId(1)
        val threadCount = 32
        val pool = Executors.newFixedThreadPool(threadCount)
        val ready = CountDownLatch(threadCount)
        val go = CountDownLatch(1)
        val winners = AtomicInteger(0)
        try {
            val futures = (1..threadCount).map {
                pool.submit {
                    ready.countDown()
                    go.await()
                    if (admission.tryBegin(id)) {
                        winners.incrementAndGet()
                    }
                }
            }
            ready.await()
            go.countDown()
            futures.forEach { it.get(10, TimeUnit.SECONDS) }
        } finally {
            pool.shutdown()
        }
        assertEquals("exactly one of $threadCount concurrent claims should win", 1, winners.get())
        assertEquals(1, admission.inFlightCount())
    }

    @Test
    fun `after a concurrent winner finishes, a fresh claim succeeds again`() {
        val admission = InboundEnvelopeAdmission()
        val id = msgId(1)
        val threadCount = 16
        val pool = Executors.newFixedThreadPool(threadCount)
        val ready = CountDownLatch(threadCount)
        val go = CountDownLatch(1)
        val winners = AtomicInteger(0)
        try {
            val futures = (1..threadCount).map {
                pool.submit {
                    ready.countDown()
                    go.await()
                    if (admission.tryBegin(id)) {
                        winners.incrementAndGet()
                    }
                }
            }
            ready.await()
            go.countDown()
            futures.forEach { it.get(10, TimeUnit.SECONDS) }
        } finally {
            pool.shutdown()
        }
        assertEquals(1, winners.get())
        admission.finish(id, terminal = true) {}
        assertTrue(admission.tryBegin(id))
    }
}
