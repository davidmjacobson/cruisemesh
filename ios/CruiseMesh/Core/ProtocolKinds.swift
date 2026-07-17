import Foundation

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
    static let attachmentManifest: UInt8 = 16
    static let attachmentChunk: UInt8 = 17
    static let reaction: UInt8 = 18
}

enum ReceiptType {
    static let delivered: UInt8 = 1
    static let read: UInt8 = 2
}

enum MeshDefaults {
    static let hopTtl: UInt8 = 7
    static let foreignCarryBudgetBytes: Int64 = 5 * 1024 * 1024
    static let carryHintDayWindow: Int64 = 7
    static let msPerDay: Int64 = 24 * 60 * 60 * 1000
    // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): the outgoing DIGEST's
    // advertised msg_id cap used to live here as `digestCarriedMsgIdsLimit`.
    // That decision -- and the carried+recently-held id list it bounds --
    // now lives in core (`engine.rs::DIGEST_ADVERTISED_MSG_IDS_LIMIT`,
    // behind `store.coreDigestAdvertisedMsgIds()`), so both platforms share
    // one source of truth instead of two constants that could drift.
    static let relayBatchLimit: UInt64 = 128
    static let ownOutboundSprayBudgetBytes: UInt64 = 256 * 1024
    static let ownReceiptSprayBudgetBytes: UInt64 = 64 * 1024
    static let relayPollIntervalNs: UInt64 = 60_000_000_000
}

func isVisibleChatKind(_ kind: UInt8) -> Bool {
    coreIsVisibleChatKind(kind: kind)
}

func isAuthoredChatKind(_ kind: UInt8) -> Bool {
    kind == ProtocolKind.text
        || kind == ProtocolKind.friendRequest
        || kind == ProtocolKind.groupInvite
        || kind == ProtocolKind.profileSync
        || kind == ProtocolKind.friendDirectory
        || kind == ProtocolKind.introducedFriendRequest
        || kind == ProtocolKind.lanEndpointHint
        || kind == ProtocolKind.attachmentManifest
        || kind == ProtocolKind.reaction
}
