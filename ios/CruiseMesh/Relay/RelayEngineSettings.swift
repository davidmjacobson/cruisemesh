import Foundation

/// Which engine runs a relay pass.
///
/// One selection for a whole pass, and there is deliberately no third value.
/// Per-stage mixing is the shape of rollback that cannot be reasoned about: a
/// pass whose uploads came from one engine and whose walk came from the other
/// has no single owner for the cursor it advanced, and no transcript that
/// explains what it did. The rollback story is "the next pass runs the other
/// engine", nothing finer. Mirrors Android `RelayPassEngine`.
enum RelayPassEngine {
    /// The engine that has been moving mail since before this refactor.
    case legacy
    /// `CoreRelayPass`, driven through `RelaySyncDriver`.
    case core
}

/// The internal relay settings that are not a person's to change: which engine
/// runs a pass, and whether the migration canary may sample.
///
/// Kept in the same defaults the rest of this device's relay settings live in
/// (`RelayConfigStore` uses `AppDefaults.current`) rather than in the message
/// store, and that is load-bearing rather than convenient. The whole selection
/// has to be removable once the legacy engine is deleted, and a flag that had
/// earned a column in the store's forward-only schema would not be: removing it
/// later would mean either a migration that drops nothing or a dead column kept
/// forever. A handful of preference keys simply stop being read. Mirrors
/// Android `RelayEngineSettings`.
///
/// The engine switch is reachable from the developer settings screen, behind the
/// same door as the manual relay fields, because a closed-test build is
/// release-signed and a canary whose flag could only be set by a unit test can
/// never produce the evidence it exists to produce.
enum RelayEngineSettings {

    /// Absent means `.legacy`.
    private static let passEngineKey = "cruisemesh.relay.passEngineCore"
    private static let shadowEnabledKey = "cruisemesh.relay.passEngineShadow"
    private static let allowRoamingDataKey = "cruisemesh.relay.allowRoamingData"
    private static let shadowDayKey = "cruisemesh.relay.passEngineShadowDay"
    private static let shadowCountKey = "cruisemesh.relay.passEngineShadowCount"
    private static let shadowLastMsKey = "cruisemesh.relay.passEngineShadowLastMs"

    /// The engine the *next* pass will use.
    ///
    /// Read once when a pass starts and not consulted again, so flipping the
    /// switch mid-pass changes nothing about the pass already running.
    ///
    /// Defaults to legacy, and stays there until canary evidence says
    /// otherwise. A default that quietly moved with a release would make "roll
    /// back" mean "ship again".
    static func passEngine() -> RelayPassEngine {
        AppDefaults.current.bool(forKey: passEngineKey) ? .core : .legacy
    }

    static func setPassEngine(_ engine: RelayPassEngine) {
        AppDefaults.current.set(engine == .core, forKey: passEngineKey)
    }

    /// Whether the migration canary may sample legacy passes.
    ///
    /// Defaults to **on**, and that is worth stating plainly rather than filing
    /// under "the default path is unchanged": a shipped device runs the canary.
    /// What it does not do is change anything the device sends, receives, marks
    /// or stores — it reads what the pass already observed and writes one
    /// bounded diagnostics record on a handful of passes a day. The mail-moving
    /// path is byte-identical with it on or off.
    static func shadowEnabled() -> Bool {
        guard AppDefaults.current.object(forKey: shadowEnabledKey) != nil else { return true }
        return AppDefaults.current.bool(forKey: shadowEnabledKey)
    }

    static func setShadowEnabled(_ enabled: Bool) {
        AppDefaults.current.set(enabled, forKey: shadowEnabledKey)
    }

    static func allowsRoamingData() -> Bool {
        AppDefaults.current.bool(forKey: allowRoamingDataKey)
    }

    static func setAllowsRoamingData(_ enabled: Bool) {
        AppDefaults.current.set(enabled, forKey: allowRoamingDataKey)
    }

    /// The canary's sampling state, across process launches.
    ///
    /// Three numbers, in the defaults the engine flag already lives in, and
    /// they are what makes "a bounded number of samples a day" true. Held only
    /// in memory the count would reset to zero on every service start and a
    /// fresh sampler always samples its first pass, so a phone whose foreground
    /// work iOS keeps relaunching would sample nearly every pass — the opposite
    /// of a bound.
    static func shadowSampler() -> CoreRelayShadowSampler {
        CoreRelayShadowSampler(
            dayIndex: Int64(AppDefaults.current.integer(forKey: shadowDayKey)),
            samplesToday: UInt32(max(0, AppDefaults.current.integer(forKey: shadowCountKey))),
            lastSampleAtMs: Int64(AppDefaults.current.integer(forKey: shadowLastMsKey))
        )
    }

    static func setShadowSampler(_ state: CoreRelayShadowSampler) {
        AppDefaults.current.set(Int(state.dayIndex), forKey: shadowDayKey)
        AppDefaults.current.set(Int(state.samplesToday), forKey: shadowCountKey)
        AppDefaults.current.set(Int(state.lastSampleAtMs), forKey: shadowLastMsKey)
    }
}

/// Whether the canary may run at all, given the engine this pass chose.
///
/// A rule rather than a condition inlined at the call site, because it is the
/// one that keeps the mechanism honest: shadowing the core engine with the core
/// planner compares a thing to itself and can only ever agree, so every record
/// it produced would be evidence of nothing while looking exactly like evidence
/// of something. Mirrors Android `relayShadowPermitted`.
func relayShadowPermitted(_ engine: RelayPassEngine, shadowEnabled: Bool) -> Bool {
    shadowEnabled && engine == .legacy
}
