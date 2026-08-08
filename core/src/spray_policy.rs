//! Digest-spray policy: how often a peer may be sprayed, how much may be
//! queued at one link, and when an unchanged offer stops being repeated.
//!
//! # Why this module exists
//!
//! Before it, every spray decision was a shell timestamp. `MeshService` kept a
//! `lastDigestAtByAddress` map that only the periodic maintenance tick read;
//! the two event-driven call sites (the HELLO handler and the failover resume)
//! wrote it and sprayed unconditionally. Field observation over 2h46m recorded
//! what that costs: 498 connects in 88 minutes, several spray triggers per
//! reconnect, 40 sprays totalling ~2,400 frames, one link receiving 28
//! consecutive sprays of a byte-identical set, and one reconnect queueing 34
//! copies of an 18,795-byte frame — ~639 KB — into a single BLE link's FIFO
//! inside one second. That is tens of seconds of radio airtime bought by one
//! reconnect, and it is the same shape that watchdog-killed an iPhone standing
//! next to a large carrier.
//!
//! Four decisions were being made ad hoc, and all four are now made here:
//!
//! 1. **Cadence.** May this peer be sprayed at all right now?
//! 2. **Identical-set suppression.** Is this the same offer the peer already
//!    refused, inside the re-offer interval? Asked *per lane*, because the
//!    recorded shape was an invariant authored set alongside a carried set
//!    that walked a cursor — see [`CoreSprayLanePlan`].
//! 3. **Byte budgets.** How much of each lane may this encounter spend, and
//!    how much may be queued at this one link in one burst? The burst
//!    allowance is charged for everything the shells queue at the link, not
//!    only the plan — see [`LINK_BURST_BYTES`].
//! 4. **Receipt-quiet backoff.** How much should the cadence stretch on a link
//!    whose sprays keep producing no evidence of progress?
//!
//! # What this module is not allowed to do
//!
//! It never removes a carried row, never acks anything, and never concludes a
//! peer is broken. `CARRY-01` and `ACK-01` are untouched by everything here:
//! suppressing an offer leaves the queue exactly as it was, and a suppressed
//! offer is re-discoverable — every gate in this file is a *delay* with a
//! finite, computable expiry, never a drop. A peer that legitimately produces
//! no receipts (a courier holding mail for someone who is not here) is a
//! normal, supported peer; the backoff bounds what that costs, it does not
//! decide anything about the peer.
//!
//! # Clocks
//!
//! Every `now_ms` in this module must come from a **monotonic** clock —
//! `SystemClock.elapsedRealtime()` on Android, the same monotonic source
//! `FailoverResumeDebounce` uses on iOS. The old `lastDigestAtByAddress` map
//! used wall-clock milliseconds, so an NTP correction landing mid-session
//! could expire a cadence window early (producing the burst the window exists
//! to prevent) or hold it open indefinitely.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::transport_policy::{should_redigest, REDIGEST_MAX_INTERVAL_MS};

// ---------------------------------------------------------------------------
// Budgets (hoisted from the shells)
// ---------------------------------------------------------------------------

/// Per-encounter budget, in sealed bytes, for spraying *foreign* carried
/// envelopes onward.
///
/// A phone that has been muling for a busy fleet can be holding megabytes for
/// third parties. Offering all of it down one BLE link's single FIFO queues
/// everything behind it for minutes, live replies to real contacts included.
/// Nothing is dropped by the cut: the carry queue is untouched, and the
/// carried cursor resumes the walk on the next round, so a backlog is walked
/// rather than discarded.
///
/// Until this constant existed, the identical number lived in
/// `InboundEnvelopeProcessor.kt` and again in `ProtocolKinds.swift`. They were
/// equal, and nothing made them stay equal.
pub const CARRIED_SPRAY_BUDGET_BYTES: u64 = 256 * 1024;

/// Per-encounter budget, in sealed bytes, for spraying this device's own
/// still-undelivered 1:1 outbound envelopes to a non-recipient mule (muling
/// hook B). Bounds one exchange's traffic, not storage, so it is far smaller
/// than the ~5 MB foreign-carry *storage* budget.
pub const OWN_OUTBOUND_SPRAY_BUDGET_BYTES: u64 = 256 * 1024;

/// Per-encounter budget, in sealed bytes, for spraying this device's own
/// pending outgoing receipts to a mule so it can carry them back toward the
/// original senders. Receipts are tiny (a cumulative watermark, no body), so
/// 64 KiB is hundreds of them — a backstop against a pathological backlog
/// rather than a normal-case limiter.
pub const OWN_RECEIPT_SPRAY_BUDGET_BYTES: u64 = 64 * 1024;

/// Sum of the three per-encounter lane budgets: the most one *unthrottled*
/// encounter could ever offer.
pub const TOTAL_ENCOUNTER_BUDGET_BYTES: u64 =
    CARRIED_SPRAY_BUDGET_BYTES + OWN_OUTBOUND_SPRAY_BUDGET_BYTES + OWN_RECEIPT_SPRAY_BUDGET_BYTES;

/// Most bytes that may be *queued* at one link in one burst, across every lane
/// and every trigger.
///
/// The per-encounter budgets above bound one plan. They never bounded a link,
/// because several triggers fire per reconnect and each built its own plan:
/// the observed 639 KB inside one second was many plans, not one oversized
/// one. This is the bound on the link itself.
///
/// # What is charged against it
///
/// Everything the shells queue at the link, not only the spray plan. The
/// digest-time encounter also runs a receipt repair pass, a per-missing-message
/// re-send loop, an unbounded group catch-up that starts from lamport 0, and a
/// carry drain — between them far more bytes than the plan, and until they were
/// charged the allowance was untouched by the largest lanes it exists to bound.
/// The shells report those bytes through
/// [`CoreSprayPolicy::note_bytes_queued`]; the plan's own bytes are charged by
/// [`CoreSprayPolicy::admit_plan`].
///
/// # It survives the link closing
///
/// The bucket is keyed by link address and is **not** dropped when the link
/// disconnects (see [`CoreSprayPolicy::note_link_closed`]). Dropping it would
/// have made the cap per link *session*, which under the recorded churn — 477
/// disconnects in 88 minutes — resets ~5.7 times a minute and bounds nothing
/// across the churn it was written for. A reconnect to the same address
/// inherits whatever the bucket had accrued in the meantime, so the bound is on
/// the radio over time rather than on one FIFO instance.
///
/// It is set to exactly [`TOTAL_ENCOUNTER_BUDGET_BYTES`] — one encounter's
/// worth — on purpose. A link that has been quiet is not throttled at all, so
/// a genuine first encounter behaves exactly as it did before this module
/// existed; what changes is that the *second* encounter's worth of bytes in
/// the same breath has to wait for the radio. Setting it lower would have
/// silently shrunk every legitimate first sync as a side effect of fixing
/// churn, which is not what issue #280 asks for.
///
/// # Continuation, not truncation
///
/// The allowance is spent by *shrinking the three lane budgets* the encounter
/// is planned with, not by cutting frames off a built plan. That matters:
/// the plan's own selection already stops on a whole envelope and hands back
/// a carried cursor, so a round that could only afford half the queue offers
/// the first half now and resumes from the cursor on the next round. Nothing
/// is dropped, nothing is truncated mid-envelope, and the caller is told when
/// to come back (`retry_after_ms`). A frame-count cap could do none of that —
/// which is the point of issue #280's third ask.
pub const LINK_BURST_BYTES: u64 = TOTAL_ENCOUNTER_BUDGET_BYTES;

/// How fast a link's burst allowance refills, in bytes per second.
///
/// This is a deliberately *conservative* estimate of what a BLE GATT link
/// actually drains — over-estimating it turns the cap into no cap at all. At
/// this rate an exhausted link takes ~10.7s to recover a full
/// [`LINK_BURST_BYTES`] allowance, which is the right order of magnitude for
/// "one burst, then let the radio breathe". LAN links are much faster than
/// this, and pay for it only in that a LAN peer's spray is paced rather than
/// instantaneous; the cadence gate dominates there anyway.
pub const LINK_DRAIN_BYTES_PER_SEC: u64 = 24 * 1024;

/// Allowance a starved link must accrue before it is worth waking a spray for
/// again. Reporting a `retry_after_ms` for the very next byte would have the
/// shells re-arming timers continuously for allowances too small to carry a
/// single envelope.
pub const MIN_USEFUL_BURST_BYTES: u64 = 16 * 1024;

// ---------------------------------------------------------------------------
// Cadence
// ---------------------------------------------------------------------------

/// Minimum interval between full sprays to one peer when the trigger is a
/// reconnect rather than a fresh encounter.
///
/// The observed churn was 498 connects in 88 minutes — roughly 5.7 per minute
/// to one peer — with several spray triggers per connect. One per minute keeps
/// a genuinely useful reconnect responsive (a peer that has been away for a
/// minute and comes back still syncs promptly) while removing the multiplier.
/// It is deliberately shorter than the 3–5 minute maintenance re-digest: a
/// reconnect is *more* likely to carry new state than an idle tick is.
pub const RECONNECT_SPRAY_MIN_INTERVAL_MS: i64 = 60_000;

/// How long after an allowed spray the peer's half of the same exchange is
/// also allowed, without re-gating.
///
/// A digest exchange is one unit of work spread over several frames in both
/// directions: our HELLO, our digest, the peer's digest, our response to it.
/// Gating each frame independently would let the cadence gate deny the
/// *response* to a digest that our own just-allowed spray provoked, which
/// would break convergence on first contact. So one allowed spray opens a
/// window in which the rest of that exchange proceeds. It is short: long
/// enough for a HELLO/DIGEST round trip on a slow BLE link plus the 5s
/// post-reject deferral, too short for reconnect churn to reuse.
///
/// # The window measures a round trip only if the digest goes first
///
/// It is opened when the digest frame is *enqueued*, which is the only moment
/// either shell can observe. That is honest arithmetic only while nothing large
/// is queued ahead of it: at [`LINK_DRAIN_BYTES_PER_SEC`] a full carried drain
/// queued first would hold the digest frame in the FIFO for ~10.7s on its own,
/// and the peer's answer would then land after the window had shut. Both shells
/// therefore enqueue the digest **before** the encounter's bulk lanes at every
/// call site, and the ordering is load-bearing rather than cosmetic.
///
/// Only [`CoreSprayPolicy::note_digest_sent`] opens the window.
/// [`CoreSprayPolicy::admit_plan`] deliberately does not re-open it: a peer
/// digesting us every few seconds would otherwise hold one window open forever
/// and never meet the cadence gate at all.
pub const SPRAY_EXCHANGE_WINDOW_MS: i64 = 15_000;

