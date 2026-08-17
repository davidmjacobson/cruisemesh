package com.cruisemesh.app.devicelink

import uniffi.cruisemesh_core.CoreLinkAction
import uniffi.cruisemesh_core.CoreLinkActionKind
import uniffi.cruisemesh_core.CoreLinkApprovingDevice
import uniffi.cruisemesh_core.CoreLinkNewDevice
import uniffi.cruisemesh_core.CoreLinkOutcome
import uniffi.cruisemesh_core.CoreLinkRole
import uniffi.cruisemesh_core.CoreLinkSummary

/**
 * One side of the §9.1-§9.2 ceremony, behind the only shape a driver needs.
 *
 * The core exports the two halves as two objects with two different sets of
 * methods -- deliberately, because only one of them has a `confirm` at all
 * (§9.2 puts the tap on the device that is already part of the person). This
 * interface is how one loop can drive either without being handed the power to
 * confirm on the wrong phone: [confirmHere] exists on both, and on the new
 * device it is unreachable, because that side's action never says
 * `confirmHere`.
 */
internal interface LinkMachine {
    val role: CoreLinkRole

    fun start(nowMs: Long): CoreLinkAction
    fun resumeSent(nowMs: Long): CoreLinkAction
    fun resumePeerBytes(nowMs: Long, bytes: ByteArray): CoreLinkAction
    fun tick(nowMs: Long): CoreLinkAction
    fun cancel(nowMs: Long): CoreLinkSummary
    fun summary(): CoreLinkSummary?

    /** §9.2's explicit action. Only ever called on the approving device. */
    fun confirmHere(nowMs: Long, matches: Boolean): CoreLinkAction

    /** §9.3's seam, once the channel is confirmed. */
    fun seal(plaintext: ByteArray): ByteArray
    fun open(frame: ByteArray): ByteArray
}

internal class NewDeviceMachine(private val core: CoreLinkNewDevice) : LinkMachine {
    override val role = CoreLinkRole.NEW_DEVICE

    override fun start(nowMs: Long) = core.start(nowMs)
    override fun resumeSent(nowMs: Long) = core.resumeSent(nowMs)
    override fun resumePeerBytes(nowMs: Long, bytes: ByteArray) = core.resumePeerBytes(nowMs, bytes)
    override fun tick(nowMs: Long) = core.tick(nowMs)
    override fun cancel(nowMs: Long) = core.cancel(nowMs)
    override fun summary() = core.summary()

    override fun confirmHere(nowMs: Long, matches: Boolean): CoreLinkAction =
        // Not "returns false" and not "does nothing": a build that reached here
        // would be a build in which the new phone confirms its own link, which
        // is the one thing §9.2 exists to prevent. Fail loudly, in a unit test,
        // long before a person is holding two phones.
        error("the new device never holds the confirm (specs/multi-device-v1.md §9.2)")

    override fun seal(plaintext: ByteArray) = core.sealChannelFrame(plaintext)
    override fun open(frame: ByteArray) = core.openChannelFrame(frame)
}

internal class ApprovingDeviceMachine(private val core: CoreLinkApprovingDevice) : LinkMachine {
    override val role = CoreLinkRole.APPROVING_DEVICE

    override fun start(nowMs: Long) = core.start(nowMs)
    override fun resumeSent(nowMs: Long) = core.resumeSent(nowMs)
    override fun resumePeerBytes(nowMs: Long, bytes: ByteArray) = core.resumePeerBytes(nowMs, bytes)
    override fun tick(nowMs: Long) = core.tick(nowMs)
    override fun cancel(nowMs: Long) = core.cancel(nowMs)
    override fun summary() = core.summary()

    override fun confirmHere(nowMs: Long, matches: Boolean): CoreLinkAction =
        if (matches) core.confirm(nowMs) else core.decline(nowMs)

    override fun seal(plaintext: ByteArray) = core.sealChannelFrame(plaintext)
    override fun open(frame: ByteArray) = core.openChannelFrame(frame)
}

/** What the person has said about the digits, or that they have not said it yet. */
internal enum class LinkConfirmDecision { WAITING, MATCHES, DOES_NOT_MATCH }

/**
 * Everything the driver tells the outside world. Pure reporting: an observer
 * that threw would be a shell bug, not a ceremony failure, so the driver does
 * not defend against it.
 */
internal interface LinkCeremonyObserver {
    /** The offer to put on screen (new device only). */
    fun onQr(qrText: String) = Unit

