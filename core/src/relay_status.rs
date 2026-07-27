//! CP2b: classification of relayd's structured HTTP rejections into the
//! semantic Cruise Pass states both shells render.
//!
//! relayd answers every rejection with a status code and, for the conditions
//! a client can act on, a stable JSON `code` field (relayd/DEPLOY.md §10):
//! 403 `family_expired` / `family_suspended`, other 401/403 for a bad token,
//! 507 `family_quota_exceeded`, 413 `envelope_too_large`, and 429
//! `rate_limited` with a `Retry-After` header. Which of those maps to which
//! user-visible state — and which of them self-heal versus need a person to
//! act — is product policy, so it lives here in the core and both shells
//! only render the result.

/// One structured relay rejection, classified. Ordered by nothing — use
/// [`relay_fault_rank`] when several faults from one sync pass compete for
/// the single status slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayFault {
    /// 403 `family_expired`: the pass lapsed; renewing it is the only fix.
    PassExpired,
    /// 403 `family_suspended`: the operator turned the family off.
    PassSuspended,
    /// Any other 401/403: the saved credential itself is bad (T11).
    TokenRejected,
    /// 507 `family_quota_exceeded`: the family's hosted storage is full.
    /// Posting fails while fetching keeps working, so this must surface even
    /// when the rest of the sync pass succeeds.
    MailboxFull,
    /// 413 `envelope_too_large`: this one envelope can never be posted.
    /// Actionable locally (send something smaller); never a support case.
    MessageTooLarge,
    /// 429 `rate_limited`: too fast, not broken. Self-heals within the
    /// `Retry-After` window ([`relay_retry_after_ms`]); the person holding
    /// the phone has nothing to do and must not be told to contact anyone.
    RateLimited,
    /// Any other non-2xx: a generic outage with no structured meaning.
    Outage,
}

/// Map one HTTP rejection to its semantic fault. The stable `code` field
/// wins over the status code when both are present (a proxy can rewrite a
/// status; the JSON body comes from relayd itself), and unknown
/// status/code combinations degrade to [`CoreRelayFault::Outage`] rather
/// than guessing.
#[uniffi::export]
pub fn relay_classify_http_error(http_status: u16, relay_code: Option<String>) -> CoreRelayFault {
    match relay_code.as_deref() {
        Some("family_expired") => return CoreRelayFault::PassExpired,
        Some("family_suspended") => return CoreRelayFault::PassSuspended,
        Some("family_quota_exceeded") => return CoreRelayFault::MailboxFull,
        Some("envelope_too_large") => return CoreRelayFault::MessageTooLarge,
        Some("rate_limited") => return CoreRelayFault::RateLimited,
        _ => {}
    }
    match http_status {
        401 | 403 => CoreRelayFault::TokenRejected,
        507 => CoreRelayFault::MailboxFull,
        413 => CoreRelayFault::MessageTooLarge,
        429 => CoreRelayFault::RateLimited,
        _ => CoreRelayFault::Outage,
    }
}

/// True for conditions that clear on their own with no action from the
/// person holding the phone ("?" on the Cruise Pass indicator); false for
/// conditions that persist until someone acts ("!"). Support guidance
/// belongs only on the persistent side.
#[uniffi::export]
pub fn relay_fault_is_transient(fault: CoreRelayFault) -> bool {
    match fault {
        CoreRelayFault::RateLimited | CoreRelayFault::Outage => true,
        CoreRelayFault::PassExpired
        | CoreRelayFault::PassSuspended
        | CoreRelayFault::TokenRejected
        | CoreRelayFault::MailboxFull
        | CoreRelayFault::MessageTooLarge => false,
    }
}

/// Which fault wins when one sync pass observes several (e.g. a few 507s and
/// then a 429 once the burst also trips the rate limiter). Higher rank =
/// more important to show: credential faults first (nothing else can work),
/// then the persistent mailbox conditions, then the self-healing ones.
/// Shared so both shells keep the same worst-of fold.
#[uniffi::export]
pub fn relay_fault_rank(fault: CoreRelayFault) -> u8 {
    match fault {
        CoreRelayFault::PassSuspended => 6,
        CoreRelayFault::PassExpired => 5,
        CoreRelayFault::TokenRejected => 4,
        CoreRelayFault::MailboxFull => 3,
        CoreRelayFault::MessageTooLarge => 2,
        CoreRelayFault::RateLimited => 1,
        CoreRelayFault::Outage => 0,
    }
}

