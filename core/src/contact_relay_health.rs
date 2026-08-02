//! When a *contact's* relay endpoint is authoritatively rejecting us.
//!
//! [`crate::relay_status`] classifies rejections of our OWN saved Cruise
//! Pass. This module covers the other half, which had no owner at all: the
//! endpoint we post to on a contact's behalf comes from *their* friend card
//! (`resolved_contact_relay`), and a card is only a snapshot of the sharer's
//! config at the moment they shared it. Rotate a token, migrate hosts, or —
//! as happened in the field on 2026-07-30 — rebuild the box and lose the
//! `families` table, and every contact keeps posting to an endpoint that
//! answers `401 unknown family token` forever.
//!
//! Before this module the shells dropped those faults on the floor
//! (`noteOwnRelayFault` returned early for any endpoint that wasn't our
//! own), so the failure was *silent*: messages sat at one tick, the retry
//! loop ran ~10x/minute indefinitely, and the only diagnosis was reading
//! logcat. Worse, the retries were not free — they burned the live relay's
//! per-family rate limit, so one dead card degraded delivery for every
//! healthy contact too.
//!
//! Policy, not mechanism: the shells observe faults and persist streaks, and
//! ask the three questions below. Both shells must answer them identically,
//! so the answers live here.

use crate::relay_status::CoreRelayFault;

/// Consecutive authoritative rejections before we stop believing a card.
///
/// Two, not one: a single 401 is genuinely unambiguous about the credential,
/// but relays get redeployed, and a request in flight across a restart can
/// answer from a half-initialised process. Requiring the *next* pass to
/// agree costs one sync interval and removes that whole class of false
/// positive. It is deliberately not higher — every extra attempt is another
/// minute of a person's messages silently not arriving.
pub const CONTACT_RELAY_STALE_STREAK: i64 = 2;

/// How long a written-off endpoint stays written off before one probe.
///
/// A stale card is normally repaired by a human (the contact re-shares it,
/// or T23 announces a new endpoint), and both of those clear the streak
/// immediately — this is only the backstop for the case nobody acts on.
/// Six hours is chosen to be useless as a hammer (144 attempts/day becomes
/// 4) while still healing an operator-side fix, like re-provisioning the
/// family row, without anyone touching the phones.
pub const CONTACT_RELAY_RECHECK_MS: i64 = 6 * 60 * 60 * 1000;

/// Does this fault mean *the card is wrong*, as opposed to *the service is
/// having a moment*?
///
/// Credential and family-state rejections are the relay telling us,
/// authoritatively, that the identity in the card is not one it will serve.
/// Retrying cannot fix that; only a new card (or an operator restoring the
/// family) can.
///
/// Everything else stays retryable on purpose. A 429 is the relay asking us
/// to slow down and is self-healing. A quota or oversize rejection is about
/// one envelope or one mailbox's fullness, not about who we are — writing
/// the card off for those would strand a contact whose family merely filled
/// their storage. An unstructured outage is a network, not an answer.
pub fn contact_relay_fault_is_authoritative(fault: CoreRelayFault) -> bool {
    match fault {
        CoreRelayFault::TokenRejected
        | CoreRelayFault::PassExpired
        | CoreRelayFault::PassSuspended => true,
        CoreRelayFault::RateLimited
        | CoreRelayFault::MailboxFull
        | CoreRelayFault::MessageTooLarge
        | CoreRelayFault::Outage => false,
    }
}

/// Should the endpoint be treated as stale, given the rejection streak
/// recorded for it (already including the fault just observed)?
#[uniffi::export]
pub fn core_contact_relay_is_stale(reject_streak: i64) -> bool {
    reject_streak >= CONTACT_RELAY_STALE_STREAK
}