    /** The digits both screens must show. `confirmHere` is §9.2's button. */
    fun onSas(sas: String, confirmHere: Boolean, warnSoftCap: Boolean) = Unit

    /** Called once per loop turn, so a screen can show that something is happening. */
    fun onProgress(sent: Boolean) = Unit
}

/**
 * The shell loop the core's driver-boundary design asks for, with no Android in
 * it (`specs/multi-device-v1.md` §9.2).
 *
 * It answers exactly one outstanding action at a time, which is the contract
 * the ceremony objects declare: a resume that does not match the action they
 * are waiting on changes nothing and is counted as a stale resume. Every wait
 * is bounded, every ending is the core's own named outcome, and the human tap
 * enters through [confirmDecision] rather than blocking a thread inside a state
 * machine.
 *
 * Nothing here is Android-specific and nothing here does IO, so both halves can
 * be run against each other in a JVM unit test over a pair of queues -- which
 * is what `LinkCeremonyDriverTest` does, and what has to be true before two
 * phones are worth picking up (§13's WP3 gate).
 */
internal class LinkCeremonyDriver(
    private val machine: LinkMachine,
    private val wire: LinkWire,
    private val clock: () -> Long,
    private val sleep: (Long) -> Unit,
    private val observer: LinkCeremonyObserver = object : LinkCeremonyObserver {},
    private val confirmDecision: () -> LinkConfirmDecision = { LinkConfirmDecision.WAITING },
    private val cancelled: () -> Boolean = { false },
) {
    /**
     * Run until the ceremony ends, and return the ending it named.
     *
     * [CoreLinkOutcome.CHANNEL_READY] means the channel is open and the wire is
     * still the way to it -- §9.3's bootstrap and §9.4's acknowledgement ride
     * the same pipe, sealed by [LinkMachine.seal].
     */
    fun run(): CoreLinkSummary {
        var action = machine.start(clock())
        while (true) {
            if (cancelled()) return machine.cancel(clock())
            when (val kind = action.kind) {
                is CoreLinkActionKind.Finished -> return kind.summary

                is CoreLinkActionKind.SendBytes -> {
                    wire.send(kind.bytes)
                    observer.onProgress(sent = true)
                    action = machine.resumeSent(clock())
                }

                is CoreLinkActionKind.ShowQr -> {
                    observer.onQr(kind.qrText)
                    action = awaitPeer(LINK_QR_POLL_MS)
                }

                is CoreLinkActionKind.AwaitPeer -> action = awaitPeer(kind.waitMs)

                is CoreLinkActionKind.ShowSas -> {
                    observer.onSas(kind.sas, kind.confirmHere, kind.warnSoftCap)
                    action = if (kind.confirmHere) {
                        when (confirmDecision()) {
                            // Still comparing two screens. The clock keeps
                            // running -- a person who walks away hits the
                            // ceremony's deadline, not a stuck thread.
                            LinkConfirmDecision.WAITING -> {
                                sleep(CONFIRM_POLL_MS)
                                machine.tick(clock())
                            }
                            LinkConfirmDecision.MATCHES -> machine.confirmHere(clock(), true)
                            LinkConfirmDecision.DOES_NOT_MATCH -> machine.confirmHere(clock(), false)
                        }
                    } else {
                        // §9.2: this side shows the digits and waits for the
                        // other one's sealed answer. There is no local override.
                        awaitPeer(LINK_QR_POLL_MS)
                    }
                }
            }
        }
    }

    /**
     * Wait for the peer, then resume with what arrived -- or tick, which is how
     * the core is told time passed without letting the shell decide what that
     * means.
     */
    private fun awaitPeer(waitMs: Long): CoreLinkAction {
        val bounded = waitMs.coerceIn(0L, LinkWireLimits.MAX_RECEIVE_WAIT_MS)
        val bytes = wire.receive(bounded)
        observer.onProgress(sent = false)
        return if (bytes != null) machine.resumePeerBytes(clock(), bytes) else machine.tick(clock())
    }

    private companion object {
        /**
         * How long to wait on the wire while an offer or the digits are on
         * screen. The core suggests a poll interval for its own
         * [CoreLinkActionKind.AwaitPeer]; these two actions carry none, because
         * from the core's side they are "a human is looking at something".
         *
         * Two seconds rather than one because these are the *long* phases -- an
         * offer can sit on a screen for minutes -- and on a relay rendezvous
         * every wait is a request against a family's own relay budget.
         */
        const val LINK_QR_POLL_MS = 2_000L

        /** How often to re-ask whether the person has answered the digits. */
        const val CONFIRM_POLL_MS = 250L
    }
}