/// How long since the last spray a peer must be before an encounter counts as
/// genuine first contact again.
///
/// First contact must never be gated — two phones meeting and beginning to
/// sync is the product. Everything else must be. This interval is what
/// separates them, and core decides it from its own record rather than
/// trusting the caller: a shell that labels every reconnect as first contact
/// is silently downgraded, because otherwise the gate would be advisory.
pub const FIRST_CONTACT_LAPSE_MS: i64 = 30 * 60_000;

/// How long an unchanged advertised set stays suppressed before it is offered
/// again in full.
///
/// Deliberately equal to `CARRIED_REWALK_MIN_INTERVAL_MS`, the interval on
/// which a converged link re-walks its carry queue from the top. Both exist
/// for the same reason — a frame can be lost in a link's FIFO when a
/// disconnect lands mid-write, and only a fresh full offer would find it
/// again — so aligning them means a converged pair goes quiet on one schedule
/// instead of two interleaved ones. Shorter would reintroduce the 28
/// identical sprays; longer would make a row lost in a FIFO undiscoverable
/// for longer than the carry lane already allows.
pub const IDENTICAL_SET_REOFFER_INTERVAL_MS: i64 = 30 * 60_000;

/// How many doublings the receipt-quiet backoff may apply.
pub const RECEIPT_QUIET_MAX_SHIFT: u32 = 5;

/// Ceiling on any computed spray interval.
///
/// The backoff **caps waste; it never concludes brokenness**. A courier
/// holding mail for someone who is not here produces no receipts and is
/// behaving perfectly. So the interval stretches and then stops stretching:
/// at worst this peer is offered everything once every half hour, forever.
pub const MAX_SPRAY_INTERVAL_MS: i64 = 30 * 60_000;

/// Longest `retry_after_ms` worth arming a shell timer for. Past this, the
/// ordinary maintenance tick is the cheaper way back.
pub const SPRAY_RETRY_ARM_MAX_MS: i64 = 60_000;

/// How long unused per-peer and per-link state is retained. Bounds the maps
/// against a busy public space without letting churn inside one encounter
/// reset anything.
pub const SPRAY_STATE_RETENTION_MS: i64 = 2 * 60 * 60_000;

/// See [`SPRAY_RETRY_ARM_MAX_MS`]. Exported so neither shell owns the number.
#[uniffi::export]
pub fn core_spray_retry_arm_max_ms() -> i64 {
    SPRAY_RETRY_ARM_MAX_MS
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// What caused a spray to be considered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreSprayTrigger {
    /// A peer appeared and identified itself (HELLO). The caller believes this
    /// is a fresh encounter. Core verifies that against its own record and
    /// downgrades the claim to [`CoreSprayTrigger::Reconnect`] if it has
    /// sprayed this peer within [`FIRST_CONTACT_LAPSE_MS`].
    FirstContact,
    /// The same peer re-established a link, was re-elected onto another route,
    /// or a failover resumed. This is the churn case.
    Reconnect,
    /// The peer sent us a DIGEST and we are about to answer it.
    PeerDigest,
    /// The periodic re-digest tick on a link that has stayed up.
    Maintenance,
}

/// Why a spray was allowed or refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreSprayGateReason {
    /// Genuine first contact with this peer. Never gated.
    FirstContact,
    /// Inside the window opened by an already-allowed spray: this is the rest
    /// of one exchange, not a new one.
    ExchangeOpen,
    /// The trigger's interval has elapsed since the last spray.
    IntervalElapsed,
    /// Too soon after the last spray to this peer.
    CadenceGated,
    /// Too soon, and the interval is stretched because this link's sprays keep
    /// producing no evidence of progress.
    ReceiptQuietBackoff,
    /// The cadence allows it, but this link has no burst allowance left.
    LinkBurstExhausted,
}

/// The answer to "may I spray this peer now, and how much?".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSprayGate {
    pub allow: bool,
    pub reason: CoreSprayGateReason,
    /// Sealed-byte budget for the foreign-carry lane this encounter.
    pub carried_budget_bytes: u64,
    /// Sealed-byte budget for this device's own outbound lane.
    pub own_outbound_budget_bytes: u64,
    /// Sealed-byte budget for this device's own receipt lane.
    pub own_receipt_budget_bytes: u64,
    /// How long until this decision could come out differently, or 0 when the
    /// spray was allowed. Always finite: no gate here is permanent.
    pub retry_after_ms: i64,
    /// Whether `retry_after_ms` is short enough to be worth arming a timer for
    /// rather than waiting for the maintenance tick.
    pub retry_worth_arming: bool,
}

/// Why a built plan was or was not admitted onto the radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreSprayAdmissionReason {
    /// The plan selected nothing; there is nothing to suppress or charge.
    Empty,
    /// At least one lane advertises a set that differs from the one last
    /// sprayed to this peer on that lane.
    SetChanged,
    /// Same set on every lane that had anything, but the re-offer interval has
    /// lapsed: DTN re-discovery.
    ReofferLapsed,
    /// Every non-empty lane is byte-identical to what this peer was last
    /// offered on it, inside the re-offer interval.
    IdenticalSuppressed,
}

/// One lane of a built plan: what it advertises, and what it would cost.
///
/// Suppression is decided **per lane**, not on the union of the three. The
/// union is the wrong unit for the shape the field recorded: 28 consecutive
/// sprays whose authored lane was invariant at 16 envelopes across all of them,
/// alongside a carried lane walking a cursor (21 → 75 rows) and so changing
/// every round. Any carried page turn changes a union digest, so a union
/// digest re-sprays the invariant authored set at full size on every admitted
/// encounter — precisely what issue #280's second ask exists to stop. Per lane,
/// the carried walk proceeds (it is doing real work) and the authored lane goes
/// quiet until it changes or the re-offer interval lapses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSprayLanePlan {
    /// Stable, order-independent digest of the `msg_id`s this lane advertises.
    pub set_digest: u64,
    /// Sealed bytes this lane would put on the wire. Zero means the lane
    /// selected nothing, which is neither suppressed nor charged.
    pub bytes: u64,
}

/// The three lanes of one built plan. Comes straight off
/// [`crate::CoreDigestSprayPlan::lanes`], so the shells never hash anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSprayPlanShape {
    pub carried: CoreSprayLanePlan,
    pub own_outbound: CoreSprayLanePlan,
    pub own_receipts: CoreSprayLanePlan,
}

/// The answer to "this is what the plan came out as; does it go on the radio?".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSprayAdmission {
    /// True when any lane was admitted. A caller with nothing else to do can
    /// read this alone and skip the whole send.
    pub send: bool,
    /// Per-lane verdicts. A caller must send exactly the admitted lanes' frames
    /// and leave every suppressed lane's bookkeeping alone: no carried cursor
    /// advance when `send_carried` is false, no hidden-kind offer recorded when
    /// `send_own_outbound` is false. A suppressed lane has to stay exactly as
    /// re-discoverable as it was.
    pub send_carried: bool,
    pub send_own_outbound: bool,
    pub send_own_receipts: bool,
    pub reason: CoreSprayAdmissionReason,
    /// Bytes charged against this link's burst allowance: the admitted lanes'
    /// bytes, and nothing for the suppressed ones.
    pub charged_bytes: u64,
    /// Soonest a suppressed lane becomes offerable again, or 0 when every
    /// non-empty lane went out.
    pub reoffer_in_ms: i64,
}

// ---------------------------------------------------------------------------
// Set digest
// ---------------------------------------------------------------------------

/// Stable 64-bit digest of an advertised set of `msg_id`s.
///
/// Order-independent by construction: each id is hashed on its own and the
/// per-id hashes are combined with addition and XOR, so the same set produces
/// the same digest whichever order the queries returned it in. It is a change
/// detector, not a security primitive — an adversary controlling what we
/// select is already inside the store.
pub(crate) fn spray_set_digest<'a, I>(msg_ids: I) -> u64
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut sum: u64 = 0;
    let mut xor: u64 = 0;
    let mut count: u64 = 0;
    for id in msg_ids {
        let h = fnv1a64(id);
        sum = sum.wrapping_add(h);
        xor ^= h;
        count += 1;
    }
    // Fold the count in so that a set and the same set with a duplicate
    // element (which XOR alone would erase in pairs) stay distinguishable.
    fnv1a64(&[sum.to_le_bytes(), xor.to_le_bytes(), count.to_le_bytes()].concat())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What one lane last offered this peer. Three of these per peer; see
/// [`CoreSprayLanePlan`] for why suppression is not decided on the union.
#[derive(Clone, Copy, Debug)]
struct LaneState {
    digest: Option<u64>,
    sprayed_at_ms: i64,
}

impl Default for LaneState {
    fn default() -> Self {
        Self {
            digest: None,
            sprayed_at_ms: i64::MIN,
        }
    }
}

