import Foundation

/// §9.4's radio silence as `MeshController` holds it: whether a request to
/// bring the mesh up may proceed, and whether the end of the pre-activation
/// window should start the mesh again.
///
/// Named rather than left as two booleans inside the controller so the answer
/// can be asserted directly. `MeshController` is a process-wide singleton with
/// a BLE stack, a Bonjour registration and a relay loop inside it, none of which
/// a unit test should be reaching into — so the one thing that must never
/// quietly change, *a start request during silence does not bring the radios
/// up*, is decided here where a test can read it. The Swift counterpart of
/// Android's `LinkRadioPlan`, though the two shells hold silence differently
/// (see `LinkVisibility`).
///
/// The bug this exists to make impossible: the silence used to be a flag that
/// only `applyLinkVisibility` read. Every other way into `startOnMeshQueue` —
/// the chat list appearing, an APNs relay wake, a background BLE relaunch, a
/// deep link — brought BLE advertising, Bonjour and the relay loop straight
/// back during the window §9.4 requires to be silent, and nothing put them down
/// again because the gate had not changed.
///
/// Confined to the controller's mesh queue; it holds no lock of its own.
struct LinkSilenceLatch {
    enum Action: Equatable {
        case nothing
        case startMesh
        case stopMesh
    }

    /// True while the mesh is held down for §9.4 and somebody wants it up:
    /// either it was running when the window opened, or a start was asked for
    /// during the window. The end of the window starts it only then, so a mesh
    /// the person had switched off stays off.
    private(set) var silenced = false

    /// A request to bring the mesh up. `gateAllowed` is the core gate's answer
    /// for advertising, read fail-closed. Returns whether the radios may come
    /// up; a refused start is remembered so the end of the window honours it.
    mutating func admitStart(gateAllowed: Bool) -> Bool {
        silenced = !gateAllowed
        return gateAllowed
    }

    /// The gate's answer, applied. `isRunning` is the controller's own state,
    /// read on its queue.
    mutating func apply(allowed: Bool, isRunning: Bool) -> Action {
        if allowed {
            guard silenced else { return .nothing }
            silenced = false
            return .startMesh
        }
        guard isRunning else { return .nothing }
        silenced = true
        return .stopMesh
    }

    /// The person switched the mesh off. Whatever the window does next, it does
    /// not switch it back on.
    mutating func personStopped() {
        silenced = false
    }
}
