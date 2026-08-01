//! Keeping a reply from rendering above the message it answers.
//!
//! Conversation order is `ORDER BY timestamp ASC, id ASC` on the *author's*
//! wall clock (see [`crate::store::MessageStore::messages_for_chat`]). That
//! choice is deliberate and correct as far as it goes — `lamport` is only
//! comparable within one sender's stream, so ordering a two-party chat by it
//! renders messages in an order neither person experienced.
//!
//! But an author's wall clock is not a fact about the conversation, it is a
//! fact about their phone. When the other person's clock runs ahead, their
//! message arrives stamped in our future, and the reply we type ten seconds
//! later is stamped *earlier* than the question. Both phones then render the
//! answer above the question, permanently, with no way for either person to
//! tell what happened. Reported from the field on 2026-07-30: "if I
//! immediately respond to Leanne's text my response appears before her
//! question".
//!
//! There is one ordering fact neither clock can contradict: a message we
//! author is causally after every message already in that chat, because we
//! had to have received them to be replying. This module enforces exactly
//! that and nothing more — a floor, not a rewrite. When the clocks agree
//! (overwhelmingly the common case) it changes nothing at all.
//!
//! Scope note: only the *display* timestamp is floored. Routing time —
//! `recipient_hint`'s day bucket and the envelope's expiry — stays on the
//! true clock, because those are keyed to real elapsed time and the hint's
//! match window only looks backwards (`CARRY_HINT_DAY_WINDOW_DAYS`). Pushing
//! a hint into tomorrow's bucket to fix a display artifact would trade a
//! cosmetic bug for an undeliverable message.

/// How far ahead of us another phone's clock may be and still pull our reply
/// forward.
///
/// Without a bound, one device with a badly wrong clock (a dead RTC coming
/// up at the epoch's far side, a manually mis-set year) would pin every
/// subsequent message in that chat to its bogus time forever — each new
/// message flooring off the last, so a single bad timestamp becomes
/// permanent and self-sustaining. Beyond this bound we leave our own
/// timestamp alone: that one message stays mis-ordered, which is bad, rather
/// than corrupting the whole conversation, which is worse.
///
/// A day is chosen to sit far above real skew (unsynchronised phone clocks
/// drift by seconds to minutes) and far below the wrong-date failures worth
/// rejecting.
pub const CAUSAL_ORDER_MAX_SKEW_MS: i64 = 24 * 60 * 60 * 1000;

/// The timestamp a newly authored message should display with, given the
/// newest timestamp already stored in that chat.
///
/// Returns `now_ms` unchanged whenever the chat contains nothing newer than
/// now — the normal case, and the reason well-synchronised phones see no
/// behaviour change at all.
pub fn causal_display_timestamp(newest_in_chat_ms: Option<i64>, now_ms: i64) -> i64 {
    let Some(newest) = newest_in_chat_ms else {
        return now_ms;
    };
    if newest < now_ms {
        return now_ms;
    }
    if newest.saturating_sub(now_ms) > CAUSAL_ORDER_MAX_SKEW_MS {
        return now_ms;
    }
    // +1ms rather than equality: `messages_for_chat` breaks timestamp ties on
    // rowid, and a reply inserted in the same millisecond as the message it
    // answers would otherwise depend on insertion order to sort correctly --
    // which is exactly the accident we are removing.
    newest.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_chat_uses_our_own_clock() {
        assert_eq!(causal_display_timestamp(None, 1_000), 1_000);
    }

    #[test]
    fn agreeing_clocks_change_nothing() {
        // The common case: everything in the chat is genuinely older.
        assert_eq!(causal_display_timestamp(Some(900), 1_000), 1_000);
        assert_eq!(causal_display_timestamp(Some(999), 1_000), 1_000);
    }

    #[test]
    fn a_reply_never_renders_above_the_message_it_answers() {
        // Their clock is five minutes ahead; our reply is ten seconds later
        // by our clock and would otherwise sort before their question.
        let theirs = 5 * 60 * 1_000;
        let ours = 10 * 1_000;
        let stamped = causal_display_timestamp(Some(theirs), ours);
        assert!(stamped > theirs, "reply must sort after the question");
        assert_eq!(stamped, theirs + 1);
    }

    #[test]
    fn a_same_millisecond_reply_still_sorts_after() {
        assert_eq!(causal_display_timestamp(Some(1_000), 1_000), 1_001);
    }

    #[test]
    fn a_wildly_wrong_clock_cannot_pin_the_conversation() {
        // A phone claiming next year must not drag every future message in
        // this chat to next year with it.
        let bogus = 1_000 + CAUSAL_ORDER_MAX_SKEW_MS + 1;
        assert_eq!(causal_display_timestamp(Some(bogus), 1_000), 1_000);
    }

    #[test]
    fn the_skew_bound_is_inclusive() {
        let edge = 1_000 + CAUSAL_ORDER_MAX_SKEW_MS;
        assert_eq!(causal_display_timestamp(Some(edge), 1_000), edge + 1);
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        assert_eq!(causal_display_timestamp(Some(i64::MAX), 1_000), 1_000);
        assert_eq!(causal_display_timestamp(Some(i64::MIN), 1_000), 1_000);
        // Saturating add keeps a pathological-but-in-window value finite.
        assert_eq!(causal_display_timestamp(Some(i64::MAX), i64::MAX), i64::MAX);
    }
}