#[derive(Clone, Debug)]
struct PeerState {
    /// Monotonic ms of the last spray that actually went out to this peer.
    last_spray_at_ms: i64,
    /// Monotonic ms until which the rest of the current exchange is allowed.
    exchange_open_until_ms: i64,
    /// Consecutive admitted sprays with no evidence of progress since.
    quiet_rounds: u32,
    /// The `quiet_rounds` value the ring has already been told about, so a
    /// deferral is recorded when the backoff *changes* and not on every tick
    /// that it holds. See [`CoreSprayPolicy::note_quiet_backoff`].
    journaled_quiet_rounds: Option<u32>,
    /// Per-lane record of the last set actually sprayed, and when.
    lanes: [LaneState; 3],
    /// Last time anything touched this record; drives retention pruning.
    touched_at_ms: i64,
}

const LANE_CARRIED: usize = 0;
const LANE_OWN_OUTBOUND: usize = 1;
const LANE_OWN_RECEIPTS: usize = 2;

impl PeerState {
    fn new(now_ms: i64) -> Self {
        Self {
            last_spray_at_ms: i64::MIN,
            exchange_open_until_ms: i64::MIN,
            quiet_rounds: 0,
            journaled_quiet_rounds: None,
            lanes: [LaneState::default(); 3],
            touched_at_ms: now_ms,
        }
    }

    fn ever_sprayed(&self) -> bool {
        self.last_spray_at_ms != i64::MIN
    }

    fn since_last_spray_ms(&self, now_ms: i64) -> i64 {
        if !self.ever_sprayed() {
            return i64::MAX;
        }
        now_ms.saturating_sub(self.last_spray_at_ms)
    }
}

#[derive(Clone, Debug)]
struct LinkState {
    /// Burst allowance remaining, in bytes.
    tokens: u64,
    last_refill_ms: i64,
    touched_at_ms: i64,
    /// Whether the ring has already been told this link ran dry. Cleared the
    /// moment it has a usable burst again, so a reconnect storm records one
    /// exhaustion per episode rather than one per reconnect.
    journaled_exhausted: bool,
}

impl LinkState {
    fn new(now_ms: i64, capacity: u64) -> Self {
        Self {
            tokens: capacity,
            last_refill_ms: now_ms,
            touched_at_ms: now_ms,
            journaled_exhausted: false,
        }
    }
}

/// Tunables. Defaults are the constants above; tests vary them so a cadence
/// case does not have to run for minutes of simulated time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SprayPolicyConfig {
    pub reconnect_min_interval_ms: i64,
    pub exchange_window_ms: i64,
    pub first_contact_lapse_ms: i64,
    pub reoffer_interval_ms: i64,
    pub max_interval_ms: i64,
    pub quiet_max_shift: u32,
    pub link_burst_bytes: u64,
    pub link_drain_bytes_per_sec: u64,
    pub carried_budget_bytes: u64,
    pub own_outbound_budget_bytes: u64,
    pub own_receipt_budget_bytes: u64,
    pub retention_ms: i64,
}

impl Default for SprayPolicyConfig {
    fn default() -> Self {
        Self {
            reconnect_min_interval_ms: RECONNECT_SPRAY_MIN_INTERVAL_MS,
            exchange_window_ms: SPRAY_EXCHANGE_WINDOW_MS,
            first_contact_lapse_ms: FIRST_CONTACT_LAPSE_MS,
            reoffer_interval_ms: IDENTICAL_SET_REOFFER_INTERVAL_MS,
            max_interval_ms: MAX_SPRAY_INTERVAL_MS,
            quiet_max_shift: RECEIPT_QUIET_MAX_SHIFT,
            link_burst_bytes: LINK_BURST_BYTES,
            link_drain_bytes_per_sec: LINK_DRAIN_BYTES_PER_SEC,
            carried_budget_bytes: CARRIED_SPRAY_BUDGET_BYTES,
            own_outbound_budget_bytes: OWN_OUTBOUND_SPRAY_BUDGET_BYTES,
            own_receipt_budget_bytes: OWN_RECEIPT_SPRAY_BUDGET_BYTES,
            retention_ms: SPRAY_STATE_RETENTION_MS,
        }
    }
}

#[derive(Default)]
struct SprayState {
    peers: HashMap<String, PeerState>,
    links: HashMap<String, LinkState>,
}

/// Every digest-spray decision, in one place, for both shells.
///
/// One instance per process. Both shells hold it in the same singleton that
/// already owns their router state, and pass the same `peer_key` (hex user id
/// — the *logical* peer, because reconnect churn moves between addresses) and
/// `link_key` (the transport address — because the FIFO being filled is a
/// link's, not a peer's).
#[derive(uniffi::Object)]
pub struct CoreSprayPolicy {
    config: SprayPolicyConfig,
    state: Mutex<SprayState>,
    /// Where spray decisions go when someone is listening.
    ///
    /// A weak-ish attachment rather than a constructor argument on purpose:
    /// both shells build this policy inside the singleton that owns their
    /// router state, and that happens before the store is necessarily open.
    /// Unattached, every emit is a lock and a `None` check.
    ///
    /// The lock is never held across a store call. Drafts are built while the
    /// spray state is held, the guard is dropped, and only then is the store
    /// touched -- so the spray mutex can never be the outer half of a lock
    /// ordering with the store's.
    journal: Mutex<Option<std::sync::Arc<crate::store::MessageStore>>>,
}

impl Default for CoreSprayPolicy {
    fn default() -> Self {
        Self::with_config(SprayPolicyConfig::default())
    }
}

impl CoreSprayPolicy {
    pub(crate) fn with_config(config: SprayPolicyConfig) -> Self {
        Self {
            config,
            state: Mutex::new(SprayState::default()),
            journal: Mutex::new(None),
        }
    }

