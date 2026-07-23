import SwiftUI

/// Delivery/read ticks for an own message, mirroring Android's `SignalTick`:
/// states are told apart by *shape*, not by a color that might match the
/// bubble. SENT is one outlined check, DELIVERED two outlined checks, READ two
/// filled discs. `checkmark.circle.fill` renders as a solid `tint` disc with the
/// check punched out to whatever is behind it (the bubble), so READ stays
/// visible on any background — including the accent-colored own bubble, where a
/// hard-coded blue used to vanish. The caller passes a `tint` that contrasts
/// with the bubble (white on the accent bubble); we never pick the color here.
struct SignalTickView: View {
    let status: TickStatus
    var tint: Color = .secondary

    var body: some View {
        HStack(spacing: -3) {
            Image(systemName: symbolName)
                .font(.system(size: 11, weight: .semibold))
            if status != .sent {
                Image(systemName: symbolName)
                    .font(.system(size: 11, weight: .semibold))
            }
        }
        .foregroundStyle(tint.opacity(opacity))
        .accessibilityLabel(status == .sent ? "Sent" : status == .delivered ? "Delivered" : "Read")
    }

    private var symbolName: String {
        status == .read ? "checkmark.circle.fill" : "checkmark.circle"
    }

    private var opacity: Double {
        switch status {
        case .sent: return 0.88
        case .delivered: return 0.74
        case .read: return 1.0
        }
    }
}
