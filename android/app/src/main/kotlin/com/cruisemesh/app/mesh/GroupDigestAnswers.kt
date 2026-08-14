package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex

/**
 * Group digests answered on a live link, so the 1:1 digest fallback does not
 * re-send from lamport 0 a group this link already caught up precisely.
 *
 * Scoped to the link, not the process: [forget] runs on every disconnect,
 * matching the lifetime the core gives `hidden_offered`. A peer that
 * reconnects without sending a group digest -- a reinstall, a wiped database,
 * a downgrade to a build that never sends one -- must still get the lamport-0
 * catch-up, and the record must not grow for the life of the service.
 */
internal class GroupDigestAnswers {
    private val byAddress = mutableMapOf<String, MutableSet<String>>()

    /** Records that [address] got a watermarked answer for [groupId]. */
    @Synchronized
    fun note(address: String, groupId: ByteArray) {
        byAddress.getOrPut(address) { mutableSetOf() } += UserIdHex.encode(groupId)
    }

    @Synchronized
    fun answered(address: String, groupId: ByteArray): Boolean =
        byAddress[address]?.contains(UserIdHex.encode(groupId)) == true

    /** Called when [address]'s link drops, so the next encounter starts clean. */
    @Synchronized
    fun forget(address: String) {
        byAddress.remove(address)
    }
}
