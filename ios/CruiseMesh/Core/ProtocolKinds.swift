/// Message body `kind` values (DESIGN.md §7.1) — must match Android + Rust.
enum ProtocolKind {
    static let text: UInt8 = 1
    static let receipt: UInt8 = 2
    static let friendRequest: UInt8 = 3
    static let groupInvite: UInt8 = 4
    static let profileSync: UInt8 = 5
    static let friendDirectory: UInt8 = 6
    static let introducedFriendRequest: UInt8 = 7
    static let lanEndpointHint: UInt8 = 8
    static let relayUpdate: UInt8 = 9
    static let attachmentManifest: UInt8 = 16
    static let reaction: UInt8 = 18
    static let groupMetadataUpdate: UInt8 = 19
}

enum ReceiptType {
    static let delivered: UInt8 = 1
    static let read: UInt8 = 2
}

enum MeshDefaults {
    static let hopTtl: UInt8 = 7
    static let foreignCarryBudgetBytes: Int64 = 5 * 1024 * 1024
    // carryHintDayWindow / msPerDay: gone -- hint aggregation moved into the
    // Rust core (core/src/recipient_hints.rs), which owns the day windows now.
    // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): the outgoing DIGEST's
    // advertised msg_id cap used to live here as `digestCarriedMsgIdsLimit`.
    // That decision -- and the carried+recently-held id list it bounds --
    // now lives in core (`engine.rs::DIGEST_ADVERTISED_MSG_IDS_LIMIT`,
    // behind `store.coreDigestAdvertisedMsgIds()`), so both platforms share
    // one source of truth instead of two constants that could drift.
    static let relayStoreBatchLimit: UInt64 = 128
    // The three per-encounter spray byte budgets that used to live here --
    // own outbound 256 KiB, receipts 64 KiB, carried 256 KiB -- are gone.
    // Android carried the same three numbers in InboundEnvelopeProcessor.kt;
    // they were equal, and nothing made them stay equal. They now live in
    // `core/src/spray_policy.rs` beside the plan that spends them, and arrive
    // at the call sites inside a `CoreSprayGate` already scaled by what the
    // link can take (#280, ledger row SPRAY-01).
    //
    // foreignCarryBudgetBytes above is deliberately NOT one of them: it bounds
    // how much foreign traffic this device *stores*, not how much it offers in
    // one encounter.
}

func isVisibleChatKind(_ kind: UInt8) -> Bool {
    coreIsVisibleChatKind(kind: kind)
}

/// The `hop_ttl` value to persist for a foreign envelope entering the carry
/// queue (DESIGN.md §5.3 store-and-forward), Android `carriedHopTtl` twin.
/// This device's carry of the envelope is itself a hop, consumed from the
/// sender-authored budget the same way the flood/relay path
/// (`MeshController.relayForeign`) already decrements on every re-flood;
/// before this, the carry path stored `hopTtl` verbatim at every stage
/// (enqueue, drain), so the displayed hop count
/// (`MeshController.messageArrival`'s `hopsTaken`) under-counted a pure carry
/// hand-off by exactly the muled leg -- a single-mule delivery showed "~0
/// hops" instead of "~1 hop". Decrementing once here, at carry enqueue time
/// (`MeshController.carryForeign`), keeps every downstream consumer (drain,
/// re-flood, digest paths) consistent without touching them, since they all
/// forward the already-decremented stored value.
///
/// Saturating: `hopTtl == 0` (this node is already the final carrier -- see
/// `relayForeign`'s "hop budget exhausted" guard) stays `0` rather than
/// underflowing. Carry/drop eligibility for a zero-TTL envelope is unchanged
/// by this function; it only affects the stored value for envelopes that do
/// get carried.
func carriedHopTtl(_ authoredHopTtl: UInt8) -> UInt8 {
    authoredHopTtl > 0 ? authoredHopTtl - 1 : 0
}

/// The hop count shown in Message info ("Arrived via another device ·
/// ~N hops"): inferred as the sender-authored budget minus whatever
/// `hop_ttl` this device received an envelope with. Android
/// `arrivalHopsTaken` twin -- extracted out of `MeshController.messageArrival`
/// so it's directly unit-testable, matching Android's pattern.
/// Saturating both directions: a `receivedHopTtl` above `initialHopTtl`
/// (shouldn't happen -- `coreInboundGate` rejects it -- but this function has
/// no store access to rely on that) clamps to `0` hops taken rather than
/// underflowing.
func arrivalHopsTaken(receivedHopTtl: UInt8, initialHopTtl: UInt8 = MeshDefaults.hopTtl) -> UInt8 {
    initialHopTtl >= receivedHopTtl ? initialHopTtl - receivedHopTtl : 0
}
