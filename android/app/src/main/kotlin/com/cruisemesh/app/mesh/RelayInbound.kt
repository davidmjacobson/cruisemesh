package com.cruisemesh.app.mesh

/**
 * What [MeshService.processInboundEnvelope] did with one relay- or
 * BLE-sourced envelope, so [MeshService.pollRelayMailbox] can decide whether
 * it is safe to ack (delete) it on the relay -- see [shouldAck].
 *
 * - [CONSUMED]: opened and delivered locally, either pairwise
 *   ([MeshService.deliverOpenedEnvelope]) or as a member of the group it was
 *   sealed for ([MeshService.deliverOpenedGroupEnvelope]). We're done with it.
 * - [CARRIED]: we could not open it (not addressed to us, and not a group we
 *   hold the key for) -- foreign traffic we flood onward and/or carry for its
 *   real recipient. This is also true when the envelope came FROM the relay
 *   via proxy-polling ([MeshService.relayProxyHints]): we're just a proxy, not
 *   the recipient.
 * - [EXPIRED]: dropped by the §5.3 expiry gate before we even tried to open
 *   it. Safe (and necessary) to ack -- it's dead weight on the relay either
 *   way, and nobody is coming back for it.
 * - [SEEN]: a redundant copy of a `msg_id` we already handled (gossip dedupe).
 *   We didn't re-derive a disposition for it, so we leave it alone on the
 *   relay; if it's still there, the pass (or a future one) that already
 *   assigned it CONSUMED/CARRIED/EXPIRED is authoritative.
 */
enum class InboundDisposition { CONSUMED, CARRIED, EXPIRED, SEEN }

/**
 * Whether a relay-fetched envelope with this [disposition] may be acked
 * (deleted from the relay mailbox).
 *
 * Only [InboundDisposition.CONSUMED] (it was ours to open, and we did) and
 * [InboundDisposition.EXPIRED] (it's dead weight regardless of who it was
 * for) are safe to remove. [InboundDisposition.CARRIED] must NOT be acked:
 * relay proxy-polling means we may have fetched a contact's envelope on
 * their behalf, and the relay copy is the durable fallback until the real
 * recipient (or another proxy) fetches and consumes it -- deleting it here
 * would silently drop the message. [InboundDisposition.SEEN] is left alone
 * for the same reason: we didn't actually evaluate this copy, so we can't
 * vouch for whether it's safe to remove.
 */
fun shouldAck(disposition: InboundDisposition): Boolean =
    disposition == InboundDisposition.CONSUMED || disposition == InboundDisposition.EXPIRED
