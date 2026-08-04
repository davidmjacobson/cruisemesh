//! Telling the reader when a message reached them, but only when its place in
//! the thread would otherwise lie to them.
//!
//! A bubble shows the *author's* send time, and should: "meet at the pool at
//! 2" composed at 1:55 and muled until 4:40 means 1:55, and re-stamping it on
//! arrival would make a whole carried batch read as though it were written at
//! the moment the two phones happened to meet. Conversation order follows the
//! same clock (`ORDER BY timestamp ASC` in
//! [`crate::store::MessageStore::messages_for_chat`]).
//!
//! The cost of that choice is that a message which took hours to arrive is
//! *spliced into the middle* of the thread, above replies the reader sent
//! while it was still in flight. Nothing on screen explains why an unread
//! message is sitting above their own answer, and if it landed far enough up
//! they will never find it -- the unread badge counts it (per-sender lamport,
//! not timestamp) but the thread gives them nowhere to look.
//!
//! This module decides which messages say so. It does not reorder anything
//! and does not touch a display timestamp -- compare [`crate::causal_order`],
//! which floors our own outgoing stamp so a reply cannot render above the
//! question it answers. Here we only annotate.
//!
//! The trigger is *displacement*, not lateness. A message that took six hours
//! and still landed at the end of the thread reads perfectly well: the thread
//! simply has a gap in it, which is ordinary life on a ship. What confuses
//! people is a message appearing *above* content that was already there. So a
//! fleet whose messages all take thirty minutes but arrive in order shows no
//! annotations at all, and the mark stays rare enough to mean something.

/// How far behind its send time a message must arrive before its arrival is
/// worth showing at all.
///
/// Two people replying at the same moment cross in the mail and arrive
/// interleaved -- everyone has seen this on every messenger ever built, and
/// nobody needs it explained. Ten minutes sits far above that crossing window
/// and far below the carried-for-hours delays that genuinely disorient a
/// reader, so the annotation stays attached to the case it was built for.
pub const LATE_ARRIVAL_MIN_DELAY_MS: i64 = 10 * 60 * 1000;

/// One row of the conversation as displayed, oldest first.
#[derive(Debug, Clone, uniffi::Record)]
pub struct LateArrivalInput {
    /// The author-clock timestamp this row renders with
    /// (`StoredMessage::timestamp`).
    pub display_ts_ms: i64,
    /// When this device first received the row (`messages.received_at`).
    ///
    /// `None` for a locally authored message, and for any message stored
    /// before arrival diagnostics were recorded. Legacy rows are never
    /// annotated -- we cannot claim an arrival time we never wrote down --
    /// but they still take part in the ordering below, standing in with
    /// their display timestamp.
    pub arrival_ts_ms: Option<i64>,
    /// Whether this device authored the row.
    pub is_own: bool,
}

/// Which rows should show their arrival time, one flag per row of `rows`.
///
/// A row is flagged when all of the following hold:
///
/// * it is not our own message, and we recorded an arrival time for it;
/// * it arrived at least [`LATE_ARRIVAL_MIN_DELAY_MS`] after it was sent; and
/// * something displayed *below* it was already here when it landed.
///
/// The last condition is the displacement test, and it is what keeps the
/// annotation rare. A row's "effective arrival" is when this device came to
/// hold it: the recorded arrival for an incoming message, and the display
/// timestamp for our own sends and for legacy rows (for our own sends those
/// are the same instant, which is exactly why one of our replies is the
/// witness that flags a message spliced above it).
///
/// One backward pass carrying the minimum effective arrival over the suffix;
/// `O(n)` and allocation-free apart from the result.
pub fn late_arrival_flags(rows: &[LateArrivalInput]) -> Vec<bool> {
    let mut flags = vec![false; rows.len()];
    // Nothing below the last row, so nothing can displace it.
    let mut min_below: Option<i64> = None;
    for (index, row) in rows.iter().enumerate().rev() {
        if let (false, Some(arrival), Some(below)) = (row.is_own, row.arrival_ts_ms, min_below) {
            let delayed = arrival.saturating_sub(row.display_ts_ms) >= LATE_ARRIVAL_MIN_DELAY_MS;
            flags[index] = delayed && arrival > below;
        }
        let effective = row.arrival_ts_ms.unwrap_or(row.display_ts_ms);
        min_below = Some(min_below.map_or(effective, |current: i64| current.min(effective)));
    }
    flags
}

