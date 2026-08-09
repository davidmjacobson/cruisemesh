//! The read-only planner the migration canary compares against.
//!
//! # What this is for
//!
//! While the legacy relay engine is still the one moving mail, there is no
//! way to find out whether the core engine would have agreed with it except
//! by asking. Running both for real is not an option — two engines posting
//! the same envelope is duplicate external I/O, and two engines marking the
//! same row is two writers to one production table. So the canary asks the
//! only safe question available: *given exactly what the legacy engine saw,
//! what would core have done?*
//!
//! Everything in this module is a pure function over captured values. There
//! is no [`crate::store::MessageStore`] here, no endpoint, no socket, no
//! clock read: a capture is taken by the shell after the legacy pass has
//! finished, handed here, and compared. That is not a convention — it is why
//! the entry point is a free function rather than a method on the store. A
//! planner holding a live store handle could write, and a planner that could
//! write would eventually be the second writer this whole mechanism exists to
//! rule out.
//!
//! # The slice
//!
//! Receipts and locally authored rows only, which is the slice the Android
//! adapter migrates first. Carried rows, presence, announce and the mailbox
//! walk are later packages and are not captured; a capture says how many rows
//! it could not speak for ([`CoreRelayShadowCapture::rows_unshadowed`]) so a
//! clean report is never mistaken for a claim about rows nobody looked at.
//!
//! # What is compared, and why each axis is a real question
//!
//! | Axis | The question |
//! |---|---|
//! | [`CoreRelayShadowMismatchKind::LaneOrderDiffers`] | Did receipts really go before authored rows? A receipt is small and unblocks a peer's queue; the ordering is load-bearing rather than incidental. |
//! | [`CoreRelayShadowMismatchKind::DestinationDiffers`] | Did both engines resolve the same mailbox for this row? A misroute is not a retry — the posted marker is terminal. |
//! | [`CoreRelayShadowMismatchKind::RequestNotConstructible`] | Could core even form the request the legacy engine sent? A row core silently skips is a row that never leaves the queue. |
//! | [`CoreRelayShadowMismatchKind::FaultConsequenceDiffers`] | Did a failure cost the same thing? One engine abandoning a lane the other continues is a throughput difference nobody would see in a status code. |
//! | [`CoreRelayShadowMismatchKind::SuccessMarkingDiffers`] | Did the same answer durably retire the same row? Marking too eagerly loses mail; marking too late re-posts it forever. |
//! | [`CoreRelayShadowMismatchKind::SelectionSkipDiffers`] | Did one engine decline to post for a recipient the other would have posted for? |
//!
//! The destination and request axes are answered by calling the *same*
//! code the real pass calls ([`crate::session::relay_pass::shadow_upload_endpoint_for`]
//! resolves the mailbox; [`crate::session::relay_pass::shadow_upload_encodable`]
//! asks the one envelope validator that `relay_encode_post_envelope` itself
//! runs), not by a second implementation of them. A canary that compared
//! against a copy would be testing the copy.
//!
//! # Secrets
//!
//! A capture carries tokens, because resolving a destination needs the
//! credential. It does not carry payloads: a row is described by the *size*
//! of its sealed body, which is all the encodability question needs. A
//! [`CoreRelayShadowReport`] carries neither: every field in it is a count or
//! an enum, and [`CoreRelayShadowMismatch`] has no free-text field at all. The
//! report is the only thing that reaches the event ring, so `SECRET-01` holds
//! by the shape of the type rather than by care at the call site.
//!
//! # What a comparison may cost
//!
//! Two bounds, both enforced here rather than by a shell, because a bound a
//! shell applies is a bound the other shell can forget. A capture is clamped
//! to [`RELAY_SHADOW_MAX_ROWS`] rows and [`RELAY_SHADOW_MAX_SKIPS`] skipped
//! recipients before anything is compared, and the report names at most one
//! mismatch per kind, carrying how many rows showed it. A device diverging
//! systematically — which is what the currently open divergences guarantee —
//! therefore costs the protocol event ring a fixed handful of records per
//! sample instead of one per row, and cannot evict the operational evidence
//! the ring exists to carry.

