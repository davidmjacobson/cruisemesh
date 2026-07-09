import SwiftUI

struct SignalTickView: View {
    let status: TickStatus
    var tint: Color = .secondary

    var body: some View {
        HStack(spacing: -4) {
            Image(systemName: "checkmark")
                .font(.system(size: 10, weight: .semibold))
            if status != .sent {
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .semibold))
                    .offset(x: -2)
            }
        }
        .foregroundStyle(status == .read ? Color.blue : tint.opacity(status == .delivered ? 0.74 : 0.88))
        .accessibilityLabel(status == .sent ? "Sent" : status == .delivered ? "Delivered" : "Read")
    }
}
