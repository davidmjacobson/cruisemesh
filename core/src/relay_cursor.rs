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
//! reaches fresh mail on the first page. Occasionally — on a slow timer, and
//! the first time a mailbox is seen at all — a pass walks the whole mailbox
//! from 0 again so the rows that are *supposed* to stay there remain
//! re-discoverable.
//!
//! ## The second bug: a sweep that could never finish
//!
//! A walk is bounded — [`relay_mailbox_walk_action`] yields after
//! [`RELAY_MAILBOX_MAX_PAGES_PER_PASS`] pages or
//! [`RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS`] envelopes and asks the shell to
//! schedule a continuation, so a deep mailbox cannot hold a sync pass hostage.
//! That bound and the sweep were incompatible for as long as a sweep had only
//! one place to start: **0**.
//!
//! A sweep is recorded complete only on the empty page that ends the walk. A
//! mailbox with more than one budget's worth of hint-matching rows never
//! reaches that page inside one pass, so the sweep stayed due, and the next
//! pass — a second later — started it again at 0. In the field this was a
//! permanent loop: the same first 512 rows re-downloaded every few seconds,
//! ~97 times in fourteen minutes, indefinitely. The frontier could not serve
//! as the resume point either, because [`relay_cursor_advance`] deliberately
//! never moves backwards, so on a mailbox whose frontier is already at the top
//! it says nothing at all about how far *this sweep* has got.
//!
//! So a sweep now carries its own cursor, persisted beside the frontier:
//! `sweep_after_id`. It advances under exactly the frontier's rule (only on a
//! fully-processed page, never backwards), a sweep resumes from it instead of
//! from 0, and completing the sweep clears it. A sweep is therefore a walk
//! that survives yields, process death, and days of interruptions and still
//! finishes — instead of one that had to fit inside a single pass or never end
//! at all.
//!
//! A cursor that survives days is a cursor that can outlive the mailbox it
//! names, so it is dated too: [`relay_sweep_restart_from_zero`] throws away a
//! sweep still unfinished a whole [`RELAY_SWEEP_INTERVAL_MS`] after it began,
//! because a relay rebuilt in the meantime has restarted its row ids at 1 and
//! the remembered cursor now points past the end of everything.
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

/// Domain separation for [`relay_hint_source_digest`], distinct from every
/// other BLAKE2b context in the crate.
const RELAY_HINT_SOURCE_DIGEST_CONTEXT: &[u8] = b"cruisemesh relay hint source set v1";

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
///
/// Starting the process no longer forces a walk (see [`relay_sweep_due`]), so
/// this is the schedule rather than a floor under one. It is not the whole
/// story, and the exceptions are worth knowing before reading a sweep count as
/// a bug:
///
/// - It is **per mailbox**. A phone walks its own relay plus every distinct
///   relay its contacts' cards resolve to, each with its own cursor row and its
///   own clock, so four sweeps per mailbox per day is not four sweeps per day.
/// - A mailbox that has never been swept sweeps once per process, and a
///   timestamp from the future sweeps immediately (both in [`relay_sweep_due`]).
/// - A change to the hint source set re-walks every mailbox
///   ([`relay_hint_source_digest`]).
pub const RELAY_SWEEP_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;

/// [`RELAY_SWEEP_INTERVAL_MS`], for shells that cannot see the constant.
#[uniffi::export]
pub fn relay_sweep_interval_ms() -> i64 {
    RELAY_SWEEP_INTERVAL_MS
}

/// How many pages one pass may fetch from a single mailbox before yielding.
///
/// A relay mailbox can hold years of deliberately unacked proxy rows, and a
/// sweep — or a first walk after a restore, which has no frontier to resume
/// from — starts at 0 and would otherwise run thousands of sequential HTTP
/// round trips without ever letting go. Four pages is small enough that no
/// single mailbox can starve the others in a multi-relay pass, and large
/// enough that an ordinary frontier pass (which almost always ends on its
/// first or second page) never notices the budget exists.
pub const RELAY_MAILBOX_MAX_PAGES_PER_PASS: u32 = 4;

/// How many envelopes one pass may take from a single mailbox before
/// yielding.
///
/// The page budget bounds *requests*; this bounds *work*. relayd fills a page
/// up to a byte cap rather than a row count, so four pages can be four rows or
/// four hundred, and it is the envelope processing — unseal, dedupe, store,
/// ack — that costs the phone. 512 is comfortably more than a quiet mailbox
/// ever produces and well short of what a background pass can be killed for.
pub const RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS: u32 = 512;

