import Foundation

/// Process-wide delegate to the core's shared foreign-carry allowance (Android
/// `CarriedOfferEpochGate.kt` twin).
///
/// There is deliberately no decision in this file: how many peers may offer
/// third-party traffic at once, how long the window lasts, and how one logical
/// peer reaching us at two addresses is collapsed to a single offer all live in
/// `core/src/transport_policy.rs`, so the two shells cannot drift.
///
/// Both lanes that offer foreign carry share this one allowance — the HELLO
/// drain and the digest spray — so a peer cannot be handed a full page by each
/// of them inside one connection burst.
///
/// Reservations are taken before a page is built. A page that comes out empty
/// is `release`d so it does not consume another peer's turn; one that actually
/// went out is `commit`ted and keeps counting until the window rolls.
///
/// This gates offering only. Nothing here removes a carried envelope or acks
/// anything: a deferred peer is offered on a later round, and a carried copy is
/// still retired only on digest-proof of receipt.
enum CarriedOfferEpochGate {
    private static let core = CoreCarriedOfferGate.withEpochMs(epochMs: coreCarriedOfferEpochMs())

    /// The shared Rust gate itself, for `MessageStore.corePlanMeshMeet`, which
    /// reserves and commits or releases the epoch slot inside one planning
    /// call. It has to be this object, or the planner's encounters would spend
    /// an allowance nobody else could see.
    static var coreState: CoreCarriedOfferGate { core }

    /// Claims one of the window's slots, or `nil` when the allowance is spent
    /// or this logical peer already had its offer. `logicalPeerId` is the
    /// peer's UserID hex, never a link address — keying it by address is what
    /// let one phone with two roles take every slot.
    static func tryReserve(
        nowMs: Int64,
        logicalPeerId: String? = nil
    ) -> CoreCarriedOfferReservation? {
        core.tryReserve(nowMs: nowMs, logicalPeerId: logicalPeerId)
    }

    static func commit(_ reservation: CoreCarriedOfferReservation) {
        core.commit(reservation: reservation)
    }

    static func release(_ reservation: CoreCarriedOfferReservation) {
        core.release(reservation: reservation)
    }
}