/// May we spend a request re-probing a written-off endpoint?
///
/// `rejected_at_ms` is when the streak last advanced. A clock that jumped
/// backwards (restore onto a different phone, manual time change) would
/// otherwise pin an endpoint as un-probeable until the clock caught up, so
/// a future timestamp re-probes immediately rather than waiting it out.
#[uniffi::export]
pub fn core_contact_relay_recheck_due(rejected_at_ms: i64, now_ms: i64) -> bool {
    probe_due(rejected_at_ms, now_ms, CONTACT_RELAY_RECHECK_MS)
}

/// Shared "has the rest window elapsed?" test.
///
/// An unrecorded mark (`<= 0`) and a mark in the future both probe
/// immediately: the first has no window to wait out, and the second means
/// the clock moved backwards under us, which must never be able to pin an
/// endpoint until real time catches up.
fn probe_due(marked_at_ms: i64, now_ms: i64, window_ms: i64) -> bool {
    if marked_at_ms <= 0 || now_ms < marked_at_ms {
        return true;
    }
    now_ms - marked_at_ms >= window_ms
}

/// The whole per-contact decision for one sync pass, in one call so neither
/// shell can implement half of it: given the persisted streak and when it
/// last advanced, may we post to this contact's card endpoint right now?
///
/// `true` for a healthy endpoint, for one that has not yet reached the
/// streak, and for a written-off one whose probe is due.
#[uniffi::export]
pub fn core_contact_relay_endpoint_usable(
    reject_streak: i64,
    rejected_at_ms: i64,
    now_ms: i64,
) -> bool {
    if !core_contact_relay_is_stale(reject_streak) {
        return true;
    }
    core_contact_relay_recheck_due(rejected_at_ms, now_ms)
}

