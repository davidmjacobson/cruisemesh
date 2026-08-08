package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.cruisemesh_core.CoreSprayAdmissionReason
import uniffi.cruisemesh_core.CoreSprayGateReason
import uniffi.cruisemesh_core.CoreSprayLanePlan
import uniffi.cruisemesh_core.CoreSprayPlanShape
import uniffi.cruisemesh_core.CoreSprayTrigger

/**
 * The Android half of issue #280: that this shell asks core rather than
 * deciding, and that the answers arrive intact across the FFI boundary.
 *
 * The decisions themselves are pinned in `core/src/spray_policy.rs`, with
 * table-driven cases and mutation-verified assertions. What is worth testing
 * here is the wiring: that the budgets Android now sprays with are core's
 * numbers (the three shell constants this branch deleted), that a peer key is
 * a peer and a link key is a link, and that the enum discriminants survive the
 * crossing.
 *
 * Every call passes an explicit monotonic `nowMs`, as the production call
 * sites do through [SprayPolicy.nowMs].
 */
class SprayPolicyTest {

    private companion object {
        init {
            HostCoreLibrary.load()
        }

        val PEER = ByteArray(16) { 0x11 }
        val OTHER_PEER = ByteArray(16) { 0x22 }
        const val LINK = "AA:BB:CC:DD:EE:01"
        const val OTHER_LINK = "AA:BB:CC:DD:EE:02"

        val EMPTY_LANE = CoreSprayLanePlan(setDigest = 0uL, bytes = 0uL)

        /** A one-lane plan (own outbound), for cases that are not about lanes. */
        fun ownLane(setDigest: ULong, bytes: ULong) = CoreSprayPlanShape(
            carried = EMPTY_LANE,
            ownOutbound = CoreSprayLanePlan(setDigest, bytes),
            ownReceipts = EMPTY_LANE,
        )
    }

    @Before
    fun resetPolicy() {
        SprayPolicy.reset()
    }

