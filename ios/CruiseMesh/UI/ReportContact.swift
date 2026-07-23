import UIKit

let abuseReportAddress = "abuse@cruisemesh.app"

/// Opens the user's email app with a pre-filled abuse report. E2E stays
/// intact: nothing sends automatically and no message content is attached —
/// the reporter writes what happened and owns their copy of anything they
/// choose to include.
func launchContactReport(contact: Contact, reporterUserId: Data) {
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
    guard let url = components.url else { return }
    UIApplication.shared.open(url)
}