/// How long a 429 asks the client to back off, in milliseconds. relayd's
/// `Retry-After` is integer delta-seconds, at least 1 and never more than 60
/// (a full window always refills a bucket completely — DEPLOY.md §10), so
/// anything outside that range is a parse artifact and gets clamped. A
/// missing or malformed header falls back to 30 s: long enough to matter,
/// short enough that an over-cautious default never visibly stalls sync.
#[uniffi::export]
pub fn relay_retry_after_ms(retry_after_header: Option<String>) -> u64 {
    const DEFAULT_SECONDS: u64 = 30;
    const MAX_SECONDS: u64 = 60;
    let seconds = retry_after_header
        .as_deref()
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS)
        .clamp(1, MAX_SECONDS);
    seconds * 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_classify_regardless_of_status() {
        // The JSON code is authoritative even when a proxy rewrote the status.
        for status in [200u16, 403, 429, 500, 507] {
            assert_eq!(
                relay_classify_http_error(status, Some("family_expired".into())),
                CoreRelayFault::PassExpired
            );
            assert_eq!(
                relay_classify_http_error(status, Some("family_suspended".into())),
                CoreRelayFault::PassSuspended
            );
            assert_eq!(
                relay_classify_http_error(status, Some("family_quota_exceeded".into())),
                CoreRelayFault::MailboxFull
            );
            assert_eq!(
                relay_classify_http_error(status, Some("envelope_too_large".into())),
                CoreRelayFault::MessageTooLarge
            );
            assert_eq!(
                relay_classify_http_error(status, Some("rate_limited".into())),
                CoreRelayFault::RateLimited
            );
        }
    }

    #[test]
    fn statuses_classify_without_a_code() {
        assert_eq!(
            relay_classify_http_error(401, None),
            CoreRelayFault::TokenRejected
        );
        assert_eq!(
            relay_classify_http_error(403, None),
            CoreRelayFault::TokenRejected
        );
        assert_eq!(
            relay_classify_http_error(507, None),
            CoreRelayFault::MailboxFull
        );
        assert_eq!(
            relay_classify_http_error(413, None),
            CoreRelayFault::MessageTooLarge
        );
        assert_eq!(
            relay_classify_http_error(429, None),
            CoreRelayFault::RateLimited
        );
    }

    #[test]
    fn unknown_shapes_degrade_to_outage() {
        assert_eq!(relay_classify_http_error(500, None), CoreRelayFault::Outage);
        assert_eq!(relay_classify_http_error(404, None), CoreRelayFault::Outage);
        assert_eq!(
            relay_classify_http_error(500, Some("something_new".into())),
            CoreRelayFault::Outage
        );
        // An unknown code on a known status still uses the status.
        assert_eq!(
            relay_classify_http_error(429, Some("something_new".into())),
            CoreRelayFault::RateLimited
        );
    }

    #[test]
    fn transient_versus_persistent_split_matches_the_ux_spec() {
        // "?" states: self-healing, never a support case.
        assert!(relay_fault_is_transient(CoreRelayFault::RateLimited));
        assert!(relay_fault_is_transient(CoreRelayFault::Outage));
        // "!" states: persist until someone acts.
        assert!(!relay_fault_is_transient(CoreRelayFault::PassExpired));
        assert!(!relay_fault_is_transient(CoreRelayFault::PassSuspended));
        assert!(!relay_fault_is_transient(CoreRelayFault::TokenRejected));
        assert!(!relay_fault_is_transient(CoreRelayFault::MailboxFull));
        assert!(!relay_fault_is_transient(CoreRelayFault::MessageTooLarge));
    }

    #[test]
    fn rank_orders_credential_then_persistent_then_transient() {
        let ordered = [
            CoreRelayFault::Outage,
            CoreRelayFault::RateLimited,
            CoreRelayFault::MessageTooLarge,
            CoreRelayFault::MailboxFull,
            CoreRelayFault::TokenRejected,
            CoreRelayFault::PassExpired,
            CoreRelayFault::PassSuspended,
        ];
        for pair in ordered.windows(2) {
            assert!(
                relay_fault_rank(pair[0]) < relay_fault_rank(pair[1]),
                "{:?} should rank below {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn retry_after_parses_clamps_and_defaults() {
        assert_eq!(relay_retry_after_ms(Some("3".into())), 3_000);
        assert_eq!(relay_retry_after_ms(Some(" 60 ".into())), 60_000);
        // relayd never advertises more than 60 s; clamp anything larger.
        assert_eq!(relay_retry_after_ms(Some("3600".into())), 60_000);
        // Zero would mean "retry immediately", defeating the point.
        assert_eq!(relay_retry_after_ms(Some("0".into())), 1_000);
        assert_eq!(relay_retry_after_ms(Some("soon".into())), 30_000);
        assert_eq!(relay_retry_after_ms(None), 30_000);
    }
}
