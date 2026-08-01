import UIKit

let abuseReportAddress = "abuse@cruisemesh.app"

/// What one tap on Report should do. Split out from the side-effecting
/// launcher so both branches are unit-testable without a device: the
/// no-mail-app path is exactly the one a reviewer's test device hits, and it
/// is the one that used to fail silently.
enum ContactReportAction: Equatable {
    /// A mail app exists; hand it the pre-filled draft.
    case openMail(URL)
    /// No mail app, or the URL could not be built. Show the address so the
    /// report is still reachable. Mirrors Android's `ui_no_email_app` toast.
    case showAddress(String)
}

/// Decide what Report does, given a way to ask whether a URL can be opened.
///
/// Pure: no `UIApplication`, no pasteboard, no view. `canOpen` is injected so
/// tests can drive both branches — see `ReportContactTests`.
func contactReportAction(
    contact: Contact,
    reporterUserId: Data,
    canOpen: (URL) -> Bool
) -> ContactReportAction {
    let body = """
    Reporting: \(coreContactDisplayName(contact: contact))
    Their ID: \(formatUserId(userId: contact.userId))
    Their safety words: \(fingerprintWords(userId: contact.userId).joined(separator: " "))
    My ID: \(formatUserId(userId: reporterUserId))

    What happened:

    """
    var components = URLComponents()
    components.scheme = "mailto"
    components.path = abuseReportAddress
    components.queryItems = [
        URLQueryItem(name: "subject", value: "CruiseMesh abuse report"),
        URLQueryItem(name: "body", value: body),
    ]
    guard let url = components.url, canOpen(url) else {
        return .showAddress(abuseReportAddress)
    }
    return .openMail(url)
}

/// What to tell someone whose phone has no mail app. Android says the same
/// thing in `ui_no_email_app`; kept here as a function so the wording is
/// pinned by a test rather than typed twice.
func noMailAppMessage(address: String) -> String {
    "No email app found. You can email \(address) directly — the address has been copied."
}

/// Opens the user's email app with a pre-filled abuse report. E2E stays
/// intact: nothing sends automatically and no message content is attached —
/// the reporter writes what happened and owns their copy of anything they
/// choose to include.
///
/// When there is no mail app the report must not dead-end: App Store
/// Guideline 1.2 requires a working way to report, and a phone with no
/// configured mail account is precisely a reviewer's device. In that case the
/// address is copied to the pasteboard and handed back so the caller can show
/// it — Android has done the equivalent since it shipped
/// (`ui/ReportContact.kt`, `ui_no_email_app`).
///
/// Returns the address to display when no mail app handled it, `nil` when mail
/// opened normally.
@discardableResult
func launchContactReport(contact: Contact, reporterUserId: Data) -> String? {
    let action = contactReportAction(
        contact: contact,
        reporterUserId: reporterUserId,
        canOpen: { UIApplication.shared.canOpenURL($0) }
    )
    switch action {
    case .openMail(let url):
        UIApplication.shared.open(url)
        return nil
    case .showAddress(let address):
        UIPasteboard.general.string = address
        return address
    }
}
