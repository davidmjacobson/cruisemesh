//! Shared resource limits for data received from peers and relays.

/// Maximum decoded sealed-envelope payload accepted by the relay and P2P
/// transports. This mirrors relayd's independently enforced admission limit.
pub const MAX_ENVELOPE_SEALED_BYTES: usize = 512 * 1024;

/// Frame type plus the public envelope header: message ID, hop count, expiry,
/// and recipient hint.
pub const ENVELOPE_FRAME_OVERHEAD: usize = 1 + 16 + 1 + 8 + 8;

/// Maximum complete application frame accepted from a peer. The envelope
/// frame is the largest supported frame, so allow the relay's full sealed
/// payload ceiling plus its fixed public header.
pub const MAX_P2P_FRAME_BYTES: usize = ENVELOPE_FRAME_OVERHEAD + MAX_ENVELOPE_SEALED_BYTES;
