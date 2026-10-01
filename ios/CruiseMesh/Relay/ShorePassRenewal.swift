import Foundation

/// Where a Shore Pass is renewed, and when the app should say so.
///
/// Plain functions over plain values, so every rule here is unit-testable
/// directly; the copy lives in the string catalog, the date formatting in the
/// screen that shows it, and the decision about *which* date is worth showing
/// in the core (`relayPassDeliveryThroughMs`) so both shells say the same thing
/// on the same day. Mirrors Android `ShorePassRenewal.kt`.
enum ShorePassRenewal {
    /// The page that turns a family's pass into a renewal checkout.
    ///
    /// `/renew/app` rather than the ordinary renewal link because this one is
    /// reached from inside the app, with no signed email link to carry: the
    /// page identifies the family from the token the app puts on it and starts
    /// the same checkout the email link starts.
    private static let renewPage = "https://cruisemesh.app/renew/app"

    /// Characters a family token may contain for it to ride a URL fragment
    /// unescaped -- RFC 3986's unreserved set.
    ///
    /// The tokens this app actually holds are hex (`DEPLOY.md` §1) or
    /// base64url, both wholly inside it. Anything else is refused rather than
    /// escaped: a percent-encoder is a second place for the app and the site
    /// to disagree about what the token was, and the failure mode of
    /// disagreeing is a renewal page that reports no such pass.
    private static let tokenUrlSafe = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"
    )

    /// The renewal page for this family's pass, or nil when there is nothing
    /// to link to.
    ///
    /// The token rides the **fragment**. A fragment is never sent to the
    /// server, so it stays out of access logs, proxies and `Referer` headers on
    /// the way -- the same reason friend links carry their payload there. A
    /// query parameter would put a live bearer credential in every log between
    /// here and the site.
    ///
    /// Nil for a deposit-class credential: that is the post-only attenuation a
    /// friend card carries, not the family token a purchase row is keyed by,
    /// so a link built from it could only ever reach the failure page.
    static func renewURL(familyToken: String) -> URL? {
        let token = familyToken.trimmingCharacters(in: .whitespacesAndNewlines)
        if token.isEmpty || relayTokenIsDeposit(token: token) { return nil }
        guard token.unicodeScalars.allSatisfy({ tokenUrlSafe.contains($0) }) else { return nil }
        return URL(string: "\(renewPage)#f=\(token)")
    }

    /// The renewal page for the pass saved on this phone, or nil when there is
    /// none to build one from.
    static func currentRenewURL() -> URL? {
        guard let config = RelayConfigStore.load() else { return nil }
        return renewURL(familyToken: config.relayToken)
    }

    /// When internet delivery runs through, or nil for "say nothing" -- no
    /// status read yet, no end date, a date already past, or a suspended pass.
    /// The rule itself lives in the core; this only tolerates the
    /// not-read-yet case the shells have and the core does not.
    static func deliveryThroughMs(status: CoreFamilyStatus?, nowMs: Int64) -> Int64? {
        guard let status else { return nil }
        return relayPassDeliveryThroughMs(status: status, nowMs: nowMs)
    }

    /// Does this pass surface offer to renew?
    ///
    /// Two occasions, and only two. An expired pass, where renewing is the
    /// whole remedy; and a pass with a known end date still ahead of it, where
    /// someone looking at that date is exactly the person who might want to act
    /// on it.
    ///
    /// Everything else says nothing. A pass with no end date has nothing to
    /// renew, a suspended one is not fixed by paying again, and a rejected
    /// setup card is a different problem with its own instructions -- offering
    /// renewal for any of those sells someone a thing that will not help them.
    static func offersRenewal(health: RelayHealth, deliveryThroughMs: Int64?) -> Bool {
        if case .expired = health { return true }
        return deliveryThroughMs != nil
    }

    /// A date to read, not a timestamp. Same format as the device list's.
    ///
    /// Shared by the Shore Pass screen and Settings, which show the same
    /// delivery-through line about the same pass: one date formatted two ways
    /// on two screens is a support question waiting to happen. Mirrors
    /// Android `passDate`.
    static func passDate(_ timestampMs: Int64) -> String {
        passDateFormatter.string(from: Date(timeIntervalSince1970: Double(timestampMs) / 1000))
    }

    private static let passDateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "MMMM d, yyyy"
        formatter.locale = .current
        return formatter
    }()
}
