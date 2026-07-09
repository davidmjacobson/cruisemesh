import SwiftUI

struct MeshStatusPill: View {
    @ObservedObject var runtime = MeshRuntimeStatus.shared
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 6) {
                Circle()
                    .fill(dotColor)
                    .frame(width: 8, height: 8)
                Text(runtime.pillText)
                    .font(.caption.weight(.medium))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Capsule().fill(Color(.secondarySystemBackground)))
        }
        .buttonStyle(.plain)
    }

    private var dotColor: Color {
        switch runtime.state {
        case .stopped: return .gray
        case .starting: return .orange
        case .meshing: return .green
        case .pausedForBluetoothAudio: return .yellow
        case .syncingViaRelay: return .blue
        }
    }
}
