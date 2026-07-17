package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.encodeEnvelopeFrame

/** Thin transport adapter for an envelope authored and persisted by Rust. */
fun encodeOutboundEnvelopeFrame(envelope: OutboundEnvelope): ByteArray =
    encodeEnvelopeFrame(
        envelope.msgId,
        envelope.hopTtl,
        envelope.expiry,
        envelope.recipientHint,
        envelope.sealed,
    )
