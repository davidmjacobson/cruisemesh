//! One relay pass, as an explicit ordered state machine.
//!
//! # What this is
//!
//! [`CoreRelayPass`] is the relay sync engine that `RelaySyncEngine.kt` and
//! `MeshController.relaySyncBlocking` each implement today, moved into Rust
//! once. It owns the stage order, request formation, response decoding,
//! response caps, store transactions, ack eligibility, cursor advancement,
//! silence evidence, health folding, budgets and continuation. What it does
//! not own is anything that touches an operating system: no socket, no TLS,
//! no timer, no thread. Those stay in the shells, and this module reaches
//! them through exactly one seam.
//!
//! ```text
//! CoreRelayPass::start(now_ms)          -> CoreRelayAction
//! CoreRelayPass::resume_http(result)    -> CoreRelayAction
//! CoreRelayPass::cancel(now_ms)         -> CoreRelayPassSummary
//! ```
//!
//! An action is either one fully-formed HTTP request, a sleep, or the pass's
//! finished summary. Exactly one action is outstanding at a time. The driver
//! executes the request exactly as written and hands back a
//! [`CoreRelayHttpResult`]; it infers no retry, no cursor, no ack, no carry
//! and no health policy, because none of those decisions are visible from
//! where it stands.
//!
//! **This package is dark.** Nothing on either shell calls it yet; C1 and C2
//! write the adapters. It is exported over UniFFI so those adapters have a
//! surface to compile against, and `core/tests/relay_pass_replay.rs` is its
//! only caller today.
//!
//! # Why one action at a time
//!
//! It is the smallest seam that makes the two rules this program exists to
//! keep structural rather than aspirational.
//!
//! `TXN-01` — no store transaction spans external I/O — becomes true by
//! construction: every store call this module makes happens between an action
//! being emitted and the next one being formed, never across the wait. There
//! is no way to hold a transaction open here even by accident, because the
//! function that opened it has returned before the driver is handed anything.
//!
//! `IDEMP-01` — duplicate, late and replayed results change nothing — becomes
//! checkable at one place: [`CoreRelayPass::resume_http`] compares the
//! result's `pass_id` and `action_id` against the single outstanding action
//! and, on any mismatch, mutates nothing and emits
//! `action_result_stale_ignored`. A parallel request queue would have made
//! that a per-lane problem instead of a one-line comparison. One request
//! outstanding, one result at a time, is the whole rule.
//!
//! The comparison is only as good as the id it compares, so the pass id an
//! action carries is *derived* rather than taken: two passes in one process
//! can never share one, whatever they were asked to be called. See
//! [`CoreRelayPass::new`].
//!
//! # The ordered stages
//!
//! [`CoreRelayStage`] is the order, and it is pinned rather than emergent:
//!
//! 1. **Prune and repair local state.** Expire outbound rows, outgoing
//!    receipts, carried rows and consumed-hidden records.
//! 2. **Announce only when changed.** An own-endpoint change clears every
//!    carried upload marker, because "already on the old mailbox" says
//!    nothing about the new one (`MARK-01`).
//! 3. **Upload receipts.**
//! 4. **Upload locally authored rows.**
//! 5. **Upload carried rows,** marking each durably on success (`MARK-01`).
//! 6. **Decide the hint-triggered rewalk.** A widened hint set drops the
//!    frontiers, because mail that arrived under a hint this device did not
//!    have sits *below* them.
//! 7. **Per eligible config: presence, then the mailbox walk.**
//! 8. **Commit silence and rejection evidence, and fold pass health.**
//! 9. **Finish, or schedule a continuation with an explicit progress reason.**
//!
//! Receipts before authored before carried is load-bearing: a receipt is
//! small and unblocks a peer's queue, so a deep carry queue must never starve
//! one. Announce before every upload is load-bearing for the same reason a
//! changed endpoint has to ride the pass that noticed it. Silence after the
//! walks is `SILENCE-01`: only then is it known whether this device's own
//! mailbox answered.
//!
//! The first family `429` ends every later network stage (`RATE-01`). It does
//! not end the pass: stage 8 still runs, because the evidence gathered before
//! the refusal is real and discarding it would lose a rejection this device
//! actually observed.
//!
//! # Cross-shell divergences this module decides
//!
//! Section 5.2 of `specs/protocol-contract-v1.md` recorded places where
//! reading the two shells gave two answers and named C0 as the package that
//! must choose. All three are decided here, a fourth found while writing this
//! module is recorded there too, and the contract rows, the affected fixture
//! and the reasons move in the same commit.
//!
//! **Presence runs before the walk.** Android walked first; iOS synced
//! presence first; neither ordering was written down as deliberate. The
//! pinned order is adopted — presence, then the walk — and the reason is a
//! liveness one rather than a taste one. The walk is the budgeted, abortable,
//! unbounded-input stage: it yields on [`crate::relay_mailbox_walk_action`],
//! it is what a `429` cuts short, and on a phone with a deep mailbox it
//! reaches its budget every single pass. Under Android's order that phone
//! never announces presence at all, because presence sits behind a stage that
//! never finishes. Under this one, presence is a single fixed-cost request
//! that has already happened before anything can consume the budget. Cost of
//! the change: on a shallow mailbox, one extra round trip of latency before
//! the first fetch — bounded, and paid only by devices for which the walk was
//! never the constraint.
//!
//! **A presence failure is recorded, not swallowed.** Android logged it and
//! moved on, so presence could never mark a config faulted; iOS recorded it
//! like any other fault. Recording is adopted. `SILENCE-01` says silence may
//! be committed only with same-pass proof that another relay answered, and a
//! swallowed failure destroys exactly that proof — a config whose presence
//! request failed and whose walk then succeeded is *not* silent, and one
//! where both failed is stronger evidence than the walk alone. Swallowing
//! made the two indistinguishable. A recorded presence failure never skips
//! the walk, on either shell's reading; the walk still runs, and
//! [`PassState::apply`] leaves the config's walk unstarted for exactly that
//! reason.
//!
//! **Presence is our own mailbox only.** Android groups contacts under their
//! poll config and issues a presence sync per config, so a contact on another
//! family's relay has their hint queried there. This module announces and
//! queries only on this device's own mailbox. Announcing our hints into
//! another family's mailbox tells that family we exist, which is a privacy
//! cost this device cannot see the benefit of; the query half carries no such
//! cost but is dropped with it here, so a contact reachable only through
//! another family's relay loses their last-seen time once the shells migrate.
//! That is the recorded cost of the decision, and C3 is the package that
//! decides whether to reinstate the query on its own — the fourth 5.2 row.
//!
//! **The quiet window is committed at the refusal, not at the end.** Android
//! wrote `max(existing, now + delay)` inside the failing request; iOS
//! accumulated a maximum across the pass and overwrote at the end. Android's
//! is adopted: the window is a floor that a later, shorter one cannot lower,
//! which is what `RATE-01` says it is, and it exists from the refusal onward
//! rather than from the end. Here that means
//! [`CoreRelayPassSummary::quiet_until_ms`] is set the moment the `429` is
//! seen, so a pass that is *cancelled* after its refusal — an app
//! backgrounded mid-pass, a driver pulled — still reports it, where an
//! accumulate-and-overwrite-at-the-end pass would report nothing.
//!
//! What this does **not** yet do is survive process death. Nothing in core
//! persists the window: it lives in this object and in the summary, so a
//! process killed before the summary is read starts the next launch with no
//! window at all. Android's `rateLimitedUntilMs` is an in-memory field too, so
//! neither shell has that property today either. Making the floor durable is
//! adapter-side work (the summary is what a shell persists) and is named in
//! the contract's 5.2 row rather than claimed here.
//!
//! # Budgets are declared, not hoped for
//!
//! [`CoreRelayPassBudgets`] rides in the plan and every count it bounds rides
//! back out in [`CoreRelayPassSummary`], so `LIVE-01` is checkable from a
//! transcript rather than by reading the loop. Requests, envelopes, response
//! bytes, uploads per lane and a wall-clock deadline are each a hard stop
//! that ends the pass in `BudgetYield` rather than a target it tries to hit.
//!
//! Two of them are *admission* limits and say so in
//! [`CoreRelayPassBudgets`]: no request is admitted once the envelope or byte
//! count has reached its bound, so the last admitted page may carry the pass
//! past it by at most that page. `max_requests` is the exception — it is
//! exact, including the ack a page earns, which is why a page that cannot
//! afford its ack holds its frontier instead of spending one more request.
//!
//! `PROGRESS-01` is the other half. A continuation is scheduled only when the
//! pass strictly advanced something — a frontier, a sweep cursor, an ingested
//! row, an upload marker — or when it is deferring into a strictly later
//! deadline it just recorded (the quiet window). A pass that did neither ends
//! with no continuation and says so. That is deliberately stronger than the
//! contract's minimum: an unchanged-state reschedule is not merely required
//! to buy something, it cannot be emitted at all.
//!
//! # Secrets
//!
//! A relay token crosses this interface, in the `Authorization` header of a
//! [`CoreRelayHttpRequest`], because a request cannot authenticate without
//! one. It goes nowhere else. No token, URL, host or sealed byte reaches a
//! protocol event, a [`CoreRelayPassSummary`], a fixture or an exported
//! archive: mailboxes are named by the credential-free
//! [`crate::relay_cursor_key`] digest and then by an archive-local pseudonym,
//! and every summary field is a count, an enum or an opaque pass id.
//! `SECRET-01` is tested against the live ring and against the summary rather
//! than asserted here.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::protocol_event::{ProtocolEventCode, ProtocolEventDraft};
use crate::relay_cursor::{
    relay_cursor_key, relay_fetch_walk_continues, relay_mailbox_walk_action,
    relay_pass_start_cursor, relay_sweep_due, relay_sweep_restart_from_zero,
    RelayMailboxWalkAction,
};
use crate::relay_status::{relay_classify_http_error, relay_retry_after_ms, CoreRelayFault};
use crate::relay_wire::{
    core_group_fanout_relay_target, relay_build_fetch_path, relay_decode_fetch_page,
    relay_decode_presence_page, relay_encode_ack_request, relay_encode_post_envelope,
    relay_encode_presence_request, relay_fetch_batch_limit, relay_fetch_shrunk_limit,
    relay_max_response_bytes, relay_validate_envelope_sizes, resolved_contact_delivery_relay,
    resolved_contact_poll_relay, resolved_contact_relay, GroupRelayMember, RelayEndpoint,
};
use crate::session::relay_policy::{
    core_family_relay_backoff_delay_ms, core_family_relay_jitter_ms, core_relay_pass_health,
    core_worse_relay_fault, CoreRelayPassHealth,
};
use crate::store::MessageStore;

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// Requests one pass may issue across every stage and every config.
///
/// Sized from what the stages below can actually ask for rather than picked
/// round: the upload lanes are capped at 96 between them, and a config costs
/// at most one presence plus [`crate::RELAY_MAILBOX_MAX_PAGES_PER_PASS`]
/// fetches and as many acks, so eight configs is 72. This is that sum with
/// room, and it exists so a mailbox count nobody predicted cannot turn into
/// an unbounded pass.
pub const RELAY_PASS_MAX_REQUESTS: u32 = 192;

/// Envelopes one pass may take from every mailbox put together.
///
/// [`crate::RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS`] bounds one mailbox; this
/// bounds the pass, because a device with several contact endpoints could
/// otherwise spend that budget once per endpoint.
pub const RELAY_PASS_MAX_ENVELOPES: u32 = 1_024;

/// Response bytes one pass may read. Two full-cap pages' worth, which no
/// healthy mailbox approaches and a hostile one cannot exceed.
pub const RELAY_PASS_MAX_RESPONSE_BYTES: u64 = 24 * 1024 * 1024;

/// Receipt uploads per pass. Small and first: a receipt unblocks a peer's
/// queue, so it is the lane that must never be starved.
pub const RELAY_PASS_MAX_RECEIPT_UPLOADS: u32 = 24;

/// Authored uploads per pass.
pub const RELAY_PASS_MAX_AUTHORED_UPLOADS: u32 = 24;

/// Carried uploads per pass. Larger than the other two because the carry
/// queue is the deep one, and still bounded because #222 was what happens
/// when it is not.
pub const RELAY_PASS_MAX_CARRIED_UPLOADS: u32 = 48;

/// Cross-family presence queries one pass may issue, across every contact.
///
/// Separate from, and smaller than, the number of contacts: this is a lane
/// with no mail in it. Every query here spends a request asking another
/// family's relay a question for this device's own benefit, so it is bounded
/// by something that does not grow when an address book does. Eight is the
/// same order as the configs the pass already walks, and each query is
/// charged against [`CoreRelayPassBudgets::max_requests`] like every other
/// request — this cap only stops presence from *being* the pass.
pub const RELAY_PASS_MAX_PRESENCE_PROBES: u32 = 8;

/// The shortest interval between two cross-family queries about the same
/// contact, across passes.
///
/// The client half of `PRESENCE-01`. A relay's cap is not a schedule, and a
/// client that asks as often as it is allowed to has turned the server's
/// limit into its cadence; this is the cadence, and the server's cap is the
/// backstop for a client that ignores it. Fifteen minutes is the window
/// Android's `ContactReachability.RECENT_WINDOW_MS` already draws its
/// "recently" copy from, and the answer is bucketed at least that coarsely,
/// so asking faster could not change what anyone is shown.
pub const RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS: i64 = 15 * 60 * 1000;

/// Coarse recency buckets for a cross-family presence answer, by age in
/// milliseconds.
///
/// Named here rather than read off the wire: a relay tells a cross-family
/// caller which bucket a hint falls in by reporting the oldest instant still
/// inside it, and this ladder maps that stamp back to the bucket. Deriving it
/// rather than trusting a response field means a *precise* answer — from an
/// older relayd, or from a mailbox this device is genuinely a member of — is
/// coarsened here too, so nothing downstream can come to depend on a
/// precision that is not contractually there.
///
/// The edges are [`crate::CONNECTION_PRESENCE_ONLINE_WINDOW_MS`] (the "seen
/// online" window both shells already use) and
/// [`RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS`] (their "recently"), then a
/// day, then everything older.
pub const RELAY_PRESENCE_RECENCY_ACTIVE: &str = "active";
pub const RELAY_PRESENCE_RECENCY_RECENT: &str = "recent";
pub const RELAY_PRESENCE_RECENCY_DAY: &str = "day";
pub const RELAY_PRESENCE_RECENCY_OLDER: &str = "older";

/// Which bucket an age falls in. A negative age (a stamp from the future,
/// which is a clock artifact rather than evidence) reads as the freshest
/// bucket the same way a zero age does; it cannot be used to invent a
/// recency, because every bucket is advisory.
pub(crate) fn relay_presence_recency(age_ms: i64) -> &'static str {
    if age_ms <= crate::CONNECTION_PRESENCE_ONLINE_WINDOW_MS {
        RELAY_PRESENCE_RECENCY_ACTIVE
    } else if age_ms <= RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS {
        RELAY_PRESENCE_RECENCY_RECENT
    } else if age_ms <= 24 * 60 * 60 * 1000 {
        RELAY_PRESENCE_RECENCY_DAY
    } else {
        RELAY_PRESENCE_RECENCY_OLDER
    }
}

/// Wall-clock budget for one pass, measured from `start`.
///
/// A pass competes with an OS watchdog, so "terminates eventually" is not the
/// property — this is. The clock is whatever the driver reports in
/// [`CoreRelayHttpResult::completed_at_ms`], so a pass whose driver has
/// stopped answering is bounded by the driver's own timeout rather than by
/// this, which is the correct division: core cannot time out a socket it
/// cannot see.
pub const RELAY_PASS_DEADLINE_MS: i64 = 20_000;