use crate::relay_status::{relay_classify_http_error, CoreRelayFault};
use crate::session::relay_pass::{
    shadow_upload_encodable, shadow_upload_endpoint_for, CoreRelayContactConfig,
    CoreRelayEndpointConfig, CoreRelayTransportError,
};

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Sampled passes per day. Small on purpose: a mismatch that only ever
/// happens on one pass in a thousand is not the class of defect this is
/// looking for, and a canary that costs a person battery is one they turn
/// off.
///
/// "Day" is the UTC calendar day the clock reads, not a rolling twenty-four
/// hours, so a window straddling midnight can hold up to twice this. That is
/// the cheaper of the two to state honestly: a rolling window needs the
/// timestamps of every sample kept, and the number that matters here is the
/// order of magnitude.
pub const RELAY_SHADOW_MAX_SAMPLES_PER_DAY: u32 = 12;

/// The quiet time between two samples. Relay passes arrive in bursts — a push
/// frame, a queue change and a poll tick can all land inside a second — and
/// sampling a burst would spend the whole day's budget on one minute of
/// evidence.
pub const RELAY_SHADOW_MIN_INTERVAL_MS: i64 = 15 * 60 * 1_000;

/// Rows one sampled pass may capture. A shell stops recording at this many so
/// it never holds more, and [`core_relay_shadow_compare`] clamps to it again
/// so a shell that did not is still bounded here.
pub const RELAY_SHADOW_MAX_ROWS: u32 = 16;

/// Skipped recipients one sampled pass may report. A family is small; a list
/// longer than this is a bug or a device with a very long contact list, and
/// neither is worth a proportional number of diagnostics records.
pub const RELAY_SHADOW_MAX_SKIPS: u32 = 32;

/// [`RELAY_SHADOW_MAX_ROWS`], for a shell. A `const` does not cross UniFFI,
/// and a shell that wrote the number down itself would be a second place it is
/// decided — the exact shape this program exists to remove.
#[uniffi::export]
pub fn core_relay_shadow_max_rows() -> u32 {
    RELAY_SHADOW_MAX_ROWS
}

/// [`RELAY_SHADOW_MAX_SKIPS`], for a shell, for the same reason.
#[uniffi::export]
pub fn core_relay_shadow_max_skips() -> u32 {
    RELAY_SHADOW_MAX_SKIPS
}

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;

/// What a device remembers between sampling decisions.
///
/// Deliberately a value the shell holds rather than state this module keeps:
/// the shell decides whether that is a field on a service or a row in its
/// preferences, and nothing here has to be told about a process restart.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CoreRelayShadowSampler {
    /// Whole days since the epoch, as counted at the last sample.
    pub day_index: i64,
    pub samples_today: u32,
    pub last_sample_at_ms: i64,
}

/// The decision, and the state to keep for the next one.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowSample {
    pub sample: bool,
    pub next: CoreRelayShadowSampler,
}

/// Whether this pass is one of the sampled ones.
///
/// Both bounds are enforced here rather than by the caller so the two shells
/// cannot drift on what "bounded per day" means. A clock that steps backwards
/// is treated as a new day rather than as a licence to sample freely: the day
/// index is compared for inequality, not for order.
#[uniffi::export]
pub fn core_relay_shadow_sample(
    state: CoreRelayShadowSampler,
    now_ms: i64,
) -> CoreRelayShadowSample {
    let day_index = now_ms.div_euclid(MS_PER_DAY);
    let fresh_day = day_index != state.day_index;
    let samples_today = if fresh_day { 0 } else { state.samples_today };

    let quiet_enough = fresh_day
        || state.last_sample_at_ms == 0
        || now_ms.saturating_sub(state.last_sample_at_ms) >= RELAY_SHADOW_MIN_INTERVAL_MS
        // A backwards clock must not be able to hold the sampler shut
        // forever, so a reading before the last sample also opens the gate.
        || now_ms < state.last_sample_at_ms;

    let sample = quiet_enough && samples_today < RELAY_SHADOW_MAX_SAMPLES_PER_DAY;
    CoreRelayShadowSample {
        sample,
        next: CoreRelayShadowSampler {
            day_index,
            samples_today: if sample {
                samples_today.saturating_add(1)
            } else {
                samples_today
            },
            last_sample_at_ms: if sample {
                now_ms
            } else {
                state.last_sample_at_ms
            },
        },
    }
}

// ---------------------------------------------------------------------------
// The capture
// ---------------------------------------------------------------------------

/// Which upload lane a captured row came from. Only the two this package
/// migrates; carried rows belong to C3 and have no shadow yet.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayShadowLane {
    Receipt,
    Authored,
}

