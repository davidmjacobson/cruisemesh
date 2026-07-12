import SwiftUI

struct AvatarView: View {
    let userId: Data
    let name: String
    var size: CGFloat = 40
    var photo: UIImage? = nil

    var body: some View {
        let displayId = formatUserId(userId: userId)
        let (color, initials) = ChatListLogic.avatarHueAndInitials(
            userId: userId,
            name: name,
            displayId: displayId
        )
        Group {
            if let photo {
                Image(uiImage: photo)
                    .resizable()
                    .scaledToFill()
            } else {
                Text(initials)
                    .font(.system(size: size * 0.35, weight: .semibold))
                    .foregroundStyle(.white)
                    .background(Circle().fill(color))
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityLabel("Avatar for \(ChatListLogic.displayNameOrId(name: name, displayId: displayId))")
    }
}