/// Every bound one pass declares. Rides in the plan and back out in the
/// summary, so a transcript can prove `LIVE-01` without reading the loop.
///
/// `max_requests` is exact: no pass issues more, and the ack a consumed page
/// earns is counted against it like everything else — a page that cannot
/// afford its ack holds its frontier and comes back next pass rather than
/// spending a request the budget does not have.
///
/// `max_envelopes` and `max_response_bytes` are *admission* limits, and the
/// difference is worth stating because a summary is read against these
/// numbers. No request is admitted once either count has reached its bound,
/// so a pass can end at most one page of rows, or one response body, past
/// them. Bounding them exactly would mean predicting a page's size before
/// asking for it, which no client can do.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRelayPassBudgets {
    pub max_requests: u32,
    pub max_envelopes: u32,
    pub max_response_bytes: u64,
    pub max_receipt_uploads: u32,
    pub max_authored_uploads: u32,
    pub max_carried_uploads: u32,
    /// Milliseconds from `start`. Zero or negative means no deadline, which
    /// only tests use.
    pub deadline_ms: i64,
}

impl Default for CoreRelayPassBudgets {
    fn default() -> Self {
        CoreRelayPassBudgets {
            max_requests: RELAY_PASS_MAX_REQUESTS,
            max_envelopes: RELAY_PASS_MAX_ENVELOPES,
            max_response_bytes: RELAY_PASS_MAX_RESPONSE_BYTES,
            max_receipt_uploads: RELAY_PASS_MAX_RECEIPT_UPLOADS,
            max_authored_uploads: RELAY_PASS_MAX_AUTHORED_UPLOADS,
            max_carried_uploads: RELAY_PASS_MAX_CARRIED_UPLOADS,
            deadline_ms: RELAY_PASS_DEADLINE_MS,
        }
    }
}

/// The deployed budgets. The only constructor a shell can reach, for the same
/// reason [`crate::CoreFamilyRelayPacer::new`] is: a second door onto these
/// numbers would be a second place they are decided.
#[uniffi::export]
pub fn core_relay_pass_default_budgets() -> CoreRelayPassBudgets {
    CoreRelayPassBudgets::default()
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// One relay endpoint this device holds a credential for.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayEndpointConfig {
    pub url: String,
    /// Crosses this private interface for auth and reaches nothing else. See
    /// the module docs.
    pub token: String,
}

/// One contact whose card may name a mailbox, as the pass sees them.
///
/// Deliberately the raw card fields rather than a resolved endpoint: the
/// resolution rules ([`resolved_contact_relay`], [`resolved_contact_poll_relay`])
/// are core policy, and handing the pass a pre-resolved endpoint would move
/// them back into whichever shell built the plan.
///
/// # Two brakes, not one
///
/// The two health flags are separate for the reason
/// [`crate::GroupRelayMember`]'s are, and this config carried only the first
/// one until the upload lanes needed both. An endpoint can be out of service
/// in two ways that justify opposite answers:
///
/// * **Rejection** — the endpoint answered, authoritatively, that it will not
///   serve us. The card is wrong. Falling back to this device's own mailbox
///   is right: a `401` proves nothing about our own relay, and when both
///   sides have since moved to the same host it really delivers.
/// * **Silence** — nothing answered at all. Falling back would put a
///   cross-family contact's mail in a mailbox they never read, and
///   `relay_posted_at` is terminal, so that is a permanent misroute rather
///   than a retry. The right answer is to post nothing to that recipient this
///   pass and keep waiting; the row stays queued for a later pass and for the
///   mesh paths.
///
/// Collapsed onto one flag, the silence case has to borrow the rejection
/// answer, which is exactly the misroute. So both are here, and
/// [`shadow_upload_endpoint_for`] treats them distinctly.
///
/// `endpoint_answering` carries a default so a caller that has not yet been
/// taught the difference keeps compiling and keeps its present behaviour
/// (whatever it folded into `endpoint_usable`), rather than silently
/// acquiring the fallback for a resting endpoint.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayContactConfig {
    pub user_id: Vec<u8>,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
    /// False once this contact's card endpoint has been written off for
    /// authoritative *rejections*
    /// ([`crate::core_contact_relay_endpoint_usable`]). Such a contact is not
    /// polled and cannot accrue more silence, and an upload for them falls
    /// back to this device's own mailbox.
    pub endpoint_usable: bool,
    /// False while this contact's card endpoint is resting because it stopped
    /// *answering*
    /// ([`crate::core_contact_relay_unreachable_endpoint_usable`]). Such a
    /// contact is not polled either, and an upload for them is declined
    /// outright this pass rather than redirected.
    #[uniffi(default = true)]
    pub endpoint_answering: bool,
}

/// Everything one pass needs that is not already in the store.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayPassPlan {
    /// This device's own family mailbox, when it has a pass.
    pub own: Option<CoreRelayEndpointConfig>,
    pub contacts: Vec<CoreRelayContactConfig>,
    /// The public user id. Seeds the anti-lockstep jitter and decides ack
    /// eligibility; never a private key.
    pub own_user_id: Vec<u8>,
    /// Recipient hints this device fetches under.
    pub fetch_hints: Vec<Vec<u8>>,
    /// Hints to announce, and hints to ask about, in stage 7's presence call.
    pub presence_announce: Vec<Vec<u8>>,
    pub presence_query: Vec<Vec<u8>>,
    /// True when this device's own relay endpoint changed since the last
    /// announcement. Stage 2's only input.
    pub own_endpoint_changed: bool,
    /// Whether a sweep has already run in this process. Feeds
    /// [`relay_sweep_due`], which is why it is an input rather than state:
    /// correctness must not depend on recovering an in-memory session.
    pub swept_this_session: bool,
    /// Consecutive family rate limits *before* this pass. The pass adds its
    /// own refusal to this when it computes the quiet window.
    pub consecutive_rate_limits: u32,
    /// A quiet window already open when the pass was built, as an absolute
    /// time. The pass refuses to issue network work inside it.
    pub quiet_until_ms: i64,
    pub budgets: CoreRelayPassBudgets,
}

// ---------------------------------------------------------------------------
// Actions and results
// ---------------------------------------------------------------------------

/// Which relay operation an action performs. The driver does not branch on
/// it — the request is complete without it — but a transcript and a crash
/// report read far better with it than with a path.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayOperation {
    PostEnvelope,
    FetchPage,
    AckPage,
    Presence,
}

/// One HTTP header. A `Vec` of these rather than a map so the order core
/// chose is the order the driver sends, which keeps a request byte-comparable
/// across the two adapter suites (C1/C2's shared vectors).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayHeader {
    pub name: String,
    pub value: String,
}

/// A complete request. The driver sends exactly this and infers nothing.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayHttpRequest {
    pub operation: CoreRelayOperation,
    pub method: String,
    /// Normalized origin, from [`crate::normalize_relay_url`].
    pub base_url: String,
    /// Path and query, already encoded.
    pub path: String,
    /// Complete headers, `Authorization` included.
    pub headers: Vec<CoreRelayHeader>,
    pub body: Vec<u8>,
    /// The most the driver may accumulate before giving up. Exceeding it is
    /// [`CoreRelayTransportError::BodyTooLarge`], which core answers by
    /// halving the page rather than by skipping the cursor.
    pub max_response_bytes: u32,
    /// The only response headers core wants back. Everything else is dropped
    /// at the driver, so a header carrying something core never asked for
    /// cannot reach a store or an event.
    pub response_headers_wanted: Vec<String>,
}

/// What an action asks of the driver: work, a wait, or nothing more.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum CoreRelayActionKind {
    Http {
        request: CoreRelayHttpRequest,
    },
    /// **This pass is over.** It was asked to run inside a quiet window it
    /// must honour, so it spent nothing and ended; `until_ms` is when the
    /// window closes, which is advice to whatever schedules the next pass.
    /// There is no resume from here — [`CoreRelayPass::summary`] carries the
    /// result, and the next attempt is a new pass.
    Sleep {
        until_ms: i64,
    },
    /// Nothing has happened and nothing is outstanding: only
    /// [`CoreRelayPass::start`] moves a pass out of this state.
    ///
    /// Returned when a result arrives before the pass began — which a driver
    /// that persisted an in-flight result across a process restart and
    /// replayed it against a freshly built pass will do. The result mutated
    /// nothing and started nothing.
    NotStarted,
    Finished {
        summary: CoreRelayPassSummary,
    },
}

/// One step of the pass.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayAction {
    /// Opaque, short, and stable for the life of the pass.
    pub pass_id: String,
    /// Strictly increasing across the actions a pass emits. A re-statement of
    /// an action still outstanding — what a stale result gets back — keeps
    /// the id it already had, because nothing new was emitted.
    pub action_id: u64,
    pub stage: CoreRelayStage,
    pub kind: CoreRelayActionKind,
}

/// Why a request produced no status.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayTransportError {
    Timeout,
    ConnectionFailed,
    Tls,
    /// The response exceeded [`CoreRelayHttpRequest::max_response_bytes`] and
    /// the driver stopped reading. Distinct from every other error because
    /// core answers it by shrinking the page, not by giving up on the config.
    BodyTooLarge,
    /// The driver was cancelled, or the process is going away.
    Cancelled,
    Other,
}

/// What the driver observed. Echoes the ids so a late answer can be
/// recognised as one.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayHttpResult {
    pub pass_id: String,
    pub action_id: u64,
    /// The HTTP status, or 0 when `error` is set.
    pub status: u16,
    /// Only the headers [`CoreRelayHttpRequest::response_headers_wanted`]
    /// asked for.
    pub headers: Vec<CoreRelayHeader>,
    pub body: Vec<u8>,
    pub error: Option<CoreRelayTransportError>,
    pub completed_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Stages, outcomes, summary
// ---------------------------------------------------------------------------

/// The pinned stage order. See the module docs for what each one is for and
/// why the ordering constraints between them are load-bearing.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoreRelayStage {
    PruneAndRepair,
    Announce,
    UploadReceipts,
    UploadAuthored,
    UploadCarried,
    RewalkDecision,
    Presence,
    MailboxWalk,
    CommitEvidence,
    Finish,
}

/// How a pass ended.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayPassOutcome {
    /// Every stage ran to the end of its work.
    Completed,
    /// A family 429 ended the remaining network stages (`RATE-01`).
    RateLimited,
    /// A declared budget stopped the pass short (`LIVE-01`).
    BudgetYield,
    /// [`CoreRelayPass::cancel`] was called.
    Cancelled,
    /// There was nothing to do: no own pass and no usable contact endpoint.
    NoConfigs,
    /// The pass was started inside a quiet window it must not spend through.
    RefusedQuietWindow,
}

/// Why a continuation is worth scheduling. `PROGRESS-01` permits exactly two
/// shapes and this enum is them: something strictly advanced, or a strictly
/// later deadline was recorded.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayProgressReason {
    /// A mailbox frontier or sweep cursor moved forward.
    CursorAdvanced,
    /// Rows were durably ingested, so the queue this pass reads is smaller.
    RowsIngested,
    /// An upload was marked, so the upload queue is shorter.
    UploadsMarked,
    /// A quiet window was recorded that is strictly later than the one in
    /// force before. Nothing advanced, and nothing needed to.
    QuietWindowExtended,
}

/// A pass that earned more work, and when it may take it.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreRelayContinuation {
    pub not_before_ms: i64,
    pub reason: CoreRelayProgressReason,
}

/// Everything one finished pass is willing to say about itself.
///
/// Every field is a count, an enum, or an opaque id. There is no URL, no
/// token, no host, no msg id and no payload, which is what makes a summary
/// safe to put in a diagnostics archive (`SECRET-01`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayPassSummary {
    pub pass_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub outcome: CoreRelayPassOutcome,
    pub health: CoreRelayPassHealth,
    /// Where the pass stopped doing work. `Finish` for a pass that ran every
    /// stage; the stage a rate-limit abort, a budget cut or a cancellation
    /// interrupted otherwise. Without this a summary cannot say whether a
    /// pass got as far as its walks.
    pub stage_reached: CoreRelayStage,

    pub requests_issued: u32,
    pub envelopes_processed: u32,
    pub response_bytes_read: u64,
    pub receipt_uploads: u32,
    pub authored_uploads: u32,
    pub carried_uploads: u32,
    pub carried_rows_marked: u32,
    pub rows_ingested: u32,
    pub rows_acked: u32,
    pub frontier_advances: u32,
    pub frontiers_held: u32,
    /// Results that named a finished pass, a wrong pass, or an action that
    /// was no longer outstanding. Every one of them mutated nothing.
    pub stale_results_ignored: u32,
    pub configs_walked: u32,
    /// Requests that faulted against a mailbox, not distinct mailboxes: one
    /// config whose presence and whose fetch both failed counts twice, which
    /// is the number worth having, because it is the work that was spent.
    pub configs_faulted: u32,
    pub silence_committed: u32,
    pub silence_discarded: u32,

    pub budgets: CoreRelayPassBudgets,
    /// The quiet window in force when the pass ended, absolute. Set the
    /// moment a 429 is seen, so a cancelled or crashed pass still carries it.
    pub quiet_until_ms: i64,
    pub continuation: Option<CoreRelayContinuation>,
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// One queued upload, already sealed by whoever authored it.
struct PendingUpload {
    lane: UploadLane,
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
    endpoint: RelayEndpoint,
    /// Set when this row is one member's copy of a group-addressed envelope's
    /// fan-out. `None` for every 1:1, receipt and carried row.
    fanout: Option<FanoutRow>,
}

/// What one member's fan-out row needs to know to record its own landing and
/// to recognise the moment the whole envelope has landed.
///
/// `msg_id` on the [`PendingUpload`] is the *row's* deterministic fan-out id
/// ([`crate::core_group_fanout_rows`]); the envelope's own id is here, because
/// that is what `relay_posted_at` is keyed on and stamping it early would
/// retire a send that most of the group never received.
struct FanoutRow {
    envelope_msg_id: Vec<u8>,
    member_user_id: Vec<u8>,
    /// How many members this envelope owes a landed row, after exclusions.
    /// The envelope is marked posted when the durable marker set for this
    /// mailbox reaches it — which is a count of *this and every earlier
    /// pass's* successes, so a partial pass resumes rather than restarting.
    members_owed: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UploadLane {
    Receipt,
    Authored,
    Carried,
}

impl UploadLane {
    fn outcome(self) -> &'static str {
        match self {
            UploadLane::Receipt => "receipt_upload_issued",
            UploadLane::Authored => "authored_upload_issued",
            UploadLane::Carried => "carried_upload_issued",
        }
    }
}

/// One mailbox this pass will walk.
struct WalkConfig {
    /// `None` for the device's own mailbox.
    contact_user_id: Option<Vec<u8>>,
    endpoint: RelayEndpoint,
    /// The credential-free [`relay_cursor_key`] digest.
    cursor_key: String,
    /// The archive-local pseudonym for this mailbox.
    actor: Option<String>,
    presence_done: bool,
    answered: bool,
    attempted: bool,
    walk: Option<WalkState>,
}

impl WalkConfig {
    fn is_own(&self) -> bool {
        self.contact_user_id.is_none()
    }
}

struct WalkState {
    sweeping: bool,
    after_id: i64,
    limit: u32,
    pages: u32,
    envelopes: u32,
    /// The highest cursor this walk has proven exists, for
    /// [`crate::relay_frontier_after_completed_sweep`].
    swept_through: i64,
    done: bool,
}