    /// Write `drafts` to the attached ring, if there is one.
    ///
    /// Call only with no spray lock held.
    fn journal(&self, drafts: Vec<crate::protocol_event::ProtocolEventDraft>) {
        if drafts.is_empty() {
            return;
        }
        let store = {
            let guard = self
                .journal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.clone()
        };
        let Some(store) = store else { return };
        // A diagnostics ring that could fail a spray would be worse than no
        // ring: the store is full, or locked, and the answer to that is to
        // carry on delivering messages.
        let _ = store.record_protocol_events(&drafts);
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, SprayState> {
        // Same reasoning as `transport_policy`'s poison recovery: this guards
        // a pair of plain maps of scheduling state with no multi-step
        // invariant, so a panic elsewhere must not turn every later call into
        // a native crash.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn prune(state: &mut SprayState, now_ms: i64, retention_ms: i64) {
        state
            .peers
            .retain(|_, peer| now_ms.saturating_sub(peer.touched_at_ms) <= retention_ms);
        state
            .links
            .retain(|_, link| now_ms.saturating_sub(link.touched_at_ms) <= retention_ms);
    }

    /// Bytes this link may have queued at it right now.
    fn allowance(&self, state: &mut SprayState, link_key: &str, now_ms: i64) -> u64 {
        let capacity = self.config.link_burst_bytes;
        let rate = self.config.link_drain_bytes_per_sec;
        let link = state
            .links
            .entry(link_key.to_string())
            .or_insert_with(|| LinkState::new(now_ms, capacity));
        let elapsed = now_ms.saturating_sub(link.last_refill_ms);
        if elapsed > 0 {
            let refill = (elapsed as u128 * rate as u128 / 1000) as u64;
            if refill > 0 {
                link.tokens = link.tokens.saturating_add(refill).min(capacity);
                link.last_refill_ms = now_ms;
            }
        } else if elapsed < 0 {
            // The monotonic clock went backwards (a shell passed a wall clock,
            // or a platform reset the base). Re-anchor rather than accrue.
            link.last_refill_ms = now_ms;
        }
        link.touched_at_ms = now_ms;
        link.tokens
    }

    /// Milliseconds until this link accrues `target` bytes of allowance.
    fn ms_to_accrue(&self, current: u64, target: u64) -> i64 {
        if current >= target {
            return 0;
        }
        let missing = target - current;
        let rate = self.config.link_drain_bytes_per_sec.max(1);
        let ms = (missing as u128 * 1000).div_ceil(rate as u128);
        i64::try_from(ms).unwrap_or(i64::MAX)
    }

    /// Split a burst allowance across the three lanes.
    ///
    /// Proportional to the configured lane budgets, never by priority. A
    /// priority split (own mail first, carry with whatever is left) reads as
    /// obviously right and starves the carry lane permanently: the field
    /// device's outbound queue alone is thousands of rows, so "whatever is
    /// left" would be nothing, forever, and this device would stop being a
    /// courier. Proportional keeps every lane moving at a reduced rate.
    fn lane_budgets(&self, allowance: u64) -> (u64, u64, u64) {
        let carried = self.config.carried_budget_bytes;
        let outbound = self.config.own_outbound_budget_bytes;
        let receipts = self.config.own_receipt_budget_bytes;
        let total = carried
            .saturating_add(outbound)
            .saturating_add(receipts)
            .max(1);
        if allowance >= total {
            return (carried, outbound, receipts);
        }
        let scale =
            |lane: u64| -> u64 { (lane as u128 * allowance as u128 / total as u128) as u64 };
        (scale(carried), scale(outbound), scale(receipts))
    }

    /// The interval this peer's next spray must wait out, for this trigger.
    ///
    /// The maintenance tick's *unstretched* window stays owned by
    /// [`should_redigest`], jitter and all, so many simultaneously-established
    /// links still do not all re-digest on the same tick; this is the number
    /// used once the receipt-quiet backoff has something to say, and the
    /// number reported back as `retry_after_ms`.
    fn interval_for(&self, trigger: CoreSprayTrigger, peer: &PeerState) -> i64 {
        let base = match trigger {
            // Verified first contact never reaches here.
            CoreSprayTrigger::FirstContact
            | CoreSprayTrigger::Reconnect
            | CoreSprayTrigger::PeerDigest => self.config.reconnect_min_interval_ms,
            CoreSprayTrigger::Maintenance => REDIGEST_MAX_INTERVAL_MS,
        };
        // The backoff is per peer and blind to which lane went quiet: any
        // admitted lane counts a round, and the only reset signals are a
        // receipt this peer authored about our own mail and a carried-delivery
        // confirmation. A peer that is purely a courier for someone absent can
        // therefore run the shift up on foreign traffic alone.
        //
        // That is survivable for what we *spend* — re-offering costs airtime
        // and stretching it is the whole point — but not for answering the
        // peer. A peer's own DIGEST is the one path that sends the receipts we
        // owe it and the 1:1 backlog its watermark asked for, and quietness on
        // the foreign-carry lane says nothing about either. Throttling that
        // answer out to half an hour would be a signal from one lane braking
        // another, and #241 is what a stuck receipt watermark costs. So the
        // peer's own digest keeps the base interval; the stretch applies to
        // the sprays we initiate.
        if matches!(trigger, CoreSprayTrigger::PeerDigest) {
            return base;
        }
        let shift = peer.quiet_rounds.min(self.config.quiet_max_shift);
        let stretched = base.saturating_mul(1_i64 << shift);
        stretched.min(self.config.max_interval_ms).max(base)
    }

    /// Debit a link's burst bucket, refilling for elapsed time first.
    fn debit(&self, state: &mut SprayState, link_key: &str, bytes: u64, now_ms: i64) {
        let capacity = self.config.link_burst_bytes;
        let rate = self.config.link_drain_bytes_per_sec;
        let link = state
            .links
            .entry(link_key.to_string())
            .or_insert_with(|| LinkState::new(now_ms, capacity));
        let elapsed = now_ms.saturating_sub(link.last_refill_ms);
        if elapsed > 0 {
            let refill = (elapsed as u128 * rate as u128 / 1000) as u64;
            link.tokens = link.tokens.saturating_add(refill).min(capacity);
            link.last_refill_ms = now_ms;
        } else if elapsed < 0 {
            link.last_refill_ms = now_ms;
        }
        link.tokens = link.tokens.saturating_sub(bytes);
        link.touched_at_ms = now_ms;
    }
}

/// Decide one lane, updating its record when it is admitted.
///
/// Returns `(send, reason, reoffer_in_ms)`. An empty lane is not suppressed,
/// not charged, and leaves no record: a lane that selected nothing this round
/// must not look "already offered" next round.
fn admit_lane(
    lane: &mut LaneState,
    plan: CoreSprayLanePlan,
    now_ms: i64,
    reoffer_interval_ms: i64,
) -> (bool, CoreSprayAdmissionReason, i64) {
    if plan.bytes == 0 {
        return (false, CoreSprayAdmissionReason::Empty, 0);
    }
    let identical = lane.digest == Some(plan.set_digest);
    let since = if lane.sprayed_at_ms == i64::MIN {
        i64::MAX
    } else {
        now_ms.saturating_sub(lane.sprayed_at_ms)
    };
    if identical && since < reoffer_interval_ms {
        return (
            false,
            CoreSprayAdmissionReason::IdenticalSuppressed,
            (reoffer_interval_ms - since).max(1),
        );
    }
    let reason = if identical {
        CoreSprayAdmissionReason::ReofferLapsed
    } else {
        CoreSprayAdmissionReason::SetChanged
    };
    lane.digest = Some(plan.set_digest);
    lane.sprayed_at_ms = now_ms;
    (true, reason, 0)
}

/// Counts are non-negative integers on the wire; a byte figure that somehow
/// exceeded `i64::MAX` is clamped rather than dropped.
fn saturate(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[uniffi::export]
impl CoreSprayPolicy {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self::default()
    }

    /// May this peer be sprayed now, and with what per-lane byte budgets?
    ///
    /// Consulted *before* any store work, so reconnect churn costs a map
    /// lookup rather than a full plan build. Mutates nothing: an allowed
    /// spray is recorded by [`Self::note_digest_sent`] or
    /// [`Self::admit_plan`], because a spray that the post-reject cooldown
    /// then defers must not arm the cadence for a burst that never happened.
    pub fn may_spray(
        &self,
        peer_key: String,
        link_key: String,
        trigger: CoreSprayTrigger,
        now_ms: i64,
    ) -> CoreSprayGate {
        let (gate, record) = self.decide_spray(peer_key.clone(), link_key, trigger, now_ms);
        // `decide_spray` has already released the spray lock, and it decided
        // under that lock whether this is a crossing worth recording. This
        // method runs on every reconnect and every maintenance tick, and almost
        // all of those record nothing -- so the common path must not pay for a
        // store read that is about to be thrown away.
        if let Some(record) = record {
            let mut event =
                crate::protocol_event::ProtocolEventDraft::new(record.code, now_ms, record.outcome)
                    .actor(self.peer_pseudonym(&peer_key))
                    .invariants(&["SPRAY-01"])
                    .count("retry_after_ms", gate.retry_after_ms.max(0));
            for (key, value) in record.counts {
                event = event.count(key, value);
            }
            self.journal(vec![event]);
        }
        gate
    }

    /// Attach this policy's decisions to a store's protocol-event ring.
    ///
    /// One call per process, wherever the shell already builds both. Optional
    /// by design -- an unattached policy behaves exactly as it did before this
    /// existed, which is what keeps the tests that predate the ring honest.
    pub fn attach_event_journal(&self, store: std::sync::Arc<crate::store::MessageStore>) {
        let mut guard = self
            .journal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(store);
    }
}

impl CoreSprayPolicy {
    /// The archive-local name for a peer, or a placeholder when nothing is
    /// listening. Never the raw key.
    fn peer_pseudonym(&self, peer_key: &str) -> String {
        let store = {
            let guard = self
                .journal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.clone()
        };
        match store {
            Some(store) => store
                .protocol_event_pseudonym("peer", peer_key.as_bytes())
                .unwrap_or_else(|_| "peer-0".to_string()),
            None => "peer-0".to_string(),
        }
    }

    /// What one admitted (or suppressed) plan is worth recording.
    ///
    /// One record per encounter, never one per envelope: this runs after the
    /// plan is built, which is once per digest exchange with a peer. The
    /// `watchdog-spray` incident is a sequence of these with a large
    /// `charged_bytes`, and `carry-storm` is a sequence of them whose carried
    /// lane never changes -- both readable without any per-row event.
    fn admission_events(
        &self,
        admission: &CoreSprayAdmission,
        lanes: &CoreSprayPlanShape,
        peer_key: &str,
        now_ms: i64,
    ) -> Vec<crate::protocol_event::ProtocolEventDraft> {
        use crate::protocol_event::{ProtocolEventCode, ProtocolEventDraft};
        if admission.reason == CoreSprayAdmissionReason::Empty {
            return Vec::new();
        }
        let peer = self.peer_pseudonym(peer_key);

        let (code, outcome) = if admission.send {
            (
                ProtocolEventCode::SprayAdmitted,
                match admission.reason {
                    CoreSprayAdmissionReason::ReofferLapsed => "reoffer_interval_lapsed",
                    _ => "advertised_set_changed",
                },
            )
        } else {
            (
                ProtocolEventCode::SpraySuppressed,
                "every_lane_identical_to_the_last_offer",
            )
        };
        vec![ProtocolEventDraft::new(code, now_ms, outcome)
            .actor(peer)
            .invariants(&["SPRAY-01"])
            .count("charged_bytes", saturate(admission.charged_bytes))
            .count("carried_bytes", saturate(lanes.carried.bytes))
            .count("own_outbound_bytes", saturate(lanes.own_outbound.bytes))
            .count("own_receipt_bytes", saturate(lanes.own_receipts.bytes))
            .count("reoffer_in_ms", admission.reoffer_in_ms.max(0))]
    }

    fn decide_spray(
        &self,
        peer_key: String,
        link_key: String,
        trigger: CoreSprayTrigger,
        now_ms: i64,
    ) -> (CoreSprayGate, Option<GateRecord>) {
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        let allowance = self.allowance(&mut state, &link_key, now_ms);
        let peer = state
            .peers
            .entry(peer_key.clone())
            .or_insert_with(|| PeerState::new(now_ms));
        peer.touched_at_ms = now_ms;
        let peer = peer.clone();

        let first_contact = matches!(trigger, CoreSprayTrigger::FirstContact)
            && peer.since_last_spray_ms(now_ms) >= self.config.first_contact_lapse_ms;
        let exchange_open =
            matches!(trigger, CoreSprayTrigger::PeerDigest) && now_ms < peer.exchange_open_until_ms;

        let (cadence_ok, reason) = if first_contact {
            (true, CoreSprayGateReason::FirstContact)
        } else if exchange_open {
            (true, CoreSprayGateReason::ExchangeOpen)
        } else {
            let interval = self.interval_for(trigger, &peer);
            let elapsed = peer.since_last_spray_ms(now_ms);
            let due = if trigger == CoreSprayTrigger::Maintenance && peer.quiet_rounds == 0 {
                // Preserve the existing jittered maintenance window exactly.
                !peer.ever_sprayed()
                    || should_redigest(now_ms, peer.last_spray_at_ms, jitter_seed(&link_key))
            } else {
                elapsed >= interval
            };
            if due {
                (true, CoreSprayGateReason::IntervalElapsed)
            } else if peer.quiet_rounds > 0 {
                (false, CoreSprayGateReason::ReceiptQuietBackoff)
            } else {
                (false, CoreSprayGateReason::CadenceGated)
            }
        };

        if !cadence_ok {
            let interval = self.interval_for(trigger, &peer);
            let retry = interval
                .saturating_sub(peer.since_last_spray_ms(now_ms))
                .max(1);
            let record = if matches!(reason, CoreSprayGateReason::ReceiptQuietBackoff) {
                Self::note_quiet_backoff(&mut state, &peer_key)
            } else {
                None
            };
            return (gate_denied(reason, retry), record);
        }
        // The cadence let this peer through, so whatever backoff level was
        // last recorded is over; the next entry into backoff is a new crossing
        // and deserves its own record.
        if let Some(peer) = state.peers.get_mut(&peer_key) {
            peer.journaled_quiet_rounds = None;
        }

        // Deliberately not `allowance > 0`. A trickle of allowance is worse
        // than none: it re-opens the gate every millisecond for a budget too
        // small to carry an envelope, and a caller that then sends one frame
        // anyway walks straight back through the hole this cap exists to
        // close. The link must have accrued something usable first.
        let target = MIN_USEFUL_BURST_BYTES.min(self.config.link_burst_bytes);
        if allowance < target {
            let retry = self.ms_to_accrue(allowance, target);
            let record = Self::note_burst_exhausted(&mut state, &link_key);
            return (
                gate_denied(CoreSprayGateReason::LinkBurstExhausted, retry.max(1)),
                record,
            );
        }
        if let Some(link) = state.links.get_mut(&link_key) {
            link.journaled_exhausted = false;
        }

        let (carried, outbound, receipts) = self.lane_budgets(allowance);
        (
            CoreSprayGate {
                allow: true,
                reason,
                carried_budget_bytes: carried,
                own_outbound_budget_bytes: outbound,
                own_receipt_budget_bytes: receipts,
                retry_after_ms: 0,
                retry_worth_arming: false,
            },
            None,
        )
    }