/// How long to wait before resuming a walk that hit its budget.
///
/// Long enough to be a genuine yield — the pass ends, its thread and its
/// sockets are released, the rest of the mesh gets a turn — and short enough
/// that draining a deep mailbox is measured in minutes rather than in sweep
/// intervals.
pub const RELAY_MAILBOX_CONTINUATION_DELAY_MS: i64 = 1_000;

/// [`RELAY_MAILBOX_MAX_PAGES_PER_PASS`], for shells that cannot see the
/// constant.
#[uniffi::export]
pub fn relay_mailbox_max_pages_per_pass() -> u32 {
    RELAY_MAILBOX_MAX_PAGES_PER_PASS
}

/// [`RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS`], for shells that cannot see the
/// constant.
#[uniffi::export]
pub fn relay_mailbox_max_envelopes_per_pass() -> u32 {
    RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS
}

/// [`RELAY_MAILBOX_CONTINUATION_DELAY_MS`], for shells that cannot see the
/// constant.
#[uniffi::export]
pub fn relay_mailbox_continuation_delay_ms() -> i64 {
    RELAY_MAILBOX_CONTINUATION_DELAY_MS
}

/// What a walk should do once the current page is fully accounted for.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayMailboxWalkAction {
    /// Under budget: fetch the next page in this same pass.
    ///
    /// Deliberately not named `Continue`: that is a keyword in one of the two
    /// languages this enum is generated into, and an escaped case name is a
    /// worse thing to read than a slightly longer one.
    ContinueWalk,
    /// Out of budget: stop this mailbox's walk, persist what has been
    /// processed, and schedule a continuation in
    /// [`RELAY_MAILBOX_CONTINUATION_DELAY_MS`].
    ///
    /// The continuation is a whole new sync pass, which is why the resume
    /// point has to be *persisted* rather than held in a local: a sweep that
    /// yields here and restarts at 0 next pass is the livelock this module's
    /// second section describes.
    YieldAndScheduleContinuation,
}

