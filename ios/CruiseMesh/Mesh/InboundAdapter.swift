import Foundation

/// The bounded, plain-value work one inbound envelope left for the shell to
/// execute, translated out of core's `CoreInboundOutcome`.
///
/// Every field is a number, a byte string, a bool or an enum: there is no
/// object here with a socket, a store handle or a decision left in it. That is
/// the point of the type. The disposition, the carry, the re-flood and the
/// dedupe record were all decided inside core's one transaction before this
/// value existed; what remains is execution — send these bytes, apply this
/// payload, then commit.
///
/// At most one payload and at most one frame, because a §6.4 envelope is
/// addressed to at most one 1:1 recipient or one group, and core's contract
/// says so too. Nothing unbounded crosses the boundary.
struct InboundExecutionPlan: Equatable {
    /// What core decided this envelope was, and the input to the relay ack
    /// rule. Reported verbatim by the caller unless its own durable delivery
    /// fails, in which case the caller reports `.failed` instead.
    let disposition: CoreInboundDisposition
    /// The opened plaintext to apply, or `nil` when nothing was delivered.
    let deliveredPayload: Data?
    /// The verified sender of `deliveredPayload`, for the caller's kind
    /// dispatch and notifications.
    let deliveredSender: Data?
    /// The hop-decremented frame to flood onward, or `nil` when no hops remain
    /// or the frame is home / an own fan-out copy.
    let relayFrame: Data?
    /// Whether core newly enqueued a carried row for this envelope.
    let carried: Bool
    /// Whether an opened pairwise envelope was consumed but deliberately
    /// dropped because its sender is blocked.
    let droppedBlocked: Bool
    /// Present exactly when `deliveredPayload` is: the DTN D4 bookkeeping the
    /// caller hands back to `coreCommitInboundDelivery` *after* its durable
    /// delivery succeeded, and drops unused when it did not.
    let commit: CoreInboundCommit?
    /// Bounded work counts, for folding into an encounter-granularity event.
    let work: CoreInboundWork
}

/// Translation only — no policy, no I/O, no store.
///
/// This exists as a named seam rather than a few lines inlined in
/// `MeshController` so the shape of what crosses the FFI boundary is something
/// a test can assert (`InboundAdapterTests`), and so the one place that could
/// grow a shell-side decision is the one place that is checked for not having
/// one.
enum InboundAdapter {

    /// Where an envelope came from, in core's terms.
    ///
    /// `MeshController` has expressed this as "a `sourceAddress` or `nil`"
    /// since the relay proxy-fetch path was added: a live BLE/LAN frame
    /// arrives on a link address, a relay-fetched row has none. The mapping is
    /// load-bearing — core's no-reinjection rule and its relay-carry
    /// classification both turn on it — so it is written down once here rather
    /// than re-derived at each call site.
    static func source(forSourceAddress sourceAddress: String?) -> CoreInboundSource {
        sourceAddress == nil ? .relay : .mesh
    }

    /// Flattens core's outcome into the plan the shell executes.
    ///
    /// `deliveredPayloads` is a list in the FFI record only because a record
    /// field cannot be an optional-of-list cheaply; core's contract already
    /// bounds it at one entry, so the first entry is taken and anything beyond
    /// it would be a core contract break rather than a case to handle here.
    static func plan(from outcome: CoreInboundOutcome) -> InboundExecutionPlan {
        InboundExecutionPlan(
            disposition: outcome.disposition,
            deliveredPayload: outcome.deliveredPayloads.first,
            deliveredSender: outcome.deliveredSender,
            relayFrame: outcome.relayFrame,
            carried: outcome.carried,
            droppedBlocked: outcome.droppedBlocked,
            commit: outcome.commit,
            work: outcome.work
        )
    }
}

extension InboundExecutionPlan {
    /// Whether newly carrying this envelope is worth waking the relay pass for.
    ///
    /// Two things must both hold. The row has to be one a relay pass could
    /// usefully act on — a mesh-sourced carry for someone this device knows, so
    /// the pass can hand it to the relay for them. And the envelope must not
    /// have come *from* the relay in the first place: a relay-fetched row is
    /// already durable server-side, core files it through its relay-carry path
    /// rather than the family one, and waking a pass for it is the re-upload
    /// loop that once burned a family's whole quota — the pass finds nothing
    /// new to send, and every fetched envelope asks for another one.
    ///
    /// The source test is core's own rule, written here in the same terms core
    /// uses (`CoreInboundSource`) rather than re-derived from a link address at
    /// the call site.
    func wakesRelayPass(source: CoreInboundSource, hintIsKnownTarget: Bool) -> Bool {
        carried && source == .mesh && hintIsKnownTarget
    }

    /// The delivered payload and everything needed to commit it, or `nil` when
    /// this envelope had no native delivery to wait on (carried, deduped,
    /// expired, rejected, or consumed by deliberate drop).
    ///
    /// Reads the three fields together because core sets them together, so a
    /// call site cannot accidentally apply a payload without holding the
    /// commit token that must follow it.
    var delivery: (payload: Data, senderUserId: Data, commit: CoreInboundCommit)? {
        guard let deliveredPayload, let deliveredSender, let commit else { return nil }
        return (deliveredPayload, deliveredSender, commit)
    }
}