    /// Record entering — or deepening — the receipt-quiet backoff, once.
    ///
    /// The crossing, not the state, and this is the difference between an
    /// archive that answers "messages stopped arriving yesterday afternoon"
    /// and one that cannot. `may_spray` is consulted for every selected route
    /// on every maintenance tick, and a backed-off peer is denied on nearly
    /// all of them: eight quiet peers on a minute tick write about 11,500
    /// records a day, which is the entire 2,000-record ring replaced every
    /// four hours by one repeated non-event, taking every frontier, sweep and
    /// retirement record from the incident with it.
    ///
    /// So a deferral is written when the backoff *changes* — entering it, and
    /// each doubling of the interval after that. Those are the decisions; the
    /// ticks in between are the same decision being re-asked. `endpoint_rested`
    /// is crossing-only for exactly this reason, and a plain cadence gate is
    /// not recorded at all.
    fn note_quiet_backoff(state: &mut SprayState, peer_key: &str) -> Option<GateRecord> {
        let peer = state.peers.get_mut(peer_key)?;
        if peer.journaled_quiet_rounds == Some(peer.quiet_rounds) {
            return None;
        }
        peer.journaled_quiet_rounds = Some(peer.quiet_rounds);
        Some(GateRecord {
            code: crate::protocol_event::ProtocolEventCode::SprayDeferred,
            outcome: "backed_off_for_want_of_receipts",
            counts: vec![("quiet_rounds", i64::from(peer.quiet_rounds))],
        })
    }

    /// Record a link running out of burst allowance, once per episode.
    ///
    /// Same argument as [`Self::note_quiet_backoff`]: during a BLE reconnect
    /// storm an exhausted link is re-asked on every reconnect, and the answer
    /// does not change until the bucket refills. Cleared as soon as the link
    /// has a usable burst again, so the next dry spell records again.
    fn note_burst_exhausted(state: &mut SprayState, link_key: &str) -> Option<GateRecord> {
        let link = state.links.get_mut(link_key)?;
        if link.journaled_exhausted {
            return None;
        }
        link.journaled_exhausted = true;
        Some(GateRecord {
            code: crate::protocol_event::ProtocolEventCode::SprayBudgetExhausted,
            outcome: "link_burst_allowance_spent",
            counts: Vec::new(),
        })
    }
}

/// A gate decision worth writing to the ring.
///
/// Built only at a crossing — see [`CoreSprayPolicy::note_quiet_backoff`] —
/// and built under the spray lock, because "has this already been recorded?"
/// is part of the same decision. The event itself is written outside the lock.
struct GateRecord {
    code: crate::protocol_event::ProtocolEventCode,
    outcome: &'static str,
    counts: Vec<(&'static str, i64)>,
}

#[uniffi::export]
impl CoreSprayPolicy {
    /// A DIGEST frame actually went out to this peer.
    ///
    /// This is what arms the cadence for the *small* half of the exchange. It
    /// matters because our digest is what provokes the peer's full spray back
    /// at us: leaving it ungated would brake nothing, which is precisely the
    /// shape issue #280 recorded (`sendDigestTo` wrote the timestamp that only
    /// the maintenance tick read).
    pub fn note_digest_sent(&self, peer_key: String, link_key: String, now_ms: i64) {
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        let window = self.config.exchange_window_ms;
        let peer = state
            .peers
            .entry(peer_key)
            .or_insert_with(|| PeerState::new(now_ms));
        peer.last_spray_at_ms = now_ms;
        peer.exchange_open_until_ms = now_ms.saturating_add(window);
        peer.touched_at_ms = now_ms;
        // Touch the link so its allowance keeps accruing against real time.
        let _ = self.allowance(&mut state, &link_key, now_ms);
    }

    /// A plan has been built. Which of its lanes go on the radio, and what does
    /// that cost?
    ///
    /// `lanes` is [`crate::CoreDigestSprayPlan::lanes`] — core computed both
    /// the per-lane digests and the per-lane byte counts while selecting, so
    /// the shells never hash or measure anything.
    ///
    /// A suppressed lane must leave the world untouched: the caller must not
    /// advance a carried cursor when the carried lane is refused, must not
    /// record hidden-kind offers when the own-outbound lane is refused, and
    /// must not send either lane's frames. Nothing here removes or acks
    /// anything either way, so `CARRY-01` and `ACK-01` are unaffected.
    ///
    /// Calling this always arms the peer's cadence, even when every lane comes
    /// back empty. The encounter really happened — the shells run a receipt
    /// repair pass, a per-missing-message re-send and a group catch-up around
    /// this call, none of which the plan can see — so treating "the plan
    /// selected nothing" as "nothing happened" left a hole that re-ran all of
    /// that on every trigger inside the exchange window.
    ///
    /// It deliberately does **not** re-open the exchange window; only
    /// [`Self::note_digest_sent`] does. See [`SPRAY_EXCHANGE_WINDOW_MS`].
    pub fn admit_plan(
        &self,
        peer_key: String,
        link_key: String,
        lanes: CoreSprayPlanShape,
        now_ms: i64,
    ) -> CoreSprayAdmission {
        let peer_key_for_event = peer_key.clone();
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        let reoffer_interval = self.config.reoffer_interval_ms;

        let peer = state
            .peers
            .entry(peer_key)
            .or_insert_with(|| PeerState::new(now_ms));
        peer.touched_at_ms = now_ms;
        peer.last_spray_at_ms = now_ms;

        let per_lane = [
            (LANE_CARRIED, lanes.carried),
            (LANE_OWN_OUTBOUND, lanes.own_outbound),
            (LANE_OWN_RECEIPTS, lanes.own_receipts),
        ];
        let mut sent = [false; 3];
        let mut charged_bytes = 0_u64;
        let mut any_changed = false;
        let mut any_lapsed = false;
        let mut any_suppressed = false;
        let mut soonest_reoffer = i64::MAX;
        for (index, plan) in per_lane {
            let (send, reason, reoffer_in) =
                admit_lane(&mut peer.lanes[index], plan, now_ms, reoffer_interval);
            sent[index] = send;
            if send {
                charged_bytes = charged_bytes.saturating_add(plan.bytes);
                match reason {
                    CoreSprayAdmissionReason::ReofferLapsed => any_lapsed = true,
                    _ => any_changed = true,
                }
            } else if matches!(reason, CoreSprayAdmissionReason::IdenticalSuppressed) {
                any_suppressed = true;
                soonest_reoffer = soonest_reoffer.min(reoffer_in);
            }
        }

        let reason = if any_changed {
            CoreSprayAdmissionReason::SetChanged
        } else if any_lapsed {
            CoreSprayAdmissionReason::ReofferLapsed
        } else if any_suppressed {
            CoreSprayAdmissionReason::IdenticalSuppressed
        } else {
            CoreSprayAdmissionReason::Empty
        };

        if charged_bytes > 0 {
            peer.quiet_rounds = peer.quiet_rounds.saturating_add(1);
            self.debit(&mut state, &link_key, charged_bytes, now_ms);
        }

        let admission = CoreSprayAdmission {
            send: sent.iter().any(|lane| *lane),
            send_carried: sent[LANE_CARRIED],
            send_own_outbound: sent[LANE_OWN_OUTBOUND],
            send_own_receipts: sent[LANE_OWN_RECEIPTS],
            reason,
            charged_bytes,
            reoffer_in_ms: if any_suppressed { soonest_reoffer } else { 0 },
        };
        // The spray lock goes before the store is touched. See `journal`.
        drop(state);
        self.journal(self.admission_events(&admission, &lanes, &peer_key_for_event, now_ms));
        admission
    }

    /// Bytes the shell queued at this link outside a spray plan.
    ///
    /// The digest-time encounter's largest lanes are not in the plan at all:
    /// the receipt repair pass, the per-missing-message re-send loop, the group
    /// catch-up that re-sends every authored group envelope from lamport 0, and
    /// the carry drain. Until they were charged, the "per-link byte cap" capped
    /// the plan rather than the link, and a second trigger inside the exchange
    /// window could re-run all of them against an untouched allowance. This is
    /// how the shells report them.
    ///
    /// It is pure accounting: it never refuses anything. What it changes is
    /// what [`Self::may_spray`] sees next time.
    pub fn note_bytes_queued(&self, link_key: String, bytes: u64, now_ms: i64) {
        if bytes == 0 {
            return;
        }
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        self.debit(&mut state, &link_key, bytes, now_ms);
    }

    /// Evidence that sprays toward this peer are achieving something: a
    /// receipt consumed from it, or carried copies it confirmed holding.
    ///
    /// Resets the receipt-quiet backoff. Absence of this is not evidence of a
    /// fault — a courier for an absent recipient legitimately never produces
    /// it — which is exactly why the backoff has a ceiling and no give-up
    /// state, and why the peer's own digest is exempt from the stretch (see
    /// [`Self::interval_for`]).
    pub fn note_receipt_progress(&self, peer_key: String, now_ms: i64) {
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        let peer = state
            .peers
            .entry(peer_key)
            .or_insert_with(|| PeerState::new(now_ms));
        peer.quiet_rounds = 0;
        peer.journaled_quiet_rounds = None;
        peer.touched_at_ms = now_ms;
    }

