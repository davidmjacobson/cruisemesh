import Foundation

func extractFriendToken(_ text: String) -> String {
    let pattern = #"CMFRIEND1:[A-Za-z0-9_-]+"#
    if let range = text.range(of: pattern, options: .regularExpression) {
        return String(text[range])
    }
    return text.trimmingCharacters(in: .whitespacesAndNewlines)
}
