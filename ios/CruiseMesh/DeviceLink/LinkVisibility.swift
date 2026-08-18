import Foundation

/// §9.4's gate, for the one path core cannot see: this shell's own radios.
///
/// A device between "the channel is confirmed" and "the roster head is
/// acknowledged" may not advertise, author, or ack ANYTHING. Core enforces that
/// for everything it holds — authoring refuses, the ack planner names nothing,
/// the hint sets come back empty, carry offers nothing — but core has never heard
/// of a BLE advertiser or a Bonjour registration, and a phone still shouting its
/// presence is not invisible whatever its store refuses to do.
///
/// It fails closed. A store that cannot say whether this device is allowed to
/// speak is not a store this device should speak on the strength of — and a store
/// that broken has nothing to send anyway.
///
/// # How this differs from Android, and why
///
/// Android's `LinkVisibility` publishes a flag its foreground `MeshService`
/// consults, and the disallow branch takes down BLE and then the LAN transport
/// while leaving the service alive. On this shell the two radios are created
/// inside `MeshController.startOnMeshQueue` and torn down together in
/// `stopOnMeshQueue`, with no seam that stops the LAN transport and keeps
/// everything else — so the disallow branch stops the whole controller and the
/// allow branch starts it again. That is strictly *more* silence than §9.4 asks
/// for (the relay loop stops too), which is the safe direction: §9.4 forbids
/// acking and authoring over the relay in the same breath. It also means a
/// device that was not meshing at all is left alone rather than started by a
/// ceremony ending.
enum LinkVisibility {
    private static let lock = NSLock()
    private static var advertisingAllowed = true
    private static var applied = true
    private static let appliedSignal = NSCondition()

    /// Whether this device may make itself visible on the mesh at all.
    static func mayAdvertise() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return advertisingAllowed
    }

    /// Re-read the core gate and ask the mesh controller to match it.
    ///
    /// This only *asks* for the change: the controller's work happens on its own
    /// serial queue, so the radios are still up when this returns. See
    /// `awaitApplied` for the caller that must not proceed until they are not.
    static func refresh(store: MessageStore) {
        let next: Bool
        do {
            next = try store.linkGate(action: .advertise).allowed
        } catch {
            NSLog("Could not read the device-link gate; staying quiet: \(error)")
            next = false
        }
        lock.lock()
        let changed = next != advertisingAllowed
        advertisingAllowed = next
        lock.unlock()
        guard changed else { return }
        // Deliberately no optimistic write to `applied` here: it stays at what
        // the radios are actually doing until the controller says otherwise, so
        // `awaitApplied(false)` blocks rather than returning on a flag this
        // function set for itself.
        MeshController.shared.applyLinkVisibility(next) { markApplied(next) }
    }

    /// Block until the mesh controller has actually applied `target`.
    ///
    /// The one caller that needs this is the new device at the top of §9.4: the
    /// silence has to be real before its first frame goes out. A device that sent
    /// its offer in that gap advertised during the very window §9.4 exists to
    /// make silent.
    ///
    /// Returns false on timeout, which the caller must treat as a failure to go
    /// quiet rather than as permission to continue.
    static func awaitApplied(_ target: Bool, timeoutMs: Int64) -> Bool {
        let deadline = Date().addingTimeInterval(Double(timeoutMs) / 1_000)
        appliedSignal.lock()
        defer { appliedSignal.unlock() }
        while applied != target {
            if !appliedSignal.wait(until: deadline) { return false }
        }
        return true
    }

    /// The mesh controller reporting that the radios now match `allowed`.
    static func markApplied(_ allowed: Bool) {
        appliedSignal.lock()
        applied = allowed
        appliedSignal.broadcast()
        appliedSignal.unlock()
    }
}
