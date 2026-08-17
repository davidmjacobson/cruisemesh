package com.cruisemesh.app.devicelink

import java.io.Closeable

/**
 * The byte pipe one link ceremony runs over
 * (`specs/multi-device-v1.md` §9.2).
 *
 * The core's two state machines have no operating system in them: they hand the
 * shell bytes to deliver and are handed back exactly what arrived. This is that
 * shell's whole side of the bargain, and it is deliberately the smallest
 * interface that can express both transports the §13 gate asks for -- a LAN
 * socket, where bytes are there the moment the peer looks, and a relay
 * rendezvous, where each side posts into a mailbox and polls.
 *
 * Nothing here knows what a Noise handshake is, what the digits mean, or which
 * side confirms. That is the point: an implementation that could tell would be
 * an implementation that could get it wrong.
 */
internal interface LinkWire : Closeable {
    /** Deliver one ceremony message. Throws if it could not be delivered. */
    fun send(bytes: ByteArray)

    /**
     * Wait up to [waitMs] for one message from the peer, returning null if
     * nothing arrived in that window. Never blocks indefinitely: the ceremony
     * has a deadline and only the driver may decide it has passed.
     */
    fun receive(waitMs: Long): ByteArray?
}

/** Ceilings shared by both transports, so neither can be handed an absurdity. */
internal object LinkWireLimits {
    /**
     * Largest single message either transport will carry. The ceremony's own
     * frames are under a hundred bytes; a sealed bootstrap chunk is the big one
     * (`LINK_CHANNEL_MAX_PLAINTEXT_BYTES` plus the seal's overhead), and this
     * leaves room around it without letting a peer's first move be a large
     * allocation.
     */
    const val MAX_MESSAGE_BYTES = 96 * 1024

    /** Longest a single [LinkWire.receive] will ever be asked to wait. */
    const val MAX_RECEIVE_WAIT_MS = 5_000L
}