/// One contact this pass may ask another family's relay about.
///
/// Not a [`WalkConfig`]: there is no mailbox here to walk. A cross-family
/// endpoint carries a deposit-class credential, which cannot fetch or ack, so
/// this device has exactly one question it may put to it — whether the person
/// whose card it came from has been around — and no cursor, no page, and no
/// silence evidence attaches to the answer.
struct PresenceProbe {
    user_id: Vec<u8>,
    endpoint: RelayEndpoint,
    /// The archive-local pseudonym for the mailbox, so a transcript can
    /// follow the query without naming a host (`SECRET-01`).
    actor: Option<String>,
}

/// What the outstanding action was for, so its result can be applied.
enum ActionIntent {
    Upload(PendingUpload),
    Presence {
        config: usize,
    },
    /// A cross-family presence query. Deliberately a separate intent from
    /// `Presence`: that one is this device's own mailbox and its failure is
    /// evidence about a config, while this one is advisory and its failure is
    /// evidence about nothing (see `apply_error`).
    PresenceProbe {
        probe: usize,
    },
    Fetch {
        config: usize,
        after_id: i64,
        limit: u32,
    },
    Ack {
        config: usize,
        page_next_cursor: i64,
        rows: u32,
        ack_ids: usize,
    },
}

struct Outstanding {
    action_id: u64,
    stage: CoreRelayStage,
    request: CoreRelayHttpRequest,
    intent: ActionIntent,
}

/// Whether stage 8 may commit the silence it collected.
///
/// A pass that ran its walks to the end knows which endpoints stayed quiet. A
/// cancelled one does not: it stopped asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SilenceEvidence {
    Commit,
    Discard,
}

#[derive(Default, Clone, Copy)]
struct Progress {
    cursor_advanced: bool,
    rows_ingested: bool,
    uploads_marked: bool,
}

impl Progress {
    fn reason(self) -> Option<CoreRelayProgressReason> {
        if self.cursor_advanced {
            Some(CoreRelayProgressReason::CursorAdvanced)
        } else if self.rows_ingested {
            Some(CoreRelayProgressReason::RowsIngested)
        } else if self.uploads_marked {
            Some(CoreRelayProgressReason::UploadsMarked)
        } else {
            None
        }
    }
}

struct PassState {
    store: Arc<MessageStore>,
    plan: CoreRelayPassPlan,
    pass_id: String,
    started: bool,
    started_at_ms: i64,
    now_ms: i64,
    next_action_id: u64,
    outstanding: Option<Outstanding>,
    stage: CoreRelayStage,
    finished: Option<CoreRelayPassSummary>,

    uploads: VecDeque<PendingUpload>,
    configs: Vec<WalkConfig>,
    config_index: usize,
    probes: Vec<PresenceProbe>,
    probe_index: usize,
    presence_probes_issued: u32,

    requests_issued: u32,
    envelopes_processed: u32,
    response_bytes_read: u64,
    receipt_uploads: u32,
    authored_uploads: u32,
    carried_uploads: u32,
    carried_rows_marked: u32,
    rows_ingested: u32,
    rows_acked: u32,
    frontier_advances: u32,
    frontiers_held: u32,
    stale_results_ignored: u32,
    configs_walked: u32,
    configs_faulted: u32,
    silence_committed: u32,
    silence_discarded: u32,

    own_relay_succeeded: bool,
    any_relay_succeeded: bool,
    worst_fault: Option<CoreRelayFault>,
    /// Contact endpoints that produced no answer at all this pass, held until
    /// stage 8 can say whether this device's own mailbox answered.
    provisional_silence: Vec<(Vec<u8>, String)>,
    /// Contacts whose endpoint answered authoritatively that it will not
    /// serve us. Needs no proof of own connectivity (`SILENCE-01`).
    rejections: Vec<Vec<u8>>,
    /// Contact endpoints that answered, so any earlier streak clears.
    recoveries: Vec<Vec<u8>>,

    quiet_until_ms: i64,
    rate_limited: bool,
    budget_yield: bool,
    cancelled: bool,
    /// The stage an abort, a budget cut or a cancellation interrupted. `None`
    /// while the pass is still walking its stages normally.
    stopped_at: Option<CoreRelayStage>,
    progress: Progress,
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// One relay pass. See the module docs.
#[derive(uniffi::Object)]
pub struct CoreRelayPass {
    state: Mutex<PassState>,
}

#[uniffi::export]
impl CoreRelayPass {
    /// Build a pass from durable store markers and an explicit plan.
    ///
    /// `pass_id` is a *label*, not the identity. It is what a driver, a log
    /// and a transcript are joined on, so it must be a short opaque token —
    /// it reaches protocol events, and a UUID or a device name in one would
    /// be both too long and a thing worth not printing.
    ///
    /// The id the pass actually carries is derived from it: the label if it
    /// is usable, `p` if it is not, plus a suffix that is unique in this
    /// process. That suffix is the load-bearing part. `IDEMP-01`'s wrong-pass
    /// half is a comparison against this id, and two live passes that shared
    /// one — which is what silently replacing every unusable label with the
    /// same constant produced — would leave that comparison deciding nothing,
    /// with action ids restarting at 1 in every pass to collide underneath
    /// it. A result belonging to the pass before can then be applied to the
    /// action outstanding now: a fetch's `200` marking an upload posted that
    /// was never sent.
    #[uniffi::constructor]
    pub fn new(store: Arc<MessageStore>, plan: CoreRelayPassPlan, pass_id: String) -> Self {
        let pass_id = derive_pass_id(&pass_id);
        CoreRelayPass {
            state: Mutex::new(PassState {
                store,
                plan,
                pass_id,
                started: false,
                started_at_ms: 0,
                now_ms: 0,
                next_action_id: 1,
                outstanding: None,
                stage: CoreRelayStage::PruneAndRepair,
                finished: None,
                uploads: VecDeque::new(),
                configs: Vec::new(),
                config_index: 0,
                probes: Vec::new(),
                probe_index: 0,
                presence_probes_issued: 0,
                requests_issued: 0,
                envelopes_processed: 0,
                response_bytes_read: 0,
                receipt_uploads: 0,
                authored_uploads: 0,
                carried_uploads: 0,
                carried_rows_marked: 0,
                rows_ingested: 0,
                rows_acked: 0,
                frontier_advances: 0,
                frontiers_held: 0,
                stale_results_ignored: 0,
                configs_walked: 0,
                configs_faulted: 0,
                silence_committed: 0,
                silence_discarded: 0,
                own_relay_succeeded: false,
                any_relay_succeeded: false,
                worst_fault: None,
                provisional_silence: Vec::new(),
                rejections: Vec::new(),
                recoveries: Vec::new(),
                quiet_until_ms: 0,
                rate_limited: false,
                budget_yield: false,
                cancelled: false,
                stopped_at: None,
                progress: Progress::default(),
            }),
        }
    }

    /// Begin the pass and return its first action.
    ///
    /// Calling `start` twice returns the outstanding action rather than
    /// restarting: a pass is a one-shot object, and a driver that re-entered
    /// here would otherwise re-run stage 1's pruning against a store the
    /// first call had already pruned.
    ///
    /// Calling it after [`CoreRelayPass::cancel`], or after the pass
    /// finished, returns that summary and does nothing else. A cancelled pass
    /// that could be started again would issue requests and touch the store
    /// with no summary willing to admit it: `cancel` freezes the summary, so
    /// every later request would be work no transcript records.
    pub fn start(&self, now_ms: i64) -> CoreRelayAction {
        let mut state = self.lock();
        if state.finished.is_some() || state.started {
            return state.restate_or_advance();
        }
        state.started = true;
        state.started_at_ms = now_ms;
        state.now_ms = now_ms;

        let configs = state.plan.contacts.len() as i64 + i64::from(state.plan.own.is_some());
        state.note(
            ProtocolEventDraft::new(ProtocolEventCode::PassStart, now_ms, "pass_started")
                .invariants(&["LIVE-01"])
                .count("configs", configs.max(0)),
        );

        // RATE-01's second clause at the front door. A pass started inside a
        // quiet window spends nothing; the caller is told when the window
        // ends rather than being left to guess.
        if state.plan.quiet_until_ms > now_ms {
            state.quiet_until_ms = state.plan.quiet_until_ms;
            state.note(
                ProtocolEventDraft::new(
                    ProtocolEventCode::PassFinish,
                    now_ms,
                    "refused_inside_quiet_window",
                )
                .invariants(&["RATE-01"])
                .count("requests", 0),
            );
            let until = state.plan.quiet_until_ms;
            let summary = state.finish(CoreRelayPassOutcome::RefusedQuietWindow, now_ms);
            return CoreRelayAction {
                pass_id: summary.pass_id.clone(),
                action_id: 0,
                stage: CoreRelayStage::Finish,
                kind: CoreRelayActionKind::Sleep { until_ms: until },
            };
        }

        state.advance()
    }

    /// Apply one driver result and return the next action.
    ///
    /// A result that names a finished pass, another pass, or an action that
    /// is not outstanding mutates nothing, is counted, and emits
    /// `action_result_stale_ignored` (`IDEMP-01`).
    pub fn resume_http(&self, result: CoreRelayHttpResult) -> CoreRelayAction {
        let mut state = self.lock();

        if state.finished.is_some() {
            state.stale_results_ignored = state.stale_results_ignored.saturating_add(1);
            // The summary already went out to whoever finished the pass, so
            // this count has to keep moving inside it as well. A driver that
            // answered twenty minutes late deserves to see that it did; a
            // summary frozen at the moment of finishing would report zero
            // ignored results however many arrived afterwards.
            let ignored = state.stale_results_ignored;
            if let Some(summary) = state.finished.as_mut() {
                summary.stale_results_ignored = ignored;
            }
            let at_ms = state.now_ms;
            state.note(
                ProtocolEventDraft::new(
                    ProtocolEventCode::ActionResultStaleIgnored,
                    at_ms,
                    "late_result_from_finished_pass",
                )
                .invariants(&["IDEMP-01"])
                .count("mutations", 0),
            );
            let summary = state.finished.clone().expect("checked just above");
            return CoreRelayAction {
                pass_id: summary.pass_id.clone(),
                action_id: 0,
                stage: CoreRelayStage::Finish,
                kind: CoreRelayActionKind::Finished { summary },
            };
        }

        let matches = state.outstanding.as_ref().is_some_and(|outstanding| {
            outstanding.action_id == result.action_id && state.pass_id == result.pass_id
        });
        if !matches {
            state.stale_results_ignored = state.stale_results_ignored.saturating_add(1);
            let at_ms = state.now_ms;
            let outcome = if state.outstanding.is_none() {
                "result_with_no_action_outstanding"
            } else if state.pass_id != result.pass_id {
                "result_from_another_pass"
            } else {
                "duplicate_or_out_of_order_result"
            };
            state.note(
                ProtocolEventDraft::new(
                    ProtocolEventCode::ActionResultStaleIgnored,
                    at_ms,
                    outcome,
                )
                .invariants(&["IDEMP-01"])
                .count("mutations", 0),
            );
            // A result for a pass that never began starts nothing. Only
            // `start` may do that: it is what sets the clock the deadline is
            // measured from, and it is where the quiet window is honoured, so
            // a stale result that fell through to the stage machine would run
            // a pass from time zero and straight through a `RATE-01` window
            // this pass was built inside.
            if !state.started {
                return CoreRelayAction {
                    pass_id: state.pass_id.clone(),
                    action_id: 0,
                    stage: state.stage,
                    kind: CoreRelayActionKind::NotStarted,
                };
            }
            return state.restate_or_advance();
        }

        let outstanding = state.outstanding.take().expect("checked just above");
        // The clock only moves forward. A driver on a device whose wall clock
        // stepped backwards mid-request must not be able to rewind a deadline
        // or a quiet window that has already been recorded.
        state.now_ms = state.now_ms.max(result.completed_at_ms);
        state.apply(outstanding, result);
        state.advance()
    }

    /// End the pass now and report what it did.
    ///
    /// Idempotent, and safe at any point: nothing is mid-transaction, because
    /// no transaction spans an action. Any result that arrives afterwards is
    /// stale by definition.
    ///
    /// A cancelled pass commits the evidence it *earned* — an authoritative
    /// rejection, an endpoint that answered — and no silence at all. Silence
    /// is the absence of an answer, and a pass pulled mid-request never gave
    /// the endpoint its chance to answer; committing it would let an app that
    /// is backgrounded during relay passes rest a healthy contact endpoint
    /// one cancellation at a time, which is the same harm as the stale
    /// endpoint demotions in #182 and #207.
    pub fn cancel(&self, now_ms: i64) -> CoreRelayPassSummary {
        let mut state = self.lock();
        if let Some(summary) = &state.finished {
            return summary.clone();
        }
        state.now_ms = state.now_ms.max(now_ms);
        state.cancelled = true;
        state.stopped_at = Some(state.stage);
        state.outstanding = None;
        let at_ms = state.now_ms;
        state.commit_evidence(at_ms, SilenceEvidence::Discard);
        state.finish(CoreRelayPassOutcome::Cancelled, at_ms)
    }

