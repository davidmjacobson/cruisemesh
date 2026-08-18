import Foundation

/// The §13 gate's relay-only leg: the ceremony over a pair of ephemeral relay
/// mailboxes, with no LAN and no BLE anywhere near it.
///
/// The mailbox pair is derived from the scanned offer's key
/// (`coreLinkRendezvousLane`), so both devices find it without either one
/// publishing an address, and the relay is told nothing except that two opaque
/// blobs want storing. Nothing about this needs a relayd change — these are
/// ordinary envelope rows under an ordinary recipient hint, which is exactly what
/// §2 promises when it says relayd stays content-agnostic.
///
/// # Why nothing here acks
///
/// A row this wire has read is left in the mailbox to expire, and the cursor is
/// what stops it being read twice. That is not laziness: a device running this
/// ceremony is, by §9.4, pre-activation and forbidden from acking anything, and
/// the safest way to keep a forbidden call from happening is not to have written
/// it. The rows are short-lived by construction (see `rendezvousExpiryMs`).
///
/// # What the two devices must already have
///
/// Both of them, a relay pass for the same family. The QR carries a relay base
/// URL and never a token — a photograph of a screen must not be worth a family's
/// mailbox — so a new phone reaches the relay leg only if someone has already
/// given it the family's Shore Pass. On Wi-Fi neither device needs anything.
///
/// Mirrors Android's `LinkRelayWire.kt`.
final class LinkRelayWire: LinkWire {
    private let config: RelayConfig
    private let sendNamespace: Data
    private let receiveNamespace: Data
    private let clock: () -> Int64
    private let sleep: (Int64) -> Void

    /// Rows already taken. The relay's own row id, monotone per mailbox.
    private var cursor: Int64 = 0

    private static let msgIdBytes = 16
    private static let fetchLimit = 8
    /// Floor on how often one mailbox is asked. A ceremony is minutes long and
    /// every look is a request against the family's own relay budget — the same
    /// budget ordinary delivery is spending at the same time.
    private static let pollIntervalMs: Int64 = 2_000
    private static let dayMs: Int64 = 24 * 60 * 60 * 1_000
    /// How long a rendezvous row stands. Longer than one ceremony's deadline so a
    /// slow confirm is not cut off mid-handshake, and far shorter than ordinary
    /// mail so an abandoned ceremony leaves nothing behind.
    private static let rendezvousExpiryMs: Int64 = 10 * 60 * 1_000

    init(
        config: RelayConfig,
        rendezvousId: Data,
        sendLane: CoreLinkLane,
        receiveLane: CoreLinkLane,
        clock: @escaping () -> Int64,
        sleep: @escaping (Int64) -> Void
    ) throws {
        self.config = config
        self.sendNamespace = try coreLinkRendezvousLane(rendezvousId: rendezvousId, lane: sendLane)
        self.receiveNamespace = try coreLinkRendezvousLane(
            rendezvousId: rendezvousId,
            lane: receiveLane
        )
        self.clock = clock
        self.sleep = sleep
    }

    func send(_ bytes: Data) throws {
        guard bytes.count <= LinkWireLimits.maxMessageBytes else {
            throw LinkWireError.tooLarge(bytes.count)
        }
        let now = clock()
        // `SystemRandomNumberGenerator` is the platform CSPRNG, which is all a
        // mailbox row id needs: it is a de-duplication key relayd stores, never a
        // secret and never authenticated by anybody.
        var generator = SystemRandomNumberGenerator()
        let msgId = Data((0..<Self.msgIdBytes).map { _ in
            UInt8.random(in: UInt8.min...UInt8.max, using: &generator)
        })
        _ = try RelayClient.postRendezvousEnvelope(
            config: config,
            msgId: msgId,
            recipientHint: computeRecipientHint(recipientUserId: sendNamespace, timestampMs: now),
            sealed: bytes,
            expiryMs: now + Self.rendezvousExpiryMs
        )
    }

    func receive(waitMs: Int64) throws -> Data? {
        let deadline = clock() + min(max(waitMs, 0), LinkWireLimits.maxReceiveWaitMs)
        while true {
            let page: RelayFetchPage
            do {
                page = try RelayClient.fetchEnvelopes(
                    config: config,
                    hints: receiveHints(now: clock()),
                    afterId: cursor,
                    limit: Self.fetchLimit
                )
            } catch {
                throw LinkWireError.transport("the relay rendezvous could not be read: \(error)")
            }
            if let row = page.envelopes.first {
                cursor = row.id
                return row.sealed
            }
            let remaining = deadline - clock()
            if remaining <= 0 { return nil }
            sleep(min(remaining, Self.pollIntervalMs))
        }
    }

    /// The mailbox to read, under each day key it could plausibly be filed under.
    ///
    /// `computeRecipientHint` rotates on the UTC day boundary, and the two phones
    /// do not share a clock. Three hints — yesterday, today, tomorrow — cost
    /// nothing against relayd's 256-hint fetch budget and mean a ceremony started
    /// at 23:59 does not silently stop being heard a minute later.
    private func receiveHints(now: Int64) -> [Data] {
        [now - Self.dayMs, now, now + Self.dayMs].map {
            computeRecipientHint(recipientUserId: receiveNamespace, timestampMs: $0)
        }
    }

    func close() {
        // Nothing to release: an HTTP rendezvous holds no socket between calls,
        // and the rows age out on their own.
    }
}
