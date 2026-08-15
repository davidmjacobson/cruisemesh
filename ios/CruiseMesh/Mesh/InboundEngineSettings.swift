import Foundation

/// Which engine dispositions one arriving envelope.
///
/// One selection per envelope, and deliberately no third value. The inbound
/// path's unit of work is a single frame — gate, open, deliver-or-carry,
/// re-flood, record — and a frame whose gate came from one engine and whose
/// carry came from the other has no single owner for the `seen` record it left
/// behind or the relay row it may have made ackable. The rollback story is
/// "the next envelope runs the other engine", nothing finer. Mirrors the shape
/// of `RelayPassEngine`.
enum InboundPathEngine {
    /// The engine that has been receiving mail since before this refactor:
    /// `MeshController.processInboundEnvelope`'s own gate, open, deliver,
    /// carry and re-flood.
    case legacy
    /// `MessageStore.processInboundFrame` — the one transactional inbound
    /// disposition in core — driven from `MeshController` through
    /// `InboundAdapter`.
    case core
}

/// The internal receive settings that are not a person's to change: which
/// engine dispositions an arriving envelope.
///
/// Kept in the same defaults the rest of this device's internal switches live
/// in (`AppDefaults.current`) rather than in the message store, for the reason
/// `RelayEngineSettings` spells out: the whole selection has to be removable
/// once the legacy path is deleted, and a flag that had earned a column in the
/// store's forward-only schema would not be. A preference key simply stops
/// being read.
///
/// The switch is reachable from the internal tools screen, behind the same door
/// as the relay engine switch, because a closed-test build is release-signed
/// and a migration whose flag could only be set by a unit test can never
/// produce the field evidence it exists to produce.
enum InboundEngineSettings {

    /// Absent means `.legacy`.
    private static let pathEngineKey = "cruisemesh.mesh.inboundPathEngineCore"

    /// The engine the *next* arriving envelope will use.
    ///
    /// Read once at the top of `processInboundEnvelope` and not consulted again
    /// while that envelope is being handled, so flipping the switch cannot mix
    /// engines within one frame.
    ///
    /// Defaults to legacy, and stays there until field evidence says otherwise.
    /// A default that quietly moved with a release would make "roll back" mean
    /// "ship again".
    static func pathEngine() -> InboundPathEngine {
        AppDefaults.current.bool(forKey: pathEngineKey) ? .core : .legacy
    }

    static func setPathEngine(_ engine: InboundPathEngine) {
        AppDefaults.current.set(engine == .core, forKey: pathEngineKey)
    }
}