    /// The pass's summary once it has finished, or `None` while it is still
    /// running. For a driver that lost the action it was handed.
    pub fn summary(&self) -> Option<CoreRelayPassSummary> {
        self.lock().finished.clone()
    }
}

impl CoreRelayPass {
    fn lock(&self) -> MutexGuard<'_, PassState> {
        match self.state.lock() {
            Ok(guard) => guard,
            // A poisoned pass is a pass whose driver panicked mid-result.
            // Continuing from the recorded state is strictly better than
            // panicking again: every store mutation this object makes has
            // already committed, so the state is consistent by construction.
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

// ---------------------------------------------------------------------------
// The driver loop
// ---------------------------------------------------------------------------

impl PassState {
    fn note(&self, draft: ProtocolEventDraft) {
        let draft = draft.pass(self.pass_id.clone());
        self.store.note_protocol_events(&[draft]);
    }

    fn note_for(&self, config: usize, draft: ProtocolEventDraft) {
        let draft = match self.configs.get(config).and_then(|c| c.actor.clone()) {
            Some(actor) => draft.actor(actor),
            None => draft,
        };
        self.note(draft);
    }

    /// Hand back the action that is still outstanding, or work out the next
    /// one. Used by a duplicate result and by a second `start`.
    fn restate_or_advance(&mut self) -> CoreRelayAction {
        if let Some(outstanding) = &self.outstanding {
            return CoreRelayAction {
                pass_id: self.pass_id.clone(),
                action_id: outstanding.action_id,
                stage: outstanding.stage,
                kind: CoreRelayActionKind::Http {
                    request: outstanding.request.clone(),
                },
            };
        }
        if let Some(summary) = self.finished.clone() {
            return CoreRelayAction {
                pass_id: self.pass_id.clone(),
                action_id: 0,
                stage: CoreRelayStage::Finish,
                kind: CoreRelayActionKind::Finished { summary },
            };
        }
        self.advance()
    }

    /// Run local stages until something needs the network or the pass is
    /// done. Every iteration either emits an action, moves to a later stage,
    /// or finishes, so the loop cannot spin.
    fn advance(&mut self) -> CoreRelayAction {
        // A finished pass has nothing left to run, and a pass nobody started
        // must not be started from here: `start` is the only door in, because
        // it is what records the time every deadline in this pass is measured
        // from and the only place the quiet window is checked.
        if let Some(summary) = self.finished.clone() {
            return CoreRelayAction {
                pass_id: self.pass_id.clone(),
                action_id: 0,
                stage: CoreRelayStage::Finish,
                kind: CoreRelayActionKind::Finished { summary },
            };
        }
        if !self.started {
            return CoreRelayAction {
                pass_id: self.pass_id.clone(),
                action_id: 0,
                stage: self.stage,
                kind: CoreRelayActionKind::NotStarted,
            };
        }
        // One action outstanding, always. A stage that emitted while applying
        // a result -- the ack that follows a page ingest is the one that does
        // -- has already claimed the seam, and the loop below must hand that
        // action back rather than form a second one on top of it.
        if self.outstanding.is_some() {
            return self.restate_or_advance();
        }
        loop {
            // Only a stage that can still *spend* is cut short. Checking this
            // unconditionally would send `Finish` back to `CommitEvidence` on
            // every iteration of a pass that is already over budget, which is
            // a livelock in the code whose job is to prevent livelocks.
            if self.stage < CoreRelayStage::CommitEvidence && self.over_budget() {
                self.budget_yield = true;
                self.uploads.clear();
                self.stopped_at = Some(self.stage);
                self.stage = CoreRelayStage::CommitEvidence;
            }

            match self.stage {
                CoreRelayStage::PruneAndRepair => {
                    self.run_prune();
                    self.stage = CoreRelayStage::Announce;
                }
                CoreRelayStage::Announce => {
                    self.run_announce();
                    self.stage = CoreRelayStage::UploadReceipts;
                    self.load_receipt_uploads();
                }
                CoreRelayStage::UploadReceipts => {
                    if let Some(action) = self.emit_next_upload() {
                        return action;
                    }
                    self.stage = CoreRelayStage::UploadAuthored;
                    self.load_authored_uploads();
                }
                CoreRelayStage::UploadAuthored => {
                    if let Some(action) = self.emit_next_upload() {
                        return action;
                    }
                    self.stage = CoreRelayStage::UploadCarried;
                    self.load_carried_uploads();
                }
                CoreRelayStage::UploadCarried => {
                    if let Some(action) = self.emit_next_upload() {
                        return action;
                    }
                    self.stage = CoreRelayStage::RewalkDecision;
                }
                CoreRelayStage::RewalkDecision => {
                    self.run_rewalk_decision();
                    self.build_walk_configs();
                    self.build_presence_probes();
                    self.config_index = 0;
                    self.stage = CoreRelayStage::Presence;
                }
                CoreRelayStage::Presence => {
                    if self.config_index >= self.configs.len() {
                        // Every mailbox has been walked. What is left are the
                        // contacts this device holds a card for and no
                        // mailbox: cross-family endpoints, asked about and
                        // nothing else. Last inside the stage on purpose —
                        // presence is advisory, so it spends what the mail
                        // did not need, never the other way round.
                        if let Some(action) = self.emit_presence_probe() {
                            return action;
                        }
                        self.stage = CoreRelayStage::CommitEvidence;
                        continue;
                    }
                    if let Some(action) = self.emit_presence() {
                        return action;
                    }
                    self.stage = CoreRelayStage::MailboxWalk;
                }
                CoreRelayStage::MailboxWalk => {
                    if let Some(action) = self.emit_walk_step() {
                        return action;
                    }
                    self.configs_walked = self.configs_walked.saturating_add(1);
                    self.config_index += 1;
                    self.stage = CoreRelayStage::Presence;
                }
                CoreRelayStage::CommitEvidence => {
                    let at_ms = self.now_ms;
                    self.commit_evidence(at_ms, SilenceEvidence::Commit);
                    self.stage = CoreRelayStage::Finish;
                }
                CoreRelayStage::Finish => {
                    let outcome = if self.cancelled {
                        CoreRelayPassOutcome::Cancelled
                    } else if self.rate_limited {
                        CoreRelayPassOutcome::RateLimited
                    } else if self.budget_yield {
                        CoreRelayPassOutcome::BudgetYield
                    } else if self.configs.is_empty()
                        && self.requests_issued == 0
                        && self.plan.own.is_none()
                    {
                        CoreRelayPassOutcome::NoConfigs
                    } else {
                        CoreRelayPassOutcome::Completed
                    };
                    let at_ms = self.now_ms;
                    let summary = self.finish(outcome, at_ms);
                    return CoreRelayAction {
                        pass_id: self.pass_id.clone(),
                        action_id: 0,
                        stage: CoreRelayStage::Finish,
                        kind: CoreRelayActionKind::Finished { summary },
                    };
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Budgets
    // -----------------------------------------------------------------------

    fn over_budget(&self) -> bool {
        let budgets = &self.plan.budgets;
        if self.requests_issued >= budgets.max_requests
            || self.envelopes_processed >= budgets.max_envelopes
            || self.response_bytes_read >= budgets.max_response_bytes
        {
            return true;
        }
        if budgets.deadline_ms > 0 {
            let elapsed = self.now_ms.saturating_sub(self.started_at_ms);
            // A negative elapsed means the clock ran backwards despite the
            // forward clamp — only possible if `start` was handed a time
            // later than every result. It reads as no elapsed time, never as
            // an expired deadline.
            if elapsed >= budgets.deadline_ms {
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Stage 1 and 2
    // -----------------------------------------------------------------------

    /// Stage 1. Every call here is best-effort on purpose: expiring rows is
    /// housekeeping, and a pass that refused to fetch anyone's mail because a
    /// prune failed would trade a small cost for the whole point of the pass.
    /// A failed prune costs disk, retries next pass, and is visible in the
    /// store's own diagnostics.
    fn run_prune(&mut self) {
        let now = self.now_ms;
        let _ = self.store.prune_expired_outbound_envelopes(now);
        let _ = self.store.prune_expired_outgoing_receipt_envelopes(now);
        let _ = self.store.prune_expired_carried(now);
        let _ = self.store.prune_expired_consumed_hidden_msg_ids(now);
        // Fan-out markers outlive their envelope by nothing: once the envelope
        // is posted in full, expired or pruned, the per-member record has no
        // question left to answer.
        let _ = self.store.prune_relay_fanout_markers();
    }

    fn run_announce(&mut self) {
        if !self.plan.own_endpoint_changed {
            return;
        }
        // MARK-01: a marker names a mailbox, and this is a different mailbox.
        // The re-offer is wholesale because recipient hints rotate daily and
        // cannot be reversed to a contact, so scoping it is not possible.
        let cleared = self
            .store
            .clear_carried_relay_upload_markers()
            .unwrap_or_default();
        // The fan-out markers name a mailbox too, and for the same reason.
        let cleared =
            cleared.saturating_add(self.store.clear_relay_fanout_markers().unwrap_or_default());
        let at_ms = self.now_ms;
        self.note(
            ProtocolEventDraft::new(
                ProtocolEventCode::CarriedRowMarked,
                at_ms,
                "markers_cleared_for_new_endpoint",
            )
            .invariants(&["MARK-01"])
            .count("markers_cleared", cleared.min(i64::MAX as u64) as i64),
        );
    }

    // -----------------------------------------------------------------------
    // Stages 3-5: uploads
    // -----------------------------------------------------------------------

    /// Where a row addressed to `recipient` should be posted, resolved here
    /// rather than by the caller so the routing rule stays core policy.
    fn upload_endpoint_for(&self, recipient: &[u8]) -> Option<RelayEndpoint> {
        shadow_upload_endpoint_for(&self.plan.contacts, self.plan.own.as_ref(), recipient)
    }

    /// The recipients no upload query should spend a batch slot on this pass.
    fn skip_recipients(&self) -> Vec<Vec<u8>> {
        unpostable_recipients(&self.plan.contacts, self.plan.own.as_ref())
    }

    fn load_receipt_uploads(&mut self) {
        let limit = u64::from(self.plan.budgets.max_receipt_uploads);
        let now = self.now_ms;
        let skip = self.skip_recipients();
        let Ok(rows) = self
            .store
            .pending_relay_outgoing_receipt_envelopes(limit, now, skip)
        else {
            return;
        };
        for row in rows {
            let Some(endpoint) = self.upload_endpoint_for(&row.recipient_user_id) else {
                continue;
            };
            self.uploads.push_back(PendingUpload {
                lane: UploadLane::Receipt,
                msg_id: row.msg_id,
                hop_ttl: row.hop_ttl,
                recipient_hint: row.recipient_hint,
                sealed: row.sealed,
                expiry_ms: row.expiry,
                endpoint,
                fanout: None,
            });
        }
    }

    fn load_authored_uploads(&mut self) {
        let budget = self.plan.budgets.max_authored_uploads as usize;
        let limit = u64::from(self.plan.budgets.max_authored_uploads);
        let now = self.now_ms;
        let skip = self.skip_recipients();
        let Ok(rows) = self
            .store
            .pending_relay_outbound_envelopes(limit, now, skip)
        else {
            return;
        };
        // Counted in *rows on the wire*, not in queue entries: a group
        // envelope becomes one row per member, and a lane budget that counted
        // envelopes would let one twelve-member group spend twelve posts
        // while claiming to have spent one.
        let mut queued = 0usize;
        for row in rows {
            if queued >= budget {
                break;
            }
            // A group-addressed row carries `recipient_user_id = group_id`,
            // so it is nobody's contact entry. Decompose it; anything else is
            // one row to one mailbox as before.
            if !self.is_contact(&row.recipient_user_id) {
                if let Ok(Some(group)) = self.store.get_group(row.recipient_user_id.clone()) {
                    queued += self.load_group_fanout(&row, &group, budget - queued);
                    continue;
                }
            }
            let Some(endpoint) = self.upload_endpoint_for(&row.recipient_user_id) else {
                continue;
            };
            self.uploads.push_back(PendingUpload {
                lane: UploadLane::Authored,
                msg_id: row.msg_id,
                hop_ttl: row.hop_ttl,
                recipient_hint: row.recipient_hint,
                sealed: row.sealed,
                expiry_ms: row.expiry,
                endpoint,
                fanout: None,
            });
            queued += 1;
        }
    }

    fn is_contact(&self, user_id: &[u8]) -> bool {
        self.plan
            .contacts
            .iter()
            .any(|contact| contact.user_id == user_id)
    }

    /// Decompose one group-addressed envelope into its per-member rows, and
    /// queue the ones that have not already landed. Returns how many rows were
    /// queued.
    ///
    /// Three rules, all of them the legacy engine's:
    ///
    /// * **One mailbox.** Group text is addressed to a group, not to a person,
    ///   so [`core_group_fanout_relay_target`] picks a single mailbox and
    ///   every row goes there. A member resting for silence contributes no
    ///   fallback, and if that leaves nowhere to post, nothing is posted this
    ///   pass — the envelope stays queued and the mesh paths still carry it.
    /// * **All or nothing for `relay_posted_at`.** The envelope is marked
    ///   posted only once every member's row has landed. What is new here is
    ///   that the *landed* ones are remembered durably, so a partial pass
    ///   resumes with the remainder instead of re-posting the whole set.
    /// * **Blocked members get no row.** Every other outbound fan-out in this
    ///   codebase drops blocked users before it sends, and a relay row is a
    ///   send. An excluded member is not owed a landing, so their absence
    ///   cannot hold the envelope open forever.
    fn load_group_fanout(
        &mut self,
        envelope: &crate::store::OutboundEnvelope,
        group: &crate::Group,
        room: usize,
    ) -> usize {
        let own = self.plan.own.clone();
        let members: Vec<GroupRelayMember> = group
            .member_user_ids
            .iter()
            .filter_map(|member_id| {
                let contact = self
                    .plan
                    .contacts
                    .iter()
                    .find(|candidate| &candidate.user_id == member_id)?;
                Some(GroupRelayMember {
                    relay_url: contact.relay_url.clone(),
                    relay_token: contact.relay_token.clone(),
                    endpoint_usable: contact.endpoint_usable,
                    endpoint_answering: contact.endpoint_answering,
                })
            })
            .collect();
        let Some(endpoint) = core_group_fanout_relay_target(
            members,
            own.as_ref().map(|o| o.url.clone()),
            own.as_ref().map(|o| o.token.clone()),
        ) else {
            return 0;
        };

        let recipients: Vec<Vec<u8>> = group
            .member_user_ids
            .iter()
            .filter(|member_id| {
                !self
                    .store
                    .is_user_blocked((*member_id).clone())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if recipients.is_empty() {
            return 0;
        }
        let members_owed = recipients.len();
        let already = self
            .store
            .relay_fanout_posted_members(envelope.msg_id.clone(), endpoint.url.clone())
            .unwrap_or_default();
        // Every row was already accepted on an earlier pass but the envelope
        // was never stamped — a pass that died between the last row and the
        // mark. Stamp it now rather than re-posting anything.
        if recipients
            .iter()
            .all(|member| already.iter().any(|posted| posted == member))
        {
            let now = self.now_ms;
            if self
                .store
                .mark_outbound_envelope_relay_posted(envelope.msg_id.clone(), now)
                .unwrap_or(false)
            {
                self.progress.uploads_marked = true;
            }
            return 0;
        }

        let rows = crate::core_group_fanout_rows(
            envelope.msg_id.clone(),
            recipients.clone(),
            envelope.hop_ttl,
            envelope.expiry,
            envelope.sealed.clone(),
            envelope.timestamp,
        );
        let mut queued = 0usize;
        for (member_user_id, row) in recipients.into_iter().zip(rows) {
            if queued >= room {
                break;
            }
            if already.iter().any(|posted| posted == &member_user_id) {
                continue;
            }
            self.uploads.push_back(PendingUpload {
                lane: UploadLane::Authored,
                msg_id: row.msg_id,
                hop_ttl: row.hop_ttl,
                recipient_hint: row.recipient_hint,
                sealed: row.sealed,
                expiry_ms: row.expiry,
                endpoint: endpoint.clone(),
                fanout: Some(FanoutRow {
                    envelope_msg_id: envelope.msg_id.clone(),
                    member_user_id,
                    members_owed,
                }),
            });
            queued += 1;
        }
        queued
    }

    fn load_carried_uploads(&mut self) {
        // Carried rows are other people's mail, addressed by hint rather than
        // by a recipient this device can name, so they go to the family
        // mailbox this device holds a member credential for. With no own
        // pass there is nowhere to put them.
        let Some(own) = self.plan.own.clone() else {
            return;
        };
        let limit = u64::from(self.plan.budgets.max_carried_uploads);
        let now = self.now_ms;
        let Ok(rows) = self.store.family_carried_envelopes(limit, now, Vec::new()) else {
            return;
        };
        for row in rows {
            self.uploads.push_back(PendingUpload {
                lane: UploadLane::Carried,
                msg_id: row.msg_id,
                hop_ttl: row.hop_ttl,
                recipient_hint: row.recipient_hint,
                sealed: row.sealed,
                expiry_ms: row.expiry,
                endpoint: RelayEndpoint {
                    url: own.url.clone(),
                    token: own.token.clone(),
                },
                fanout: None,
            });
        }
    }

    fn emit_next_upload(&mut self) -> Option<CoreRelayAction> {
        loop {
            let upload = self.uploads.pop_front()?;
            let Some(request) = shadow_upload_request(
                &upload.endpoint,
                upload.msg_id.clone(),
                upload.hop_ttl,
                upload.recipient_hint.clone(),
                upload.sealed.clone(),
                upload.expiry_ms,
            ) else {
                // A row core cannot encode can never be posted. Skipping it
                // costs one row; failing the stage would cost every row
                // behind it, on every pass, forever.
                continue;
            };
            let outcome = upload.lane.outcome();
            let bytes = upload.sealed.len() as i64;
            let stage = self.stage;
            return Some(
                self.emit(
                    stage,
                    request,
                    ActionIntent::Upload(upload),
                    ProtocolEventDraft::new(ProtocolEventCode::ActionEmitted, self.now_ms, outcome)
                        .invariants(&["LIVE-01"])
                        .count("envelope_bytes", bytes),
                ),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Stage 6: rewalk decision
    // -----------------------------------------------------------------------

    fn run_rewalk_decision(&mut self) {
        let own_user_id = self.plan.own_user_id.clone();
        let Ok(invalidated) = self.store.note_relay_hint_sources(own_user_id) else {
            return;
        };
        if !invalidated {
            return;
        }
        let at_ms = self.now_ms;
        self.note(
            ProtocolEventDraft::new(
                ProtocolEventCode::SweepRestarted,
                at_ms,
                "hint_set_changed_frontiers_dropped",
            )
            .invariants(&["CURSOR-01", "PROGRESS-01"])
            .count("frontiers_dropped", 1),
        );
    }

    fn build_walk_configs(&mut self) {
        let mut configs: Vec<WalkConfig> = Vec::new();
        if let Some(own) = &self.plan.own {
            let endpoint = RelayEndpoint {
                url: own.url.clone(),
                token: own.token.clone(),
            };
            configs.push(self.walk_config(None, endpoint));
        }
        let own = self.plan.own.clone();
        for contact in self.plan.contacts.clone() {
            // Both brakes drop a contact out of the poll set, for the same
            // reason: proxy-polling an endpoint that rejects us is pure
            // waste, and polling one that is resting for silence both spends
            // a request on a host that is not answering and would let the
            // rest accrue more silence against itself.
            if !contact.endpoint_usable || !contact.endpoint_answering {
                continue;
            }
            // A deposit-class credential can post and nothing else, so an
            // endpoint that resolves to one is not a mailbox this device may
            // read. That is enforced by the token class, not by politeness.
            let Some(endpoint) = resolved_contact_poll_relay(
                contact.relay_url.clone(),
                contact.relay_token.clone(),
                own.as_ref().map(|o| o.url.clone()),
                own.as_ref().map(|o| o.token.clone()),
            ) else {
                continue;
            };
            // The same mailbox twice is the same work twice. A *mailbox* is
            // the (url, token) pair, not the host: one relay hosts every
            // family, so a contact whose card names our own host with their
            // own family's credential is a different mailbox with different
            // mail in it. Deduping on the host alone silently dropped exactly
            // that contact — the legacy member-class card `resolved_contact_
            // poll_relay` keeps working on purpose — and left the pass
            // sending only our own token.
            if configs.iter().any(|existing| {
                existing.endpoint.url == endpoint.url && existing.endpoint.token == endpoint.token
            }) {
                continue;
            }
            configs.push(self.walk_config(Some(contact.user_id.clone()), endpoint));
        }
        self.configs = configs;
    }

    fn walk_config(&self, contact_user_id: Option<Vec<u8>>, endpoint: RelayEndpoint) -> WalkConfig {
        let cursor_key = relay_cursor_key(endpoint.url.clone(), endpoint.token.clone());
        let actor = if cursor_key.is_empty() {
            None
        } else {
            self.store
                .protocol_pseudonym("mailbox", cursor_key.as_bytes())
        };
        WalkConfig {
            contact_user_id,
            endpoint,
            cursor_key,
            actor,
            presence_done: false,
            answered: false,
            attempted: false,
            walk: None,
        }
    }

    // -----------------------------------------------------------------------
    // Stage 7a: presence
    // -----------------------------------------------------------------------

    fn emit_presence(&mut self) -> Option<CoreRelayAction> {
        let index = self.config_index;
        let config = self.configs.get(index)?;
        if config.presence_done {
            return None;
        }
        // Presence is announced only on the device's own mailbox. Announcing
        // our hints into a contact's family mailbox would tell their whole
        // family we exist, and there is nothing there that answers for us.
        if !config.is_own() {
            self.configs[index].presence_done = true;
            return None;
        }
        if self.plan.presence_announce.is_empty() && self.plan.presence_query.is_empty() {
            self.configs[index].presence_done = true;
            return None;
        }
        let endpoint = self.configs[index].endpoint.clone();
        let Some(request) = build_presence_request(
            &endpoint,
            self.plan.presence_announce.clone(),
            self.plan.presence_query.clone(),
        ) else {
            self.configs[index].presence_done = true;
            return None;
        };
        self.configs[index].presence_done = true;
        let at_ms = self.now_ms;
        let actor = self.configs[index].actor.clone();
        let mut draft =
            ProtocolEventDraft::new(ProtocolEventCode::ActionEmitted, at_ms, "presence_issued")
                .invariants(&["LIVE-01"]);
        if let Some(actor) = actor {
            draft = draft.actor(actor);
        }
        let stage = CoreRelayStage::Presence;
        Some(self.emit(
            stage,
            request,
            ActionIntent::Presence { config: index },
            draft,
        ))
    }

    // -----------------------------------------------------------------------
    // Stage 7a': the cross-family presence query
    // -----------------------------------------------------------------------

    /// Contacts reachable only through another family's relay.
    ///
    /// The other half of the presence-scope decision, and the half that was
    /// dropped with it. Announcing into another family's mailbox tells that
    /// family this device exists, which is a privacy cost with nothing on the
    /// other side of it, and that stays dropped: a probe announces nothing.
    /// The *query* carries no such cost — it names hints the asker already
    /// derives from a card they were given — and without it a contact whose
    /// only endpoint is their own family's relay stops yielding a last-seen
    /// at all, which is the regression this reinstates.
    ///
    /// Two contacts get no probe. One whose endpoint resolves to a mailbox
    /// this device may *poll* is already answered by that config's own
    /// presence call. One whose endpoint this device has written off
    /// (`endpoint_usable == false`) is resting, and a resting endpoint is not
    /// asked anything — including this.
    fn build_presence_probes(&mut self) {
        let own = self.plan.own.clone();
        let mut probes: Vec<PresenceProbe> = Vec::new();
        for contact in self.plan.contacts.clone() {
            if !contact.endpoint_usable {
                continue;
            }
            if resolved_contact_poll_relay(
                contact.relay_url.clone(),
                contact.relay_token.clone(),
                own.as_ref().map(|o| o.url.clone()),
                own.as_ref().map(|o| o.token.clone()),
            )
            .is_some()
            {
                continue;
            }
            // No fallback endpoint: a contact whose card names no mailbox has
            // nowhere to be asked, and asking our own mailbox about them
            // would be the announce-into-someone-else's-family mistake with
            // the arrow reversed.
            let Some(endpoint) =
                resolved_contact_relay(contact.relay_url, contact.relay_token, None, None)
            else {
                continue;
            };
            if probes
                .iter()
                .any(|existing| existing.user_id == contact.user_id)
            {
                continue;
            }
            let cursor_key = relay_cursor_key(endpoint.url.clone(), endpoint.token.clone());
            let actor = if cursor_key.is_empty() {
                None
            } else {
                self.store
                    .protocol_pseudonym("mailbox", cursor_key.as_bytes())
            };
            probes.push(PresenceProbe {
                user_id: contact.user_id,
                endpoint,
                actor,
            });
        }
        self.probes = probes;
        self.probe_index = 0;
    }

    /// At most one query per contact per pass, and at most
    /// [`RELAY_PASS_MAX_PRESENCE_PROBES`] per pass whatever the address book
    /// looks like. `probe_index` only ever moves forward, so a contact
    /// skipped for cadence is skipped for this pass rather than reconsidered.
    fn emit_presence_probe(&mut self) -> Option<CoreRelayAction> {
        while self.probe_index < self.probes.len() {
            if self.presence_probes_issued >= RELAY_PASS_MAX_PRESENCE_PROBES {
                return None;
            }
            let index = self.probe_index;
            self.probe_index += 1;
            let user_id = self.probes[index].user_id.clone();
            let endpoint = self.probes[index].endpoint.clone();
            let actor = self.probes[index].actor.clone();
            // The client-side staleness floor. A cached answer that is still
            // inside it is the answer, and no request is spent re-asking for
            // a bucket that could not have changed.
            if !self
                .store
                .contact_presence_probe_due(
                    user_id.clone(),
                    self.now_ms,
                    RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS,
                )
                .unwrap_or(false)
            {
                continue;
            }
            let query = crate::recent_presence_hints_for(user_id.clone(), self.now_ms);
            if query.is_empty() {
                continue;
            }
            let Some(request) = build_presence_request(&endpoint, Vec::new(), query) else {
                continue;
            };
            // Stamped at emission, not at the answer: a relay that times out,
            // or an older one that refuses the credential outright, must not
            // be asked again next pass. Failure costs the same wait as
            // success.
            let _ = self
                .store
                .mark_contact_presence_probed(user_id, self.now_ms);
            self.presence_probes_issued = self.presence_probes_issued.saturating_add(1);
            let at_ms = self.now_ms;
            let mut draft = ProtocolEventDraft::new(
                ProtocolEventCode::ActionEmitted,
                at_ms,
                "cross_family_presence_issued",
            )
            .invariants(&["LIVE-01", "PRESENCE-01"]);
            if let Some(actor) = actor {
                draft = draft.actor(actor);
            }
            let stage = CoreRelayStage::Presence;
            return Some(self.emit(
                stage,
                request,
                ActionIntent::PresenceProbe { probe: index },
                draft,
            ));
        }
        None
    }

    /// Record what a cross-family relay answered: a bucket, never a stamp.
    fn apply_presence_probe(&mut self, probe: usize, action_id: i64, result: CoreRelayHttpResult) {
        // Our own internet demonstrably works — the same evidence a config's
        // answer carries (`SILENCE-01`). It stops there: a probe answer is
        // not evidence *about the contact's endpoint*, because a deposit
        // credential could answer presence on a relay whose mailbox is
        // failing, so it neither clears a rejection streak nor ends silence.
        self.any_relay_succeeded = true;
        let Some(user_id) = self.probes.get(probe).map(|p| p.user_id.clone()) else {
            return;
        };
        let actor = self.probes.get(probe).and_then(|p| p.actor.clone());
        let recency = match relay_decode_presence_page(result.body) {
            Ok(page) => page
                .presence
                .first()
                .map(|row| relay_presence_recency(page.now_ms.saturating_sub(row.last_seen_ms))),
            Err(_) => None,
        };
        // An empty answer is a real answer: the relay has no presence row for
        // any hint we asked about. Nothing is recorded, because "not seen" is
        // the absence of evidence rather than evidence of absence, and the
        // cached bucket from an earlier pass is not made worse by it.
        if let Some(recency) = recency {
            let _ = self
                .store
                .record_contact_presence(user_id, recency, self.now_ms);
        }
        let mut draft = ProtocolEventDraft::new(
            ProtocolEventCode::ActionResultAccepted,
            self.now_ms,
            "cross_family_presence_accepted",
        )
        .action(action_id)
        .invariants(&["PRESENCE-01"])
        .count("status", i64::from(result.status))
        .count("recorded", i64::from(recency.is_some()));
        if let Some(actor) = actor {
            draft = draft.actor(actor);
        }
        self.note(draft);
    }

    // -----------------------------------------------------------------------
    // Stage 7b: the mailbox walk
    // -----------------------------------------------------------------------

    fn emit_walk_step(&mut self) -> Option<CoreRelayAction> {
        let index = self.config_index;
        if index >= self.configs.len() {
            return None;
        }
        if self.configs[index].walk.is_none() {
            self.start_walk(index);
        }
        let walk = self.configs[index].walk.as_ref()?;
        if walk.done {
            return None;
        }
        if self.plan.fetch_hints.is_empty() {
            self.configs[index].walk.as_mut()?.done = true;
            return None;
        }
        let after_id = walk.after_id;
        let limit = walk.limit;
        let endpoint = self.configs[index].endpoint.clone();
        let Some(request) =
            build_fetch_request(&endpoint, self.plan.fetch_hints.clone(), after_id, limit)
        else {
            self.configs[index].walk.as_mut()?.done = true;
            return None;
        };
        let at_ms = self.now_ms;
        let actor = self.configs[index].actor.clone();
        let mut draft =
            ProtocolEventDraft::new(ProtocolEventCode::ActionEmitted, at_ms, "fetch_issued")
                .invariants(&["PAGE-01", "LIVE-01"])
                .count("requested_rows", i64::from(limit))
                .count("from_cursor", after_id.max(0));
        if let Some(actor) = actor {
            draft = draft.actor(actor);
        }
        // Attempted, not answered. Stage 8 needs both: a contact endpoint may
        // only be rested for silence if this pass actually asked it something
        // (SILENCE-01), and an endpoint nobody polled is not silent.
        self.configs[index].attempted = true;
        Some(self.emit(
            CoreRelayStage::MailboxWalk,
            request,
            ActionIntent::Fetch {
                config: index,
                after_id,
                limit,
            },
            draft,
        ))
    }

    fn start_walk(&mut self, index: usize) {
        let key = self.configs[index].cursor_key.clone();
        let cursor =
            self.store
                .relay_fetch_cursor(key.clone())
                .unwrap_or(crate::store::RelayFetchCursor {
                    after_id: 0,
                    last_sweep_at_ms: 0,
                    sweep_after_id: 0,
                    sweep_started_at_ms: 0,
                });
        let now = self.now_ms;
        let sweeping = relay_sweep_due(
            self.plan.swept_this_session,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            now,
        );
        let mut sweep_progress = cursor.sweep_after_id;
        if sweeping
            && relay_sweep_restart_from_zero(sweep_progress, cursor.sweep_started_at_ms, now)
        {
            let _ = self.store.reset_relay_sweep_progress(key.clone(), now);
            sweep_progress = 0;
            let actor = self.configs[index].actor.clone();
            let mut draft = ProtocolEventDraft::new(
                ProtocolEventCode::SweepRestarted,
                now,
                "resume_cursor_could_not_be_dated",
            )
            .invariants(&["PROGRESS-01"]);
            if let Some(actor) = actor {
                draft = draft.actor(actor);
            }
            self.note(draft);
        }
        let after_id = relay_pass_start_cursor(sweeping, cursor.after_id, sweep_progress);
        self.configs[index].walk = Some(WalkState {
            sweeping,
            after_id,
            limit: relay_fetch_batch_limit(),
            pages: 0,
            envelopes: 0,
            swept_through: after_id,
            done: false,
        });
    }

    // -----------------------------------------------------------------------
    // Emitting
    // -----------------------------------------------------------------------

    fn emit(
        &mut self,
        stage: CoreRelayStage,
        request: CoreRelayHttpRequest,
        intent: ActionIntent,
        draft: ProtocolEventDraft,
    ) -> CoreRelayAction {
        let action_id = self.next_action_id;
        self.next_action_id = self.next_action_id.saturating_add(1);
        self.requests_issued = self.requests_issued.saturating_add(1);
        self.note(draft.action(action_id as i64));
        self.outstanding = Some(Outstanding {
            action_id,
            stage,
            request: request.clone(),
            intent,
        });
        CoreRelayAction {
            pass_id: self.pass_id.clone(),
            action_id,
            stage,
            kind: CoreRelayActionKind::Http { request },
        }
    }

    // -----------------------------------------------------------------------
    // Applying a result
    // -----------------------------------------------------------------------

    fn apply(&mut self, outstanding: Outstanding, result: CoreRelayHttpResult) {
        self.response_bytes_read = self
            .response_bytes_read
            .saturating_add(result.body.len() as u64);

        // A body over the declared cap is answered by asking for less of the
        // same cursor, never by moving past it (PAGE-01). It is the one
        // transport error with a recovery, so it is handled before the rest.
        if result.error == Some(CoreRelayTransportError::BodyTooLarge) {
            self.apply_oversize(&outstanding);
            return;
        }
        if let Some(error) = result.error {
            self.apply_transport_error(&outstanding, error);
            return;
        }

        let status = result.status;
        if (200..300).contains(&status) {
            self.apply_success(outstanding, result);
            return;
        }

        let code = relay_error_code(&result.body);
        let fault = relay_classify_http_error(status, code);
        self.worst_fault = Some(core_worse_relay_fault(self.worst_fault, fault));

        let actor = self.actor_of(&outstanding.intent);
        let mut draft = ProtocolEventDraft::new(
            ProtocolEventCode::RequestRejected,
            self.now_ms,
            relay_fault_outcome(fault),
        )
        .action(outstanding.action_id as i64)
        .invariants(&["SILENCE-01", "RATE-01"])
        .count("status", i64::from(status));
        if let Some(actor) = actor.clone() {
            draft = draft.actor(actor);
        }
        self.note(draft);

        // Held before the rate-limit branch, not after it: a `429` on an ack
        // is still an ack that did not succeed, and `CURSOR-01`'s claim is
        // that the hold is *written down* rather than merely not-done. Held
        // by falling out of the function early, the one transcript that most
        // needs to explain itself — a rate-limited pass — would be the one
        // where a consumed page's frontier goes unrecorded.
        self.hold_frontier_if_ack_failed(&outstanding.intent);

        if fault == CoreRelayFault::RateLimited {
            let retry_after_ms = relay_retry_after_ms(header_value(&result.headers, "Retry-After"));
            self.abort_for_rate_limit(retry_after_ms);
            return;
        }

        match &outstanding.intent {
            ActionIntent::Upload(upload) => {
                // 413 (too large) and 409 (msg_id conflict) are per-envelope
                // and terminal for that one row; the lane continues with the
                // rest. Neither marks the row posted — a non-2xx never reaches
                // `apply_success` — so the envelope stays queued and delivers
                // by mesh/carry and resurfaces on a later pass (DEDUP-01).
                // Every other fault says something about the mailbox rather
                // than the row, so the lane stops spending on it.
                if fault != CoreRelayFault::MessageTooLarge
                    && fault != CoreRelayFault::MsgIdConflict
                {
                    let url = upload.endpoint.url.clone();
                    self.uploads.retain(|queued| queued.endpoint.url != url);
                }
            }
            // A refused cross-family presence query is recorded (the event
            // above) and costs nothing else. Deliberately: the credential a
            // probe carries is post-only, so a relay that has never heard of
            // this route answers `403 deposit_only` and an older one answers
            // `404`, and neither says anything about the contact's card, the
            // contact's mailbox, or the contact. Treating it as a rejection
            // would let a relay upgrade schedule quietly write off every
            // cross-family endpoint in an address book. The rate-limit branch
            // ran before this one, so a `429` still ends the pass and still
            // honours `Retry-After` (`RATE-01`).
            ActionIntent::PresenceProbe { .. } => {}
            ActionIntent::Presence { config }
            | ActionIntent::Fetch { config, .. }
            | ActionIntent::Ack { config, .. } => {
                let index = *config;
                self.configs_faulted = self.configs_faulted.saturating_add(1);
                // An authoritative refusal needs no proof of own
                // connectivity: the server plainly answered (SILENCE-01).
                if let Some(user_id) = self
                    .configs
                    .get(index)
                    .and_then(|c| c.contact_user_id.clone())
                {
                    if crate::contact_relay_fault_is_authoritative(fault) {
                        self.rejections.push(user_id);
                    }
                }
                if matches!(outstanding.intent, ActionIntent::Presence { .. }) {
                    // Decision (b) again, and the half that is easy to lose:
                    // a recorded presence failure must not skip the walk.
                    // Presence runs before the walk, so the config has no
                    // walk yet — and fabricating a finished one here is how a
                    // relay that answers `404` on `/presence` (an older
                    // relayd, a proxy in front of one) would stop this device
                    // fetching its own mail on every pass, forever, while the
                    // summary said the pass completed.
                    if let Some(config) = self.configs.get_mut(index) {
                        config.walk = None;
                    }
                } else if let Some(walk) = self.configs.get_mut(index).and_then(|c| c.walk.as_mut())
                {
                    walk.done = true;
                } else if let Some(config) = self.configs.get_mut(index) {
                    config.walk = Some(WalkState {
                        sweeping: false,
                        after_id: 0,
                        limit: 0,
                        pages: 0,
                        envelopes: 0,
                        swept_through: 0,
                        done: true,
                    });
                }
            }
        }
    }

    fn apply_oversize(&mut self, outstanding: &Outstanding) {
        let ActionIntent::Fetch {
            config,
            after_id,
            limit,
        } = &outstanding.intent
        else {
            // Only a fetch can produce a page over the cap. Anything else is
            // a driver bug, and treating it as an ordinary transport failure
            // is the safe reading.
            self.apply_transport_error(outstanding, CoreRelayTransportError::BodyTooLarge);
            return;
        };
        let index = *config;
        let after_id = *after_id;
        let actor = self.configs.get(index).and_then(|c| c.actor.clone());
        match relay_fetch_shrunk_limit(*limit) {
            Some(smaller) => {
                if let Some(walk) = self.configs.get_mut(index).and_then(|c| c.walk.as_mut()) {
                    walk.limit = smaller;
                    walk.after_id = after_id;
                }
                let mut draft = ProtocolEventDraft::new(
                    ProtocolEventCode::RequestRejected,
                    self.now_ms,
                    "body_cap_exceeded_retry_smaller",
                )
                .action(outstanding.action_id as i64)
                .invariants(&["PAGE-01", "LIVE-01"])
                .count("requested_rows", i64::from(*limit))
                .count("retry_rows", i64::from(smaller))
                .count("from_cursor", after_id.max(0));
                if let Some(actor) = actor {
                    draft = draft.actor(actor);
                }
                self.note(draft);
            }
            None => {
                // A single row that still blows the cap is not a paging
                // problem; nothing smaller can be asked for.
                if let Some(walk) = self.configs.get_mut(index).and_then(|c| c.walk.as_mut()) {
                    walk.done = true;
                }
                self.worst_fault = Some(core_worse_relay_fault(
                    self.worst_fault,
                    CoreRelayFault::Outage,
                ));
                let mut draft = ProtocolEventDraft::new(
                    ProtocolEventCode::RequestRejected,
                    self.now_ms,
                    "single_row_over_body_cap",
                )
                .action(outstanding.action_id as i64)
                .invariants(&["PAGE-01"]);
                if let Some(actor) = actor {
                    draft = draft.actor(actor);
                }
                self.note(draft);
            }
        }
    }

    fn apply_transport_error(&mut self, outstanding: &Outstanding, error: CoreRelayTransportError) {
        if error == CoreRelayTransportError::Cancelled {
            self.cancelled = true;
        }
        self.worst_fault = Some(core_worse_relay_fault(
            self.worst_fault,
            CoreRelayFault::Outage,
        ));
        let actor = self.actor_of(&outstanding.intent);
        let mut draft = ProtocolEventDraft::new(
            ProtocolEventCode::RequestRejected,
            self.now_ms,
            transport_error_outcome(error),
        )
        .action(outstanding.action_id as i64)
        .invariants(&["SILENCE-01"])
        .count("status", 0);
        if let Some(actor) = actor {
            draft = draft.actor(actor);
        }
        self.note(draft);

        self.hold_frontier_if_ack_failed(&outstanding.intent);
        match &outstanding.intent {
            ActionIntent::Upload(upload) => {
                let url = upload.endpoint.url.clone();
                self.uploads.retain(|queued| queued.endpoint.url != url);
            }
            // See `apply_error`: a probe that did not answer is advisory work
            // that did not happen, and nothing rests on it.
            ActionIntent::PresenceProbe { .. } => {}
            ActionIntent::Presence { config }
            | ActionIntent::Fetch { config, .. }
            | ActionIntent::Ack { config, .. } => {
                let index = *config;
                self.configs_faulted = self.configs_faulted.saturating_add(1);
                if matches!(outstanding.intent, ActionIntent::Presence { .. }) {
                    // Decision (b): a presence failure is recorded, not
                    // swallowed — but it does not end the config's walk, and
                    // the walk it does not end is what may still prove the
                    // endpoint answers.
                    if let Some(config) = self.configs.get_mut(index) {
                        config.walk = None;
                    }
                } else if let Some(walk) = self.configs.get_mut(index).and_then(|c| c.walk.as_mut())
                {
                    walk.done = true;
                }
            }
        }
    }

    /// `CURSOR-01`, written down rather than merely not-done: an ack that did
    /// not succeed leaves its page's frontier where it was, through the same
    /// store call an advance would have used, so the transcript carries a
    /// `frontier_held` record naming why. A page consumed but unacked comes
    /// back on the next pass and re-ingests as nothing new.
    fn hold_frontier_if_ack_failed(&mut self, intent: &ActionIntent) {
        let ActionIntent::Ack {
            config,
            page_next_cursor,
            rows,
            ..
        } = intent
        else {
            return;
        };
        self.commit_page(*config, *page_next_cursor, false, *rows);
    }

    fn apply_success(&mut self, outstanding: Outstanding, result: CoreRelayHttpResult) {
        let action_id = outstanding.action_id as i64;
        match outstanding.intent {
            ActionIntent::Upload(upload) => {
                self.any_relay_succeeded = true;
                if Some(&upload.endpoint.url) == self.plan.own.as_ref().map(|o| &o.url) {
                    self.own_relay_succeeded = true;
                }
                let marked = match upload.lane {
                    UploadLane::Receipt => {
                        self.receipt_uploads = self.receipt_uploads.saturating_add(1);
                        self.store
                            .mark_outgoing_receipt_envelope_relay_posted(
                                upload.msg_id.clone(),
                                self.now_ms,
                            )
                            .unwrap_or(false)
                    }
                    UploadLane::Authored => {
                        self.authored_uploads = self.authored_uploads.saturating_add(1);
                        match &upload.fanout {
                            // A group envelope's `relay_posted_at` is terminal
                            // for every member at once, so it may only be
                            // stamped when the last member's row has landed.
                            // The per-member marker is written first and
                            // durably, which is what lets the next pass resume
                            // with the remainder instead of re-posting the
                            // rows that already landed.
                            Some(fanout) => self.mark_fanout_row(&upload.endpoint, fanout),
                            None => self
                                .store
                                .mark_outbound_envelope_relay_posted(
                                    upload.msg_id.clone(),
                                    self.now_ms,
                                )
                                .unwrap_or(false),
                        }
                    }
                    UploadLane::Carried => {
                        self.carried_uploads = self.carried_uploads.saturating_add(1);
                        // MARK-01: the marker is written now, inside the pass
                        // that earned it, not at the end. A pass that dies
                        // after this point has still recorded the upload, and
                        // the next launch does not re-post the row.
                        let marked = self
                            .store
                            .mark_carried_envelope_relay_uploaded(
                                upload.msg_id.clone(),
                                upload.endpoint.url.clone(),
                            )
                            .unwrap_or(false);
                        if marked {
                            self.carried_rows_marked = self.carried_rows_marked.saturating_add(1);
                        }
                        marked
                    }
                };
                if marked {
                    self.progress.uploads_marked = true;
                }
                self.note(
                    ProtocolEventDraft::new(
                        ProtocolEventCode::ActionResultAccepted,
                        self.now_ms,
                        "upload_accepted",
                    )
                    .action(action_id)
                    .invariants(&["IDEMP-01", "MARK-01"])
                    .count("status", i64::from(result.status))
                    .count("marked", i64::from(marked)),
                );
            }
            ActionIntent::Presence { config } => {
                self.mark_answered(config);
                self.note_for(
                    config,
                    ProtocolEventDraft::new(
                        ProtocolEventCode::ActionResultAccepted,
                        self.now_ms,
                        "presence_accepted",
                    )
                    .action(action_id)
                    .count("status", i64::from(result.status)),
                );
            }
            ActionIntent::PresenceProbe { probe } => {
                self.apply_presence_probe(probe, action_id, result);
            }
            ActionIntent::Fetch {
                config,
                after_id,
                limit,
            } => {
                self.mark_answered(config);
                self.apply_fetch(config, after_id, limit, action_id, result);
            }
            ActionIntent::Ack {
                config,
                page_next_cursor,
                rows,
                ack_ids,
            } => {
                self.mark_answered(config);
                self.rows_acked = self.rows_acked.saturating_add(ack_ids as u32);
                self.note_for(
                    config,
                    ProtocolEventDraft::new(
                        ProtocolEventCode::ActionResultAccepted,
                        self.now_ms,
                        "ack_accepted",
                    )
                    .action(action_id)
                    .invariants(&["ACK-01"])
                    .count("status", i64::from(result.status))
                    .count("rows_acked", ack_ids as i64),
                );
                self.commit_page(config, page_next_cursor, true, rows);
            }
        }
    }

    /// Record one landed fan-out row, and stamp the envelope if that was the
    /// last one owed. Returns whether anything durable changed.
    fn mark_fanout_row(&mut self, endpoint: &RelayEndpoint, fanout: &FanoutRow) -> bool {
        let now = self.now_ms;
        let recorded = self
            .store
            .mark_relay_fanout_row_posted(
                fanout.envelope_msg_id.clone(),
                fanout.member_user_id.clone(),
                endpoint.url.clone(),
                now,
            )
            .unwrap_or(false);
        let landed = self
            .store
            .relay_fanout_posted_members(fanout.envelope_msg_id.clone(), endpoint.url.clone())
            .unwrap_or_default()
            .len();
        if landed < fanout.members_owed {
            // Partial. Nothing terminal is written: the remaining members
            // are still eligible next pass, and the ones counted here are not.
            return recorded;
        }
        self.store
            .mark_outbound_envelope_relay_posted(fanout.envelope_msg_id.clone(), now)
            .unwrap_or(false)
            || recorded
    }

    fn mark_answered(&mut self, config: usize) {
        self.any_relay_succeeded = true;
        if let Some(entry) = self.configs.get_mut(config) {
            entry.answered = true;
            if entry.is_own() {
                self.own_relay_succeeded = true;
            }
            if let Some(user_id) = entry.contact_user_id.clone() {
                if !self.recoveries.contains(&user_id) {
                    self.recoveries.push(user_id);
                }
            }
        }
    }

    fn apply_fetch(
        &mut self,
        config: usize,
        after_id: i64,
        _limit: u32,
        action_id: i64,
        result: CoreRelayHttpResult,
    ) {
        let page = match relay_decode_fetch_page(result.body) {
            Ok(page) => page,
            Err(_) => {
                self.worst_fault = Some(core_worse_relay_fault(
                    self.worst_fault,
                    CoreRelayFault::Outage,
                ));
                self.note_for(
                    config,
                    ProtocolEventDraft::new(
                        ProtocolEventCode::RequestRejected,
                        self.now_ms,
                        "page_did_not_decode",
                    )
                    .action(action_id)
                    .invariants(&["PAGE-01"]),
                );
                if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                    walk.done = true;
                }
                return;
            }
        };

        let rows = page.envelopes.len() as u32;
        let continues = relay_fetch_walk_continues(rows, after_id, page.next_cursor);
        self.envelopes_processed = self.envelopes_processed.saturating_add(rows);
        if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
            walk.pages = walk.pages.saturating_add(1);
            walk.envelopes = walk.envelopes.saturating_add(rows);
        }

        if rows == 0 {
            // The empty page is the only end of a walk (PAGE-01). If this was
            // a sweep, it is also the only thing that licenses repairing a
            // frontier that sits above the top of the mailbox.
            let (sweeping, swept_through) = self
                .configs
                .get(config)
                .and_then(|c| c.walk.as_ref())
                .map(|walk| (walk.sweeping, walk.swept_through))
                .unwrap_or((false, 0));
            if sweeping {
                let key = self.configs[config].cursor_key.clone();
                let _ = self
                    .store
                    .note_relay_sweep_completed(key, self.now_ms, swept_through);
            }
            if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                walk.done = true;
            }
            self.note_for(
                config,
                ProtocolEventDraft::new(
                    ProtocolEventCode::PageIngested,
                    self.now_ms,
                    "empty_page_is_eof",
                )
                .action(action_id)
                .invariants(&["PAGE-01"])
                .count("rows_returned", 0),
            );
            return;
        }

        if !continues {
            // A non-empty page that did not move the cursor would loop the
            // walk over the same rows. relayd cannot produce one; a broken or
            // hostile server can, and that is exactly when a client must not
            // spin. The walk terminates here without advancing anything.
            if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                walk.done = true;
            }
            self.frontiers_held = self.frontiers_held.saturating_add(1);
            self.note_for(
                config,
                ProtocolEventDraft::new(
                    ProtocolEventCode::FrontierHeld,
                    self.now_ms,
                    "page_did_not_advance_the_cursor",
                )
                .action(action_id)
                .invariants(&["PAGE-01", "CURSOR-01"])
                .count("rows_returned", i64::from(rows))
                .count("frontier_before", after_id.max(0)),
            );
            return;
        }

        // TXN-01, first transaction. `ingest_relay_page` opens it, commits
        // it, and returns — before an ack request exists, let alone is sent.
        // The pass and action ids ride in so the `page_ingested` record joins
        // the `action_emitted` above it and the ack below it: TXN-01's first
        // transaction is the one point of a transcript that most needs to say
        // which pass consumed the page.
        let ingest = match self.store.ingest_relay_page(
            page.envelopes,
            self.now_ms,
            Some(self.pass_id.clone()),
            action_id,
        ) {
            Ok(ingest) => ingest,
            Err(_) => {
                // Nothing was persisted, so nothing may be acked and the
                // frontier must not move. The walk stops on this config.
                if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                    walk.done = true;
                }
                self.frontiers_held = self.frontiers_held.saturating_add(1);
                self.note_for(
                    config,
                    ProtocolEventDraft::new(
                        ProtocolEventCode::FrontierHeld,
                        self.now_ms,
                        "page_ingest_failed",
                    )
                    .action(action_id)
                    .invariants(&["CURSOR-01", "TXN-01"]),
                );
                return;
            }
        };
        self.rows_ingested = self.rows_ingested.saturating_add(ingest.rows_ingested);
        if ingest.rows_ingested > 0 {
            self.progress.rows_ingested = true;
        }
        if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
            walk.swept_through = walk.swept_through.max(page.next_cursor);
        }

        let ack_ids = self
            .store
            .core_relay_ack_ids_with_consumed(
                ingest.rows.clone(),
                self.plan.own_user_id.clone(),
                self.now_ms,
            )
            .unwrap_or_default();

        if ack_ids.is_empty() {
            // Nothing earned an ack — a page of carried proxy copies, for
            // instance — so the frontier may move on the ingest alone.
            self.commit_page(config, page.next_cursor, ingest.fully_processed, rows);
            return;
        }

        // `max_requests` is exact, and the ack is a request. A page that
        // cannot afford one holds its frontier and comes back next pass —
        // re-ingesting as nothing new — rather than spending a request the
        // budget does not have. Without this the one request a pass can issue
        // past its declared budget is the one nothing gates.
        if self.requests_issued >= self.plan.budgets.max_requests {
            self.budget_yield = true;
            self.note_for(
                config,
                ProtocolEventDraft::new(
                    ProtocolEventCode::BudgetYield,
                    self.now_ms,
                    "ack_deferred_request_budget",
                )
                .action(action_id)
                .invariants(&["LIVE-01", "CURSOR-01"])
                .count("requests_issued", i64::from(self.requests_issued))
                .count("ack_ids", ack_ids.len() as i64),
            );
            self.commit_page(config, page.next_cursor, false, rows);
            return;
        }

        let endpoint = self.configs[config].endpoint.clone();
        let Some(request) = build_ack_request(&endpoint, ack_ids.clone()) else {
            self.commit_page(config, page.next_cursor, false, rows);
            return;
        };
        let actor = self.configs[config].actor.clone();
        let mut draft =
            ProtocolEventDraft::new(ProtocolEventCode::ActionEmitted, self.now_ms, "ack_issued")
                .invariants(&["ACK-01", "TXN-01"])
                .count("ack_ids", ack_ids.len() as i64)
                .count("rows_returned", i64::from(rows));
        if let Some(actor) = actor {
            draft = draft.actor(actor);
        }
        // The ack action is emitted *after* the ingest transaction closed, so
        // the two-transaction shape of TXN-01 is structural rather than
        // remembered.
        self.emit(
            CoreRelayStage::MailboxWalk,
            request,
            ActionIntent::Ack {
                config,
                page_next_cursor: page.next_cursor,
                rows,
                ack_ids: ack_ids.len(),
            },
            draft,
        );
    }

    /// TXN-01, second transaction: advance (or hold) the frontier now that
    /// the acks this page earned are known to have succeeded or failed.
    fn commit_page(
        &mut self,
        config: usize,
        page_next_cursor: i64,
        fully_processed: bool,
        rows: u32,
    ) {
        let Some(entry) = self.configs.get(config) else {
            return;
        };
        let key = entry.cursor_key.clone();
        let sweeping = entry.walk.as_ref().is_some_and(|walk| walk.sweeping);

        let before = self
            .store
            .relay_fetch_cursor(key.clone())
            .map(|cursor| {
                if sweeping {
                    cursor.sweep_after_id
                } else {
                    cursor.after_id
                }
            })
            .unwrap_or(0);
        let after = if sweeping {
            self.store
                .advance_relay_sweep_cursor(
                    key.clone(),
                    page_next_cursor,
                    fully_processed,
                    self.now_ms,
                )
                .unwrap_or(before)
        } else {
            self.store
                .advance_relay_fetch_cursor(key.clone(), page_next_cursor, fully_processed)
                .unwrap_or(before)
        };

        if after > before {
            self.frontier_advances = self.frontier_advances.saturating_add(1);
            self.progress.cursor_advanced = true;
            if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                walk.after_id = after;
                walk.swept_through = walk.swept_through.max(after);
            }
        } else {
            self.frontiers_held = self.frontiers_held.saturating_add(1);
            // A page that did not move the cursor must not be re-fetched in
            // this pass: the same request would return the same page. The
            // walk ends here and the rows come back next pass.
            if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                walk.done = true;
            }
            let _ = rows;
            return;
        }

        // LIVE-01's per-mailbox half. The budget is checked after the page's
        // cursors are persisted, so a yield never strands work.
        let action = self
            .configs
            .get(config)
            .and_then(|c| c.walk.as_ref())
            .map(|walk| relay_mailbox_walk_action(walk.pages, walk.envelopes))
            .unwrap_or(RelayMailboxWalkAction::YieldAndScheduleContinuation);
        if action == RelayMailboxWalkAction::YieldAndScheduleContinuation {
            if let Some(walk) = self.configs.get_mut(config).and_then(|c| c.walk.as_mut()) {
                walk.done = true;
            }
            self.budget_yield = true;
            let (pages, envelopes) = self
                .configs
                .get(config)
                .and_then(|c| c.walk.as_ref())
                .map(|walk| (walk.pages, walk.envelopes))
                .unwrap_or((0, 0));
            self.note_for(
                config,
                ProtocolEventDraft::new(
                    ProtocolEventCode::BudgetYield,
                    self.now_ms,
                    "walk_budget_exhausted",
                )
                .invariants(&["LIVE-01", "PROGRESS-01"])
                .count("pages_used", i64::from(pages))
                .count("envelopes_used", i64::from(envelopes))
                .count("cursor_after", after.max(0)),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Rate-limit abort
    // -----------------------------------------------------------------------

    /// RATE-01: the first family 429 ends every remaining network stage.
    ///
    /// It does not end the pass. Stage 8 still runs, because the rejections
    /// and answers observed before the refusal are real evidence and throwing
    /// them away would lose a rejection the server actually returned.
    fn abort_for_rate_limit(&mut self, retry_after_ms: u64) {
        self.rate_limited = true;
        let jitter = core_family_relay_jitter_ms(self.plan.own_user_id.clone());
        let delay = core_family_relay_backoff_delay_ms(
            retry_after_ms,
            self.plan.consecutive_rate_limits.saturating_add(1),
            jitter,
        );
        // Decision (c): committed here, at the refusal, as a floor. A pass
        // that is cancelled or killed after this point still carries it.
        let quiet_until = self
            .now_ms
            .saturating_add(delay.min(i64::MAX as u64) as i64);
        self.quiet_until_ms = self.quiet_until_ms.max(quiet_until);

        let remaining = self.uploads.len() as i64;
        self.uploads.clear();
        for config in &mut self.configs {
            if let Some(walk) = config.walk.as_mut() {
                walk.done = true;
            } else {
                config.walk = Some(WalkState {
                    sweeping: false,
                    after_id: 0,
                    limit: 0,
                    pages: 0,
                    envelopes: 0,
                    swept_through: 0,
                    done: true,
                });
            }
            config.presence_done = true;
        }
        self.config_index = self.configs.len();
        // Advisory work does not outlive a refusal either: RATE-01 ends every
        // remaining network stage, and the cross-family queries are in one.
        self.probe_index = self.probes.len();
        self.stopped_at = Some(self.stage);
        self.stage = CoreRelayStage::CommitEvidence;

        // The upload stages run before `configs` is built, so during them the
        // mailbox being refused is our own. Naming it from the plan rather
        // than from an empty list keeps the abort record attributable to the
        // mailbox that actually said no.
        let key = self
            .configs
            .first()
            .map(|config| config.cursor_key.clone())
            .filter(|key| !key.is_empty())
            .or_else(|| {
                self.plan
                    .own
                    .as_ref()
                    .map(|own| relay_cursor_key(own.url.clone(), own.token.clone()))
            })
            .unwrap_or_default();
        let _ = self.store.note_relay_rate_limit_abort(
            key,
            delay.min(i64::MAX as u64) as i64,
            i64::from(self.requests_issued),
            remaining,
            self.now_ms,
        );
    }

    // -----------------------------------------------------------------------
    // Stage 8: evidence
    // -----------------------------------------------------------------------

    fn commit_evidence(&mut self, now_ms: i64, silence: SilenceEvidence) {
        // Silence evidence collected during the walks: a contact endpoint
        // this pass attempted and that never answered.
        self.provisional_silence.clear();
        for config in &self.configs {
            let Some(user_id) = config.contact_user_id.clone() else {
                continue;
            };
            if config.answered || !config.attempted {
                continue;
            }
            let key = config.cursor_key.clone();
            if key.is_empty() {
                continue;
            }
            self.provisional_silence.push((user_id, key));
        }

        for user_id in std::mem::take(&mut self.rejections) {
            let _ = self.store.note_contact_relay_rejected(user_id, now_ms);
        }
        for user_id in std::mem::take(&mut self.recoveries) {
            let _ = self.store.clear_contact_relay_unreachable(user_id);
        }

        let provisional = std::mem::take(&mut self.provisional_silence);
        if silence == SilenceEvidence::Discard {
            // A cancelled pass. Every endpoint it was still waiting on was
            // denied its answer by the cancellation rather than by anything
            // the endpoint did, and there is no way from here to tell those
            // two apart per config, so none of it is committed.
            self.silence_discarded = self
                .silence_discarded
                .saturating_add(provisional.len() as u32);
            if !provisional.is_empty() {
                self.note(
                    ProtocolEventDraft::new(
                        ProtocolEventCode::SilenceObserved,
                        now_ms,
                        "silence_discarded_pass_cancelled",
                    )
                    .invariants(&["SILENCE-01"])
                    .count("provisional", provisional.len() as i64)
                    .count("committed", 0),
                );
            }
            return;
        }
        let silence = provisional;
        if self.own_relay_succeeded {
            for (user_id, key) in silence {
                if self
                    .store
                    .note_contact_relay_unreachable(user_id, key, now_ms)
                    .is_ok()
                {
                    self.silence_committed = self.silence_committed.saturating_add(1);
                }
            }
        } else {
            // SILENCE-01. Without proof another relay answered, "the contact
            // went quiet" and "this phone has no internet" are the same
            // observation, and acting on it writes off a healthy contact.
            self.silence_discarded = self.silence_discarded.saturating_add(silence.len() as u32);
            if !silence.is_empty() {
                self.note(
                    ProtocolEventDraft::new(
                        ProtocolEventCode::SilenceObserved,
                        now_ms,
                        "silence_discarded_no_proof",
                    )
                    .invariants(&["SILENCE-01"])
                    .count("provisional", silence.len() as i64)
                    .count("committed", 0),
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Stage 9: finish
    // -----------------------------------------------------------------------

    fn finish(&mut self, outcome: CoreRelayPassOutcome, now_ms: i64) -> CoreRelayPassSummary {
        if let Some(summary) = &self.finished {
            return summary.clone();
        }
        let health = core_relay_pass_health(
            self.worst_fault,
            self.own_relay_succeeded,
            self.any_relay_succeeded,
        );

        // PROGRESS-01. A continuation is scheduled only when the pass bought
        // something: a cursor that strictly advanced, rows that were durably
        // ingested, an upload that was marked — or a quiet window strictly
        // later than the one in force when the pass began, which is the other
        // permitted shape. A pass that did neither ends with none, which is
        // strictly stronger than the rule requires and is what makes an
        // unchanged-state reschedule unrepresentable rather than merely
        // forbidden.
        let continuation = if self.rate_limited && self.quiet_until_ms > self.plan.quiet_until_ms {
            Some(CoreRelayContinuation {
                not_before_ms: self.quiet_until_ms,
                reason: CoreRelayProgressReason::QuietWindowExtended,
            })
        } else if outcome == CoreRelayPassOutcome::Cancelled
            || outcome == CoreRelayPassOutcome::RefusedQuietWindow
        {
            None
        } else if self.budget_yield {
            self.progress.reason().map(|reason| CoreRelayContinuation {
                not_before_ms: now_ms.saturating_add(crate::relay_mailbox_continuation_delay_ms()),
                reason,
            })
        } else {
            None
        };

        if let Some(continuation) = continuation {
            self.note(
                ProtocolEventDraft::new(
                    ProtocolEventCode::ContinuationScheduled,
                    now_ms,
                    continuation_outcome(continuation.reason),
                )
                .invariants(&["PROGRESS-01"])
                .count(
                    "delay_ms",
                    continuation.not_before_ms.saturating_sub(now_ms).max(0),
                )
                .count("deadline_after_ms", continuation.not_before_ms.max(0)),
            );
        }

        let summary = CoreRelayPassSummary {
            pass_id: self.pass_id.clone(),
            started_at_ms: self.started_at_ms,
            finished_at_ms: now_ms,
            outcome,
            health,
            stage_reached: self.stopped_at.unwrap_or(self.stage),
            requests_issued: self.requests_issued,
            envelopes_processed: self.envelopes_processed,
            response_bytes_read: self.response_bytes_read,
            receipt_uploads: self.receipt_uploads,
            authored_uploads: self.authored_uploads,
            carried_uploads: self.carried_uploads,
            carried_rows_marked: self.carried_rows_marked,
            rows_ingested: self.rows_ingested,
            rows_acked: self.rows_acked,
            frontier_advances: self.frontier_advances,
            frontiers_held: self.frontiers_held,
            stale_results_ignored: self.stale_results_ignored,
            configs_walked: self.configs_walked,
            configs_faulted: self.configs_faulted,
            silence_committed: self.silence_committed,
            silence_discarded: self.silence_discarded,
            budgets: self.plan.budgets,
            quiet_until_ms: self.quiet_until_ms,
            continuation,
        };

        if outcome != CoreRelayPassOutcome::RefusedQuietWindow {
            self.note(
                ProtocolEventDraft::new(
                    ProtocolEventCode::PassFinish,
                    now_ms,
                    pass_outcome_token(outcome),
                )
                .invariants(&["LIVE-01"])
                .count("requests", i64::from(summary.requests_issued))
                .count("envelopes", i64::from(summary.envelopes_processed))
                .count("rows_ingested", i64::from(summary.rows_ingested))
                .count("rows_acked", i64::from(summary.rows_acked))
                .count("stale_results", i64::from(summary.stale_results_ignored)),
            );
        }

        self.finished = Some(summary.clone());
        self.outstanding = None;
        summary
    }

    fn actor_of(&self, intent: &ActionIntent) -> Option<String> {
        match intent {
            ActionIntent::Upload(_) => None,
            ActionIntent::PresenceProbe { probe } => {
                self.probes.get(*probe).and_then(|p| p.actor.clone())
            }
            ActionIntent::Presence { config }
            | ActionIntent::Fetch { config, .. }
            | ActionIntent::Ack { config, .. } => {
                self.configs.get(*config).and_then(|c| c.actor.clone())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// The id a pass carries, derived from the label the caller asked for.
///
/// Two properties, and the second is the one with teeth:
///
/// * it is an opaque id — short, lowercase, no punctuation beyond `_-` —
///   because it reaches protocol events, and
/// * it is **unique in this process**, because it is half of `IDEMP-01`'s
///   wrong-pass comparison. Action ids restart at 1 in every pass, so two
///   passes sharing an id makes a late result from the first
///   indistinguishable from the answer the second is waiting for.
///
/// A usable label is kept as the root so a transcript still reads the way the
/// caller meant it to; anything else becomes `p`. Either way the suffix makes
/// the result distinct, and the whole thing stays inside the opaque-id length.
fn derive_pass_id(requested: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);

    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut suffix = String::new();
    let mut value = sequence;
    while {
        const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        suffix.insert(0, DIGITS[(value % 36) as usize] as char);
        value /= 36;
        value > 0
    } {}

    let root = if crate::protocol_event::is_opaque_id(requested) {
        requested
    } else {
        "p"
    };
    // 24 is the opaque-id limit; leave room for the separator and the suffix.
    let room = 24usize.saturating_sub(suffix.len() + 1);
    let root = &root[..root.len().min(room)];
    format!("{root}-{suffix}")
}

/// Where a row addressed to `recipient` is posted.
///
/// Lives outside [`PassState`] so the migration canary in
/// [`crate::session::relay_shadow`] can ask the question against captured
/// values without a store, and gets the *same* answer the running pass would
/// give rather than a second implementation's opinion of it. A live pass
/// reaches it through [`PassState::upload_endpoint_for`].
///
/// A card whose endpoint this device has already written off for *rejections*
/// is worse than no card at all: [`resolved_contact_relay`] returns the
/// contact endpoint unconditionally, so one dead field would beat a working
/// alternative forever and the messages would never leave the queue. Skipping
/// it falls through to our own, exactly as though the card had carried no
/// relay fields — which is what [`resolved_contact_delivery_relay`] does.
///
/// An endpoint resting for *silence* takes the other answer: `None`, meaning
/// "post nothing to this recipient this pass". See
/// [`CoreRelayContactConfig`] for why the two cannot share one answer. `None`
/// writes no marker of any kind, so the row is queued exactly as it was and
/// the mesh paths still carry it.
pub(crate) fn shadow_upload_endpoint_for(
    contacts: &[CoreRelayContactConfig],
    own: Option<&CoreRelayEndpointConfig>,
    recipient: &[u8],
) -> Option<RelayEndpoint> {
    let Some(contact) = contacts
        .iter()
        .find(|candidate| candidate.user_id == recipient)
    else {
        // Not a contact of ours at all — a group id, or an invite recipient.
        // Our own mailbox, as before.
        return resolved_contact_relay(
            None,
            None,
            own.map(|o| o.url.clone()),
            own.map(|o| o.token.clone()),
        );
    };
    if !contact.endpoint_answering {
        return None;
    }
    resolved_contact_delivery_relay(
        contact.relay_url.clone(),
        contact.relay_token.clone(),
        own.map(|o| o.url.clone()),
        own.map(|o| o.token.clone()),
        contact.endpoint_usable,
    )
}

/// Every recipient this pass cannot post to, so the upload queries never spend
/// a batch slot on them.
///
/// Without this an unreachable contact's rows are selected, skipped, and
/// selected again next pass: the batch is bounded, the query orders by
/// recipient rank and then by age, and one contact with a deep queue and a
/// dead endpoint can therefore refill it every pass while live rows behind
/// them never move. The legacy Android engine's `unpostableRecipients`
/// exclusion exists to prevent exactly that; this is the same set, computed
/// from the same two brakes.
pub(crate) fn unpostable_recipients(
    contacts: &[CoreRelayContactConfig],
    own: Option<&CoreRelayEndpointConfig>,
) -> Vec<Vec<u8>> {
    contacts
        .iter()
        .filter(|contact| shadow_upload_endpoint_for(contacts, own, &contact.user_id).is_none())
        .map(|contact| contact.user_id.clone())
        .collect()
}

/// The complete envelope-post request for one row, or `None` when the row
/// cannot be encoded at all.
///
/// Extracted for the same reason as [`shadow_upload_endpoint_for`]: the
/// canary must compare against the bytes a real pass would send, and the only
/// way to guarantee that is for both to come out of one function.
pub(crate) fn shadow_upload_request(
    endpoint: &RelayEndpoint,
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
) -> Option<CoreRelayHttpRequest> {
    let body =
        relay_encode_post_envelope(msg_id, hop_ttl, recipient_hint, sealed, expiry_ms).ok()?;
    Some(CoreRelayHttpRequest {
        operation: CoreRelayOperation::PostEnvelope,
        method: "POST".to_string(),
        base_url: endpoint.url.clone(),
        path: "/envelopes".to_string(),
        headers: auth_headers(&endpoint.token, true),
        body,
        max_response_bytes: relay_max_response_bytes(),
        response_headers_wanted: vec!["Retry-After".to_string()],
    })
}

/// Whether an envelope with these field sizes can be encoded for posting.
///
/// The same validator [`relay_encode_post_envelope`] runs, asked without the
/// payload. The canary needs the answer, not the bytes: a captured row's
/// sealed body can be half a megabyte and holding sixteen of them whole,
/// copying them across a language boundary and cloning them again to encode
/// a body that is immediately discarded costs tens of megabytes on a phone
/// for a question about lengths. Because it is the one validator rather than
/// a second reading of it, a rule that changes changes both answers.
pub(crate) fn shadow_upload_encodable(
    msg_id: &[u8],
    recipient_hint: &[u8],
    sealed_len: u64,
) -> bool {
    relay_validate_envelope_sizes(msg_id.len(), recipient_hint.len(), sealed_len).is_ok()
}

/// The complete fetch request for one step of a mailbox walk, or `None` when
/// the cursor or hint set cannot form a path.
pub(crate) fn build_fetch_request(
    endpoint: &RelayEndpoint,
    hints: Vec<Vec<u8>>,
    after_id: i64,
    limit: u32,
) -> Option<CoreRelayHttpRequest> {
    let path = relay_build_fetch_path(hints, after_id, limit).ok()?;
    Some(CoreRelayHttpRequest {
        operation: CoreRelayOperation::FetchPage,
        method: "GET".to_string(),
        base_url: endpoint.url.clone(),
        path,
        headers: auth_headers(&endpoint.token, false),
        body: Vec::new(),
        max_response_bytes: relay_max_response_bytes(),
        response_headers_wanted: vec!["Retry-After".to_string()],
    })
}

/// The complete ack request for one page's ids, or `None` when they cannot be
/// encoded.
pub(crate) fn build_ack_request(
    endpoint: &RelayEndpoint,
    ack_ids: Vec<i64>,
) -> Option<CoreRelayHttpRequest> {
    let body = relay_encode_ack_request(ack_ids).ok()?;
    Some(CoreRelayHttpRequest {
        operation: CoreRelayOperation::AckPage,
        method: "POST".to_string(),
        base_url: endpoint.url.clone(),
        path: "/envelopes/ack".to_string(),
        headers: auth_headers(&endpoint.token, true),
        body,
        max_response_bytes: relay_max_response_bytes(),
        response_headers_wanted: vec!["Retry-After".to_string()],
    })
}

/// The complete presence request, or `None` when the hints cannot be encoded.
pub(crate) fn build_presence_request(
    endpoint: &RelayEndpoint,
    announce: Vec<Vec<u8>>,
    query: Vec<Vec<u8>>,
) -> Option<CoreRelayHttpRequest> {
    let body = relay_encode_presence_request(announce, query).ok()?;
    Some(CoreRelayHttpRequest {
        operation: CoreRelayOperation::Presence,
        method: "POST".to_string(),
        base_url: endpoint.url.clone(),
        path: "/presence".to_string(),
        headers: auth_headers(&endpoint.token, true),
        body,
        max_response_bytes: relay_max_response_bytes(),
        response_headers_wanted: vec!["Retry-After".to_string()],
    })
}

// ---------------------------------------------------------------------------
// Adapter vectors
// ---------------------------------------------------------------------------

/// One request shape, named, for a driver adapter to assert against.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayAdapterVector {
    pub name: String,
    pub request: CoreRelayHttpRequest,
}

/// The requests a driver must put on the wire unchanged.
///
/// One table, consumed by the Android JVM suite and — when C2 lands — the
/// Swift one, so "byte-exact" is a thing both adapters check against the same
/// bytes rather than each against its own reading of this module.
///
/// Every vector here is built by the *same* function the running pass calls to
/// form that request — [`shadow_upload_request`], [`build_fetch_request`],
/// [`build_ack_request`], [`build_presence_request`] — so a change to a path,
/// a header, a wanted response header or an encoding moves the table with it
/// and both adapter suites go red in the same commit. A vector written out
/// here as its own literal would be a second reading of the pass, and the
/// suites would stay green describing a request core had stopped sending;
/// `relay_shadow_canary.rs` pins each of the four against a request a live
/// pass actually emitted so this stays true.
///
/// What a vector deliberately does *not* carry is the transport headers a
/// shell adds around every relay call — a user agent, a tunnel-bypass hint.
/// Those belong to the HTTP client, are identical for both engines because
/// both go through it, and are not protocol decisions. The adapter suites
/// prove that half by comparing a legacy request and a driver request
/// recorded off the same server, rather than by asserting a header list here.
#[uniffi::export]
pub fn core_relay_adapter_vectors() -> Vec<CoreRelayAdapterVector> {
    let endpoint = RelayEndpoint {
        url: "https://relay.example".to_string(),
        token: "member-token".to_string(),
    };
    let mut vectors = Vec::new();

    if let Some(request) = shadow_upload_request(
        &endpoint,
        vec![0x11; 16],
        4,
        vec![0x22; 8],
        vec![0x33; 48],
        1_700_000_000_000,
    ) {
        vectors.push(CoreRelayAdapterVector {
            name: "post-envelope".to_string(),
            request,
        });
    }

    if let Some(request) =
        build_fetch_request(&endpoint, vec![vec![0x22; 8], vec![0x44; 8]], 8, 256)
    {
        vectors.push(CoreRelayAdapterVector {
            name: "fetch-page".to_string(),
            request,
        });
    }

    if let Some(request) = build_ack_request(&endpoint, vec![3, 5, 8]) {
        vectors.push(CoreRelayAdapterVector {
            name: "ack-page".to_string(),
            request,
        });
    }

    if let Some(request) =
        build_presence_request(&endpoint, vec![vec![0x22; 8]], vec![vec![0x44; 8]])
    {
        vectors.push(CoreRelayAdapterVector {
            name: "presence".to_string(),
            request,
        });
    }

    vectors
}

/// The headers every request carries. `Authorization` is the only place a
/// token appears anywhere in this module.
fn auth_headers(token: &str, has_body: bool) -> Vec<CoreRelayHeader> {
    let mut headers = vec![CoreRelayHeader {
        name: "Authorization".to_string(),
        value: format!("Bearer {token}"),
    }];
    if has_body {
        headers.push(CoreRelayHeader {
            name: "Content-Type".to_string(),
            value: "application/json".to_string(),
        });
    }
    headers.push(CoreRelayHeader {
        name: "Accept".to_string(),
        value: "application/json".to_string(),
    });
    headers
}

fn header_value(headers: &[CoreRelayHeader], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

/// The relay's own `code`, which is authoritative when present — a proxy can
/// rewrite a status, but the body comes from the relay.
fn relay_error_code(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        code: Option<String>,
    }
    if body.is_empty() || body.len() > 4096 {
        return None;
    }
    serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .and_then(|b| b.code)
}

fn relay_fault_outcome(fault: CoreRelayFault) -> &'static str {
    match fault {
        CoreRelayFault::RateLimited => "rate_limited",
        CoreRelayFault::MailboxFull => "mailbox_full",
        CoreRelayFault::MessageTooLarge => "envelope_too_large",
        CoreRelayFault::PassExpired => "pass_expired",
        CoreRelayFault::PassSuspended => "pass_suspended",
        CoreRelayFault::TokenRejected => "token_rejected",
        CoreRelayFault::MsgIdConflict => "msg_id_conflict",
        CoreRelayFault::Outage => "outage",
    }
}

fn transport_error_outcome(error: CoreRelayTransportError) -> &'static str {
    match error {
        CoreRelayTransportError::Timeout => "transport_timeout",
        CoreRelayTransportError::ConnectionFailed => "transport_no_answer",
        CoreRelayTransportError::Tls => "transport_tls_failed",
        CoreRelayTransportError::BodyTooLarge => "transport_body_too_large",
        CoreRelayTransportError::Cancelled => "transport_cancelled",
        CoreRelayTransportError::Other => "transport_failed",
    }
}

fn pass_outcome_token(outcome: CoreRelayPassOutcome) -> &'static str {
    match outcome {
        CoreRelayPassOutcome::Completed => "completed",
        CoreRelayPassOutcome::RateLimited => "aborted_rate_limited",
        CoreRelayPassOutcome::BudgetYield => "budget_yield",
        CoreRelayPassOutcome::Cancelled => "cancelled",
        CoreRelayPassOutcome::NoConfigs => "no_configs",
        CoreRelayPassOutcome::RefusedQuietWindow => "refused_inside_quiet_window",
    }
}

fn continuation_outcome(reason: CoreRelayProgressReason) -> &'static str {
    match reason {
        CoreRelayProgressReason::CursorAdvanced => "cursor_advanced",
        CoreRelayProgressReason::RowsIngested => "rows_ingested",
        CoreRelayProgressReason::UploadsMarked => "uploads_marked",
        CoreRelayProgressReason::QuietWindowExtended => "quiet_window_extended",
    }
}
