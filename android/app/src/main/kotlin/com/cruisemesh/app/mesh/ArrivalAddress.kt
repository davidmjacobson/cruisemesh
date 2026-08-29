package com.cruisemesh.app.mesh

/**
 * Where an inbound envelope arrived from, for the two different questions the
 * delivery tree asks about it: what to call the source in a log line, and
 * whether there is a live link to answer on.
 *
 * Keeping those apart is the whole point of the type. An envelope fetched
 * from the relay has no link, so the receive path labelled it with the
 * literal string `"relay"` and then passed that label down as if it were an
 * address -- including into [MeshRouter.sendToAddress], which no transport is
 * ever registered under. Every such send was a guaranteed no-op plus a
 * warning, so the delivered/read receipt for a message that came in over the
 * internet never went back on the spot, not even when the sender was right
 * there on Bluetooth or the same Wi-Fi. It healed later (the durable
 * outgoing-receipt envelope still rides the relay upload lane, and
 * [ReceiptRepair] re-offers it on the next encounter), but the sender's ticks
 * waited on a sync pass for a peer that was reachable the whole time.
 *
 * iOS never flattened the two: it carries its source as an optional all the
 * way down and branches on it at every receipt site, which is the shape this
 * type reproduces. [link] is that same discriminant, made impossible to
 * forget -- it is null exactly when the relay handed the envelope over, and
 * it is the only thing here that may be given to something that sends on an
 * address.
 */
@JvmInline
value class ArrivalAddress private constructor(private val value: String?) {

    /**
     * The live BLE/LAN link this envelope arrived on, or null when it came
     * from the relay. Never [label]: a label is not somewhere a frame can go.
     */
    val link: String? get() = value

    /** What this source is called in a log line. */
    val label: String get() = value ?: RELAY_LABEL

    /** Whether a frame can be answered on the exact link this arrived on. */
    val isLiveLink: Boolean get() = value != null

    override fun toString(): String = label

    companion object {
        /** The name relay-fetched envelopes have always carried in the log. */
        const val RELAY_LABEL = "relay"

        /** The relay handed this envelope over; there is no link to reply on. */
        val relay = ArrivalAddress(null)

        /**
         * [sourceAddress] is the receive path's existing discriminant: null
         * means the relay produced this envelope, non-null means it arrived
         * over a live link.
         */
        fun of(sourceAddress: String?): ArrivalAddress =
            if (sourceAddress == null) relay else ArrivalAddress(sourceAddress)
    }
}