    /// Consecutive admitted sprays to this peer with no progress since.
    /// Diagnostics and tests.
    pub fn quiet_rounds(&self, peer_key: String) -> u32 {
        self.locked()
            .peers
            .get(&peer_key)
            .map_or(0, |peer| peer.quiet_rounds)
    }

    /// Bytes this link may currently have queued at it. Diagnostics and tests.
    pub fn link_allowance_bytes(&self, link_key: String, now_ms: i64) -> u64 {
        let mut state = self.locked();
        self.allowance(&mut state, &link_key, now_ms)
    }

    /// A link went away.
    ///
    /// Nothing is dropped. Neither the peer's cadence nor the link's burst
    /// allowance is reset, because a disconnect is exactly the event reconnect
    /// churn produces: 477 of them in 88 minutes was the recorded rate, and
    /// clearing either record on each one would reset the very gates that churn
    /// is what they exist to bound. The bucket keeps accruing against real time
    /// and a reconnect to the same address inherits it; retention pruning is
    /// what eventually collects it.
    ///
    /// The call is kept (rather than deleted) so the shells have one place to
    /// keep the accrual clock anchored to real time across a gap.
    pub fn note_link_closed(&self, link_key: String, now_ms: i64) {
        let mut state = self.locked();
        Self::prune(&mut state, now_ms, self.config.retention_ms);
        let _ = self.allowance(&mut state, &link_key, now_ms);
    }

    /// Mesh stopped. Everything is scheduling state; none of it survives.
    pub fn clear(&self) {
        let mut state = self.locked();
        state.peers.clear();
        state.links.clear();
    }
}

fn gate_denied(reason: CoreSprayGateReason, retry_after_ms: i64) -> CoreSprayGate {
    CoreSprayGate {
        allow: false,
        reason,
        carried_budget_bytes: 0,
        own_outbound_budget_bytes: 0,
        own_receipt_budget_bytes: 0,
        retry_after_ms,
        retry_worth_arming: retry_after_ms <= SPRAY_RETRY_ARM_MAX_MS,
    }
}

fn jitter_seed(link_key: &str) -> u64 {
    fnv1a64(link_key.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport_policy::REDIGEST_MIN_INTERVAL_MS;

    const PEER: &str = "7065657231";
    const LINK: &str = "AA:BB:CC:DD:EE:01";

    fn fast_policy() -> CoreSprayPolicy {
        // Short intervals so a cadence case is a handful of lines rather than
        // minutes of simulated time. Ratios match the shipped constants.
        CoreSprayPolicy::with_config(SprayPolicyConfig {
            reconnect_min_interval_ms: 1_000,
            exchange_window_ms: 200,
            first_contact_lapse_ms: 30_000,
            reoffer_interval_ms: 10_000,
            max_interval_ms: 8_000,
            quiet_max_shift: 3,
            ..SprayPolicyConfig::default()
        })
    }

    /// Shipped byte budgets, cadence and suppression disabled: isolates the
    /// per-link burst cap so a byte assertion is not really a cadence
    /// assertion wearing its coat.
    fn burst_policy() -> CoreSprayPolicy {
        CoreSprayPolicy::with_config(SprayPolicyConfig {
            reconnect_min_interval_ms: 0,
            reoffer_interval_ms: 0,
            ..SprayPolicyConfig::default()
        })
    }

    fn spray(policy: &CoreSprayPolicy, trigger: CoreSprayTrigger, now: i64) -> CoreSprayGate {
        policy.may_spray(PEER.into(), LINK.into(), trigger, now)
    }

    fn empty_lane() -> CoreSprayLanePlan {
        CoreSprayLanePlan {
            set_digest: 0,
            bytes: 0,
        }
    }

    /// A one-lane plan (own outbound), for cases that are not about lanes.
    fn plan(set_digest: u64, bytes: u64) -> CoreSprayPlanShape {
        CoreSprayPlanShape {
            carried: empty_lane(),
            own_outbound: CoreSprayLanePlan { set_digest, bytes },
            own_receipts: empty_lane(),
        }
    }

    fn admit(
        policy: &CoreSprayPolicy,
        set_digest: u64,
        bytes: u64,
        now: i64,
    ) -> CoreSprayAdmission {
        policy.admit_plan(PEER.into(), LINK.into(), plan(set_digest, bytes), now)
    }

    // -- cadence -----------------------------------------------------------

    #[test]
    fn first_contact_is_never_gated() {
        let policy = fast_policy();
        // Two phones meeting must begin to sync immediately. This is the case
        // that must never regress: gating it breaks the product.
        let gate = spray(&policy, CoreSprayTrigger::FirstContact, 0);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::FirstContact);
        assert_eq!(gate.carried_budget_bytes, CARRIED_SPRAY_BUDGET_BYTES);
    }

    #[test]
    fn reconnect_churn_is_gated_between_intervals() {
        let policy = fast_policy();
        let cases: &[(i64, CoreSprayTrigger, bool)] = &[
            (0, CoreSprayTrigger::FirstContact, true),
            (10, CoreSprayTrigger::Reconnect, false),
            (200, CoreSprayTrigger::Reconnect, false),
            (999, CoreSprayTrigger::Reconnect, false),
            (1_000, CoreSprayTrigger::Reconnect, true),
        ];
        for (now, trigger, expected) in cases {
            if *now == 0 {
                assert!(spray(&policy, *trigger, *now).allow);
                policy.note_digest_sent(PEER.into(), LINK.into(), *now);
                continue;
            }
            let gate = spray(&policy, *trigger, *now);
            assert_eq!(gate.allow, *expected, "at {now}ms");
            if !gate.allow {
                assert!(gate.retry_after_ms > 0, "a denial must name its expiry");
            }
        }
    }

    #[test]
    fn a_shell_claiming_first_contact_on_every_reconnect_is_still_gated() {
        // The gate would be advisory if the caller's label were trusted. Core
        // downgrades an unearned FirstContact from its own record.
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        for now in [5, 50, 500, 900] {
            let gate = spray(&policy, CoreSprayTrigger::FirstContact, now);
            assert!(
                !gate.allow,
                "claimed first contact at {now}ms must be downgraded"
            );
        }
    }

    #[test]
    fn first_contact_returns_after_the_lapse() {
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        let gate = spray(&policy, CoreSprayTrigger::FirstContact, 30_000);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::FirstContact);
    }

    #[test]
    fn the_peers_half_of_an_allowed_exchange_is_not_re_gated() {
        // Our own spray provoked this digest. Denying the answer to it would
        // make first contact half-work.
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        let gate = spray(&policy, CoreSprayTrigger::PeerDigest, 150);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::ExchangeOpen);
        // Once the window closes, an unprovoked peer digest is churn again.
        assert!(!spray(&policy, CoreSprayTrigger::PeerDigest, 500).allow);
    }

    // -- identical-set suppression ----------------------------------------

    #[test]
    fn an_unchanged_set_is_suppressed_and_a_changed_one_is_not() {
        let policy = fast_policy();
        let first = admit(&policy, 0xabc, 4_096, 0);
        assert!(first.send);
        assert_eq!(first.reason, CoreSprayAdmissionReason::SetChanged);

        let same = admit(&policy, 0xabc, 4_096, 2_000);
        assert!(!same.send);
        assert_eq!(same.reason, CoreSprayAdmissionReason::IdenticalSuppressed);
        assert!(same.reoffer_in_ms > 0);
        assert_eq!(same.charged_bytes, 0);

        // Any set change sprays immediately, whatever the interval says.
        let changed = admit(&policy, 0xdef, 4_096, 2_001);
        assert!(changed.send);
        assert_eq!(changed.reason, CoreSprayAdmissionReason::SetChanged);
    }

    #[test]
    fn suppression_lapses_so_rows_stay_rediscoverable() {
        // The DTN re-offer mission: a frame lost in a link FIFO is only found
        // again by a fresh full offer. Suppression is a cadence, not a
        // forever.
        let policy = fast_policy();
        assert!(admit(&policy, 7, 4_096, 0).send);
        assert!(!admit(&policy, 7, 4_096, 9_999).send);
        let lapsed = admit(&policy, 7, 4_096, 10_000);
        assert!(lapsed.send);
        assert_eq!(lapsed.reason, CoreSprayAdmissionReason::ReofferLapsed);
    }

    #[test]
    fn an_invariant_authored_lane_goes_quiet_while_the_carried_walk_proceeds() {
        // The recorded shape: 28 consecutive sprays, `authored=16` invariant
        // across all of them, `carried=` swinging 21 -> 75 as the cursor
        // walked. A union-of-all-lanes digest changes on every carried page
        // turn, so it would re-spray the invariant authored set at full size
        // every round -- the exact thing suppression exists to stop.
        let policy = fast_policy();
        let mut authored_sends = 0_u32;
        let mut carried_sends = 0_u32;
        for round in 0..8_i64 {
            let admitted = policy.admit_plan(
                PEER.into(),
                LINK.into(),
                CoreSprayPlanShape {
                    // The carried lane turns a page every round.
                    carried: CoreSprayLanePlan {
                        set_digest: round as u64 + 1,
                        bytes: 8_192,
                    },
                    // The authored lane never changes.
                    own_outbound: CoreSprayLanePlan {
                        set_digest: 0xa017_0ded_u64,
                        bytes: 16_384,
                    },
                    own_receipts: empty_lane(),
                },
                round * 200,
            );
            if admitted.send_carried {
                carried_sends += 1;
            }
            if admitted.send_own_outbound {
                authored_sends += 1;
            }
        }
        assert_eq!(carried_sends, 8, "the carried walk must not be suppressed");
        assert_eq!(
            authored_sends, 1,
            "an invariant authored set must be offered once, not every round"
        );
    }

