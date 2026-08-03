import Foundation

/// Which piece of evidence a connection-history line is reporting, and -- for
/// the two message directions -- WHICH WAY the message went.
///
/// Declaration order is deliberate: it is the tie-break when two pieces of
/// evidence share a timestamp, most informative first. A message crossing in
/// either direction says more than a bare presence ping, which says more than
/// a link coming up or going down.
///
/// Mirrors `PeerEvidence` in the Android shell.
enum PeerEvidence: Int, CaseIterable {
    /// A message THEY sent landed on this phone.
    case messageReceived
    /// A message WE sent reached them -- their delivery receipt came back.
    case messageDelivered
    case presenceSeen
    case connected
    case disconnected
}

/// One rendered connection-history line, before it is turned into copy.
struct PeerStatusLine: Equatable {
    let evidence: PeerEvidence
    let transport: PeerConnectionTransport
    let atMs: Int64
}

enum ConnectionActivityLogic {
    /// Picks the newest piece of evidence across every path recorded for one
    /// friend.
    ///
    /// Every timestamp on every row competes on its own: the winner is the
    /// single most recent moment, and the transport reported is the one that
    /// moment happened on. An earlier version chose a row first and only then
    /// looked for a timestamp on it, so a row whose newest field was, say, a
    /// stale delivery confirmation could outrank a path that had seen the
    /// friend seconds ago.
    ///
    /// Returns nil when there is no evidence at all, so the caller can say so
    /// plainly rather than inventing a time.
    static func latestPeerStatus(_ rows: [PeerConnectionSummary]) -> PeerStatusLine? {
        let candidates: [PeerStatusLine] = rows.flatMap { row -> [PeerStatusLine] in
            let fields: [(PeerEvidence, Int64?)] = [
                (.messageReceived, row.lastReceivedAtMs),
                (.messageDelivered, row.lastDeliveredAtMs),
                (.presenceSeen, row.lastSeenAtMs),
                (.connected, row.lastConnectedAtMs),
                (.disconnected, row.lastDisconnectedAtMs),
            ]
            return fields.compactMap { evidence, atMs in
                atMs.map { PeerStatusLine(evidence: evidence, transport: row.transport, atMs: $0) }
            }
        }
        return candidates.max { left, right in
            if left.atMs != right.atMs { return left.atMs < right.atMs }
            return left.evidence.rawValue > right.evidence.rawValue
        }
    }

    /// The evidence a single recorded event represents.
    static func evidence(of kind: PeerConnectionEventKind) -> PeerEvidence {
        switch kind {
        case .connected: return .connected
        case .disconnected: return .disconnected
        case .presenceSeen: return .presenceSeen
        case .messageDelivered: return .messageDelivered
        case .messageReceived: return .messageReceived
        }
    }
}