/// One row the legacy engine handled, and what happened to it.
///
/// These are *observations*, not instructions: nothing here is acted on, and
/// there is no payload — [`Self::sealed_len`] is the size of the sealed body,
/// which is the whole of what "could core have encoded this row" turns on.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowStep {
    pub lane: CoreRelayShadowLane,
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub recipient_hint: Vec<u8>,
    /// Who the row is addressed to, which is what a destination is resolved
    /// from.
    pub recipient_user_id: Vec<u8>,
    /// How many bytes the sealed payload was. The bytes themselves are
    /// deliberately not captured: sixteen half-megabyte rows held whole,
    /// copied across the language boundary and cloned again to build a body
    /// that is immediately thrown away is tens of megabytes on a phone, for a
    /// question about a length.
    pub sealed_len: u64,
    pub expiry_ms: i64,
    /// The mailbox the legacy engine resolved for this row, or `None` when it
    /// declined to post at all.
    pub legacy_endpoint: Option<CoreRelayEndpointConfig>,
    /// The HTTP status the legacy engine observed, or 0 when there was none.
    pub status: u16,
    /// The relay's own stable error code, when the body carried one.
    pub relay_code: Option<String>,
    pub transport_error: Option<CoreRelayTransportError>,
    /// Whether the legacy engine durably retired the row.
    pub legacy_marked_posted: bool,
    /// Whether the legacy engine went on to offer the next row in this lane
    /// to the same mailbox after this one failed.
    pub legacy_continued_lane: bool,
}

/// Everything one sampled legacy pass is asked to remember.
///
/// Both lists are clamped by [`core_relay_shadow_compare`] before anything is
/// read, so an over-long capture costs a truncated comparison rather than an
/// unbounded one.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowCapture {
    pub own: Option<CoreRelayEndpointConfig>,
    pub contacts: Vec<CoreRelayContactConfig>,
    /// In the order the legacy engine issued them.
    pub steps: Vec<CoreRelayShadowStep>,
    /// Recipients the legacy engine excluded from its own queue query before
    /// any row was selected.
    pub skipped_recipients: Vec<Vec<u8>>,
    /// Rows this capture deliberately cannot speak for — group fan-out rows
    /// and carried rows, which later packages own. Reported so a clean run is
    /// never read as a claim about them.
    pub rows_unshadowed: u32,
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// The ways the two engines can disagree about this slice. See the table in
/// the module docs for what each one is asking.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayShadowMismatchKind {
    LaneOrderDiffers,
    DestinationDiffers,
    RequestNotConstructible,
    FaultConsequenceDiffers,
    SuccessMarkingDiffers,
    SelectionSkipDiffers,
}

impl CoreRelayShadowMismatchKind {
    /// The stable token this kind is recorded under. Event outcomes are API;
    /// these names may not drift.
    pub const fn as_token(self) -> &'static str {
        match self {
            CoreRelayShadowMismatchKind::LaneOrderDiffers => "shadow_lane_order_differs",
            CoreRelayShadowMismatchKind::DestinationDiffers => "shadow_destination_differs",
            CoreRelayShadowMismatchKind::RequestNotConstructible => {
                "shadow_request_not_constructible"
            }
            CoreRelayShadowMismatchKind::FaultConsequenceDiffers => {
                "shadow_fault_consequence_differs"
            }
            CoreRelayShadowMismatchKind::SuccessMarkingDiffers => "shadow_success_marking_differs",
            CoreRelayShadowMismatchKind::SelectionSkipDiffers => "shadow_selection_skip_differs",
        }
    }
}

/// One *kind* of disagreement, with how many rows showed it.
///
/// Deliberately not one entry per row. The divergences a canary finds are
/// overwhelmingly systematic — a rule one engine applies and the other does
/// not shows up on every row it touches — so a per-row list is the same fact
/// repeated at the cost of the diagnostics ring, where every record it writes
/// evicts an older one carrying something nobody else recorded. A kind, the
/// first place it was seen, and a count answer every question a per-row list
/// would have, in a bounded number of records.
///
/// There is no field here that could hold a message, an endpoint or a
/// credential, and that is the whole design: this is the type that reaches
/// the event ring.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowMismatch {
    pub kind: CoreRelayShadowMismatchKind,
    /// The first index that showed this kind: into
    /// [`CoreRelayShadowCapture::steps`], or into
    /// [`CoreRelayShadowCapture::skipped_recipients`] for a
    /// [`CoreRelayShadowMismatchKind::SelectionSkipDiffers`].
    pub first_index: u32,
    /// How many rows (or skipped recipients) showed it. Never zero.
    pub rows: u32,
}