    #[test]
    fn a_suppressed_lane_does_not_charge_its_bytes() {
        let policy = fast_policy();
        let shape = CoreSprayPlanShape {
            carried: CoreSprayLanePlan {
                set_digest: 1,
                bytes: 1_000,
            },
            own_outbound: CoreSprayLanePlan {
                set_digest: 2,
                bytes: 2_000,
            },
            own_receipts: CoreSprayLanePlan {
                set_digest: 3,
                bytes: 3_000,
            },
        };
        let first = policy.admit_plan(PEER.into(), LINK.into(), shape, 0);
        assert_eq!(first.charged_bytes, 6_000);
        // Only the carried lane turns over.
        let second = policy.admit_plan(
            PEER.into(),
            LINK.into(),
            CoreSprayPlanShape {
                carried: CoreSprayLanePlan {
                    set_digest: 9,
                    bytes: 1_000,
                },
                ..shape
            },
            10,
        );
        assert!(second.send_carried);
        assert!(!second.send_own_outbound);
        assert!(!second.send_own_receipts);
        assert_eq!(second.charged_bytes, 1_000);
        assert!(second.reoffer_in_ms > 0);
    }

    #[test]
    fn an_empty_plan_is_neither_suppressed_nor_charged_but_still_arms_the_cadence() {
        let policy = fast_policy();
        let empty = admit(&policy, 0, 0, 0);
        assert!(!empty.send);
        assert_eq!(empty.reason, CoreSprayAdmissionReason::Empty);
        assert_eq!(empty.charged_bytes, 0);
        assert_eq!(policy.quiet_rounds(PEER.into()), 0);
        // The encounter still happened: the shells run a receipt repair pass,
        // a per-missing-message re-send and a group catch-up around this call,
        // none of which the plan can see. Treating an empty plan as "nothing
        // happened" let all of that re-run on every trigger.
        assert!(!spray(&policy, CoreSprayTrigger::Reconnect, 10).allow);
    }

    #[test]
    fn a_lane_that_selected_nothing_is_not_remembered_as_offered() {
        let policy = fast_policy();
        assert!(admit(&policy, 0x55, 2_048, 0).send);
        // A round where the lane had nothing must not overwrite the record...
        assert!(!admit(&policy, 0, 0, 100).send);
        // ...and must not make the same set look fresh again either.
        assert!(!admit(&policy, 0x55, 2_048, 200).send);
    }

    #[test]
    fn the_set_digest_is_order_independent_and_change_sensitive() {
        let a: &[u8] = b"aaaaaaaaaaaaaaaa";
        let b: &[u8] = b"bbbbbbbbbbbbbbbb";
        let c: &[u8] = b"cccccccccccccccc";
        assert_eq!(spray_set_digest([a, b, c]), spray_set_digest([c, a, b]));
        assert_ne!(spray_set_digest([a, b]), spray_set_digest([a, b, c]));
        assert_ne!(spray_set_digest([a, b]), spray_set_digest([a, c]));
        // XOR alone would erase a duplicated pair; the count fold prevents it.
        assert_ne!(spray_set_digest([a, b]), spray_set_digest([a, b, a, a]));
        assert_eq!(
            spray_set_digest(std::iter::empty::<&[u8]>()),
            spray_set_digest([])
        );
    }

    // -- byte budgets ------------------------------------------------------

    #[test]
    fn budgets_come_from_core_not_the_shells() {
        let policy = CoreSprayPolicy::new();
        let gate = policy.may_spray(PEER.into(), LINK.into(), CoreSprayTrigger::FirstContact, 0);
        assert_eq!(gate.carried_budget_bytes, 256 * 1024);
        assert_eq!(gate.own_outbound_budget_bytes, 256 * 1024);
        assert_eq!(gate.own_receipt_budget_bytes, 64 * 1024);
    }

    #[test]
    fn one_link_cannot_be_queued_more_than_its_burst_in_one_second() {
        // The recorded failure: 34 copies of an 18,795-byte frame — 639 KB —
        // queued at one peer inside one second. Bytes, not frames, is what
        // has to be bounded: 34 frames sounded modest.
        let policy = burst_policy();
        let frame = 18_795_u64;
        let mut queued = 0_u64;
        let mut denied = false;
        // All 34 inside the same millisecond, as recorded (~100ms), so no
        // allowance accrues mid-burst to muddy the arithmetic.
        for round in 0..34_u64 {
            let gate =
                policy.may_spray(PEER.into(), LINK.into(), CoreSprayTrigger::FirstContact, 0);
            if !gate.allow {
                assert_eq!(gate.reason, CoreSprayGateReason::LinkBurstExhausted);
                assert!(gate.retry_after_ms > 0);
                denied = true;
                break;
            }
            // A conforming caller plans inside the budgets it was handed.
            let granted = gate.carried_budget_bytes
                + gate.own_outbound_budget_bytes
                + gate.own_receipt_budget_bytes;
            // Each trigger advertises a different set, so identical-set
            // suppression cannot stand in for the byte cap being tested.
            let planned = frame.min(granted);
            let admitted = policy.admit_plan(PEER.into(), LINK.into(), plan(round, planned), 0);
            if admitted.send {
                queued += admitted.charged_bytes;
            }
        }
        assert!(denied, "the burst cap must actually bite");
        assert!(
            queued <= LINK_BURST_BYTES,
            "queued {queued} bytes at one link"
        );
        assert!(
            queued < 639_030,
            "the recorded 639 KB burst must be impossible"
        );
    }

    #[test]
    fn the_recorded_reconnect_burst_is_impossible_under_the_shipped_policy() {
        // The same 34 triggers inside one second, but with every shipped gate
        // in play. Cadence lets one encounter through; the rest cost a map
        // lookup. This is the whole point of the issue.
        let policy = CoreSprayPolicy::new();
        let frame = 18_795_u64;
        let mut queued = 0_u64;
        let mut admitted_plans = 0_u32;
        for round in 0..34_u64 {
            let now = round as i64 * 30; // ~1 second of reconnect churn
            let gate = policy.may_spray(
                PEER.into(),
                LINK.into(),
                CoreSprayTrigger::FirstContact,
                now,
            );
            if !gate.allow {
                continue;
            }
            // A different advertised set every trigger, so what bounds this is
            // the cadence gate alone -- suppression is not allowed to stand in
            // for it.
            let admitted = policy.admit_plan(PEER.into(), LINK.into(), plan(round, frame), now);
            if admitted.send {
                admitted_plans += 1;
                queued += admitted.charged_bytes;
            }
        }
        assert_eq!(admitted_plans, 1, "churn must not buy a spray per trigger");
        assert_eq!(queued, frame);
    }

    #[test]
    fn an_exhausted_link_recovers_on_its_own_and_says_when() {
        let policy = burst_policy();
        // Spend the whole allowance.
        policy.admit_plan(PEER.into(), LINK.into(), plan(1, LINK_BURST_BYTES), 0);
        assert_eq!(policy.link_allowance_bytes(LINK.into(), 0), 0);
        let gate = policy.may_spray(PEER.into(), LINK.into(), CoreSprayTrigger::FirstContact, 0);
        assert!(!gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::LinkBurstExhausted);
        assert!(gate.retry_after_ms > 0 && gate.retry_after_ms < 60_000);
        assert!(gate.retry_worth_arming);
        // Allowance is time, not luck.
        let later = gate.retry_after_ms;
        assert!(policy.link_allowance_bytes(LINK.into(), later) >= MIN_USEFUL_BURST_BYTES);
        assert_eq!(
            policy.link_allowance_bytes(LINK.into(), 60_000),
            LINK_BURST_BYTES
        );
    }

    #[test]
    fn a_partial_allowance_keeps_every_lane_moving() {
        // A priority split would give the carry lane nothing forever on a
        // device whose own outbound queue is thousands of rows.
        let policy = burst_policy();
        let half = LINK_BURST_BYTES / 2;
        policy.admit_plan(PEER.into(), LINK.into(), plan(1, half), 0);
        let gate = policy.may_spray(PEER.into(), LINK.into(), CoreSprayTrigger::FirstContact, 0);
        assert!(gate.allow);
        assert!(
            gate.carried_budget_bytes > 0,
            "the courier lane must not starve"
        );
        assert!(gate.own_outbound_budget_bytes > 0);
        assert!(gate.own_receipt_budget_bytes > 0);
        let total = gate.carried_budget_bytes
            + gate.own_outbound_budget_bytes
            + gate.own_receipt_budget_bytes;
        assert!(
            total <= half + 3,
            "lane budgets must sum within the allowance"
        );
    }

    #[test]
    fn a_backwards_clock_does_not_mint_allowance() {
        let policy = CoreSprayPolicy::new();
        policy.admit_plan(PEER.into(), LINK.into(), plan(1, LINK_BURST_BYTES), 10_000);
        assert_eq!(policy.link_allowance_bytes(LINK.into(), 10_000), 0);
        // Ten seconds backwards re-anchors the accrual clock; it does not
        // hand back the ten seconds' worth of allowance that never elapsed.
        assert_eq!(policy.link_allowance_bytes(LINK.into(), 0), 0);
        assert!(policy.link_allowance_bytes(LINK.into(), 1) < MIN_USEFUL_BURST_BYTES);
    }

    // -- receipt-quiet backoff --------------------------------------------

