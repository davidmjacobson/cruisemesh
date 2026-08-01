//! Where the last relay-mailbox walk got to, and when the next full sweep is
//! due.
//!
//! ## The bug this exists to kill
//!
//! Both shells used to start every relay sync pass at cursor 0 and page
//! forward until the mailbox ran out. That is fine for a mailbox with a
//! handful of rows and catastrophic for a real one, because most rows are
//! never acked *on purpose*:
//!
//! - a proxy-fetched envelope comes back `CARRIED`, not `CONSUMED`, and the
//!   relay copy is deliberately left in place as the durable fallback (the
//!   DTN ack-safety invariant — never ack a relay copy unless this device was
//!   the envelope's sole true endpoint consumer);
//! - a legacy shared-mailbox group-hint row is never acked at all.
//!
//! So the mailbox only grows, relayd returns rows in ascending id order, and
//! a *fresh* message therefore has the highest id and is fetched **last**. In
//! the field this reached ~29k rows at 16 rows per page: thousands of
//! sequential HTTP round trips before the newest message was even looked at,
//! sustained at 60–130 pages a minute, with passes regularly dying on a
//! timeout before they ever reached the end. A message that should land in
//! seconds took minutes, or never arrived at all.
//!
//! ## The fix
//!
//! Remember the frontier. A normal pass resumes from the highest id whose
//! page was fully processed, so it fetches only what is genuinely new and
//! reaches fresh mail on the first page. Occasionally — at cold start, and on
//! a slow timer — a pass walks the whole mailbox from 0 again so the rows
//! that are *supposed* to stay there remain re-discoverable.
//!
//! Policy lives here, as plain functions, so both shells answer every
//! question the same way and every answer is unit-testable without a relay,
//! a socket, or a store.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::BASE64URL_NOPAD;

use crate::relay_wire::normalize_relay_url;

/// Domain separation for [`relay_cursor_key`]. Distinct from every other
/// BLAKE2b context in the crate so a cursor key can never collide with, or be
/// mistaken for, a deposit token.
const RELAY_CURSOR_KEY_CONTEXT: &[u8] = b"cruisemesh relay fetch cursor key v1";

/// How long a frontier-only run may go before the next full walk from 0.
///
/// The sweep is not about *our* mail — the frontier already delivers that.
/// It exists because a relay mailbox is shared infrastructure: rows we
/// deliberately left behind (a `CARRIED` proxy copy, a legacy group-hint row)
/// have to stay re-discoverable by this phone, because the recipient may only
/// ever be reachable through a phone that re-offers them over Bluetooth. A
/// proxy that fetched-and-carried and then lost its local carry queue — a
/// reinstall, a cleared cache, an expiry sweep — recovers exactly here.
///
/// Six hours is chosen the same way [`crate::CONTACT_RELAY_RECHECK_MS`] was:
/// long enough that the walk it triggers is a rounding error against a day of
/// polling, short enough that no phone is more than a quarter of a day away
/// from re-offering what it is carrying for someone else. It is deliberately
/// *not* the delivery path — nothing a person sends waits on it.
pub const RELAY_SWEEP_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;

/// [`RELAY_SWEEP_INTERVAL_MS`], for shells that cannot see the constant.
#[uniffi::export]
pub fn relay_sweep_interval_ms() -> i64 {
    RELAY_SWEEP_INTERVAL_MS
}

