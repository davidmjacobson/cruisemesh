import SwiftUI
import UIKit

/// A message body with its links rendered as links (6.6).
///
/// Detection belongs to the Rust core (`coreDetectLinks`). A second parser
/// written in Swift would be exactly the platform disagreement the core
/// exists to prevent, and `NSDataDetector` would happily linkify schemes the
/// product refuses. So this view never decides what a link is; it only draws
/// what the core found.
///
/// The core returns UTF-16 ranges over *this exact body* plus the
/// destination, and by contract the destination is byte-for-byte the
/// substring the range covers. This view therefore styles the range in
/// place and never substitutes display text: a link whose visible label
/// reads as one address while resolving to another is not something we have
/// to police here, it is something this rendering cannot express.
struct MessageBodyText: View {
    let text: String
    /// Sent bubbles are white on the accent fill; received bubbles are
    /// primary text on a 24% contact tint. The link styling has to read on
    /// both, so colour is never the only difference -- see `linkColor`.
    let isOwn: Bool
    var onStatus: (String) -> Void = { _ in }

    /// The web address a tap asked for, held until the person confirms
    /// leaving the app. 6.6 asks for the confirmation explicitly.
    @State private var pendingWebLink: URL?

    var body: some View {
        Text(MessageBodyText.attributedBody(text: text, linkColor: linkColor))
            .environment(\.openURL, OpenURLAction { url in
                // The core owns the allow-list. Do not add a second scheme
                // check here: if something should open or be refused, change
                // `link_detect.rs` so both platforms move together.
                switch coreLinkOpenableScheme(url: url.absoluteString) {
                case .some(.https):
                    pendingWebLink = url
                    return .handled
                case .some(.cruiseMesh):
                    // Straight back out to the system, which hands it to the
                    // app's own `onOpenURL` routing. No second routing table
                    // lives here. Until the scheme is registered (6.1) the
                    // open fails, and saying so is better than a dead tap.
                    UIApplication.shared.open(url) { opened in
                        if !opened {
                            onStatus(String(localized: "Couldn't open that link"))
                        }
                    }
                    return .handled
                case .none:
                    return .discarded
                }
            })
            .confirmationDialog(
                "Open this link?",
                isPresented: Binding(
                    get: { pendingWebLink != nil },
                    set: { if !$0 { pendingWebLink = nil } }
                ),
                titleVisibility: .visible,
                presenting: pendingWebLink
            ) { url in
                // `UIApplication.shared.open`, not the `openURL` action:
                // this view overrode that action above, and routing the
                // confirmed open back through it would just re-ask.
                Button("Open in browser") {
                    UIApplication.shared.open(url)
                }
                Button("Copy link") {
                    UIPasteboard.general.string = url.absoluteString
                    onStatus(String(localized: "Copied"))
                }
                Button("Cancel", role: .cancel) {}
            } message: { url in
                // The destination in full, unshortened: the same characters
                // the bubble shows, so the dialog cannot disagree with it.
                Text(url.absoluteString)
            }
    }

    /// Colour alone never carries the link, because a received bubble's tint
    /// is whatever colour that contact happens to have. Every link is also
    /// underlined and bold (see `attributedBody`), so on the accent-filled
    /// sent bubble -- where white is the only text colour with real contrast
    /// -- the link is still obviously a link.
    private var linkColor: Color {
        isOwn ? .white : .accentColor
    }

    /// `text` with every link the core found styled in place.
    ///
    /// Static, and deliberately separable from the view: the UTF-16 ->
    /// `String.Index` mapping is the one part of this file that can be
    /// *wrong* rather than merely ugly. Rust byte offsets are neither
    /// Swift's nor Kotlin's unit, which is why the core emits UTF-16 code
    /// units -- one emoji ahead of a link shifts every offset, and a bad
    /// index lands mid-character.
    static func attributedBody(text: String, linkColor: Color) -> AttributedString {
        var out = AttributedString(text)
        if text.isEmpty { return out }
        let units = text.utf16
        for link in coreDetectLinks(body: text) {
            guard
                let lowUnit = units.index(
                    units.startIndex,
                    offsetBy: Int(link.startUtf16),
                    limitedBy: units.endIndex
                ),
                let highUnit = units.index(
                    units.startIndex,
                    offsetBy: Int(link.endUtf16),
                    limitedBy: units.endIndex
                ),
                let low = String.Index(lowUnit, within: text),
                let high = String.Index(highUnit, within: text),
                low < high,
                let url = URL(string: link.url),
                let start = AttributedString.Index(low, within: out),
                let end = AttributedString.Index(high, within: out)
            else { continue }
            // Scoped accessors, not bare `.foregroundColor` /
            // `.underlineStyle`: UIKit and SwiftUI both define those names,
            // and this file imports both.
            var style = AttributeContainer()
            style.foundation.link = url
            style.foundation.inlinePresentationIntent = .stronglyEmphasized
            style.swiftUI.foregroundColor = linkColor
            style.swiftUI.underlineStyle = .single
            out[start..<end].mergeAttributes(style)
        }
        return out
    }
}