    #[test]
    fn receipt_quiet_stretches_the_cadence_and_progress_resets_it() {
        let policy = fast_policy();
        let mut now = 0_i64;
        let mut intervals = Vec::new();
        for round in 0..4_u64 {
            // Each round: spray (set changes every time, so suppression never
            // fires), then measure how long the gate stays shut.
            policy.admit_plan(PEER.into(), LINK.into(), plan(round, 1_024), now);
            let mut waited = 0_i64;
            while !spray(&policy, CoreSprayTrigger::Reconnect, now + waited).allow {
                waited += 100;
                assert!(waited <= 60_000, "the backoff must not be unbounded");
            }
            intervals.push(waited);
            now += waited;
        }
        assert!(
            intervals.windows(2).all(|w| w[1] >= w[0]),
            "intervals must not shrink while receipts stay quiet: {intervals:?}"
        );
        assert!(
            intervals[3] > intervals[0],
            "the cadence must actually stretch"
        );
        assert_eq!(policy.quiet_rounds(PEER.into()), 4);

        policy.note_receipt_progress(PEER.into(), now);
        assert_eq!(policy.quiet_rounds(PEER.into()), 0);
    }

    #[test]
    fn the_backoff_is_bounded_and_never_gives_up() {
        // A courier holding mail for an absent recipient produces no receipts
        // forever, and is behaving correctly. The backoff caps waste; it must
        // never conclude the peer is broken.
        let policy = fast_policy();
        let mut now = 0_i64;
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, now).allow);
        for round in 0..40_u64 {
            policy.admit_plan(PEER.into(), LINK.into(), plan(round, 1_024), now);
            let mut waited = 0_i64;
            while !spray(&policy, CoreSprayTrigger::Reconnect, now + waited).allow {
                waited += 250;
                assert!(
                    waited <= 8_000,
                    "round {round}: interval exceeded the configured ceiling"
                );
            }
            now += waited;
        }
        // Still spraying after 40 receipt-free rounds.
        assert!(spray(&policy, CoreSprayTrigger::Reconnect, now).allow);
    }

    #[test]
    fn first_contact_is_exempt_from_the_backoff() {
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        for round in 0..6_u64 {
            policy.admit_plan(
                PEER.into(),
                LINK.into(),
                plan(round, 1_024),
                round as i64 * 9_000,
            );
        }
        // A genuinely fresh encounter after the lapse still syncs at once,
        // however quiet the peer has been.
        let now = 6 * 9_000 + 30_000;
        let gate = spray(&policy, CoreSprayTrigger::FirstContact, now);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::FirstContact);
    }

    // -- composition -------------------------------------------------------

    /// The three gates that now stand between a peer and a spray:
    ///
    /// 1. #277's post-reject cooldown (shell; a 5s window after a notify
    ///    teardown, which re-arms the burst rather than dropping it),
    /// 2. #269's failover debounce (a coalescing window that always fires),
    /// 3. this module's cadence gate.
    ///
    /// Each is a delay with a finite expiry, so their composition is also a
    /// delay with a finite expiry. This test is the proof by simulation: a
    /// peer that reconnects on a pathological schedule, with both other gates
    /// permanently re-arming, still gets sprayed.
    #[test]
    fn the_three_gates_cannot_compose_into_no_spray_ever() {
        let policy = fast_policy();
        const COOLDOWN_MS: i64 = 5_000;
        const DEBOUNCE_MS: i64 = 3_000;

        let mut now = 0_i64;
        let mut sprays = 0_u32;
        let mut last_spray_at = -1_i64;
        // Reconnects every 120ms for two simulated minutes — worse churn than
        // the 498-in-88-minutes the field recorded.
        while now < 120_000 {
            // Gate 1: the cooldown defers, it never drops. Gate 2: the
            // debounce coalesces the deferral into one resume.
            let effective = now + COOLDOWN_MS + DEBOUNCE_MS;
            let gate = policy.may_spray(
                PEER.into(),
                LINK.into(),
                CoreSprayTrigger::Reconnect,
                effective,
            );
            if gate.allow {
                let admitted = policy.admit_plan(
                    PEER.into(),
                    LINK.into(),
                    plan(sprays as u64, 8_192),
                    effective,
                );
                if admitted.send {
                    sprays += 1;
                    last_spray_at = effective;
                }
            } else {
                assert!(gate.retry_after_ms > 0, "every denial must expire");
            }
            now += 120;
        }
        assert!(
            sprays > 0,
            "a legitimate peer must never be starved of sprays"
        );
        assert!(
            last_spray_at > 60_000,
            "sprays must keep happening, not just at the start"
        );
        // And churn must not buy one spray per reconnect: 1000 reconnects.
        assert!(sprays < 60, "churn bought {sprays} sprays in two minutes");
    }

    #[test]
    fn a_deferred_burst_does_not_arm_the_cadence_it_never_spent() {
        // `may_spray` must not record anything: the post-reject cooldown can
        // still defer a burst core just allowed, and arming on the intent
        // would gate the re-entry that actually does the work.
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        // The shell defers instead of sending. Nothing was recorded, so the
        // re-entry after the cooldown is still first contact.
        let gate = spray(&policy, CoreSprayTrigger::FirstContact, 5_000);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::FirstContact);
    }

    #[test]
    fn a_link_closing_resets_neither_peer_cadence_nor_the_burst_allowance() {
        // A disconnect is what reconnect churn produces -- 477 of them in 88
        // minutes was the recorded rate. Resetting either record on one would
        // hand the churn back the exact bound it defeats.
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        policy.admit_plan(PEER.into(), LINK.into(), plan(1, LINK_BURST_BYTES), 0);
        assert_eq!(policy.link_allowance_bytes(LINK.into(), 0), 0);

        policy.note_link_closed(LINK.into(), 0);

        // The link's spent allowance survives the disconnect: it recovers with
        // time, not with a reconnect.
        assert_eq!(policy.link_allowance_bytes(LINK.into(), 0), 0);
        // And the peer's cadence survives a move to a fresh address.
        assert!(
            !policy
                .may_spray(
                    PEER.into(),
                    "AA:BB:CC:DD:EE:02".into(),
                    CoreSprayTrigger::Reconnect,
                    100
                )
                .allow
        );
    }

    #[test]
    fn bytes_queued_outside_a_plan_are_charged_to_the_link() {
        // The encounter's largest lanes are not in the plan: the receipt
        // repair pass, the per-missing-message re-send loop, and the group
        // catch-up that re-sends every authored group envelope from lamport 0.
        // Until they were charged, a second trigger inside the exchange window
        // re-ran all of them against an untouched allowance.
        let policy = burst_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        // An empty plan, as the group-resend case produces: nothing new to
        // mule, but hundreds of KB already queued around it.
        let admitted = policy.admit_plan(PEER.into(), LINK.into(), plan(0, 0), 0);
        assert_eq!(admitted.charged_bytes, 0);
        policy.note_bytes_queued(LINK.into(), LINK_BURST_BYTES, 0);

        let gate = spray(&policy, CoreSprayTrigger::PeerDigest, 1);
        assert!(!gate.allow, "the second trigger must see a spent link");
        assert_eq!(gate.reason, CoreSprayGateReason::LinkBurstExhausted);
        assert!(gate.retry_after_ms > 0);
    }

    #[test]
    fn answering_a_peers_digest_does_not_hold_the_exchange_window_open_forever() {
        // Otherwise a peer digesting us every few seconds would keep extending
        // its own exemption and never meet the cadence gate at all.
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        // Inside the 200ms window: allowed, and answered with a real plan.
        for (round, now) in [(1_u64, 50_i64), (2, 150)] {
            let gate = spray(&policy, CoreSprayTrigger::PeerDigest, now);
            assert!(gate.allow);
            assert_eq!(gate.reason, CoreSprayGateReason::ExchangeOpen);
            policy.admit_plan(PEER.into(), LINK.into(), plan(round, 4_096), now);
        }
        // The window is measured from our own digest, not from the answers.
        assert!(!spray(&policy, CoreSprayTrigger::PeerDigest, 250).allow);
    }

    #[test]
    fn a_quiet_courier_link_does_not_throttle_the_answer_that_peer_asked_for() {
        // A peer's own DIGEST is the only path that sends the receipts we owe
        // it and the 1:1 backlog its watermark asks for. Quietness on the
        // foreign-carry lane says nothing about either, so it must not stretch
        // that answer out to the ceiling (#241 is what a stuck receipt
        // watermark costs).
        let policy = fast_policy();
        for round in 0..6_u64 {
            policy.admit_plan(
                PEER.into(),
                LINK.into(),
                plan(round, 1_024),
                round as i64 * 9_000,
            );
        }
        assert!(policy.quiet_rounds(PEER.into()) >= 5, "the peer is quiet");
        let last = 5_i64 * 9_000;
        // A spray we would have initiated is stretched...
        assert!(!spray(&policy, CoreSprayTrigger::Reconnect, last + 1_500).allow);
        // ...but the peer's own digest still gets its answer at the base
        // interval.
        let gate = spray(&policy, CoreSprayTrigger::PeerDigest, last + 1_500);
        assert!(gate.allow);
        assert_eq!(gate.reason, CoreSprayGateReason::IntervalElapsed);
    }

    #[test]
    fn maintenance_keeps_its_jittered_three_to_five_minute_window() {
        let policy = CoreSprayPolicy::new();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        assert!(
            !spray(
                &policy,
                CoreSprayTrigger::Maintenance,
                REDIGEST_MIN_INTERVAL_MS - 1
            )
            .allow
        );
        assert!(spray(&policy, CoreSprayTrigger::Maintenance, 5 * 60_000).allow);
    }

    #[test]
    fn state_is_pruned_but_not_inside_an_encounter() {
        let policy = fast_policy();
        assert!(spray(&policy, CoreSprayTrigger::FirstContact, 0).allow);
        policy.note_digest_sent(PEER.into(), LINK.into(), 0);
        // Well inside retention: still gated.
        assert!(!spray(&policy, CoreSprayTrigger::Reconnect, 500).allow);
        // Past retention the record is gone, which is the same answer first
        // contact would give anyway.
        let gate = spray(
            &policy,
            CoreSprayTrigger::FirstContact,
            SPRAY_STATE_RETENTION_MS + 1,
        );
        assert!(gate.allow);
        policy.clear();
        assert_eq!(policy.quiet_rounds(PEER.into()), 0);
    }
}
