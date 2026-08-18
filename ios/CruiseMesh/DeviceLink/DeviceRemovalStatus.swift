import Combine
import Foundation

/// §10 step 5, seen from the phone that was removed.
///
/// Core is where the decision lives: a signed roster of this person's own devices
/// that buries this one ejects it — the roster is stored, the fleet projection is
/// cleared, and the activation stage becomes `CoreLinkActivationStage.revoked`,
/// which refuses advertising, authoring and acking alike. This object is the one
/// thing core cannot do from there: tell the screens.
///
/// It reads the stage rather than remembering an event, so a device that was
/// ejected in a previous process still knows on the next launch. `markRemoved()`
/// exists only so the mesh controller can flip the surface in the same breath as
/// applying a notice, instead of leaving the person looking at a chat list until
/// something re-reads the store.
///
/// # Which way it fails
///
/// The opposite way to `LinkVisibility`, deliberately. That one fails closed
/// toward silence, because a store it cannot read is not one to shout on the
/// strength of. This one keeps its last answer on a read it cannot make, because
/// the cost of guessing wrong here is telling a person their phone was removed
/// when it was not — and the radios are gated by core's own answer regardless, so
/// a wrong guess here never puts a removed device back on the air.
///
/// # Threading
///
/// The truth is behind an `NSLock` so `MeshController`'s serial queue can read it
/// without hopping anywhere; the `@Published` mirror is written on the main queue
/// because SwiftUI reads it. Same split `LinkVisibility` uses.
///
/// Mirrors Android's `DeviceRemovalStatus.kt`.
final class DeviceRemovalStatus: ObservableObject, @unchecked Sendable {
    static let shared = DeviceRemovalStatus()

    private let lock = NSLock()
    private var value = false

    /// Whether this device's person has removed it from their devices, for the
    /// screens. Written on the main queue only.
    @Published private(set) var removed = false

    /// The same answer, readable from any queue.
    var isRemoved: Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    /// Re-read the stage. Cheap: one select against a single-row table.
    func refresh(store: MessageStore) {
        do {
            set(try store.linkActivation().stage == .revoked)
        } catch {
            NSLog("Could not read this device's link stage; keeping the last answer: \(error)")
        }
    }

    /// A notice this device just applied said it was the one being buried.
    func markRemoved() { set(true) }

    private func set(_ next: Bool) {
        lock.lock()
        let changed = next != value
        value = next
        lock.unlock()
        guard changed else { return }
        DispatchQueue.main.async { self.removed = next }
    }
}