/// A stable, credential-free name for one relay mailbox: the URL and the
/// token that reads it, hashed together.
///
/// Both halves matter. The URL alone would conflate two families hosted on
/// the same relay, and their mailboxes have unrelated id spaces. The token
/// alone would survive a host migration it should not survive.
///
/// Hashed rather than stored plainly for two reasons. It keeps a relay
/// credential out of the message database, which is what a `.cmbak` backup
/// and every debug DB pull carry around. And it makes *rotation* correct by
/// construction: a rotated token is a different key, the new key has no row,
/// an absent row reads as cursor 0, and the first pass after a rotation
/// therefore walks the mailbox from the beginning — which is exactly right,
/// because a new credential may see a different set of rows than the old one
/// did.
///
/// The URL is normalized first, so a config saved as `relay.example` and one
/// saved as `https://relay.example/` are one mailbox, not two.
#[uniffi::export]
pub fn relay_cursor_key(relay_url: String, relay_token: String) -> String {
    let url = normalize_relay_url(relay_url);
    let token = relay_token.trim();
    if url.is_empty() || token.is_empty() {
        // Not a usable endpoint. An empty key is never persisted (the store
        // treats it as "no cursor"), so such a config always walks from 0
        // rather than sharing a row with every other incomplete config.
        return String::new();
    }
    let mut hasher = Blake2bVar::new(32).expect("valid blake2b output length");
    hasher.update(RELAY_CURSOR_KEY_CONTEXT);
    hasher.update(url.as_bytes());
    hasher.update(&[0u8]);
    hasher.update(token.as_bytes());
    let mut out = [0u8; 32];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    BASE64URL_NOPAD.encode(&out)
}

/// Must this pass walk the whole mailbox from 0?
///
/// `swept_this_session` is per-process, not persisted: the first pass after a
/// cold start always sweeps. That is the cheap, self-healing answer to every
/// way a persisted cursor can go stale in a way we cannot detect from a
/// response — most importantly a relay rebuilt from scratch, whose row ids
/// restart at 1 and would otherwise sit forever below a frontier we still
/// remember. Restarting the app fixes it; the timer fixes it unattended.
///
/// A `last_sweep_at_ms` in the future (a clock that jumped backwards, a
/// restore onto a phone set to a different time) sweeps immediately rather
/// than pinning the mailbox as un-swept until real time catches up — the same
/// rule [`crate::core_contact_relay_recheck_due`] applies for the same reason.
#[uniffi::export]
pub fn relay_sweep_due(swept_this_session: bool, last_sweep_at_ms: i64, now_ms: i64) -> bool {
    if !swept_this_session {
        return true;
    }
    if last_sweep_at_ms <= 0 || now_ms < last_sweep_at_ms {
        return true;
    }
    now_ms - last_sweep_at_ms >= RELAY_SWEEP_INTERVAL_MS
}

/// The `after=` this pass starts its walk at: 0 for a sweep, the remembered
/// frontier otherwise. A negative persisted value (corrupt row, hand-edited
/// database) reads as 0 rather than being sent to a relay that would reject
/// it.
#[uniffi::export]
pub fn relay_pass_start_cursor(sweeping: bool, persisted_after_id: i64) -> i64 {
    if sweeping || persisted_after_id < 0 {
        0
    } else {
        persisted_after_id
    }
}

/// The frontier to persist after one page, given what was already persisted.
///
/// This is the mirror of the DTN ack-safety invariant, applied to *skipping*
/// rather than to deleting: moving the cursor past an envelope means no
/// ordinary pass will ever present it again. So it only moves when the page
/// reached a terminal disposition for **every** envelope in it — consumed,
/// carried, expired, seen, or rejected — and when the acks that page earned
/// were actually delivered. Anything else (a store write that threw, an ack
/// request that failed) leaves the frontier where it was, and those envelopes
/// come back next pass.
///
/// It also never moves *backwards*. A sweep walks from 0 and therefore
/// reports page cursors far below the frontier for most of its run; taking
/// the maximum means a sweep re-reads the mailbox without ever costing the
/// frontier its position, so an interrupted sweep cannot turn into a
/// re-walk-everything-next-pass loop.
#[uniffi::export]
pub fn relay_cursor_advance(
    persisted_after_id: i64,
    page_next_cursor: i64,
    page_fully_processed: bool,
) -> i64 {
    let persisted = persisted_after_id.max(0);
    if !page_fully_processed || page_next_cursor <= persisted {
        return persisted;
    }
    page_next_cursor
}

