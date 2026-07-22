package com.cruisemesh.app.mesh

// FA15: wire-level constants shared by MeshService, RelaySyncEngine, and
// InboundEnvelopeProcessor. Moved verbatim from MeshService.kt when the relay
// and envelope pipelines were extracted; values mirror core/DESIGN.md and must
// not drift from them.

/** `kind` bytes from DESIGN.md §7.1. */
internal const val KIND_TEXT: UByte = 1u
internal const val KIND_RECEIPT: UByte = 2u
internal const val KIND_FRIEND_REQUEST: UByte = 3u
internal const val KIND_GROUP_INVITE: UByte = 4u
internal const val KIND_PROFILE_SYNC: UByte = 5u
internal const val KIND_FRIEND_DIRECTORY: UByte = 6u
internal const val KIND_INTRODUCED_FRIEND_REQUEST: UByte = 7u
internal const val KIND_LAN_ENDPOINT_HINT: UByte = 8u
internal const val KIND_GROUP_METADATA_UPDATE: UByte = 19u

/** `receipt_type` values (DESIGN.md §7.2): delivered = recipient decrypted and stored it, read = recipient viewed the chat. */
internal const val RECEIPT_TYPE_DELIVERED: UByte = 1u
internal const val RECEIPT_TYPE_READ: UByte = 2u

/** DESIGN.md §5.3: hop budget a freshly authored envelope's §6.4 header starts with. Mirrors core's `DEFAULT_HOP_TTL`. */
internal const val DEFAULT_HOP_TTL: UByte = 7u

internal const val MS_PER_DAY: Long = 24L * 60 * 60 * 1000

internal const val RELAY_STORE_BATCH_LIMIT: ULong = 128uL