/// What one comparison found. Counts and enums only.
///
/// [`Self::mismatches`] holds at most one entry per
/// [`CoreRelayShadowMismatchKind`], so a report is bounded by the number of
/// kinds however badly a device diverges.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowReport {
    pub steps_compared: u32,
    pub rows_unshadowed: u32,
    pub skips_compared: u32,
    /// Rows and skipped recipients the capture carried past the caps and this
    /// comparison therefore did not look at. Counted rather than dropped, for
    /// the same reason as [`CoreRelayShadowCapture::rows_unshadowed`].
    pub rows_truncated: u32,
    pub mismatches: Vec<CoreRelayShadowMismatch>,
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// Compare one captured legacy pass against what core would have planned.
///
/// Pure: no store, no clock, no network. Called after the legacy pass has
/// already finished, so nothing it returns can change what that pass did.
///
/// The capture is clamped to [`RELAY_SHADOW_MAX_ROWS`] and
/// [`RELAY_SHADOW_MAX_SKIPS`] here, so the work and the report are bounded by
/// this function rather than by whichever shell built the capture.
#[uniffi::export]
pub fn core_relay_shadow_compare(capture: CoreRelayShadowCapture) -> CoreRelayShadowReport {
    let mut found = MismatchTally::default();
    let mut seen_authored = false;

    let step_limit = (RELAY_SHADOW_MAX_ROWS as usize).min(capture.steps.len());
    let skip_limit = (RELAY_SHADOW_MAX_SKIPS as usize).min(capture.skipped_recipients.len());
    let rows_truncated =
        (capture.steps.len() - step_limit) + (capture.skipped_recipients.len() - skip_limit);

    for (index, step) in capture.steps.iter().take(step_limit).enumerate() {
        let index = index as u32;

        // Fairness order. Core runs the whole receipt lane before the first
        // authored row, so a receipt appearing after an authored row is the
        // starvation this ordering exists to prevent.
        match step.lane {
            CoreRelayShadowLane::Authored => seen_authored = true,
            CoreRelayShadowLane::Receipt => {
                if seen_authored {
                    found.note(CoreRelayShadowMismatchKind::LaneOrderDiffers, index);
                }
            }
        }

        let planned = shadow_upload_endpoint_for(
            &capture.contacts,
            capture.own.as_ref(),
            &step.recipient_user_id,
        );
        let legacy = step.legacy_endpoint.as_ref();
        let agrees = match (&planned, legacy) {
            (Some(planned), Some(legacy)) => {
                planned.url == legacy.url && planned.token == legacy.token
            }
            (None, None) => true,
            _ => false,
        };
        if !agrees {
            found.note(CoreRelayShadowMismatchKind::DestinationDiffers, index);
        }

        // Only a row the legacy engine actually posted can be asked whether
        // core could have posted it: a row neither engine sends is not a
        // request-formation question.
        if legacy.is_some() {
            let constructible = planned.is_some()
                && shadow_upload_encodable(&step.msg_id, &step.recipient_hint, step.sealed_len);
            if !constructible {
                found.note(CoreRelayShadowMismatchKind::RequestNotConstructible, index);
            }
        }

        let succeeded = step.transport_error.is_none() && (200..300).contains(&step.status);
        if succeeded != step.legacy_marked_posted && legacy.is_some() {
            found.note(CoreRelayShadowMismatchKind::SuccessMarkingDiffers, index);
        }

        if legacy.is_some() && !succeeded && core_continues_lane(step) != step.legacy_continued_lane
        {
            found.note(CoreRelayShadowMismatchKind::FaultConsequenceDiffers, index);
        }
    }

    for (index, recipient) in capture
        .skipped_recipients
        .iter()
        .take(skip_limit)
        .enumerate()
    {
        if shadow_upload_endpoint_for(&capture.contacts, capture.own.as_ref(), recipient).is_some()
        {
            found.note(
                CoreRelayShadowMismatchKind::SelectionSkipDiffers,
                index as u32,
            );
        }
    }

    CoreRelayShadowReport {
        steps_compared: step_limit as u32,
        rows_unshadowed: capture.rows_unshadowed,
        skips_compared: skip_limit as u32,
        rows_truncated: rows_truncated as u32,
        mismatches: found.into_vec(),
    }
}

