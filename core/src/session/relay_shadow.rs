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
//! functions the real pass calls ([`crate::session::relay_pass::shadow_upload_endpoint_for`],
//! [`crate::session::relay_pass::shadow_upload_request`]), not by a second
//! implementation of them. A canary that compared against a copy would be
//! testing the copy.
//!
//! # Secrets
//!
//! A capture carries tokens and sealed bytes, because resolving a
//! destination needs the credential and forming a request needs the body. A
//! [`CoreRelayShadowReport`] carries neither: every field in it is a count or
//! an enum, and [`CoreRelayShadowMismatch`] has no free-text field at all. The
//! report is the only thing that reaches the event ring, so `SECRET-01` holds
//! by the shape of the type rather than by care at the call site.

use crate::relay_status::{relay_classify_http_error, CoreRelayFault};
use crate::session::relay_pass::{
    shadow_upload_endpoint_for, shadow_upload_request, CoreRelayContactConfig,
    CoreRelayEndpointConfig, CoreRelayTransportError,
};

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Sampled passes per day. Small on purpose: a mismatch that only ever
/// happens on one pass in a thousand is not the class of defect this is
/// looking for, and a canary that costs a person battery is one they turn
/// off.
pub const RELAY_SHADOW_MAX_SAMPLES_PER_DAY: u32 = 12;

/// The quiet time between two samples. Relay passes arrive in bursts — a push
/// frame, a queue change and a poll tick can all land inside a second — and
/// sampling a burst would spend the whole day's budget on one minute of
/// evidence.
pub const RELAY_SHADOW_MIN_INTERVAL_MS: i64 = 15 * 60 * 1_000;

/// Rows one sampled pass may capture. The cap is on memory, not on interest:
/// a sealed payload can be half a megabyte, and a capture is held whole while
/// it is compared.
pub const RELAY_SHADOW_MAX_ROWS: u32 = 16;

/// [`RELAY_SHADOW_MAX_ROWS`], for a shell. A `const` does not cross UniFFI,
/// and a shell that wrote the number down itself would be a second place it is
/// decided — the exact shape this program exists to remove.
#[uniffi::export]
pub fn core_relay_shadow_max_rows() -> u32 {
    RELAY_SHADOW_MAX_ROWS
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
/// the only reason the sealed bytes are present is that forming the request
/// core would have sent requires them.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowStep {
    pub lane: CoreRelayShadowLane,
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub recipient_hint: Vec<u8>,
    /// Who the row is addressed to, which is what a destination is resolved
    /// from.
    pub recipient_user_id: Vec<u8>,
    pub sealed: Vec<u8>,
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

/// One disagreement, named by kind and located by position.
///
/// There is no field here that could hold a message, an endpoint or a
/// credential, and that is the whole design: this is the type that reaches
/// the event ring.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowMismatch {
    pub kind: CoreRelayShadowMismatchKind,
    /// Index into [`CoreRelayShadowCapture::steps`], or into
    /// [`CoreRelayShadowCapture::skipped_recipients`] for a
    /// [`CoreRelayShadowMismatchKind::SelectionSkipDiffers`].
    pub index: u32,
}

/// What one comparison found. Counts and enums only.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayShadowReport {
    pub steps_compared: u32,
    pub rows_unshadowed: u32,
    pub skips_compared: u32,
    pub mismatches: Vec<CoreRelayShadowMismatch>,
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// Compare one captured legacy pass against what core would have planned.
///
/// Pure: no store, no clock, no network. Called after the legacy pass has
/// already finished, so nothing it returns can change what that pass did.
#[uniffi::export]
pub fn core_relay_shadow_compare(capture: CoreRelayShadowCapture) -> CoreRelayShadowReport {
    let mut mismatches: Vec<CoreRelayShadowMismatch> = Vec::new();
    let mut seen_authored = false;

    for (index, step) in capture.steps.iter().enumerate() {
        let index = index as u32;

        // Fairness order. Core runs the whole receipt lane before the first
        // authored row, so a receipt appearing after an authored row is the
        // starvation this ordering exists to prevent.
        match step.lane {
            CoreRelayShadowLane::Authored => seen_authored = true,
            CoreRelayShadowLane::Receipt => {
                if seen_authored {
                    mismatches.push(CoreRelayShadowMismatch {
                        kind: CoreRelayShadowMismatchKind::LaneOrderDiffers,
                        index,
                    });
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
            mismatches.push(CoreRelayShadowMismatch {
                kind: CoreRelayShadowMismatchKind::DestinationDiffers,
                index,
            });
        }

        // Only a row the legacy engine actually posted can be asked whether
        // core could have posted it: a row neither engine sends is not a
        // request-formation question.
        if legacy.is_some() {
            let constructible = planned.as_ref().is_some_and(|endpoint| {
                shadow_upload_request(
                    endpoint,
                    step.msg_id.clone(),
                    step.hop_ttl,
                    step.recipient_hint.clone(),
                    step.sealed.clone(),
                    step.expiry_ms,
                )
                .is_some()
            });
            if !constructible {
                mismatches.push(CoreRelayShadowMismatch {
                    kind: CoreRelayShadowMismatchKind::RequestNotConstructible,
                    index,
                });
            }
        }

        let succeeded = step.transport_error.is_none() && (200..300).contains(&step.status);
        if succeeded != step.legacy_marked_posted && legacy.is_some() {
            mismatches.push(CoreRelayShadowMismatch {
                kind: CoreRelayShadowMismatchKind::SuccessMarkingDiffers,
                index,
            });
        }

        if legacy.is_some() && !succeeded && core_continues_lane(step) != step.legacy_continued_lane
        {
            mismatches.push(CoreRelayShadowMismatch {
                kind: CoreRelayShadowMismatchKind::FaultConsequenceDiffers,
                index,
            });
        }
    }

    for (index, recipient) in capture.skipped_recipients.iter().enumerate() {
        if shadow_upload_endpoint_for(&capture.contacts, capture.own.as_ref(), recipient).is_some()
        {
            mismatches.push(CoreRelayShadowMismatch {
                kind: CoreRelayShadowMismatchKind::SelectionSkipDiffers,
                index: index as u32,
            });
        }
    }

    CoreRelayShadowReport {
        steps_compared: capture.steps.len() as u32,
        rows_unshadowed: capture.rows_unshadowed,
        skips_compared: capture.skipped_recipients.len() as u32,
        mismatches,
    }
}

/// Whether the core engine would offer the next row of this lane to the same
/// mailbox after this failure.
///
/// The rule is `relay_pass`'s: a `413` is terminal for one row and says
/// nothing about the mailbox, so the lane continues; everything else is
/// evidence about the mailbox, so the lane stops spending on it. A family
/// `429` ends the pass outright, which is a stronger form of the same answer.
fn core_continues_lane(step: &CoreRelayShadowStep) -> bool {
    if step.transport_error.is_some() {
        return false;
    }
    relay_classify_http_error(step.status, step.relay_code.clone())
        == CoreRelayFault::MessageTooLarge
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
            sealed: vec![3u8; 64],
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
        assert_eq!(report.mismatches[0].index, 1);
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
