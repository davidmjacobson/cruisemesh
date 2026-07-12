import Foundation

func extractFriendToken(_ text: String) -> String {
    let pattern = #"CMFRIEND1:\S+"#
    if let range = text.range(of: pattern, options: .regularExpression) {
        return String(text[range])
    }
    return text.trimmingCharacters(in: .whitespacesAndNewlines)
}
