import SwiftUI

struct AvatarView: View {
    let userId: Data
    let name: String
    var size: CGFloat = 40

    var body: some View {
        let displayId = formatUserId(userId: userId)
        let (color, initials) = ChatListLogic.avatarHueAndInitials(
            userId: userId,
            name: name,
            displayId: displayId
        )
        Text(initials)
            .font(.system(size: size * 0.35, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(Circle().fill(color))
            .accessibilityLabel("Avatar for \(ChatListLogic.displayNameOrId(name: name, displayId: displayId))")
    }
}
