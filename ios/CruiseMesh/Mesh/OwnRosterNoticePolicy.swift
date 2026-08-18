import Foundation

/// Who may be handed this person's own roster, and who may be believed when one
/// arrives (`specs/multi-device-v1.md` §10 step 5).
///
/// The frame carries the DL-3 roster document in the clear — keys, ids, counters
/// and one signature, with nowhere for an endpoint to live (DL-5) — so **which
/// links it may cross is the whole of its safety**, and core says so at
/// `encode_own_roster` rather than leaving it to a call site. Core cannot enforce
/// it: a Noise static key is a thing this shell holds and core has never seen.
///
/// The bar is the one the clone guard already uses: a LAN link whose Noise
/// session key is this identity's own agreement key. Only a device holding this
/// person's own secret can present that, which is exactly the set the notice is
/// for. A cleartext BLE HELLO cannot clear it and is not meant to — a removed
/// device that only ever meets its fleet over BLE does not converge, and the spec
/// records that rather than weakening the test.
///
/// Enforced on **both** directions. On send, or a stranger who claims our user id
/// in a HELLO could ask us how many devices we have and what their keys are. On
/// receive, or the same stranger could hand us a document and have us act on it
/// — core still refuses anything the person root did not sign, but a frame this
/// device should never have read is not one to hand to the store at all.
///
/// Mirrors Android's `OwnRosterNoticePolicy.kt`.
enum OwnRosterNoticePolicy {

    /// Mirrors `CAP_OWN_ROSTER_NOTICE` in `core/src/protocol.rs`.
    ///
    /// Duplicated as a number because a capability mask is not exported across
    /// the binding, and pinned against `coreOwnCapabilities()` by a unit test so
    /// a bit that moves in core cannot drift silently here.
    static let capabilityBit: UInt32 = 1 << 4

    /// Whether a notice may cross this link at all — the test that must pass
    /// before one is written to it, and again before one read from it is opened.
    static func mayCross(
        isLanLink: Bool,
        ownAgreePk: Data,
        sessionRemoteStaticKey: Data?
    ) -> Bool {
        ownIdentityHelloIsAuthenticated(
            isLanLink: isLanLink,
            ownAgreePk: ownAgreePk,
            sessionRemoteStaticKey: sessionRemoteStaticKey
        )
    }

    /// Whether this peer's HELLO2 said it understands the frame.
    ///
    /// Asked in addition to `mayCross`, never instead of it. A build that
    /// predates the frame drops the unknown type without touching the link, so
    /// this test is not what keeps the notice safe — it is what keeps a pointless
    /// roster off the wire toward a phone that cannot read one.
    static func peerReadsNotices(peerCapabilities: UInt32) -> Bool {
        (peerCapabilities & capabilityBit) != 0
    }
}