/// Should the walk fetch another page?
///
/// Termination is decided by an **empty page**, never by a short one. A
/// server is free to clamp `limit=` below what we asked for — relayd's own
/// `MAX_FETCH_LIMIT` does exactly that above 500 — and a client that reads
/// `page.len() < limit` as end-of-mailbox would stop one page in and silently
/// never see the rest, which for an ascending-id mailbox means never seeing
/// anything new at all.
///
/// Short pages are not an edge case any more, either: relayd now stops
/// filling a page once its cumulative `sealed` bytes would push the response
/// past what a client will decode, so a mailbox holding large attachment
/// chunks routinely answers a 256-row ask with a handful of rows. That page is
/// complete and its cursor is sound — the walk simply continues from it.
///
/// The cursor check is the other half: a page that returns rows without
/// advancing `next_cursor` past `after` would loop forever on the same rows.
/// relayd cannot produce that (its cursor is the last row's id, and ids are
/// strictly increasing within a page), so this only ever fires against a
/// broken or hostile server — which is precisely when a client must not spin.
#[uniffi::export]
pub fn relay_fetch_walk_continues(
    page_envelope_count: u32,
    after_id: i64,
    page_next_cursor: i64,
) -> bool {
    page_envelope_count > 0 && page_next_cursor > after_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mailbox_key_is_stable_and_credential_free() {
        let key = relay_cursor_key("https://relay.example".into(), "token-aaaa".into());
        assert_eq!(
            key,
            relay_cursor_key("relay.example/".into(), " token-aaaa ".into()),
            "url normalization and token trimming must name the same mailbox"
        );
        assert!(!key.is_empty());
        assert!(!key.contains("token-aaaa"), "the credential must not leak");
        assert!(!key.contains("relay.example"));
    }

    #[test]
    fn rotating_the_token_or_moving_hosts_yields_a_fresh_cursor() {
        // Both are "unknown key" at the store, which reads as cursor 0 — the
        // first pass on a new credential re-walks, which is the only safe
        // thing to do when the row set it can see may have changed.
        let base = relay_cursor_key("https://relay.example".into(), "token-aaaa".into());
        assert_ne!(
            base,
            relay_cursor_key("https://relay.example".into(), "token-bbbb".into())
        );
        assert_ne!(
            base,
            relay_cursor_key("https://other.example".into(), "token-aaaa".into())
        );
    }

    #[test]
    fn two_families_on_one_host_are_not_one_mailbox() {
        assert_ne!(
            relay_cursor_key("https://relay.example".into(), "family-one".into()),
            relay_cursor_key("https://relay.example".into(), "family-two".into())
        );
    }

    #[test]
    fn an_unusable_endpoint_has_no_key() {
        assert_eq!(relay_cursor_key(String::new(), "token".into()), "");
        assert_eq!(
            relay_cursor_key("https://relay.example".into(), "  ".into()),
            ""
        );
        // An http:// URL is rejected by normalize_relay_url, so it cannot
        // acquire a cursor either.
        assert_eq!(
            relay_cursor_key("http://relay.example".into(), "token".into()),
            ""
        );
    }

    #[test]
    fn the_first_pass_of_a_process_always_sweeps() {
        // Cold start: whatever the persisted timestamp says.
        assert!(relay_sweep_due(false, 0, 1_000));
        assert!(relay_sweep_due(false, 1_000, 1_000));
        assert!(relay_sweep_due(false, i64::MAX, 1_000));
    }

    #[test]
    fn later_passes_sweep_only_once_the_interval_has_elapsed() {
        let swept_at = 1_000_000i64;
        assert!(!relay_sweep_due(true, swept_at, swept_at));
        assert!(!relay_sweep_due(
            true,
            swept_at,
            swept_at + RELAY_SWEEP_INTERVAL_MS - 1
        ));
        assert!(relay_sweep_due(
            true,
            swept_at,
            swept_at + RELAY_SWEEP_INTERVAL_MS
        ));
        assert_eq!(relay_sweep_interval_ms(), 6 * 60 * 60 * 1000);
    }

    #[test]
    fn a_never_recorded_or_backwards_clock_sweeps_rather_than_stalling() {
        assert!(relay_sweep_due(true, 0, 5_000));
        assert!(relay_sweep_due(true, -1, 5_000));
        assert!(relay_sweep_due(true, 5_000_000, 1_000));
    }

    #[test]
    fn a_sweep_starts_at_zero_and_a_normal_pass_resumes() {
        assert_eq!(relay_pass_start_cursor(true, 9_000), 0);
        assert_eq!(relay_pass_start_cursor(false, 9_000), 9_000);
        assert_eq!(relay_pass_start_cursor(false, 0), 0);
        // Corrupt/hand-edited row: never send a negative `after`.
        assert_eq!(relay_pass_start_cursor(false, -5), 0);
    }

    #[test]
    fn a_fully_processed_page_moves_the_frontier() {
        assert_eq!(relay_cursor_advance(0, 16, true), 16);
        assert_eq!(relay_cursor_advance(16, 32, true), 32);
    }

    #[test]
    fn a_page_that_did_not_finish_leaves_the_frontier_alone() {
        // The whole safety rule: an envelope that never reached a terminal
        // disposition must be presented again next pass, so nothing may be
        // persisted past it.
        assert_eq!(relay_cursor_advance(16, 32, false), 16);
        assert_eq!(relay_cursor_advance(0, 999, false), 0);
    }

    #[test]
    fn the_frontier_never_moves_backwards() {
        // A sweep re-reads pages far below the frontier; that must not undo
        // it, or every sweep would be followed by a full re-walk.
        assert_eq!(relay_cursor_advance(9_000, 16, true), 9_000);
        assert_eq!(relay_cursor_advance(9_000, 9_000, true), 9_000);
        assert_eq!(relay_cursor_advance(9_000, 9_001, true), 9_001);
        // A negative persisted value is clamped, not propagated.
        assert_eq!(relay_cursor_advance(-3, 5, true), 5);
        assert_eq!(relay_cursor_advance(-3, 5, false), 0);
    }

    #[test]
    fn only_an_empty_page_ends_the_walk() {
        assert!(!relay_fetch_walk_continues(0, 100, 100));
        assert!(relay_fetch_walk_continues(1, 100, 101));
    }

    #[test]
    fn a_server_that_clamps_the_limit_does_not_end_the_walk_early() {
        // We ask for 256 and a server hands back 50. That is not
        // end-of-mailbox, and treating it as one would strand every row above
        // id 50 — which, in an ascending-id mailbox, is all the new mail.
        assert!(relay_fetch_walk_continues(50, 0, 50));
        assert!(relay_fetch_walk_continues(1, 0, 1));
    }

    /// A byte-budgeted server returns short pages *by design*, so the
    /// short-page rule is load-bearing rather than defensive. A mailbox of
    /// large attachment chunks answers a 256-row ask with a few rows every
    /// time; if that ended the walk, the newest mail — which has the highest
    /// ids — would never be reached at all.
    #[test]
    fn a_page_truncated_by_a_byte_budget_keeps_the_walk_going() {
        // 12 rows out of an ask of 256, then 9, then 1: each one continues,
        // and each hands the next page the cursor it advanced to.
        assert!(relay_fetch_walk_continues(12, 0, 12));
        assert!(relay_fetch_walk_continues(9, 12, 21));
        assert!(relay_fetch_walk_continues(1, 21, 22));
        // Only running out of rows ends it.
        assert!(!relay_fetch_walk_continues(0, 22, 22));
    }

    #[test]
    fn a_cursor_that_does_not_advance_ends_the_walk_instead_of_looping() {
        // Rows returned but the cursor stood still: a broken or hostile
        // server. Stop rather than fetch the same page forever.
        assert!(!relay_fetch_walk_continues(16, 100, 100));
        assert!(!relay_fetch_walk_continues(16, 100, 99));
    }
}
