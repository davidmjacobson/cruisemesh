import Combine
import SwiftUI

/**
 The pill's own clock, ticking every ten seconds.

 Deliberately not `ConnectivityClock` (thirty seconds), which ages the chat
 list's reachability badges. The pill and the Connection details health card
 now consume the same core verdict, and the spec's rule is that the two can
 never contradict each other -- but one classification only buys that if both
 shells ask it at comparable times. The bounded `Checking` window is ten
 seconds: on the slower tick the page open beside this pill would resolve to a
 fault while the pill still showed a neutral "still checking" dot for up to
 twenty seconds more. Ten matches `clockTickMs` in `ConnectionDetailsModel`,
 and `PILL_TICK_MS` on Android.
 */
@MainActor
final class MeshStatusPillClock: ObservableObject {
    static let shared = MeshStatusPillClock()
    @Published private(set) var nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
    private var timer: AnyCancellable?

    private init() {
        timer = Timer.publish(every: 10, on: .main, in: .common)
            .autoconnect()
            .sink { [weak self] date in
                self?.nowMs = Int64(date.timeIntervalSince1970 * 1_000)
            }
    }
}

/**
 The home screen's one-line connection summary.

 Its severity is the core's verdict, reached through `MeshStatusPillLogic
 .build` -- the same classification the Connection details health card renders,
 so the two can never disagree about the same phone. Everything it observes is
 already observable state: the runtime, the direct links, our pass health, the
 ten-second pill clock, and the local Wi-Fi *listening flag* (mapped and
 deduplicated by `LanListeningSignal`, never the whole LAN snapshot).
 */
struct MeshStatusPill: View {
    @ObservedObject var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @ObservedObject private var lan = LanListeningSignal.shared
    @ObservedObject private var bluetooth = BluetoothAccess.shared
    @ObservedObject private var clock = MeshStatusPillClock.shared
    @State private var pulse = false
    /// Held across renders so the core's bounded-Checking window is measured
    /// from when the wait actually began; a mark restamped on every render can
    /// never expire.
    @State private var checkingClock = CheckingClock()
    let onTap: () -> Void

    /// True only when internet delivery has stopped in a way that needs the
    /// person to act -- see `MeshStatusPillLogic`. False the rest of the time,
    /// which is nearly always.
    private func hasActionableFault(_ service: InternetDeliveryService?) -> Bool {
        MeshStatusPillLogic.faultSuffix(
            runtimeState: runtime.state,
            relayHealth: connectivity.relay,
            service: service
        ) != nil
    }

    private func status(service: InternetDeliveryService?) -> MeshStatusPillStatus {
        let availability = BluetoothAvailability.observed(
            authorizationBlocked: bluetooth.isAuthorizationBlocked,
            radioState: bluetooth.radioState
        )
        let relay = ConnectionInputs.relay(connectivity.relay, configured: service != nil)
        // The same instant the classification is given: a mark stamped from a
        // fresher clock than `nowMs` would look like it came from the future
        // and resolve the bound instantly, so Checking would never be shown.
        let checkingSinceMs = checkingClock.mark(
            pending: connectionCheckPending(
                runtime: ConnectionInputs.runtime(runtime.state, bluetooth: availability),
                bluetooth: ConnectionInputs.bluetooth(
                    runtime.state,
                    availability: availability
                ),
                localWifi: ConnectionInputs.localWifi(
                    runtime.state,
                    listening: lan.isListening
                ),
                relay: relay
            ),
            nowMs: clock.nowMs
        )
        return MeshStatusPillLogic.build(
            runtimeState: runtime.state,
            runtimeText: runtime.pillText,
            nearbyCount: connectivity.nearbyPeerIds.count,
            bluetooth: availability,
            lanListening: lan.isListening,
            relayHealth: connectivity.relay,
            service: service,
            checkingSinceMs: checkingSinceMs,
            nowMs: clock.nowMs
        )
    }

    var body: some View {
        // Read once per render, not once per property: a saved pass changes
        // only from the Shore Pass screen, and decoding it twice for one pill
        // is work nobody asked for.
        let service = InternetDeliveryService.of(RelayConfigStore.load())
        let status = self.status(service: service)
        let pulsing = shouldPulse(service: service)
        return Button(action: onTap) {
            HStack(spacing: 6) {
                Circle()
                    .fill(color(for: status.dot))
                    .frame(width: 8, height: 8)
                    .scaleEffect(pulsing ? (pulse ? 1.22 : 0.84) : 1)
                    .opacity(pulsing ? (pulse ? 1 : 0.62) : 1)
                Text(status.text)
                    .font(.caption.weight(.medium))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Capsule().fill(Color(.secondarySystemBackground)))
        }
        .buttonStyle(.plain)
        .onAppear {
            withAnimation(.easeInOut(duration: 1.4).repeatForever(autoreverses: true)) {
                pulse = true
            }
        }
    }

    private func shouldPulse(service: InternetDeliveryService?) -> Bool {
        // A steady dot reads as a state to deal with; a pulsing one reads as
        // work in progress, which a fault is not.
        if hasActionableFault(service) { return false }
        switch runtime.state {
        case .starting, .meshing: return true
        case .stopped, .syncingViaRelay: return false
        }
    }

    /// The four semantic colors, painted. Amber covers both degraded verdicts
    /// because that is the core's own severity split and what the same verdict
    /// draws on Android; the words beside the dot still name the fault, so
    /// nothing here is communicated by color alone.
    private func color(for dot: MeshStatusDotColor) -> Color {
        switch dot {
        case .green: return .green
        case .blue: return .blue
        case .amber: return .orange
        case .neutral: return .gray
        }
    }
}

/// How urgent a home-screen connectivity callout is (Android parity).
enum ConnectivityWarningSeverity {
    case blocking
    case caution
}

struct ConnectivityWarning: Equatable {
    let title: String
    let body: String
    let actionLabel: String
    var secondaryActionLabel: String? = nil
    var severity: ConnectivityWarningSeverity = .blocking
}

/// Hard-to-miss banner when Bluetooth permission is denied or the radio is off.
struct ConnectivityWarningBanner: View {
    let warning: ConnectivityWarning
    let onAction: () -> Void
    var onSecondaryAction: (() -> Void)? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(iconColor)
                    .padding(.top, 2)
                VStack(alignment: .leading, spacing: 4) {
                    Text(warning.title)
                        .font(.subheadline.weight(.semibold))
                    Text(warning.body)
                        .font(.caption)
                        .foregroundStyle(.primary.opacity(0.9))
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
            }
            Button(action: onAction) {
                Text(warning.actionLabel)
                    .font(.subheadline.weight(.semibold))
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(buttonTint)
            if let secondaryActionLabel = warning.secondaryActionLabel,
               let onSecondaryAction {
                Button(action: onSecondaryAction) {
                    Text(secondaryActionLabel)
                        .font(.subheadline.weight(.semibold))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(background)
        .foregroundStyle(foreground)
    }

    private var background: Color {
        switch warning.severity {
        case .blocking: return Color.red.opacity(0.16)
        case .caution: return Color.orange.opacity(0.16)
        }
    }

    private var foreground: Color {
        switch warning.severity {
        case .blocking: return Color.red.opacity(0.95)
        case .caution: return Color.orange.opacity(0.95)
        }
    }

    private var iconColor: Color {
        switch warning.severity {
        case .blocking: return .red
        case .caution: return .orange
        }
    }

    private var buttonTint: Color {
        switch warning.severity {
        case .blocking: return .red
        case .caution: return .orange
        }
    }
}
