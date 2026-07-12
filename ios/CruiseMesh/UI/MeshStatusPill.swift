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
