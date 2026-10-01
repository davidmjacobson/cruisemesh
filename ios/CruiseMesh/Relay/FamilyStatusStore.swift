import Foundation
import os.log

/// The family's pass status, read once per app session.
///
/// Once per session rather than per screen open, because what this carries is
/// a renewal date: it moves when someone renews, which is minutes at the very
/// fastest and months in the ordinary case. Polling it on every visit to the
/// Shore Pass screen would spend a round trip on a ship's Wi-Fi to re-learn a
/// date that has not moved, and the sync pass -- the thing that actually has
/// to be quick -- shares that link.
///
/// A failed read leaves `status` as it was, which for a cold session is nil:
/// the end-date line is something extra a screen shows when it knows, never a
/// surface that reports its own absence. The pass's *health* has its own live
/// signal (`MeshConnectivityStatus.relay`) and is not this object's business.
///
/// Shared `ObservableObject`, the same pattern as `MeshConnectivityStatus`.
/// Mirrors Android `FamilyStatusStore`.
@MainActor
final class FamilyStatusStore: ObservableObject {
    static let shared = FamilyStatusStore()

    private static let log = Logger(subsystem: "com.cruisemesh", category: "FamilyStatus")

    /// The last status read this session, or nil if none has landed.
    @Published private(set) var status: CoreFamilyStatus?

    /// The configuration the value in `status` describes, so swapping in a
    /// different pass re-reads rather than showing the previous family's end
    /// date.
    private var readFor: RelayConfig?
    private var isReading = false

    private init() {}

    /// Reads the status for `config` unless this session already has it.
    ///
    /// Safe to call from any screen that shows a pass, as often as it likes --
    /// the first call does the work and the rest return immediately.
    func refresh(config: RelayConfig) {
        // A deposit credential is post-only; asking would earn the same
        // structured 403 the other read routes give it, so the round trip is
        // skipped rather than spent to be refused.
        if relayTokenIsDeposit(token: config.relayToken) { return }
        if readFor == config { return }
        // A different pass than the one `status` describes. Drop the old
        // family's answer before asking rather than after: whatever is shown
        // while the new read is in flight must not be the previous family's
        // end date. Mirrors Android `FamilyStatusStore.refresh`.
        if readFor != nil {
            readFor = nil
            status = nil
        }
        if isReading { return }
        isReading = true
        Task {
            let fetched = await Task.detached(priority: .utility) {
                try? RelayClient.fetchFamilyStatus(config: config)
            }.value
            isReading = false
            guard let fetched else {
                // Already logged by the client with its status line. Nothing
                // here is worth telling anyone: the screen simply shows no
                // end-date line, exactly as it does before the first read.
                Self.log.info("Family status unavailable")
                return
            }
            readFor = config
            status = fetched
        }
    }

    /// Reads the status for the pass saved on this phone, if there is one.
    func refresh() {
        guard let config = RelayConfigStore.load() else {
            clear()
            return
        }
        refresh(config: config)
    }

    /// Drops the cached status, for a test or a pass that was just replaced.
    func clear() {
        readFor = nil
        status = nil
    }
}
