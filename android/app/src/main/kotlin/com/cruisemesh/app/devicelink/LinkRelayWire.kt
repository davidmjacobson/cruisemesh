package com.cruisemesh.app.devicelink

import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import java.io.IOException
import java.security.SecureRandom
import uniffi.cruisemesh_core.CoreLinkLane
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.coreLinkRendezvousLane

/**
 * The §13 gate's relay-only leg: the ceremony over a pair of ephemeral relay
 * mailboxes, with no LAN and no BLE anywhere near it.
 *
 * The mailbox pair is derived from the scanned offer's key
 * ([coreLinkRendezvousLane]), so both devices find it without either one
 * publishing an address, and the relay is told nothing except that two opaque
 * blobs want storing. Nothing about this needs a relayd change -- these are
 * ordinary envelope rows under an ordinary recipient hint, which is exactly
 * what §2 promises when it says relayd stays content-agnostic.
 *
 * # Why nothing here acks
 *
 * A row this wire has read is left in the mailbox to expire, and the cursor is
 * what stops it being read twice. That is not laziness: a device running this
 * ceremony is, by §9.4, pre-activation and forbidden from acking anything, and
 * the safest way to keep a forbidden call from happening is not to have written
 * it. The rows are short-lived by construction (see [rendezvousExpiryMs]).
 *
 * # What the two devices must already have
 *
 * Both of them, a relay pass for the same family. The QR carries a relay base
 * URL and never a token -- a photograph of a screen must not be worth a
 * family's mailbox -- so a new phone reaches the relay leg only if someone has
 * already given it the family's Shore Pass. On LAN neither device needs
 * anything. This is a dev-tool constraint today; what a family sees is WP6's.
 */
internal class LinkRelayWire(
    private val config: RelayConfig,
    rendezvousId: ByteArray,
    private val sendLane: CoreLinkLane,
    private val receiveLane: CoreLinkLane,
    private val clock: () -> Long,
    private val sleep: (Long) -> Unit,
) : LinkWire {

    private val sendNamespace = coreLinkRendezvousLane(rendezvousId, sendLane)
    private val receiveNamespace = coreLinkRendezvousLane(rendezvousId, receiveLane)
    private val random = SecureRandom()

    /** Rows already taken. The relay's own row id, monotone per mailbox. */
    private var cursor = 0L

    override fun send(bytes: ByteArray) {
        require(bytes.size <= LinkWireLimits.MAX_MESSAGE_BYTES) {
            "link message is too large to send: ${bytes.size}"
        }
        val now = clock()
        val msgId = ByteArray(MSG_ID_BYTES).also(random::nextBytes)
        RelayClient.postEnvelope(
            config = config,
            msgId = msgId,
            hopTtl = 0u,
            recipientHint = computeRecipientHint(sendNamespace, now),
            sealed = bytes,
            expiryMs = now + rendezvousExpiryMs,
            network = null,
        )
    }

    override fun receive(waitMs: Long): ByteArray? {
        val deadline = clock() + waitMs.coerceIn(0L, LinkWireLimits.MAX_RECEIVE_WAIT_MS)
        while (true) {
            val page = try {
                RelayClient.fetchEnvelopes(config, receiveHints(clock()), cursor, FETCH_LIMIT)
            } catch (e: Exception) {
                throw IOException("the relay rendezvous could not be read", e)
            }
            val row = page.envelopes.firstOrNull()
            if (row != null) {
                cursor = row.id
                return row.sealed
            }
            val remaining = deadline - clock()
            if (remaining <= 0) return null
            sleep(minOf(remaining, POLL_INTERVAL_MS))
        }
    }

    /**
     * The mailbox to read, under each day key it could plausibly be filed
     * under.
     *
     * [computeRecipientHint] rotates on the UTC day boundary, and the two
     * phones do not share a clock. Three hints -- yesterday, today, tomorrow --
     * cost nothing against relayd's 256-hint fetch budget and mean a ceremony
     * started at 23:59 does not silently stop being heard a minute later.
     */
    private fun receiveHints(now: Long): List<ByteArray> =
        listOf(now - DAY_MS, now, now + DAY_MS).map { computeRecipientHint(receiveNamespace, it) }

    override fun close() {
        // Nothing to release: an HTTP rendezvous holds no socket between calls,
        // and the rows age out on their own.
    }

    private companion object {
        const val MSG_ID_BYTES = 16
        const val FETCH_LIMIT = 8
        /**
         * Floor on how often one mailbox is asked. A ceremony is minutes long
         * and every look is a request against the family's own relay budget --
         * the same budget ordinary delivery is spending at the same time.
         */
        const val POLL_INTERVAL_MS = 2_000L
        const val DAY_MS = 24 * 60 * 60 * 1_000L

        /**
         * How long a rendezvous row stands. Longer than one ceremony's deadline
         * so a slow confirm is not cut off mid-handshake, and far shorter than
         * ordinary mail so an abandoned ceremony leaves nothing behind.
         */
        const val rendezvousExpiryMs = 10 * 60 * 1_000L
    }
}
