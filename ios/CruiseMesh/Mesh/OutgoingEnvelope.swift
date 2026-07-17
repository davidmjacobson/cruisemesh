import Foundation

/// Thin transport adapter for an envelope authored and persisted by Rust.
func encodeOutboundEnvelopeFrame(_ envelope: OutboundEnvelope) -> Data {
    encodeEnvelopeFrame(
        msgId: envelope.msgId,
        hopTtl: envelope.hopTtl,
        expiry: envelope.expiry,
        recipientHint: envelope.recipientHint,
        sealed: envelope.sealed
    )
}
