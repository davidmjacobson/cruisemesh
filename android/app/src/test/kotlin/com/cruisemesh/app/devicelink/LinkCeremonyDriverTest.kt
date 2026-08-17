package com.cruisemesh.app.devicelink

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreLinkApprovingDevice
import uniffi.cruisemesh_core.CoreLinkNewDevice
import uniffi.cruisemesh_core.CoreLinkOutcome
import uniffi.cruisemesh_core.CoreLinkSummary
import uniffi.cruisemesh_core.LinkBudgets

/**
 * The shell loop the §13 WP3 gate rests on, driven end to end in process.
 *
 * The core already proves its two state machines against each other
 * (`core/tests/device_link_ceremony.rs`). What is unproven until here is the
 * Kotlin that will actually be holding them on two phones: that it answers the
 * one outstanding action, that it never confirms on the wrong device, and that
 * it behaves the same whether bytes arrive the instant they are sent or sit in
 * a mailbox until somebody polls.
 *
 * Both transports the gate names are represented by their *shape*, which is the
 * only thing the ceremony can tell apart: [Shape.LIVE] is a LAN socket, and
 * [Shape.MAILBOX] is a relay rendezvous, where every read costs a poll and the
 * clock really moves.
 */
class LinkCeremonyDriverTest {

    private enum class Shape { LIVE, MAILBOX }

    /** One direction of an in-memory pipe, with the transport's shape baked in. */
    private class QueueWire(
        private val inbox: LinkedBlockingQueue<ByteArray>,
        private val outbox: LinkedBlockingQueue<ByteArray>,
        private val shape: Shape,
    ) : LinkWire {
        override fun send(bytes: ByteArray) {
            outbox.put(bytes)
        }

        override fun receive(waitMs: Long): ByteArray? =
            when (shape) {
                Shape.LIVE -> inbox.poll(waitMs, TimeUnit.MILLISECONDS)
                // A mailbox does not hand anything over the moment it lands:
                // the reader looks, finds nothing, and comes back later.
                Shape.MAILBOX -> inbox.poll()
            }

        override fun close() = Unit
    }

    private class Run(
        val newcomer: CoreLinkSummary,
        val approver: CoreLinkSummary,
        val newcomerSas: String?,
        val approverSas: String?,
        val newcomerWasAskedToConfirm: Boolean,
    )

    /**
     * Drive both halves to an ending, each on its own thread -- which is how
     * they will really run, one per phone.
     */
    private fun link(
        shape: Shape,
        activeDeviceCount: UInt = 1u,
        answer: LinkConfirmDecision = LinkConfirmDecision.MATCHES,
        newcomerLanEndpoints: List<String> = listOf("192.168.1.24:45892"),
    ): Run {
        val toNewcomer = LinkedBlockingQueue<ByteArray>()
        val toApprover = LinkedBlockingQueue<ByteArray>()
        val budgets = LinkBudgets(deadlineMs = 20_000, pollIntervalMs = 20, qrLifetimeMs = 60_000)

        CoreLinkNewDevice(newcomerLanEndpoints, emptyList(), now(), budgets).use { newcomer ->
            CoreLinkApprovingDevice.scan(newcomer.qrText(), activeDeviceCount, budgets).use { approver ->
                var newcomerSas: String? = null
                var approverSas: String? = null
                var newcomerAskedToConfirm = false

                val newcomerDriver = LinkCeremonyDriver(
                    machine = NewDeviceMachine(newcomer),
                    wire = QueueWire(toNewcomer, toApprover, shape),
                    clock = ::now,
                    sleep = ::sleep,
                    observer = object : LinkCeremonyObserver {
                        override fun onSas(sas: String, confirmHere: Boolean, warnSoftCap: Boolean) {
                            newcomerSas = sas
                            newcomerAskedToConfirm = newcomerAskedToConfirm || confirmHere
                        }
                    },
                )
                val approverDriver = LinkCeremonyDriver(
                    machine = ApprovingDeviceMachine(approver),
                    wire = QueueWire(toApprover, toNewcomer, shape),
                    clock = ::now,
                    sleep = ::sleep,
                    observer = object : LinkCeremonyObserver {
                        override fun onSas(sas: String, confirmHere: Boolean, warnSoftCap: Boolean) {
                            approverSas = sas
                        }
                    },
                    confirmDecision = { if (approverSas == null) LinkConfirmDecision.WAITING else answer },
                )

                var newcomerSummary: CoreLinkSummary? = null
                var approverSummary: CoreLinkSummary? = null
                val threads = listOf(
                    Thread { newcomerSummary = newcomerDriver.run() },
                    Thread { approverSummary = approverDriver.run() },
                )
                threads.forEach { it.isDaemon = true; it.start() }
                threads.forEach { it.join(30_000) }

                return Run(
                    newcomer = requireNotNull(newcomerSummary) { "the new device never finished" },
                    approver = requireNotNull(approverSummary) { "the approving device never finished" },
                    newcomerSas = newcomerSas,
                    approverSas = approverSas,
                    newcomerWasAskedToConfirm = newcomerAskedToConfirm,
                )
            }
        }
    }