/// Has this mailbox's walk used up its budget for this pass?
///
/// Called after each page is processed and its cursors are persisted, so a
/// yield never strands work: everything counted here has already reached a
/// terminal disposition and been written down.
#[uniffi::export]
pub fn relay_mailbox_walk_action(
    pages_fetched: u32,
    envelopes_fetched: u32,
) -> RelayMailboxWalkAction {
    if pages_fetched >= RELAY_MAILBOX_MAX_PAGES_PER_PASS
        || envelopes_fetched >= RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS
    {
        RelayMailboxWalkAction::YieldAndScheduleContinuation
    } else {
        RelayMailboxWalkAction::ContinueWalk
    }
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

/// A stable name for the *set of ids* this device's relay fetch hints are
/// derived from: our own user id, every group we are a member of, and every
/// contact we proxy-poll for.
///
/// This exists to solve a gap the frontier has. relayd's `next_cursor` is the
/// id of the last row matching *the hints you sent*, so an ordinary pass walks
/// the frontier straight past rows belonging to hints this device did not have
/// yet. Import a group and up to [`crate::CARRY_HINT_DAY_WINDOW_DAYS`] days of
/// that group's rows are already sitting below an advanced frontier: no
/// ordinary pass will ever ask for them again, and the sweep timestamp is
/// recent, so no valve in [`relay_sweep_due`] fires either. The mail is simply
/// invisible until the next scheduled sweep — which used to be minutes away on
/// a phone that restarts constantly, and is now up to
/// [`RELAY_SWEEP_INTERVAL_MS`].
///
/// So the id set gets a digest, and a change to it invalidates the frontier
/// (see `MessageStore::note_relay_hint_sources`). Digesting the *sources*
/// rather than the hints themselves is the whole trick: hints are day-salted
/// and rotate every UTC midnight, so hashing them would force a re-walk daily
/// for no reason, while the id set behind them only moves when a contact or
/// group membership actually changes.
///
/// Any change counts, not only a widening. A digest cannot tell an addition
/// from a removal, and buying that distinction would mean storing the id set
/// itself — a contact list, in a database we try to keep free of anything a
/// leak would enrich. Removing a contact therefore costs one extra re-walk,
/// which is a rare, user-initiated event and cheap besides.
///
/// Ids are sorted and length-framed before hashing, so the digest depends on
/// the set and not on row order or on where one id ends and the next begins.
#[uniffi::export]
pub fn relay_hint_source_digest(mut source_ids: Vec<Vec<u8>>) -> String {
    source_ids.sort();
    source_ids.dedup();
    let mut hasher = Blake2bVar::new(32).expect("valid blake2b output length");
    hasher.update(RELAY_HINT_SOURCE_DIGEST_CONTEXT);
    for id in &source_ids {
        hasher.update(&(id.len() as u64).to_be_bytes());
        hasher.update(id);
    }
    let mut out = [0u8; 32];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    BASE64URL_NOPAD.encode(&out)
}

/// Must this pass walk the whole mailbox from 0?
///
/// The answer comes from the *persisted* `last_sweep_at_ms`, cold start
/// included. It used to be an unconditional yes for the first pass of every
/// process, on the theory that a restart is a cheap moment to re-check
/// everything. On a phone it is not cheap and it is not occasional: the mesh
/// service is killed and restarted all day (Doze, swipe-away, memory
/// pressure), every restart forced a full walk, and a full walk re-downloads
/// the sealed body of every row still in the mailbox — including all the rows
/// left there on purpose, which is most of them. So the restart rate, not
/// [`RELAY_SWEEP_INTERVAL_MS`], was deciding how much data this app moved, and
/// a churny phone could sweep many times a day instead of four.
///
/// What that costs, stated plainly: a relay rebuilt from scratch, whose row
/// ids restart at 1 underneath a frontier we still remember, is no longer
/// repaired by restarting the app.
///
/// It is worth being precise about what "repaired" ever meant here, because it
/// is less than it sounds. [`relay_cursor_advance`] never moves the frontier
/// *backwards*, and a sweep only re-reads pages — it never lowers `after_id`.
/// So on a mailbox whose ids restarted under a frontier of, say, 29000, that
/// frontier stays at 29000 for good. Ordinary passes send `after=29000` and see
/// nothing; relayd's live push gates on the same client-supplied value, so the
/// socket is blind too. Only a sweep, which starts from 0, sees that mail. The
/// mailbox is therefore in permanent sweep-cadence delivery either way — this
/// change moves that cadence from minutes (one forced walk per app restart, and
/// phones restart all day) to up to [`RELAY_SWEEP_INTERVAL_MS`].
///
/// That is a real regression for one rare operator event, accepted against a
/// constant cost paid by every phone every day. Lowering the frontier when a
/// completed sweep proves the mailbox's ids have regressed would fix it
/// properly and is the obvious follow-up; nothing here forecloses it.
///
/// The sweep's own resume cursor could have made that case *worse* — a cursor
/// remembered from the old id space points past the end of a rebuilt mailbox,
/// so the resumed walk sees one empty page and records a sweep that covered
/// nothing at all. [`relay_sweep_restart_from_zero`] is what stops it: a sweep
/// still unfinished a whole interval after it began walks from 0 rather than
/// resuming, so the phone that was offline while the relay was rebuilt heals
/// on its first pass back, exactly as it did before the cursor existed.
///
/// Three valves stay open, because they are the states a stored sweep
/// timestamp genuinely cannot speak for:
///
/// - **A sweep already under way** (`sweep_progress_after_id > 0`) is due
///   until it finishes, whatever the timestamp says. A bounded walk
///   ([`relay_mailbox_walk_action`]) hands a deep mailbox back after a few
///   pages, and only the empty page at the end of the mailbox writes
///   `last_sweep_at`; so between the first yield and the last page the
///   timestamp still describes the *previous* sweep, and reading it alone
///   would abandon a half-finished walk — leaving `sweep_after_id` stranded
///   in the middle of the mailbox and the coverage it exists to provide
///   quietly incomplete. This branch is also what makes the resume cursor
///   safe to trust: progress is only ever read by a pass that is sweeping,
///   and a pass that finds progress is always sweeping.
///
/// - **Never swept** (`last_sweep_at_ms <= 0`) sweeps. This is also the
///   entire "heal promptly after an install" story and the reason no extra
///   cold-start grace period is warranted on top of it. A fresh install has
///   no `relay_fetch_cursors` row; a rotated token or a moved host hashes to a
///   different [`relay_cursor_key`], which has no row of its own. Both states
///   read as 0 here and sweep on their first pass. A grace period would buy
///   those cases nothing they don't already have, and would hand back a share
///   of exactly the restart-driven cost this rule exists to remove.
///   `swept_this_session` guards this branch alone, so a store write that
///   keeps failing costs one walk per process rather than one per pass.
/// - **A timestamp in the future** (a clock that jumped backwards, a restore
///   onto a phone set to a different time) sweeps immediately rather than
///   pinning the mailbox as un-swept until real time catches up — the same
///   rule [`crate::core_contact_relay_recheck_due`] applies for the same
///   reason. One completed sweep rewrites the timestamp to now, so it settles;
///   a sweep that never *finishes* does not, because both shells record
///   completion only on the empty page that ends the walk. A mailbox too large
///   to walk inside one service lifetime used to keep re-walking from 0 for
///   that reason; it no longer does, because the sweep resumes from
///   `sweep_after_id` and so converges on the empty page across as many passes
///   as it takes.
///
/// One case the stored timestamp cannot speak for is deliberately handled
/// elsewhere rather than by a valve here: gaining a contact or a group widens
/// the fetch-hint set, and the mail that arrives under a hint we did not have
/// yet sits *below* an already-advanced frontier where no sweep schedule can
/// help. `MessageStore::note_relay_hint_sources` invalidates the frontier
/// itself for that, which is the only thing that actually reaches those rows.
/// It leaves `last_sweep_at` strictly alone, and the first branch below is why
/// that matters: a zeroed timestamp reads as never-swept, and a process that
/// has already swept passes `swept_this_session: true`, so zeroing it here
/// would answer "not due" from then until the service restarted — a membership
/// change would quietly retire the schedule for the lifetime of the process.
/// It zeroes the sweep *progress* alongside the frontier, for the same reason
/// it zeroes the frontier — a partial sweep's coverage was computed against a
/// narrower hint set, so it no longer means what it claims — and zeroing
/// progress is safe there precisely because it does not force a sweep: it
/// lands back on the "no sweep under way" reading, and the frontier reset is
/// what actually re-walks the mailbox.
#[uniffi::export]
pub fn relay_sweep_due(
    swept_this_session: bool,
    last_sweep_at_ms: i64,
    sweep_progress_after_id: i64,
    now_ms: i64,
) -> bool {
    if sweep_progress_after_id > 0 {
        return true;
    }
    if last_sweep_at_ms <= 0 {
        return !swept_this_session;
    }
    if now_ms < last_sweep_at_ms {
        return true;
    }
    now_ms - last_sweep_at_ms >= RELAY_SWEEP_INTERVAL_MS
}

/// The `after=` this pass starts its walk at.
///
/// An ordinary pass resumes from the remembered frontier. A sweep resumes from
/// its own remembered progress: 0 the first time, and wherever the last
/// bounded pass left off after that.
///
/// The two cursors are separate on purpose and cannot be collapsed into one.
/// The frontier never moves backwards (see [`relay_cursor_advance`]), which is
/// what stops a sweep from costing an ordinary pass its position; that same
/// property makes it useless as a sweep's resume point, because on a mailbox
/// whose frontier already sits at the top it carries no information about how
/// far this sweep has walked. Reading the frontier as sweep progress would
/// skip the whole mailbox; reading 0 as sweep progress restarts the walk on
/// every yield, which is the livelock. Only a cursor belonging to the sweep
/// answers correctly.
///
/// A negative persisted value on either cursor (corrupt row, hand-edited
/// database) reads as 0 rather than being sent to a relay that would reject
/// it.
///
/// Progress is only trusted while it still describes a mailbox that exists;
/// [`relay_sweep_restart_from_zero`] is the guard, and the shell zeroes the
/// progress it passes here when that guard fires.
#[uniffi::export]
pub fn relay_pass_start_cursor(
    sweeping: bool,
    persisted_after_id: i64,
    sweep_progress_after_id: i64,
) -> i64 {
    if sweeping {
        return sweep_progress_after_id.max(0);
    }
    persisted_after_id.max(0)
}

/// Must this sweep throw its resume cursor away and walk from 0?
///
/// A resume cursor is a row id, and a row id only means anything inside the id
/// space it was recorded in. A relay rebuilt from scratch — a fresh volume, a
/// re-created mailbox — restarts its ids at 1, and a cursor remembered from
/// the old space then points *past the end* of the new mailbox. The resumed
/// walk fetches one page, gets nothing, and reads that as end-of-mailbox: it
/// records a completed sweep that covered no rows at all, stamps
/// `last_sweep_at`, and hands the mailbox back to the schedule for a fresh
/// [`RELAY_SWEEP_INTERVAL_MS`] while real mail sits below the frontier
/// unreachable by any ordinary pass. That is the one case where resuming is
/// worse than restarting, and it is worth saying plainly that a sweep that
/// starts at 0 has never had it.
///
/// The tempting fix — treat *any* empty first page after a resume as
/// suspicious and re-walk from 0 — is a livelock in disguise. A sweep yields
/// on a fixed budget, so roughly one sweep in four yields exactly at the end
/// of the mailbox; the next pass then resumes to a genuinely empty page, would
/// restart at 0, would re-walk the whole mailbox, and would land on the same
/// budget boundary again. That is the loop this module exists to remove, with
/// a longer period.
///
/// So the question asked here is not "was the page empty" but "could the
/// mailbox have been replaced since this sweep started". A sweep that yielded
/// a second ago and resumes into an empty page is simply finished. A sweep
/// still holding progress a whole [`RELAY_SWEEP_INTERVAL_MS`] after it began —
/// a phone that went offline mid-walk and came back days later, which is the
/// ordinary shape of this app's life, and the window in which an operator
/// rebuilds a relay — has a cursor no one should trust. It re-walks, once, and
/// the walk it starts is dated so the next pass resumes it normally instead of
/// restarting it again.
///
/// Two more readings of the timestamp, both of them "I cannot date this
/// sweep, so I will not trust it":
///
/// - progress with no start date at all. Only a store upgraded across the
///   column's introduction can produce it, and only for a sweep that was
///   part-way through at the time.
/// - a start date in the future (a clock that jumped backwards, a restore onto
///   a phone set to another time), which the rest of this module treats the
///   same way for the same reason.
///
/// Progress of 0 is never restarted: there is nothing to throw away, the walk
/// already starts at 0, and answering `true` there would make every sweep
/// re-walk before it had walked at all.
#[uniffi::export]
pub fn relay_sweep_restart_from_zero(
    sweep_progress_after_id: i64,
    sweep_started_at_ms: i64,
    now_ms: i64,
) -> bool {
    if sweep_progress_after_id <= 0 {
        return false;
    }
    if sweep_started_at_ms <= 0 || now_ms < sweep_started_at_ms {
        return true;
    }
    now_ms - sweep_started_at_ms >= RELAY_SWEEP_INTERVAL_MS
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
///
/// The same function decides the sweep's own `sweep_after_id`
/// (`MessageStore::advance_relay_sweep_cursor`), because the rule is the same
/// rule: a page that did not finish must be presented again, and a cursor that
/// could slip backwards would re-walk ground the sweep had already covered.
/// The only difference is who clears it — a sweep's progress is reset to 0 by
/// completion (`note_relay_sweep_completed`) and by a hint-set change
/// (`note_relay_hint_sources`), which are explicit writes rather than
/// anything this function can express.
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
    fn a_hint_source_digest_names_the_set_not_the_listing_order() {
        let a = vec![1u8; 32];
        let b = vec![2u8; 32];
        let c = vec![3u8; 32];
        assert_eq!(
            relay_hint_source_digest(vec![a.clone(), b.clone(), c.clone()]),
            relay_hint_source_digest(vec![c.clone(), a.clone(), b.clone()])
        );
        // A contact listed twice (it is also a group member, say) is one id.
        assert_eq!(
            relay_hint_source_digest(vec![a.clone(), b.clone()]),
            relay_hint_source_digest(vec![a.clone(), b.clone(), a.clone()])
        );
    }

    #[test]
    fn gaining_or_losing_a_hint_source_changes_the_digest() {
        let own = vec![1u8; 32];
        let contact = vec![2u8; 32];
        let alone = relay_hint_source_digest(vec![own.clone()]);
        let together = relay_hint_source_digest(vec![own.clone(), contact.clone()]);
        assert_ne!(alone, together);
        // Symmetric: losing one lands back on the earlier digest, which is why
        // a removal costs the same single re-walk an addition does.
        assert_eq!(alone, relay_hint_source_digest(vec![own]));
        assert_ne!(together, relay_hint_source_digest(vec![contact]));
    }

    #[test]
    fn ids_are_framed_so_a_boundary_cannot_be_moved_unnoticed() {
        // Without length framing, [0xAA, 0xBBCC] and [0xAABB, 0xCC] would hash
        // the same bytes in the same order and collide. Ids are all 32 bytes
        // today, so this pins the framing rather than a live bug.
        assert_ne!(
            relay_hint_source_digest(vec![vec![0xAA], vec![0xBB, 0xCC]]),
            relay_hint_source_digest(vec![vec![0xAA, 0xBB], vec![0xCC]])
        );
    }

    #[test]
    fn an_empty_source_set_still_has_a_digest() {
        // Not reachable in practice -- our own id is always in the set -- but
        // the digest must be total, since the store compares it unconditionally.
        assert!(!relay_hint_source_digest(Vec::new()).is_empty());
    }

    #[test]
    fn a_cold_start_with_a_recent_sweep_does_not_sweep_again() {
        // The regression this rule exists for. The mesh service is killed and
        // restarted all day; if every restart re-walked the mailbox, the
        // restart rate would set the bandwidth bill and the interval would
        // mean nothing.
        let swept_at = 1_000_000i64;
        assert!(!relay_sweep_due(false, swept_at, 0, swept_at));
        assert!(!relay_sweep_due(false, swept_at, 0, swept_at + 1));
        assert!(!relay_sweep_due(
            false,
            swept_at,
            0,
            swept_at + RELAY_SWEEP_INTERVAL_MS - 1
        ));
    }

    #[test]
    fn a_cold_start_with_a_stale_sweep_still_sweeps() {
        let swept_at = 1_000_000i64;
        assert!(relay_sweep_due(
            false,
            swept_at,
            0,
            swept_at + RELAY_SWEEP_INTERVAL_MS
        ));
        assert!(relay_sweep_due(
            false,
            swept_at,
            0,
            swept_at + 10 * RELAY_SWEEP_INTERVAL_MS
        ));
    }

    #[test]
    fn later_passes_sweep_only_once_the_interval_has_elapsed() {
        let swept_at = 1_000_000i64;
        assert!(!relay_sweep_due(true, swept_at, 0, swept_at));
        assert!(!relay_sweep_due(
            true,
            swept_at,
            0,
            swept_at + RELAY_SWEEP_INTERVAL_MS - 1
        ));
        assert!(relay_sweep_due(
            true,
            swept_at,
            0,
            swept_at + RELAY_SWEEP_INTERVAL_MS
        ));
        assert_eq!(relay_sweep_interval_ms(), 6 * 60 * 60 * 1000);
    }

    #[test]
    fn a_mailbox_never_swept_sweeps_on_its_first_pass() {
        // Fresh install, a rotated token, or a moved host reads as 0 here and
        // must walk from the beginning. This is the promptness a
        // cold-start grace period would otherwise have had to provide.
        assert!(relay_sweep_due(false, 0, 0, 5_000));
        assert!(relay_sweep_due(false, -1, 0, 5_000));
        // ...but only once per process. A store write that keeps failing must
        // not turn every single pass into a full walk.
        assert!(!relay_sweep_due(true, 0, 0, 5_000));
        assert!(!relay_sweep_due(true, -1, 0, 5_000));
    }

    #[test]
    fn a_backwards_clock_sweeps_rather_than_pinning_the_mailbox() {
        // A timestamp in the future, from either side of a restart. Recording
        // the sweep rewrites it to now, so this resolves in one pass.
        assert!(relay_sweep_due(true, 5_000_000, 0, 1_000));
        assert!(relay_sweep_due(false, 5_000_000, 0, 1_000));
        assert!(relay_sweep_due(false, i64::MAX, 0, 1_000));
    }

    /// The livelock, stated as a rule: a sweep that yielded mid-mailbox is
    /// still due, so the next pass finishes it rather than treating the
    /// mailbox as swept and abandoning `sweep_after_id` in the middle.
    #[test]
    fn a_sweep_left_half_finished_stays_due() {
        let swept_at = 1_000_000i64;
        // Timestamp says "swept an hour ago"; progress says "no it wasn't".
        assert!(relay_sweep_due(true, swept_at, 512, swept_at + 3_600_000));
        assert!(relay_sweep_due(false, swept_at, 512, swept_at + 1));
        // ...and once the empty page clears progress, the schedule takes over
        // again. This is the pairing that ends the loop: progress is the only
        // thing that keeps a sweep due, and completion is the only thing that
        // clears it.
        assert!(!relay_sweep_due(true, swept_at, 0, swept_at + 3_600_000));
    }

    #[test]
    fn a_sweep_resumes_from_its_progress_and_a_normal_pass_from_the_frontier() {
        // The regression. A sweep that yielded at 512 must not start at 0
        // again -- that re-downloads the same first pages every continuation,
        // forever, and never reaches the empty page that completes it.
        assert_eq!(relay_pass_start_cursor(true, 29_000, 512), 512);
        assert_eq!(relay_pass_start_cursor(true, 29_000, 0), 0);
        // A sweep never reads the frontier: on a mailbox already walked to the
        // top, the frontier would skip everything the sweep exists to re-see.
        assert_eq!(relay_pass_start_cursor(true, 29_000, 16), 16);
        // An ordinary pass never reads sweep progress, for the mirror-image
        // reason: it would re-fetch everything above the progress point.
        assert_eq!(relay_pass_start_cursor(false, 9_000, 512), 9_000);
        assert_eq!(relay_pass_start_cursor(false, 0, 512), 0);
        // Corrupt/hand-edited row: never send a negative `after`.
        assert_eq!(relay_pass_start_cursor(false, -5, 0), 0);
        assert_eq!(relay_pass_start_cursor(true, 0, -5), 0);
    }

    /// The rebuilt-relay hole the resume cursor would otherwise have opened: a
    /// phone offline for days comes back to a mailbox whose ids restarted at 1,
    /// resumes at a cursor from the old id space, and would take the empty page
    /// that produces as proof the mailbox was swept.
    #[test]
    fn a_sweep_stalled_since_before_the_last_interval_walks_from_zero_again() {
        let started = 1_000_000i64;
        // Offline for days: the sweep has been "in progress" the whole time.
        assert!(relay_sweep_restart_from_zero(
            20_000,
            started,
            started + 3 * RELAY_SWEEP_INTERVAL_MS
        ));
        assert!(relay_sweep_restart_from_zero(
            20_000,
            started,
            started + RELAY_SWEEP_INTERVAL_MS
        ));
        // Undatable progress (a store upgraded mid-sweep) is not trusted
        // either, and neither is a start date in the future.
        assert!(relay_sweep_restart_from_zero(20_000, 0, started));
        assert!(relay_sweep_restart_from_zero(20_000, started + 1, started));
    }

    /// The other half, and the reason this is a staleness question rather than
    /// an empty-page question: a sweep yields on a fixed budget, so about one
    /// sweep in four yields exactly at the end of the mailbox and resumes into
    /// a perfectly honest empty page. Restarting *that* at 0 would re-walk the
    /// whole mailbox and land on the same boundary again — the livelock this
    /// module exists to remove, with a longer period.
    #[test]
    fn a_sweep_that_yielded_moments_ago_resumes_rather_than_restarting() {
        let started = 1_000_000i64;
        assert!(!relay_sweep_restart_from_zero(20_000, started, started));
        assert!(!relay_sweep_restart_from_zero(
            20_000,
            started,
            started + 1_000
        ));
        assert!(!relay_sweep_restart_from_zero(
            20_000,
            started,
            started + RELAY_SWEEP_INTERVAL_MS - 1
        ));
        // Nothing to restart: a sweep at 0 already starts at 0, and answering
        // yes here would make every sweep re-walk before it had walked.
        assert!(!relay_sweep_restart_from_zero(0, 0, started));
        assert!(!relay_sweep_restart_from_zero(
            0,
            started,
            started + 10 * RELAY_SWEEP_INTERVAL_MS
        ));
    }

    /// A restart is dated, so it cannot repeat: the walk it begins is fresh,
    /// and the pass after it resumes normally.
    #[test]
    fn a_restarted_sweep_is_not_restarted_again_next_pass() {
        let restarted_at = 5_000_000i64;
        // The shell zeroes progress and stamps the restart, then walks from 0.
        assert_eq!(relay_pass_start_cursor(true, 29_000, 0), 0);
        // A second later, holding progress from that walk.
        assert!(!relay_sweep_restart_from_zero(
            512,
            restarted_at,
            restarted_at + 1_000
        ));
        assert_eq!(relay_pass_start_cursor(true, 29_000, 512), 512);
    }

    #[test]
    fn a_walk_yields_once_it_runs_out_of_pages_or_envelopes() {
        assert_eq!(
            relay_mailbox_walk_action(0, 0),
            RelayMailboxWalkAction::ContinueWalk
        );
        assert_eq!(
            relay_mailbox_walk_action(RELAY_MAILBOX_MAX_PAGES_PER_PASS - 1, 12),
            RelayMailboxWalkAction::ContinueWalk
        );
        assert_eq!(
            relay_mailbox_walk_action(RELAY_MAILBOX_MAX_PAGES_PER_PASS, 12),
            RelayMailboxWalkAction::YieldAndScheduleContinuation
        );
        // Either budget alone ends the pass: relayd fills a page to a byte cap,
        // so two pages can be worth more work than four.
        assert_eq!(
            relay_mailbox_walk_action(2, RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS),
            RelayMailboxWalkAction::YieldAndScheduleContinuation
        );
        assert_eq!(
            relay_mailbox_walk_action(2, RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS - 1),
            RelayMailboxWalkAction::ContinueWalk
        );
    }

    #[test]
    fn the_budget_constants_are_visible_to_both_shells() {
        assert_eq!(relay_mailbox_max_pages_per_pass(), 4);
        assert_eq!(relay_mailbox_max_envelopes_per_pass(), 512);
        assert_eq!(relay_mailbox_continuation_delay_ms(), 1_000);
    }

    /// End to end, in the shape a shell runs it: a mailbox deeper than one
    /// pass's budget is swept across several continuations and then completes,
    /// instead of re-reading its first pages forever.
    #[test]
    fn a_bounded_sweep_converges_across_continuations() {
        let swept_at = 1_000_000i64;
        let now = swept_at + RELAY_SWEEP_INTERVAL_MS;
        // Ten pages of 128 rows, then the empty page.
        let mailbox_end = 1_280i64;
        let mut progress = 0i64;
        let mut sweep_started = 0i64; // stamped by the sweep's first page
        let mut frontier = 29_000i64; // already at the top; useless to a sweep
        let mut passes = 0i64;
        loop {
            passes += 1;
            assert!(passes < 20, "the walk must terminate, not livelock");
            // A continuation a second after the last yield.
            let pass_now = now + passes * RELAY_MAILBOX_CONTINUATION_DELAY_MS;
            assert!(relay_sweep_due(true, swept_at, progress, pass_now));
            // Nothing here is stale: this sweep is minutes old at most, so the
            // rebuilt-relay guard must stay out of the way rather than
            // restarting the walk it is watching.
            assert!(!relay_sweep_restart_from_zero(
                progress,
                sweep_started,
                pass_now
            ));
            let mut after = relay_pass_start_cursor(true, frontier, progress);
            let mut pages = 0u32;
            let mut envelopes = 0u32;
            loop {
                if after >= mailbox_end {
                    // The empty page: sweep complete, progress and the date
                    // that goes with it cleared.
                    progress = 0;
                    assert!(!relay_sweep_due(true, pass_now, progress, pass_now));
                    assert_eq!(passes, 3, "4 pages a pass, 10 pages of mailbox");
                    return;
                }
                let next = after + 128;
                pages += 1;
                envelopes += 128;
                if progress == 0 {
                    sweep_started = pass_now;
                }
                progress = relay_cursor_advance(progress, next, true);
                frontier = relay_cursor_advance(frontier, next, true);
                assert!(relay_fetch_walk_continues(128, after, next));
                after = next;
                if relay_mailbox_walk_action(pages, envelopes)
                    == RelayMailboxWalkAction::YieldAndScheduleContinuation
                {
                    break;
                }
            }
            // Every yield leaves the sweep strictly further along than the
            // last one did. Without a resume cursor this is where it went
            // back to 0.
            assert_eq!(progress, passes * 512);
            assert_eq!(frontier, 29_000, "a sweep never costs the frontier");
        }
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
