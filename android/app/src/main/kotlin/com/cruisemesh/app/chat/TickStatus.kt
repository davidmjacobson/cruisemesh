package com.cruisemesh.app.chat

/**
 * A single message's tick state (DESIGN.md §7.2: ✓ sent, ✓✓ delivered, ✓✓
 * filled/tinted read). Only meaningful for messages *we* authored --
 * [ChatScreen] never computes this for the contact's own bubbles, since
 * receipts only ever describe what the peer did with messages we sent them.
 */
enum class TickStatus {
    /** Sealed and handed to the sync engine -- no receipt seen yet. */
    SENT,

    /** The recipient's device decrypted and stored it (a delivered receipt). */
    DELIVERED,

    /** The recipient viewed the chat (a read receipt). */
    READ,
}

/**
 * Derives [TickStatus] for one of our own messages at [lamport], given the
 * chat's current cumulative delivered/read watermarks (DESIGN.md §7.2:
 * receipts are cumulative -- "delivered/read through lamport N"). Pure and
 * store-free (both watermarks are cheap [uniffi.cruisemesh_core.MessageStore.receiptThrough]
 * reads the caller does once per chat load, see [ChatScreen]), so it's
 * unit-testable without a store or Compose -- see TickStatusTest.
 *
 * Read is checked first: a peer that raced its two receipts, or whose
 * delivered receipt got lost in transit (DTN, best-effort, see
 * `MeshService`'s read-receipt docs), can legitimately report a
 * `readThrough` at or above a `deliveredThrough` that lags behind, and
 * "read" is the more informative status to show in that case.
 */
fun tickStatusFor(lamport: ULong, deliveredThrough: ULong, readThrough: ULong): TickStatus = when (
    uniffi.cruisemesh_core.coreTickStatusFor(lamport, deliveredThrough, readThrough)
) {
    uniffi.cruisemesh_core.CoreTickStatus.READ -> TickStatus.READ
    uniffi.cruisemesh_core.CoreTickStatus.DELIVERED -> TickStatus.DELIVERED
    uniffi.cruisemesh_core.CoreTickStatus.SENT -> TickStatus.SENT
}
