package com.cruisemesh.app.mesh

/**
 * Pure, unit-testable atomic admission gate for concurrent inbound envelope
 * dispatch (audit FA5). [MeshService.processInboundEnvelope] is invoked from
 * four independent threads that can all be handling a copy of the *same*
 * `msg_id` at once: the central-GATT binder thread, the peripheral-GATT
 * binder thread, [LanTransport]'s `connectionExecutor`, and the relay-sync
 * thread. [GossipState.seenIds] is internally synchronized for each
 * individual `contains`/`record` call, but the surrounding check-then-record
 * window in `processInboundEnvelope` is *not* atomic on its own: two threads
 * can both observe "not seen yet" before either one calls `record`, so two
 * copies of one `msg_id` (e.g. arriving over BLE and LAN simultaneously --
 * routine for a nearby contact) both pass the gate and both get
 * delivered/flooded/carried.
 *
 * This class closes that window with a separate, purely-local in-flight set:
 * [tryBegin] atomically claims a `msg_id` for the calling thread -- any
 * concurrent duplicate is rejected outright, with no dispatch at all (the
 * caller should treat a `false` result exactly like [CoreInboundGate.SEEN]).
 * [finish] releases the claim once `processInboundEnvelope` reaches whatever
 * terminal state it was already going to reach. When [terminal] is true,
 * [onTerminal] runs *before* the claim is released, still under this
 * instance's lock -- so a fresh `tryBegin` for the same `msg_id` can never
 * observe the claim released before the real seen-set `record` lands. Every
 * method is `@Synchronized` on this instance (same one-monitor pattern as
 * [NotifyFailureTracker]), so `tryBegin`/`finish` compose into one atomic
 * operation even though [onTerminal] may call out to the (already
 * independently thread-safe) Rust [GossipState.seenIds].
 *
 * No Android or uniffi imports here on purpose: this is a plain data
 * structure, fully covered by JVM unit tests.
 */
class InboundEnvelopeAdmission {
    private val inFlight = HashSet<String>()

    /**
     * Claims [msgId] for the calling thread. Returns `true` the first time
     * any thread calls this for a given `msg_id` while no other copy of it
     * is currently in flight (i.e. between a `tryBegin` and its matching
     * `finish`); returns `false` for every concurrent duplicate. A caller
     * that gets `false` must drop the envelope without any dispatch -- a
     * copy of this exact `msg_id` is already being handled right now by
     * whichever thread got `true`.
     */
    @Synchronized
    fun tryBegin(msgId: ByteArray): Boolean = inFlight.add(msgId.toAdmissionKey())

    /**
     * Releases [msgId]'s in-flight claim. Pass `terminal = true` (the common
     * case -- matches the existing record-at-terminal-return sites in
     * [MeshService.processInboundEnvelope]) when the caller is about to
     * permanently record this `msg_id` as seen: [onTerminal] runs first,
     * still under this instance's lock, so no other thread can re-claim
     * [msgId] until after the record lands. Pass `terminal = false` for a
     * non-terminal outcome (DTN D4's FAILED path, or a carry that itself
     * failed to persist) that must leave `msg_id` re-presentable: the claim
     * is released with no [onTerminal] call, so the next copy -- on retry,
     * or arriving from any transport -- re-enters [tryBegin] as a fresh
     * attempt instead of being permanently rejected.
     */
    @Synchronized
    fun finish(msgId: ByteArray, terminal: Boolean, onTerminal: () -> Unit = {}) {
        if (terminal) {
            onTerminal()
        }
        inFlight.remove(msgId.toAdmissionKey())
    }

    /** Number of `msg_id`s currently claimed, for tests/diagnostics. */
    @Synchronized
    fun inFlightCount(): Int = inFlight.size
}

private val HEX_CHARS = "0123456789abcdef".toCharArray()

/** Lowercase hex encode, local to this file -- avoids a dependency on the `chat` package's [com.cruisemesh.app.chat.UserIdHex]. */
private fun ByteArray.toAdmissionKey(): String {
    val out = CharArray(size * 2)
    for (i in indices) {
        val b = this[i].toInt() and 0xFF
        out[i * 2] = HEX_CHARS[b ushr 4]
        out[i * 2 + 1] = HEX_CHARS[b and 0x0F]
    }
    return String(out)
}
