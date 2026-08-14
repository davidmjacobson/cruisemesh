import Foundation

/// Map a failed friend-card / deep-link parse to the sentence the user should
/// read. A newer scheme than this build implements (`CoreError.unsupportedLink`)
/// is "update the app", never a crash and never a half-parsed contact
/// (`specs/multi-device-v1.md` WPT).
func friendImportFailureText(_ error: Error, text: String) -> String {
    if let core = error as? CoreError, case .UnsupportedLink = core {
        return String(localized: "This link needs a newer version of CruiseMesh. Update the app, then try again.")
    }
    if text.contains("CMFRIEND") || text.contains("CMLINK") {
        return String(localized: "That looks like a friend card but part of it is missing. Copy the whole message and try again.")
    }
    return String(localized: "Not a CruiseMesh friend card")
}