/// One entry per kind, in the order the kinds were first seen.
///
/// A `Vec` rather than a map because there are six kinds: the linear scan is
/// cheaper than hashing, and the insertion order is what makes a report read
/// in the order a person watching the pass would have seen the trouble.
#[derive(Default)]
struct MismatchTally {
    entries: Vec<CoreRelayShadowMismatch>,
}

impl MismatchTally {
    fn note(&mut self, kind: CoreRelayShadowMismatchKind, index: u32) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.kind == kind) {
            entry.rows = entry.rows.saturating_add(1);
            return;
        }
        self.entries.push(CoreRelayShadowMismatch {
            kind,
            first_index: index,
            rows: 1,
        });
    }

    fn into_vec(self) -> Vec<CoreRelayShadowMismatch> {
        self.entries
    }
}

/// Whether the core engine would offer the next row of this lane to the same
/// mailbox after this failure.
///
/// The rule is `relay_pass`'s: a `413` (too large) and a `409` (msg_id
/// conflict) are terminal for one row and say nothing about the mailbox, so the
/// lane continues; everything else is evidence about the mailbox, so the lane
/// stops spending on it. A family `429` ends the pass outright, which is a
/// stronger form of the same answer.
fn core_continues_lane(step: &CoreRelayShadowStep) -> bool {
    if step.transport_error.is_some() {
        return false;
    }
    matches!(
        relay_classify_http_error(step.status, step.relay_code.clone()),
        CoreRelayFault::MessageTooLarge | CoreRelayFault::MsgIdConflict
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(url: &str, token: &str) -> CoreRelayEndpointConfig {
        CoreRelayEndpointConfig {
            url: url.to_string(),
            token: token.to_string(),
        }
    }

    fn step(lane: CoreRelayShadowLane, recipient: &[u8]) -> CoreRelayShadowStep {
        CoreRelayShadowStep {
            lane,
            msg_id: vec![1u8; 16],
            hop_ttl: 3,
            recipient_hint: vec![2u8; 8],
            recipient_user_id: recipient.to_vec(),
            sealed_len: 64,
            expiry_ms: 1_000,
            legacy_endpoint: Some(endpoint("https://relay.example", "member-token")),
            status: 200,
            relay_code: None,
            transport_error: None,
            legacy_marked_posted: true,
            legacy_continued_lane: true,
        }
    }

    fn capture(steps: Vec<CoreRelayShadowStep>) -> CoreRelayShadowCapture {
        CoreRelayShadowCapture {
            own: Some(endpoint("https://relay.example", "member-token")),
            contacts: Vec::new(),
            steps,
            skipped_recipients: Vec::new(),
            rows_unshadowed: 0,
        }
    }

    fn kinds(report: &CoreRelayShadowReport) -> Vec<CoreRelayShadowMismatchKind> {
        report.mismatches.iter().map(|m| m.kind).collect()
    }

    #[test]
    fn an_agreeing_pass_reports_nothing() {
        let report = core_relay_shadow_compare(capture(vec![
            step(CoreRelayShadowLane::Receipt, b"contact"),
            step(CoreRelayShadowLane::Authored, b"contact"),
        ]));
        assert_eq!(report.steps_compared, 2);
        assert!(report.mismatches.is_empty());
    }

    #[test]
    fn a_receipt_behind_an_authored_row_is_a_fairness_mismatch() {
        let report = core_relay_shadow_compare(capture(vec![
            step(CoreRelayShadowLane::Authored, b"contact"),
            step(CoreRelayShadowLane::Receipt, b"contact"),
        ]));
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::LaneOrderDiffers]
        );
        assert_eq!(report.mismatches[0].first_index, 1);
        assert_eq!(report.mismatches[0].rows, 1);
    }

    #[test]
    fn a_different_mailbox_is_a_destination_mismatch() {
        let mut only = step(CoreRelayShadowLane::Authored, b"contact");
        only.legacy_endpoint = Some(endpoint("https://elsewhere.example", "other-token"));
        let report = core_relay_shadow_compare(capture(vec![only]));
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::DestinationDiffers]
        );
    }

    #[test]
    fn a_row_core_cannot_encode_is_reported_rather_than_skipped() {
        let mut only = step(CoreRelayShadowLane::Authored, b"contact");
        // An empty msg id fails envelope validation, so no request exists.
        only.msg_id = Vec::new();
        let report = core_relay_shadow_compare(capture(vec![only]));
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::RequestNotConstructible]
        );
    }

    #[test]
    fn marking_a_failed_row_posted_is_a_mismatch() {
        let mut only = step(CoreRelayShadowLane::Authored, b"contact");
        only.status = 500;
        only.legacy_marked_posted = true;
        only.legacy_continued_lane = false;
        let report = core_relay_shadow_compare(capture(vec![only]));
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::SuccessMarkingDiffers]
        );
    }

    #[test]
    fn an_oversize_envelope_lets_the_lane_continue_on_both_engines() {
        let mut only = step(CoreRelayShadowLane::Authored, b"contact");
        only.status = 413;
        only.legacy_marked_posted = false;
        only.legacy_continued_lane = true;
        let report = core_relay_shadow_compare(capture(vec![only]));
        assert!(report.mismatches.is_empty());
    }

    #[test]
    fn a_mailbox_fault_that_only_one_engine_abandons_is_reported() {
        let mut only = step(CoreRelayShadowLane::Authored, b"contact");
        only.status = 507;
        only.relay_code = Some("mailbox_full".to_string());
        only.legacy_marked_posted = false;
        only.legacy_continued_lane = true;
        let report = core_relay_shadow_compare(capture(vec![only]));
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::FaultConsequenceDiffers]
        );
    }

    #[test]
    fn a_recipient_only_one_engine_declines_is_reported() {
        let mut taken = capture(Vec::new());
        taken.skipped_recipients = vec![b"contact".to_vec()];
        let report = core_relay_shadow_compare(taken);
        assert_eq!(
            kinds(&report),
            vec![CoreRelayShadowMismatchKind::SelectionSkipDiffers]
        );
        assert_eq!(report.skips_compared, 1);
    }

    #[test]
    fn a_skip_both_engines_agree_on_is_not_a_mismatch() {
        let mut taken = capture(Vec::new());
        taken.own = None;
        taken.skipped_recipients = vec![b"contact".to_vec()];
        assert!(core_relay_shadow_compare(taken).mismatches.is_empty());
    }

    #[test]
    fn unshadowed_rows_ride_through_to_the_report() {
        let mut taken = capture(Vec::new());
        taken.rows_unshadowed = 4;
        assert_eq!(core_relay_shadow_compare(taken).rows_unshadowed, 4);
    }

    // -----------------------------------------------------------------------
    // What a diverging device may cost
    // -----------------------------------------------------------------------

    /// A row that diverges on every axis at once, so one row contributes one
    /// entry of each kind rather than a list per row.
    fn diverging(lane: CoreRelayShadowLane) -> CoreRelayShadowStep {
        let mut row = step(lane, b"contact");
        row.legacy_endpoint = Some(endpoint("https://elsewhere.example", "other-token"));
        row.msg_id = Vec::new();
        row.status = 507;
        row.relay_code = Some("mailbox_full".to_string());
        row.legacy_marked_posted = true;
        row.legacy_continued_lane = true;
        row
    }

    #[test]
    fn a_systematically_diverging_pass_reports_one_entry_per_kind() {
        let rows: Vec<CoreRelayShadowStep> =
            std::iter::once(diverging(CoreRelayShadowLane::Authored))
                .chain((0..40).map(|_| diverging(CoreRelayShadowLane::Receipt)))
                .collect();
        let mut taken = capture(rows);
        taken.skipped_recipients = (0..200u32).map(|i| i.to_be_bytes().to_vec()).collect();

        let report = core_relay_shadow_compare(taken);

        // Every kind at most once, whatever the row count.
        let mut seen = kinds(&report);
        let before = seen.len();
        seen.sort_unstable_by_key(|kind| kind.as_token());
        seen.dedup();
        assert_eq!(seen.len(), before, "a kind was reported more than once");
        assert!(report.mismatches.len() <= 6);

        // And the counts still say how widespread each one was.
        let lane_order = report
            .mismatches
            .iter()
            .find(|m| m.kind == CoreRelayShadowMismatchKind::LaneOrderDiffers)
            .expect("receipts behind an authored row");
        assert_eq!(lane_order.first_index, 1);
        assert_eq!(lane_order.rows, RELAY_SHADOW_MAX_ROWS - 1);
    }

    #[test]
    fn an_over_long_capture_is_clamped_rather_than_compared_whole() {
        let rows: Vec<CoreRelayShadowStep> = (0..2_000)
            .map(|_| step(CoreRelayShadowLane::Authored, b"contact"))
            .collect();
        let mut taken = capture(rows);
        taken.skipped_recipients = (0..500u32).map(|i| i.to_be_bytes().to_vec()).collect();

        let report = core_relay_shadow_compare(taken);

        assert_eq!(report.steps_compared, RELAY_SHADOW_MAX_ROWS);
        assert_eq!(report.skips_compared, RELAY_SHADOW_MAX_SKIPS);
        assert_eq!(
            report.rows_truncated,
            (2_000 - RELAY_SHADOW_MAX_ROWS) + (500 - RELAY_SHADOW_MAX_SKIPS),
        );
    }

    // -----------------------------------------------------------------------
    // Sampling
    // -----------------------------------------------------------------------

    #[test]
    fn the_first_pass_of_a_day_is_sampled() {
        let decision = core_relay_shadow_sample(CoreRelayShadowSampler::default(), 1_000);
        assert!(decision.sample);
        assert_eq!(decision.next.samples_today, 1);
    }

    #[test]
    fn a_burst_of_passes_costs_one_sample() {
        let mut state = CoreRelayShadowSampler::default();
        let mut sampled = 0;
        for step in 0..20 {
            let decision = core_relay_shadow_sample(state, 1_000 + step * 250);
            if decision.sample {
                sampled += 1;
            }
            state = decision.next;
        }
        assert_eq!(sampled, 1);
    }

    #[test]
    fn the_daily_bound_holds_and_then_resets() {
        let mut state = CoreRelayShadowSampler::default();
        let mut sampled = 0;
        // Well spaced, so only the daily bound can stop it.
        for step in 0..64 {
            let decision =
                core_relay_shadow_sample(state, step * (RELAY_SHADOW_MIN_INTERVAL_MS + 1));
            if decision.sample {
                sampled += 1;
            }
            state = decision.next;
        }
        assert_eq!(sampled, RELAY_SHADOW_MAX_SAMPLES_PER_DAY);

        let next_day = core_relay_shadow_sample(state, (state.day_index + 1) * MS_PER_DAY);
        assert!(next_day.sample);
        assert_eq!(next_day.next.samples_today, 1);
    }

    #[test]
    fn a_backwards_clock_neither_jams_the_sampler_nor_frees_it() {
        let noon = 10 * MS_PER_DAY + MS_PER_DAY / 2;
        let first = core_relay_shadow_sample(CoreRelayShadowSampler::default(), noon);
        assert!(first.sample);

        // A wall clock corrected backwards inside the same day. The gate opens
        // once: a sampler that only ever compared forwards would be shut until
        // an interval elapsed from a reading in the future, which on this
        // device never arrives.
        let morning = 10 * MS_PER_DAY + 1_000;
        let second = core_relay_shadow_sample(first.next, morning);
        assert!(second.sample);
        assert_eq!(second.next.samples_today, 2);

        // And opening is not the same as unlatching: the very next pass at the
        // corrected reading is inside the quiet window measured from it.
        let third = core_relay_shadow_sample(second.next, morning);
        assert!(!third.sample);
        assert_eq!(third.next.samples_today, 2);
    }

    #[test]
    fn every_mismatch_kind_has_a_distinct_stable_token() {
        let kinds = [
            CoreRelayShadowMismatchKind::LaneOrderDiffers,
            CoreRelayShadowMismatchKind::DestinationDiffers,
            CoreRelayShadowMismatchKind::RequestNotConstructible,
            CoreRelayShadowMismatchKind::FaultConsequenceDiffers,
            CoreRelayShadowMismatchKind::SuccessMarkingDiffers,
            CoreRelayShadowMismatchKind::SelectionSkipDiffers,
        ];
        let mut tokens: Vec<&str> = kinds.iter().map(|kind| kind.as_token()).collect();
        tokens.sort_unstable();
        let unique = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), unique);
        for token in tokens {
            assert!(
                crate::protocol_event::is_stable_token(token),
                "{token} is not a stable token"
            );
        }
    }
}