/// FFI wrapper for [`late_arrival_flags`]; both shells call this once per
/// conversation reload, over the same list they are about to render.
#[uniffi::export]
pub fn core_late_arrival_flags(rows: Vec<LateArrivalInput>) -> Vec<bool> {
    late_arrival_flags(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60 * 1000;
    const HOUR: i64 = 60 * MINUTE;

    /// An incoming message sent at `sent`, received at `received`.
    fn theirs(sent: i64, received: i64) -> LateArrivalInput {
        LateArrivalInput {
            display_ts_ms: sent,
            arrival_ts_ms: Some(received),
            is_own: false,
        }
    }

    /// One of our own sends, which we hold from the moment we write it.
    fn ours(sent: i64) -> LateArrivalInput {
        LateArrivalInput {
            display_ts_ms: sent,
            arrival_ts_ms: None,
            is_own: true,
        }
    }

    /// An incoming message stored before arrival diagnostics existed.
    fn legacy(sent: i64) -> LateArrivalInput {
        LateArrivalInput {
            display_ts_ms: sent,
            arrival_ts_ms: None,
            is_own: false,
        }
    }

    #[test]
    fn an_empty_or_single_message_thread_is_never_annotated() {
        assert!(late_arrival_flags(&[]).is_empty());
        assert_eq!(late_arrival_flags(&[theirs(0, 6 * HOUR)]), vec![false]);
    }

    #[test]
    fn a_slow_thread_that_stayed_in_order_says_nothing() {
        // Every message took hours, but each still landed at the end of the
        // thread. The reader sees a gap, which needs no explanation.
        let rows = vec![
            theirs(0, 3 * HOUR),
            theirs(HOUR, 4 * HOUR),
            theirs(2 * HOUR, 5 * HOUR),
        ];
        assert_eq!(late_arrival_flags(&rows), vec![false, false, false]);
    }

    #[test]
    fn crossed_replies_are_not_annotated() {
        // Both typed at once; hers is displayed above his but arrived after.
        // Displaced, yes -- but seconds apart, which everybody understands.
        let rows = vec![theirs(1_000, 12_000), ours(2_000)];
        assert_eq!(late_arrival_flags(&rows), vec![false, false]);
    }

    #[test]
    fn a_carried_message_spliced_above_our_reply_is_annotated() {
        // The field case: her message is muled for three hours and lands
        // above the two replies we sent in the meantime.
        let rows = vec![
            ours(0),
            theirs(MINUTE, 3 * HOUR),
            ours(30 * MINUTE),
            ours(45 * MINUTE),
        ];
        assert_eq!(late_arrival_flags(&rows), vec![false, true, false, false]);
    }

    #[test]
    fn our_own_messages_are_never_annotated() {
        // Even with a wildly displaced neighbour, our sends stay unmarked --
        // we were there when we wrote them.
        let rows = vec![ours(0), theirs(MINUTE, 5 * HOUR), ours(2 * HOUR)];
        assert!(!late_arrival_flags(&rows)[0]);
        assert!(!late_arrival_flags(&rows)[2]);
    }

    #[test]
    fn the_delay_floor_is_inclusive() {
        let below = vec![theirs(0, LATE_ARRIVAL_MIN_DELAY_MS - 1), ours(MINUTE)];
        let at = vec![theirs(0, LATE_ARRIVAL_MIN_DELAY_MS), ours(MINUTE)];
        assert!(!late_arrival_flags(&below)[0]);
        assert!(late_arrival_flags(&at)[0]);
    }

    #[test]
    fn a_legacy_row_is_never_annotated_but_still_witnesses() {
        // We have no arrival time for the legacy row, so it makes no claim of
        // its own -- but it was plainly already here, so a message spliced
        // above it is still displaced.
        let rows = vec![theirs(0, 4 * HOUR), legacy(HOUR)];
        assert_eq!(late_arrival_flags(&rows), vec![true, false]);
    }

    #[test]
    fn a_group_annotates_only_the_displaced_sender() {
        // Three senders interleaved; only the carried one is out of place.
        let rows = vec![
            theirs(0, MINUTE),
            theirs(10 * MINUTE, 6 * HOUR),
            theirs(20 * MINUTE, 21 * MINUTE),
            ours(30 * MINUTE),
        ];
        assert_eq!(late_arrival_flags(&rows), vec![false, true, false, false]);
    }

    #[test]
    fn a_whole_carried_batch_annotates_every_displaced_row() {
        // A mega-carrier hands over a backlog at once: each row of the batch
        // is displaced by the reply below it, and each says so.
        let rows = vec![
            theirs(0, 5 * HOUR),
            theirs(MINUTE, 5 * HOUR),
            theirs(2 * MINUTE, 5 * HOUR),
            ours(HOUR),
        ];
        assert_eq!(late_arrival_flags(&rows), vec![true, true, true, false]);
    }

    #[test]
    fn a_sender_clock_running_ahead_does_not_annotate() {
        // causal_order's skew case: their phone stamps the future, so the
        // arrival looks earlier than the send. Negative delay, no flag, no
        // panic.
        let rows = vec![theirs(9 * HOUR, HOUR), ours(2 * HOUR)];
        assert_eq!(late_arrival_flags(&rows), vec![false, false]);
    }

    #[test]
    fn arrival_ties_do_not_annotate() {
        // Delivered in the same instant as what sits below it: nothing was
        // here first, so nothing was displaced.
        let rows = vec![theirs(0, 4 * HOUR), theirs(HOUR, 4 * HOUR)];
        assert_eq!(late_arrival_flags(&rows), vec![false, false]);
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let rows = vec![
            theirs(i64::MIN, i64::MAX),
            theirs(i64::MAX, i64::MIN),
            ours(i64::MAX),
        ];
        let flags = late_arrival_flags(&rows);
        assert!(flags[0], "a maximally displaced row still annotates");
        assert!(!flags[1]);
        assert!(!flags[2]);
    }
}
