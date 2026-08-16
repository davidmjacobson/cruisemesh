import Foundation

/// Which engine sequences one encounter — everything that happens between two
/// phones from the moment they identify each other until the burst is over.
///
/// One selection per encounter, and deliberately no third value. A per-lane
/// mix — core picks the digest, the shell picks the drain — is the shape of
/// rollback nobody can reason about: the digest, the targeted drain and the
/// mule spray share one carried-offer reservation, one byte allowance and one
/// cadence verdict, so two owners inside one encounter would each spend the
/// other's budget. The rollback story is "the next encounter runs the other
/// engine", nothing finer. Mirrors the shape of `InboundPathEngine`.
enum MeetEngine {
    /// The sequencing `MeshController` has run since before this refactor.
    case legacy
    /// `MessageStore.corePlanMeshMeet` — the one encounter planner in core —
    /// driven from `MeshController` through `MeetAdapter`.
    case core
}

/// The internal encounter settings that are not a person's to change: which
/// engine sequences one encounter.
///
/// Kept in the same defaults the rest of this device's internal switches live
/// in (`AppDefaults.current`) rather than in the message store, for the reason
/// `RelayEngineSettings` spells out: the whole selection has to be removable
/// once the legacy sequencing is deleted, and a flag that had earned a column
/// in the store's forward-only schema would not be. A preference key simply
/// stops being read.
///
/// The switch is reachable from the internal tools screen, behind the same
/// door as the relay and receive engine switches, because a closed-test build
/// is release-signed and a migration whose flag could only be set by a unit
/// test can never produce the field evidence it exists to produce.
///
/// There is deliberately no shadow comparison. Running both sequencers over
/// one encounter would put two digests, two drains and two sprays on the same
/// link, so the comparison would itself be a far larger behaviour change than
/// the thing it set out to measure.
enum MeetEngineSettings {

    /// Absent means `.legacy`.
    private static let meetEngineKey = "cruisemesh.mesh.meetEngineCore"

    /// The engine the *next* encounter will use.
    ///
    /// Read once at the top of an encounter and not consulted again while that
    /// encounter runs, so flipping the switch cannot split one burst between
    /// two sequencers.
    ///
    /// Defaults to legacy, and stays there until field evidence says
    /// otherwise. A default that quietly moved with a release would make "roll
    /// back" mean "ship again".
    static func meetEngine() -> MeetEngine {
        AppDefaults.current.bool(forKey: meetEngineKey) ? .core : .legacy
    }

    static func setMeetEngine(_ engine: MeetEngine) {
        AppDefaults.current.set(engine == .core, forKey: meetEngineKey)
    }
}