    /** **The gate's LAN shape**, driven by the loop that will drive the phones. */
    @Test
    fun twoDevicesLinkOverALiveLink() {
        val run = link(Shape.LIVE)
        assertLinked(run)
    }

    /**
     * **The gate's relay-only shape.** Nothing arrives the moment it is sent;
     * each side polls, finds an empty mailbox, and ticks. The ceremony cannot
     * tell, which is the property under test.
     */
    @Test
    fun twoDevicesLinkOverAStoreAndForwardRendezvous() {
        val run = link(Shape.MAILBOX)
        assertLinked(run)
    }

    private fun assertLinked(run: Run) {
        assertEquals(CoreLinkOutcome.CHANNEL_READY, run.newcomer.outcome)
        assertEquals(CoreLinkOutcome.CHANNEL_READY, run.approver.outcome)

        // The digits a person compares are the same on both screens, and the
        // channel both ends hold is the same channel.
        assertNotNull(run.newcomerSas)
        assertEquals(run.newcomerSas, run.approverSas)
        assertEquals(run.newcomer.sas, run.approver.sas)
        assertTrue(
            run.newcomer.channelBinding!!.contentEquals(run.approver.channelBinding!!),
        )

        // §9.2: the confirm lives on the device that is already part of the
        // person. The new phone is never offered it, on any transport.
        assertFalse(
            "the new device was offered the confirm",
            run.newcomerWasAskedToConfirm,
        )

        // Nothing was answered out of turn: a resume that did not match the
        // outstanding action would have been counted here instead of acted on.
        assertEquals(0u, run.newcomer.staleResumesIgnored)
        assertEquals(0u, run.approver.staleResumesIgnored)
    }

    /**
     * The digits did not match, so nothing is adopted and both sides say why --
     * including the phone that had no button, which learns it from the channel
     * rather than from a timeout.
     */
    @Test
    fun digitsThatDoNotMatchEndTheCeremonyOnBothSides() {
        val run = link(Shape.LIVE, answer = LinkConfirmDecision.DOES_NOT_MATCH)
        assertEquals(CoreLinkOutcome.DECLINED, run.approver.outcome)
        assertEquals(CoreLinkOutcome.DECLINED, run.newcomer.outcome)
    }

    /**
     * §14.3's hard cap, felt at the only moment it can be: the scan. The
     * scanner refuses before a single byte is sent, so the phone showing the QR
     * is left waiting and learns nothing about the other person's device count.
     */
    @Test
    fun aPersonAtTheHardCapLinksNothing() {
        val budgets = LinkBudgets(deadlineMs = 20_000, pollIntervalMs = 20, qrLifetimeMs = 60_000)
        CoreLinkNewDevice(listOf("192.168.1.24:45892"), emptyList(), now(), budgets).use { newcomer ->
            CoreLinkApprovingDevice.scan(newcomer.qrText(), 16u, budgets).use { approver ->
                val summary = LinkCeremonyDriver(
                    machine = ApprovingDeviceMachine(approver),
                    wire = QueueWire(
                        LinkedBlockingQueue(),
                        LinkedBlockingQueue(),
                        Shape.LIVE,
                    ),
                    clock = ::now,
                    sleep = ::sleep,
                ).run()
                assertEquals(CoreLinkOutcome.DEVICE_CAP_REACHED, summary.outcome)
                assertEquals(0u, summary.messagesSent)
                assertNull(newcomer.summary())
            }
        }
    }

    private fun now(): Long = System.currentTimeMillis()

    private fun sleep(ms: Long) {
        if (ms > 0) Thread.sleep(minOf(ms, 50L))
    }
}
