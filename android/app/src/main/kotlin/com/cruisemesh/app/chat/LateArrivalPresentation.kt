package com.cruisemesh.app.chat

import uniffi.cruisemesh_core.CoreMessageReceivedAt
import uniffi.cruisemesh_core.LateArrivalInput
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreLateArrivalFlags

/**
 * Which messages should show when they reached this phone, and at what time.
 *
 * A bubble carries the sender's send time, so a message carried for hours is
 * spliced into the middle of the thread -- above replies sent while it was in
 * flight. The core decides which of those are confusing enough to annotate
 * (`core/src/late_arrival.rs`: displaced by something already here, and at
 * least ten minutes behind its send time); this maps the answer onto the
 * stable keys the chat screens look bubbles up by.
 *
 * Returns arrival times only for the flagged messages -- absent means "render
 * nothing", which is the overwhelmingly common case.
 */
fun lateArrivalTimesByKey(
    visibleMessages: List<StoredMessage>,
    receivedTimes: List<CoreMessageReceivedAt>,
    ownUserId: ByteArray,
): Map<String, Long> {
    if (visibleMessages.isEmpty()) return emptyMap()
    val receivedByKey = receivedTimes.associate { arrivalKey(it.senderUserId, it.lamport) to it.receivedAtMs }
    val inputs = visibleMessages.map { message ->
        LateArrivalInput(
            displayTsMs = message.timestamp,
            arrivalTsMs = receivedByKey[arrivalKey(message.senderUserId, message.lamport)],
            isOwn = message.senderUserId.contentEquals(ownUserId),
        )
    }
    val flags = coreLateArrivalFlags(inputs)
    return buildMap {
        visibleMessages.forEachIndexed { index, message ->
            if (flags.getOrElse(index) { false }) {
                inputs[index].arrivalTsMs?.let { put(messageStableKey(message), it) }
            }
        }
    }
}

/**
 * A message is unique within one chat by (sender, lamport) -- the same pair
 * `MessageStore::chat_received_times` keys its rows by. Deliberately not
 * [messageStableKey], which also carries `kind`: the arrival rows don't
 * report a kind, and adding one to the query would buy nothing.
 */
private fun arrivalKey(senderUserId: ByteArray, lamport: ULong): String =
    "${senderUserId.contentHashCode()}:$lamport"
