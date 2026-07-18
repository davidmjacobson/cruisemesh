import SwiftUI

struct AvatarView: View {
    let userId: Data
    let name: String
    var size: CGFloat = 40
    var photo: UIImage? = nil
    var isGroup: Bool = false
    var reachability: ReachabilityLevel? = nil

    var body: some View {
        let displayId = formatUserId(userId: userId)
        let (color, initials) = ChatListLogic.avatarHueAndInitials(
            userId: userId,
            name: name,
            displayId: displayId
        )
        ZStack(alignment: .bottomTrailing) {
            SwiftUI.Group {
                if let photo, !isGroup {
                    Image(uiImage: photo)
                        .resizable()
                        .scaledToFill()
                } else if isGroup {
                    Image(systemName: "person.2.fill")
                        .font(.system(size: size * 0.42, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Circle().fill(color))
                } else {
                    Text(initials)
                        .font(.system(size: size * 0.35, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Circle().fill(color))
                }
            }
            .frame(width: size, height: size)
            .clipShape(Circle())

            if let reachability, reachability != .offline {
                let badgeSize = size * 0.28
                let badgeColor: Color = switch reachability {
                case .nearby: .green
                case .onlineRelay: .blue
                case .recent, .meshCarry: .orange
                case .offline: .gray
                }
                ZStack {
                    Circle()
                        .fill(reachability == .meshCarry ? Color(.systemBackground) : badgeColor)
                    Circle()
                        .stroke(
                            reachability == .meshCarry ? badgeColor : Color(.systemBackground),
                            lineWidth: 2
                        )
                }
                    .frame(width: badgeSize, height: badgeSize)
            }
        }
        .frame(width: size, height: size)
        .accessibilityLabel(accessibilityLabel(displayId: displayId))
    }

    private func accessibilityLabel(displayId: String) -> String {
        let base = "\(isGroup ? "Group avatar" : "Avatar") for \(ChatListLogic.displayNameOrId(name: name, displayId: displayId))"
        guard let reachability,
              let suffix = ContactReachability.contentDescriptionSuffix(reachability) else { return base }
        return "\(base). \(suffix)"
    }
}
