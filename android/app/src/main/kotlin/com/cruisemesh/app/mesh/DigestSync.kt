package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.DigestEntry

/**
 * Pure logic for interpreting an incoming per-chat DIGEST frame (DESIGN.md
 * §7.3). Split out from [MeshService] -- which owns the actual store/router
 * calls -- so this part is unit-testable without an Android Service or a BLE
 * stack, following the pattern set by [MeshRouterState] and
 * [ReconnectBackoffTracker]. See [MeshService.handleDigest] for how these are
 * used together.
 */
object DigestSync {

    /**
     * A DIGEST's wire `chatId` must equal the userId [MeshRouter] recorded
     * for the link it arrived on (via that link's HELLO) -- see
     * [MeshService]'s class KDoc for why "wire chatId = the frame's own
     * sender" is the convention every frame type follows. [helloUserId] is
     * null when no HELLO has been seen yet on this link, which makes a
     * DIGEST arriving before it out of order by construction.
     */
    fun isExpectedChatId(digestChatId: ByteArray, helloUserId: ByteArray?): Boolean =
        helloUserId != null && digestChatId.contentEquals(helloUserId)

    /**
     * The lamport a peer's digest [entries] reports having contiguously for
     * messages authored by [senderUserId]. An [entries] list with no matching
     * [DigestEntry] means the peer has nothing contiguous for that sender in
     * this chat, which is exactly what `0` means to both the retry logic and
     * [uniffi.cruisemesh_core.MessageStore.messagesAfter]: "send everything
     * after zero." Senders absent from the digest stay absent -- nothing is
     * inferred or summed across entries.
     */
    fun throughLamportForSender(entries: List<DigestEntry>, senderUserId: ByteArray): ULong =
        entries.firstOrNull { it.senderUserId.contentEquals(senderUserId) }?.throughLamport ?: 0uL

    /**
     * Convenience wrapper for the common "what does the peer already have of
     * our own authored stream?" lookup used by outgoing message resend.
     */
    fun throughLamportForSelf(entries: List<DigestEntry>, ownUserId: ByteArray): ULong =
        throughLamportForSender(entries, ownUserId)
}
