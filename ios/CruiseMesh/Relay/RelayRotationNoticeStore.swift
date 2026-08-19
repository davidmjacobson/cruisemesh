import Foundation

/// One fact, kept for the surface: **the relay refused to re-key this family,
/// for good** (`specs/multi-device-v1.md` §10 step 2).
///
/// Removing a device promises the removed phone loses the family's Shore Pass
/// mailbox. Nearly always it does — but two answers from the relay mean it
/// never will from this device: a family whose token the operator manages
/// rather than the family (`rotation_unsupported`), and a family whose rotation
/// authority is somebody else's key (`rotation_unauthorized`, which is what
/// every household after the first gets on a *shared* pass).
/// `RelayRotationDriver` stops asking in both cases, correctly — and used to
/// stop silently, which left a person holding a promise the app had privately
/// given up on.
///
/// So it is written down here instead. Durable rather than per-process, because
/// the refusal outlives the launch it happened on and the person may not open
/// Your devices for a week; cleared the moment a rotation is planned afresh or
/// one lands, because either makes the note untrue.
///
/// Mirrors Android `RelayRotationNoticeStore`.
enum RelayRotationNoticeStore {
    private static let blockedKey = "cruisemesh.relay.rotationBlocked"

    /// True when a device removed from this person's fleet may still be able to
    /// reach the family mailbox because the pass could not be changed.
    static func blocked() -> Bool {
        AppDefaults.current.bool(forKey: blockedKey)
    }

    static func setBlocked(_ blocked: Bool) {
        // Only a change is written: this runs on every relay pass that settles
        // a rotation.
        guard AppDefaults.current.bool(forKey: blockedKey) != blocked else { return }
        AppDefaults.current.set(blocked, forKey: blockedKey)
    }
}