/// Classify a rejection observed against a contact's endpoint into the
/// streak delta to persist: `1` to advance toward writing the card off, `0`
/// to leave the streak untouched.
///
/// Note that a transient fault does not *reset* the streak. A dead endpoint
/// that also happens to rate-limit us must not be able to launder its way
/// back to healthy on the strength of the 429 — only an actual success
/// clears it (`clear_contact_relay_rejection`).
#[uniffi::export]
pub fn core_contact_relay_streak_delta(fault: CoreRelayFault) -> i64 {
    if contact_relay_fault_is_authoritative(fault) {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// The other way a card endpoint dies: it stops answering at all.
// ---------------------------------------------------------------------------
//
// Everything above needs the endpoint to *answer* — the streak only advances
// on a classified HTTP rejection. A retired host answers nothing: the request
// fails at the transport (DNS gone, connection refused, TLS failure, an
// unusable URL), which is not an HTTP rejection and so never reaches
// `core_contact_relay_streak_delta` at all. That half of the original report
// — "host retired" as distinct from "token revoked" — was still an unbounded
// retry loop against an address that will never come back.
//
// It is treated separately from a rejection rather than folded into the same
// streak, because the two justify different responses:
//
// * A rejection is the endpoint *disowning the credential*. That is proof the
//   card is wrong, so delivery falls back to our own relay — which really
//   delivers when both sides have since moved to the same new host, the exact
//   shape of the migration that produced the field report.
//
// * Silence is not proof of anything about the card. The host may simply be
//   rebooting. Falling back on that evidence would post a cross-family
//   contact's mail into *our* mailbox, which they never read — and because
//   `relay_posted_at` is terminal (a posted envelope is never offered to the
//   relay path again), that misroute would be permanent message loss, not a
//   retry. So silence earns only a rest: stop spending requests on the
//   endpoint, leave the envelope queued for a later pass and for the BLE/LAN
//   paths, and probe again soon.
//
// This state is deliberately per-process and unpersisted, which also keeps it
// out of the "their card is stale, ask them to re-share it" set the contact
// sheet reports — the right prompt for a revoked token and the wrong one for
// a host that is down this afternoon.

/// Consecutive passes an endpoint may fail to answer before we rest it.
///
/// Two, matching [`CONTACT_RELAY_STALE_STREAK`], and for the same reason: one
/// silent pass is ordinary (a dropped packet, a host mid-restart) and the
/// next pass agreeing costs one sync interval to rule that out.
pub const CONTACT_RELAY_UNREACHABLE_STREAK: i64 = 2;

/// How long a rested endpoint is left alone before one probe.
///
/// Far shorter than [`CONTACT_RELAY_RECHECK_MS`] because the conditions
/// differ in who has to act. A rejected card stays broken until a human
/// re-shares it, so probing it often is pure waste; a host that is not
/// answering usually comes back on its own, and nobody is going to tell us
/// when it does. Half an hour stops the hammering — the loop this replaces
/// ran ~10x/minute forever — while still picking a rebooted relay back up
/// within the hour rather than at the end of a working day.
pub const CONTACT_RELAY_UNREACHABLE_REST_MS: i64 = 30 * 60 * 1000;

/// Streak delta for an attempt that got no HTTP answer at all.
///
/// Gated on proof, in the same pass, that a *different* relay endpoint did
/// answer this device — in practice our own configured relay. Without that
/// proof the failure is most likely our own connectivity, and counting it
/// would let one flight, tunnel or dead Wi-Fi rest every contact's endpoint
/// at once. With it, the comparison is meaningful: this device's internet
/// demonstrably works and that specific host still said nothing.
///
/// Note the asymmetry with [`core_contact_relay_streak_delta`], which needs
/// no such proof: a 401 is the endpoint speaking, so it is evidence about the
/// card no matter what the rest of the network is doing.
///
/// Callers pass the observation and act on the delta; they must not test
/// `other_relay_answered` themselves first. A shell that guards the call with
/// its own `if` and then passes a literal `true` has moved the rule back into
/// both shells, where the two copies can drift — which is the whole reason
/// this module exists.
#[uniffi::export]
pub fn core_contact_relay_unreachable_delta(other_relay_answered: bool) -> i64 {
    if other_relay_answered {
        1
    } else {
        0
    }
}

/// May we spend a request on an endpoint that has been going unanswered?
///
/// `rested_at_ms` is when the unreachable streak last advanced. Mirrors
/// [`core_contact_relay_endpoint_usable`]: `true` below the streak, and
/// `true` again once the rest window is up so a recovered host is found
/// without anyone touching the phone.
#[uniffi::export]
pub fn core_contact_relay_unreachable_endpoint_usable(
    unreachable_streak: i64,
    rested_at_ms: i64,
    now_ms: i64,
) -> bool {
    if unreachable_streak < CONTACT_RELAY_UNREACHABLE_STREAK {
        return true;
    }
    probe_due(rested_at_ms, now_ms, CONTACT_RELAY_UNREACHABLE_REST_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_and_family_rejections_are_authoritative() {
        assert!(contact_relay_fault_is_authoritative(
            CoreRelayFault::TokenRejected
        ));
        assert!(contact_relay_fault_is_authoritative(
            CoreRelayFault::PassExpired
        ));
        assert!(contact_relay_fault_is_authoritative(
            CoreRelayFault::PassSuspended
        ));
    }

    #[test]
    fn service_conditions_never_write_off_a_card() {
        // The field bug is a dead card; a full mailbox or a busy relay is a
        // healthy card behind a busy service, and must stay retryable.
        for fault in [
            CoreRelayFault::RateLimited,
            CoreRelayFault::MailboxFull,
            CoreRelayFault::MessageTooLarge,
            CoreRelayFault::Outage,
        ] {
            assert!(!contact_relay_fault_is_authoritative(fault), "{fault:?}");
            assert_eq!(core_contact_relay_streak_delta(fault), 0, "{fault:?}");
        }
    }

    #[test]
    fn one_rejection_is_not_enough_but_two_are() {
        assert!(!core_contact_relay_is_stale(0));
        assert!(!core_contact_relay_is_stale(1));
        assert!(core_contact_relay_is_stale(2));
        assert!(core_contact_relay_is_stale(9));
    }

    #[test]
    fn a_healthy_endpoint_is_usable_regardless_of_timestamps() {
        assert!(core_contact_relay_endpoint_usable(0, 0, 1_000));
        assert!(core_contact_relay_endpoint_usable(1, 500, 1_000));
    }

    #[test]
    fn a_written_off_endpoint_is_unusable_until_the_probe_is_due() {
        let rejected = 1_000_000i64;
        assert!(!core_contact_relay_endpoint_usable(2, rejected, rejected));
        assert!(!core_contact_relay_endpoint_usable(
            2,
            rejected,
            rejected + CONTACT_RELAY_RECHECK_MS - 1
        ));
        assert!(core_contact_relay_endpoint_usable(
            2,
            rejected,
            rejected + CONTACT_RELAY_RECHECK_MS
        ));
    }

    #[test]
    fn a_backwards_clock_re_probes_rather_than_pinning_the_endpoint() {
        // Restore onto a second phone, or a manual clock change: never let
        // that strand a contact until real time catches up.
        assert!(core_contact_relay_recheck_due(5_000_000, 1_000));
        assert!(core_contact_relay_endpoint_usable(2, 5_000_000, 1_000));
    }

    #[test]
    fn an_unrecorded_rejection_time_re_probes() {
        assert!(core_contact_relay_recheck_due(0, 0));
    }

    #[test]
    fn silence_only_counts_when_another_relay_answered() {
        // The whole guard: without same-pass proof that this device's
        // internet works, an unanswered endpoint is our outage, not theirs.
        // A plane, a tunnel or a dead Wi-Fi association fails every endpoint
        // at once, and must not rest a single one of them.
        assert_eq!(core_contact_relay_unreachable_delta(false), 0);
        assert_eq!(core_contact_relay_unreachable_delta(true), 1);
    }

    #[test]
    fn a_rejection_needs_no_connectivity_proof_but_silence_does() {
        // The asymmetry is the point: a 401 is the endpoint speaking, so it
        // is evidence about the card whatever the network is doing. If these
        // two ever agree, one of them has lost its rationale.
        assert_eq!(
            core_contact_relay_streak_delta(CoreRelayFault::TokenRejected),
            1
        );
        assert_eq!(core_contact_relay_unreachable_delta(false), 0);
    }

    #[test]
    fn one_silent_pass_is_not_enough_but_two_are() {
        let rested = 1_000_000i64;
        assert!(core_contact_relay_unreachable_endpoint_usable(0, 0, rested));
        assert!(core_contact_relay_unreachable_endpoint_usable(
            1, rested, rested
        ));
        assert!(!core_contact_relay_unreachable_endpoint_usable(
            2, rested, rested
        ));
    }

    #[test]
    fn a_rested_endpoint_is_probed_again_once_the_window_is_up() {
        let rested = 1_000_000i64;
        assert!(!core_contact_relay_unreachable_endpoint_usable(
            2,
            rested,
            rested + CONTACT_RELAY_UNREACHABLE_REST_MS - 1
        ));
        assert!(core_contact_relay_unreachable_endpoint_usable(
            2,
            rested,
            rested + CONTACT_RELAY_UNREACHABLE_REST_MS
        ));
        // Same backwards-clock rule as the rejection side.
        assert!(core_contact_relay_unreachable_endpoint_usable(
            2, 5_000_000, 1_000
        ));
    }

    #[test]
    fn a_host_that_is_down_recovers_far_sooner_than_a_revoked_card() {
        // A rejected card stays broken until a person re-shares it, so
        // probing it often is waste; an unanswered host usually fixes itself
        // and nobody will tell us when it does. Pinned through the two
        // decisions rather than the two constants, so it keeps saying
        // something if either window changes shape.
        let marked = 1_000_000i64;
        let an_hour_later = marked + 60 * 60 * 1_000;
        assert!(core_contact_relay_unreachable_endpoint_usable(
            2,
            marked,
            an_hour_later
        ));
        assert!(!core_contact_relay_endpoint_usable(
            2,
            marked,
            an_hour_later
        ));
    }
}