    @Test
    fun `the per-encounter byte budgets now come from core`() {
        // These three numbers were `private const val`s in
        // InboundEnvelopeProcessor.kt, duplicated in ProtocolKinds.swift. This
        // test is what makes their deletion permanent: if someone reintroduces
        // a shell constant, the shell and core answers can differ again, and
        // the only place that can now supply them is here.
        val gate = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 0)
        assertTrue(gate.allow)
        assertEquals(256uL * 1024uL, gate.carriedBudgetBytes)
        assertEquals(256uL * 1024uL, gate.ownOutboundBudgetBytes)
        assertEquals(64uL * 1024uL, gate.ownReceiptBudgetBytes)
    }

    @Test
    fun `a fresh encounter syncs at once and reconnect churn does not`() {
        val first = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 0)
        assertTrue("two phones meeting must never be gated", first.allow)
        assertEquals(CoreSprayGateReason.FIRST_CONTACT, first.reason)
        SprayPolicy.noteDigestSent(PEER, LINK, nowMs = 0)

        // The 498-connects-in-88-minutes case, at the same address and at a
        // fresh one: same phone, same gate.
        for (nowMs in listOf(200L, 1_000L, 30_000L)) {
            val churn = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.RECONNECT, nowMs)
            assertFalse("reconnect at ${nowMs}ms", churn.allow)
            assertTrue("a denial must name its expiry", churn.retryAfterMs > 0)
        }
        assertFalse(
            "a new address is not a new peer",
            SprayPolicy.maySpray(PEER, OTHER_LINK, CoreSprayTrigger.RECONNECT, nowMs = 500).allow,
        )
        // A different phone is unaffected by any of it.
        assertTrue(
            SprayPolicy.maySpray(OTHER_PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 500).allow,
        )
    }

    @Test
    fun `answering the digest our own spray provoked is not churn`() {
        assertTrue(SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 0).allow)
        SprayPolicy.noteDigestSent(PEER, LINK, nowMs = 0)
        val answer = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.PEER_DIGEST, nowMs = 400)
        assertTrue("the peer's half of one exchange", answer.allow)
        assertEquals(CoreSprayGateReason.EXCHANGE_OPEN, answer.reason)
    }

    @Test
    fun `an unchanged advertised set is not re-sprayed at full size`() {
        val sent = SprayPolicy.admitPlan(PEER, LINK, ownLane(0xABCDuL, 8_192uL), nowMs = 0)
        assertTrue(sent.send)
        assertEquals(CoreSprayAdmissionReason.SET_CHANGED, sent.reason)

        val repeat = SprayPolicy.admitPlan(PEER, LINK, ownLane(0xABCDuL, 8_192uL), nowMs = 60_000)
        assertFalse("the 28 identical sprays", repeat.send)
        assertEquals(CoreSprayAdmissionReason.IDENTICAL_SUPPRESSED, repeat.reason)
        assertTrue("suppression must expire", repeat.reofferInMs > 0)

        val changed = SprayPolicy.admitPlan(PEER, LINK, ownLane(0x1234uL, 8_192uL), nowMs = 60_001)
        assertTrue("a set change sprays immediately", changed.send)
    }

    @Test
    fun `an invariant authored lane goes quiet while the carried walk proceeds`() {
        // The recorded shape: authored invariant at 16 envelopes across every
        // spray while the carried lane walked its cursor. InboundEnvelopeProcessor
        // sends exactly the lanes this admission names, so the per-lane answer
        // has to survive the crossing.
        var carriedSends = 0
        var authoredSends = 0
        repeat(6) { round ->
            val admission = SprayPolicy.admitPlan(
                PEER,
                LINK,
                CoreSprayPlanShape(
                    carried = CoreSprayLanePlan(setDigest = round.toULong() + 1uL, bytes = 8_192uL),
                    ownOutbound = CoreSprayLanePlan(setDigest = 0xA17uL, bytes = 16_384uL),
                    ownReceipts = EMPTY_LANE,
                ),
                nowMs = round * 200L,
            )
            if (admission.sendCarried) carriedSends++
            if (admission.sendOwnOutbound) authoredSends++
        }
        assertEquals("the carried walk must not be suppressed", 6, carriedSends)
        assertEquals("an invariant authored set is offered once", 1, authoredSends)
    }

    @Test
    fun `a disconnect resets neither the peer cadence nor the link allowance`() {
        assertTrue(SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 0).allow)
        SprayPolicy.noteDigestSent(PEER, LINK, nowMs = 0)
        // Everything this encounter queued outside the plan -- the receipt
        // repair pass, the per-missing-message re-send loop, the group catch-up
        // and the carry drain -- is charged here, exactly as MeshService does.
        SprayPolicy.noteBytesQueued(LINK, 576L * 1024, nowMs = 0)
        // This is what MeshService.recordPeerDisconnected does. It resets
        // nothing: a disconnect is what reconnect churn produces.
        SprayPolicy.noteLinkClosed(LINK, nowMs = 0)
        val gate = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.PEER_DIGEST, nowMs = 1)
        assertFalse(gate.allow)
        assertEquals(CoreSprayGateReason.LINK_BURST_EXHAUSTED, gate.reason)
        assertTrue("a denial must name its expiry", gate.retryAfterMs > 0)
    }

    @Test
    fun `progress evidence clears the receipt-quiet backoff`() {
        assertTrue(SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.FIRST_CONTACT, nowMs = 0).allow)
        var now = 0L
        // Four receipt-free sprays: the cadence stretches.
        repeat(4) { round ->
            SprayPolicy.admitPlan(PEER, LINK, ownLane(round.toULong(), 1_024uL), nowMs = now)
            now += 61_000
        }
        val stretched = SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.RECONNECT, now)
        assertFalse(stretched.allow)
        assertEquals(CoreSprayGateReason.RECEIPT_QUIET_BACKOFF, stretched.reason)

        // A receipt (or a confirmed carried delivery) is the evidence that
        // clears it -- the two places InboundEnvelopeProcessor reports.
        SprayPolicy.noteReceiptProgress(PEER, now)
        assertTrue(SprayPolicy.maySpray(PEER, LINK, CoreSprayTrigger.RECONNECT, now).allow)
    }

    @Test
    fun `the re-arm horizon is core's number`() {
        // MeshService.rearmGatedSpray consults gate.retryWorthArming rather
        // than comparing against a local constant; this is the number behind
        // that flag, and it lives in core.
        assertEquals(60_000L, SprayPolicy.retryArmMaxMs())
    }
}
