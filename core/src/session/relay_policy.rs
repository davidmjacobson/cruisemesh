//! Family relay pacing, 429 backoff, pending rerun, and the pass health fold.
//!
//! ## What lives here and why
//!
//! A CruiseMesh family shares one relay request budget. Every phone in the
//! family spends from the same bucket, so the rules for *how fast a phone may
//! ask* and *what it does when the relay says "too fast"* are protocol
//! decisions, not platform decisions. Until this module existed they were
//! written twice — `FamilyRelayBackpressure.kt` and
//! `FamilyRelayBackpressure.swift` each carried their own constants, their own
//! exponential curve, and their own idea of what to seed the anti-lockstep
//! jitter from. Two copies of a rate limiter is how a family melts its own
//! budget: the re-upload storm of #222 was a rerun that ignored the quiet
//! window a pass had just recorded, and the mid-upload abort of #260/#261 was
//! the other half of the same rule.
//!
//! Contract v1 names the rule `RATE-01`: *the first family 429 ends remaining
//! pass network work; `Retry-After` is a floor, and pending nudges cannot
//! bypass the quiet window.* The floor clamp ([`crate::relay_retry_after_ms`])
//! was already core. This module takes the rest of it.
//!
//! ## The deployed behaviour is the specification
//!
//! This is a hoist, not a redesign. The pacing interval, the backoff curve,
//! its exponent clamp, its cap, the jitter window and the rerun decision are
//! copied from what the Android shell ships today, because that is what the
//! fleet is running. The one deliberate change is the jitter *input*, and it
//! is called out in its own section below.
//!
//! ## Jitter is derived here, from public identity bytes
//!
//! Repeated 429s widen the quiet period. If every phone in a family used the
//! same widened window they would wake in lockstep and re-collide, so each
//! phone adds a small stable offset inside [`FAMILY_RELAY_JITTER_WINDOW_MS`].
//! Stable matters as much as distinct: an offset that changes per process
//! would let a restarting phone jump the queue.
//!
//! Both shells were computing that offset with a *platform hash* of the user
//! id — Android with `ByteArray.contentHashCode()` (`java.util.Arrays.hashCode`,
//! a 31-multiply over bytes) and iOS with a hand-written FNV-1a, added
//! precisely because Swift's own `hashValue` is process-randomized and would
//! not have been stable at all. So the two shells produced different offsets
//! for the same identity, and neither offset was specified anywhere.
//!
//! [`core_family_relay_jitter_ms`] replaces both with a BLAKE2b derivation
//! under a domain-separation context, following the same pattern as every
//! other keyed-name digest in this crate ([`crate::relay_cursor_key`],
//! [`crate::relay_hint_source_digest`]). The input is the *public* user id —
//! the value already printed on a friend card — never a private key, and never
//! a value a platform computes. The output range is unchanged, so the shape of
//! the backoff a family sees is unchanged; only which phone draws which offset
//! moves, and it moves once.
//!
//! ## Everything takes an explicit `now_ms`
//!
//! [`CoreFamilyRelayPacer`] is handed the clock rather than reading one. Both
//! shells feed it a *monotonic* reading (`SystemClock.elapsedRealtime()` on
//! Android, `DispatchTime.now()` on iOS) because a pacer that can be rewound by
//! a wall-clock correction would hand out a wait as long as the correction.
//! Arithmetic here saturates, so a rollback that does happen produces a bounded
//! wait rather than a panic or a wraparound; the tests pin that.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;

use crate::relay_status::{relay_fault_rank, CoreRelayFault};

// ---------------------------------------------------------------------------
// Network cost policy
// ---------------------------------------------------------------------------

/// Whether the operating system can say that the selected internet path is
/// roaming. iOS deliberately reports [`CoreRelayRoaming::Unknown`]: it has no
/// public roaming bit, and core must not invent one from a transport name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayRoaming {
    Yes,
    No,
    Unknown,
}

/// What the relay gate should do for the selected network path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayNetworkVerdict {
    /// The path may run every relay lane.
    Permitted,
    /// Do not start a relay pass. This is an offline-like deferral, never a
    /// relay failure.
    DeferredRoaming,
    /// Run lightweight sync, but leave carried-envelope uploads queued.
    DeferredConstrained,
}

/// Decide whether the current network permits Shore Pass work.
///
/// Android supplies a real roaming bit and is gated precisely on it. iOS has
/// no public roaming API — `CTCarrier` was deprecated in iOS 16 and reports
/// dummy values on current releases — so it supplies `Unknown`, and core
/// deliberately declines to guess. The only signal iOS could stand in with is
/// "expensive", which means cellular, so inferring roaming from it would take
/// Shore Pass away from every iPhone that is off Wi-Fi at home. That trade is
/// not worth making: a roaming iPhone is already protected by the system's own
/// Data Roaming setting, which blocks the traffic at the modem and is stronger
/// than any policy this function could express.
///
/// Constrained paths (Android Data Saver, iOS Low Data Mode) still permit
/// lightweight sync while their carried lane is deferred by
/// [`CoreRelayNetworkVerdict::DeferredConstrained`], on both platforms.
#[uniffi::export]
pub fn core_relay_network_permitted(
    roaming: CoreRelayRoaming,
    constrained: bool,
    user_allows_roaming: bool,
) -> CoreRelayNetworkVerdict {
    if roaming == CoreRelayRoaming::Yes && !user_allows_roaming {
        return CoreRelayNetworkVerdict::DeferredRoaming;
    }
    if constrained {
        return CoreRelayNetworkVerdict::DeferredConstrained;
    }
    CoreRelayNetworkVerdict::Permitted
}

/// A phone's conservative share of the family request budget: one request per
/// 500 ms, serialized. Deployed value; do not change it without a relayd-side
/// measurement, because the bucket it spends from is shared family-wide.
pub const FAMILY_RELAY_REQUEST_INTERVAL_MS: i64 = 500;

/// First quiet window after a 429, before the exponential widening and before
/// the `Retry-After` floor is applied.
pub const FAMILY_RELAY_BACKOFF_BASE_MS: u64 = 1_000;

/// Ceiling on the exponential term. relayd's own `Retry-After` never exceeds
/// 60 s ([`crate::relay_retry_after_ms`]), so a client window wider than that
/// is punishing a family for a condition the server considers already over.
pub const FAMILY_RELAY_BACKOFF_CAP_MS: u64 = 60_000;

/// Width of the per-identity anti-lockstep offset. Small enough to be
/// invisible next to a one-second floor, wide enough to separate a family's
/// phones by more than the pacer interval.
pub const FAMILY_RELAY_JITTER_WINDOW_MS: u64 = 1_000;

/// Largest exponent the 429 curve uses: `1_000 << 6` already exceeds
/// [`FAMILY_RELAY_BACKOFF_CAP_MS`], so nothing beyond it can change an answer.
/// Clamping rather than shifting also keeps a phone that somehow accumulates
/// thousands of consecutive rate limits from shifting past the width of the
/// integer.
const FAMILY_RELAY_BACKOFF_MAX_EXPONENT: u32 = 6;

/// Domain separation for [`core_family_relay_jitter_ms`], distinct from every
/// other BLAKE2b context in the crate so a jitter draw can never collide with
/// a cursor key, a hint-source digest, a deposit token or a message id.
const FAMILY_RELAY_JITTER_CONTEXT: &[u8] = b"cruisemesh family relay backoff jitter v1";

// ---------------------------------------------------------------------------
// Pacing
// ---------------------------------------------------------------------------

/// Serial request pacer. Reserves the next slot and reports how long the
/// caller must wait for it; performing the wait is the shell's job, because
/// only the shell knows whether it is holding a thread, a queue or a timer.
///
/// Reservation, not throttling: the answer is computed and committed in one
/// step, so two racing callers get two different slots rather than the same
/// one twice.
#[derive(uniffi::Object)]
pub struct CoreFamilyRelayPacer {
    interval_ms: i64,
    next_request_at_ms: std::sync::Mutex<i64>,
}

#[uniffi::export]
impl CoreFamilyRelayPacer {
    /// The deployed pacer: [`FAMILY_RELAY_REQUEST_INTERVAL_MS`] between
    /// requests.
    ///
    /// The only constructor a shell can reach, deliberately. The interval is
    /// the family's shared budget expressed as one number, and the exit
    /// criterion for this module is that the number exists once — so there is
    /// no exported door through which a platform could pass its own.
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self::with_interval_ms(FAMILY_RELAY_REQUEST_INTERVAL_MS)
    }

    /// Claims the next request slot and returns the wait, in milliseconds,
    /// before it may be used. Never negative.
    ///
    /// `now_ms` must come from a monotonic source — see the module docs.
    pub fn reserve(&self, now_ms: i64) -> i64 {
        let mut next = match self.next_request_at_ms.lock() {
            Ok(guard) => guard,
            // Every panic-poisoned state in this struct is a plain integer;
            // there is no half-updated invariant to protect, and refusing to
            // pace would be worse than continuing from the recorded slot.
            Err(poisoned) => poisoned.into_inner(),
        };
        let request_at_ms = now_ms.max(*next);
        *next = request_at_ms.saturating_add(self.interval_ms);
        request_at_ms.saturating_sub(now_ms)
    }
}

impl CoreFamilyRelayPacer {
    /// Rust-only constructor, used to test the reservation arithmetic at
    /// intervals the deployed pacer never runs at. Kept out of the
    /// `#[uniffi::export]` block above on purpose: exported, it would be a
    /// second way to choose the pacing interval, reachable from either shell
    /// and asserted by nothing — which is the divergence this module exists to
    /// close, re-opened as a one-liner.
    ///
    /// A negative interval is clamped to zero rather than allowed to run the
    /// reservation backwards.
    pub(crate) fn with_interval_ms(interval_ms: i64) -> Self {
        Self {
            interval_ms: interval_ms.max(0),
            next_request_at_ms: std::sync::Mutex::new(0),
        }
    }
}

impl Default for CoreFamilyRelayPacer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// The stable anti-lockstep offset for one identity, in
/// `0..=FAMILY_RELAY_JITTER_WINDOW_MS`.
///
/// `identity_public_bytes` is a public value — the user id from a friend card.
/// Passing a private key here would be a bug, not a stronger derivation: the
/// offset is observable in request timing, so anything secret fed into it is
/// leaked at whatever rate the phone gets rate limited.
///
/// Empty input is answered rather than rejected, because an identity that has
/// not loaded yet must still be able to back off; it simply shares the offset
/// every other empty identity draws.
#[uniffi::export]
pub fn core_family_relay_jitter_ms(identity_public_bytes: Vec<u8>) -> u64 {
    let mut hasher = Blake2bVar::new(8).expect("valid blake2b output length");
    hasher.update(FAMILY_RELAY_JITTER_CONTEXT);
    hasher.update(&(identity_public_bytes.len() as u64).to_be_bytes());
    hasher.update(&identity_public_bytes);
    let mut out = [0u8; 8];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    u64::from_be_bytes(out) % (FAMILY_RELAY_JITTER_WINDOW_MS + 1)
}

/// How long to stay quiet after a family 429.
///
/// `Retry-After` is a floor and not a ceiling: the server states the minimum
/// it will tolerate, and a client that has now been refused several times in a
/// row has evidence the server's minimum is not enough. So the answer is the
/// larger of the advertised window and this phone's exponential term, plus the
/// phone's stable offset.
///
/// `consecutive_rate_limits` is the count *including* the 429 being handled,
/// so the first one yields [`FAMILY_RELAY_BACKOFF_BASE_MS`]. Zero is treated
/// as one rather than rejected.
///
/// `jitter_ms` is a separate argument rather than an identity so that the
/// curve is exactly testable; [`CoreFamilyRelayBackoff::on_rate_limited`]
/// composes it with [`core_family_relay_jitter_ms`].
#[uniffi::export]
pub fn core_family_relay_backoff_delay_ms(
    retry_after_ms: u64,
    consecutive_rate_limits: u32,
    jitter_ms: u64,
) -> u64 {
    let exponent = consecutive_rate_limits
        .saturating_sub(1)
        .min(FAMILY_RELAY_BACKOFF_MAX_EXPONENT);
    let exponential_ms =
        (FAMILY_RELAY_BACKOFF_BASE_MS << exponent).min(FAMILY_RELAY_BACKOFF_CAP_MS);
    let floor_ms = retry_after_ms.max(exponential_ms);
    floor_ms.saturating_add(jitter_ms)
}

/// The ceiling on the exponential term, for shells and tests that want to
/// assert they are reading core's number rather than one of their own.
#[uniffi::export]
pub fn core_family_relay_backoff_cap_ms() -> u64 {
    FAMILY_RELAY_BACKOFF_CAP_MS
}

/// Consecutive-429 counter plus the curve. One instance per relay-syncing
/// engine; the count is what makes repeated refusals widen the window, and
/// [`CoreFamilyRelayBackoff::on_successful_pass`] is what stops a phone
/// carrying a punishment it has already served.
#[derive(uniffi::Object)]
pub struct CoreFamilyRelayBackoff {
    consecutive_rate_limits: std::sync::Mutex<u32>,
}

#[uniffi::export]
impl CoreFamilyRelayBackoff {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            consecutive_rate_limits: std::sync::Mutex::new(0),
        }
    }

    /// Records one family 429 and returns the quiet window it earns.
    ///
    /// `retry_after_ms` is the already-clamped advertised window from
    /// [`crate::relay_retry_after_ms`]; passing a raw header value here would
    /// let a malformed header collapse or inflate the floor.
    pub fn on_rate_limited(&self, retry_after_ms: u64, identity_public_bytes: Vec<u8>) -> u64 {
        let mut count = self.count();
        *count = count.saturating_add(1);
        let consecutive = *count;
        drop(count);
        core_family_relay_backoff_delay_ms(
            retry_after_ms,
            consecutive,
            core_family_relay_jitter_ms(identity_public_bytes),
        )
    }

    /// A pass that finished with no new 429 clears the streak. Only a whole
    /// completed pass counts: clearing per successful *request* would reset
    /// the widening on the first request after every refusal and flatten the
    /// curve back to the base window.
    pub fn on_successful_pass(&self) {
        *self.count() = 0;
    }

    pub fn consecutive_rate_limits(&self) -> u32 {
        *self.count()
    }
}

impl CoreFamilyRelayBackoff {
    fn count(&self) -> std::sync::MutexGuard<'_, u32> {
        match self.consecutive_rate_limits.lock() {
            Ok(guard) => guard,
            // A poisoned counter is still a valid counter — see the note in
            // `CoreFamilyRelayPacer::reserve`.
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Default for CoreFamilyRelayBackoff {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pending rerun
// ---------------------------------------------------------------------------

/// What a relay engine does with a nudge that arrived while a pass was already
/// running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayRerunAction {
    /// A nudge is pending and nothing forbids syncing: start another pass now.
    RunAgain,
    /// A nudge is pending but the quiet window is still open: hand the nudge
    /// to the coalesced retry timer instead of starting a pass inside it.
    ScheduleRateLimitRetry,
    /// Nothing pending, or syncing is impossible: release the worker.
    Stop,
}

/// `RATE-01`'s second clause, as one decision.
///
/// The rate-limit gate at the front door of a sync request only guards the
/// front door. A nudge that arrives while a pass is already in flight just
/// sets a pending flag, and a pending rerun that starts immediately ignores
/// the window the pass it followed had just recorded. On a phone with a deep
/// carry queue that is back-to-back passes under a second apart, each
/// re-posting a full batch into "too fast", around the clock — the re-upload
/// storm of #222.
///
/// So the rerun consults the same window the front door does. The pending
/// nudge is never *lost*: `ScheduleRateLimitRetry` means it becomes the
/// coalesced retry at the window's end, which is also why several nudges
/// arriving during one window cost one pass rather than one pass each.
///
/// `backoff_remaining_ms` may be negative — an elapsed window — and that reads
/// as no window at all rather than as a very short one.
#[uniffi::export]
pub fn core_relay_rerun_action(
    pending_requested: bool,
    can_sync: bool,
    backoff_remaining_ms: i64,
) -> CoreRelayRerunAction {
    if !pending_requested || !can_sync {
        return CoreRelayRerunAction::Stop;
    }
    if backoff_remaining_ms > 0 {
        return CoreRelayRerunAction::ScheduleRateLimitRetry;
    }
    CoreRelayRerunAction::RunAgain
}

// ---------------------------------------------------------------------------
// Pass health fold
// ---------------------------------------------------------------------------

/// The health one completed relay pass earns, as a domain fact. The shells map
/// it to their own display type and attach their own timestamp; nothing here
/// is a string, and nothing here is localized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayPassHealth {
    /// Our own relay answered and so did every other one we tried.
    Ok,
    /// 507: the family's hosted storage is full.
    QuotaFull,
    /// 413: one queued envelope can never post as-is.
    MessageTooLarge,
    /// 429: self-healing, never an error to act on.
    RateLimited,
    /// 403 `family_expired` on a pass that could not fetch either: relayd's
    /// grace window is over and every operation is refused.
    Expired,
    /// 403 `family_expired` on a pass whose own mailbox still answered: the
    /// read-only grace window. Envelopes queued for us keep arriving and keep
    /// being acked; only new posts take the 403.
    ///
    /// Split out from [`CoreRelayPassHealth::Expired`] because the two need
    /// different sentences. Folded together, a family inside the window was
    /// told their pass had stopped working while their friends' messages were
    /// visibly still landing, which reads as the app being half-broken rather
    /// than as a pass that needs renewing.
    ExpiredReadOnly,
    /// 403 `family_suspended`.
    Suspended,
    /// Any other 401/403 against our own saved credential.
    TokenRejected,
    /// Something failed and said nothing structured about why.
    Failing,
}

/// Worst-of fold for the faults one pass observed against our OWN saved
/// config, using the shared ranking in [`relay_fault_rank`].
///
/// [`CoreRelayFault::Outage`] is deliberately never folded in by callers: an
/// unstructured failure is what the pass's success flags already express as
/// [`CoreRelayPassHealth::Failing`], and admitting it here would let a single
/// dead contact endpoint outrank nothing at all while still displacing the
/// `None` that means "no structured rejection seen".
#[uniffi::export]
pub fn core_worse_relay_fault(
    current: Option<CoreRelayFault>,
    observed: CoreRelayFault,
) -> CoreRelayFault {
    match current {
        None => observed,
        Some(current) => {
            if relay_fault_rank(observed) > relay_fault_rank(current) {
                observed
            } else {
                current
            }
        }
    }
}

/// Fold one pass's worst fault and its success flags into a single health.
///
/// The mailbox-level faults (quota, oversized, rate-limited) surface even when
/// polling succeeded, because relayd keeps serving fetches while rejecting
/// posts; before that was true these rejections vanished into a green check
/// and a silent retry loop.
///
/// [`CoreRelayFault::PassExpired`] belongs in that same group and for the same
/// reason. For relayd's seven-day `FAMILY_EXPIRY_GRACE_MS` an expired pass
/// keeps fetching and acking so nobody's last messages are stranded
/// mid-cruise, and only POSTs take the 403. So the success flags read
/// "reachable" for a week while every new message is rejected, and folding
/// expiry below them told a paying family their pass was working when nothing
/// they wrote was leaving the phone.
///
/// The other two credential faults keep the older precedence, and that is not
/// an oversight: relayd rejects EVERY operation for a suspended family and for
/// an unknown token, so neither can co-occur with a successful poll at all.
/// Expiry-in-grace is the only credential fault that can, which is why it is
/// the only one that moves.
///
/// The success flags then separate the two expiry shapes. A pass that took the
/// 403 on its posts and still got its own mailbox answered is inside the grace
/// window ([`CoreRelayPassHealth::ExpiredReadOnly`]); one that got nothing at
/// all is past it ([`CoreRelayPassHealth::Expired`]). That distinction is
/// derived here rather than read off the wire on purpose: relayd returns the
/// same 403 and the same `family_expired` code either way, deliberately, so
/// that clients need exactly one renewal flow — the asymmetry a person can
/// actually see is which requests worked, and that is what this reads.
#[uniffi::export]
pub fn core_relay_pass_health(
    fault: Option<CoreRelayFault>,
    own_relay_succeeded: bool,
    any_relay_succeeded: bool,
) -> CoreRelayPassHealth {
    match fault {
        Some(CoreRelayFault::MailboxFull) => return CoreRelayPassHealth::QuotaFull,
        Some(CoreRelayFault::MessageTooLarge) => return CoreRelayPassHealth::MessageTooLarge,
        Some(CoreRelayFault::RateLimited) => return CoreRelayPassHealth::RateLimited,
        Some(CoreRelayFault::PassExpired) => {
            return if own_relay_succeeded && any_relay_succeeded {
                CoreRelayPassHealth::ExpiredReadOnly
            } else {
                CoreRelayPassHealth::Expired
            };
        }
        _ => {}
    }
    if own_relay_succeeded && any_relay_succeeded {
        return CoreRelayPassHealth::Ok;
    }
    match fault {
        Some(CoreRelayFault::PassSuspended) => CoreRelayPassHealth::Suspended,
        Some(CoreRelayFault::TokenRejected) => CoreRelayPassHealth::TokenRejected,
        _ => CoreRelayPassHealth::Failing,
    }
}

// ---------------------------------------------------------------------------
// Conformance vectors
// ---------------------------------------------------------------------------
//
// These tables are exported, not `#[cfg(test)]`, on purpose. The exit
// criterion for this hoist is that the Rust suite, the Android JVM suite and
// the Swift XCTest suite consume *the same values*, so that a marshalling bug
// at either FFI boundary shows up as a vector mismatch rather than as a
// platform test that quietly asserts something slightly different. A file
// checked into `core/tests/` could not be read by an XCTest bundle without new
// resource plumbing; a table that crosses UniFFI can be read by all three with
// none. They are a few dozen rows and cost nothing at runtime.
//
// So the placement is deliberate and load-bearing, not an oversight: moving
// them behind a `cfg`/feature to trim the shipped FFI surface would not fail
// loudly, it would silently drop the Swift suite's only source of expectations
// and end the three-way agreement. Anyone who wants them off the release
// surface has to rehome the Android and Swift suites in the same change.
// `CoreBindingSmokeTest.kt` / `CoreBindingSmokeTests.swift` cover these shapes
// without reading a table, so that lowering coverage at least survives it.

/// One row of the 429 backoff curve.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayBackoffVector {
    /// Stable name, so a failure names the case rather than an index.
    pub name: String,
    pub retry_after_ms: u64,
    pub consecutive_rate_limits: u32,
    pub jitter_ms: u64,
    pub expected_delay_ms: u64,
}

/// One step of a pacer sequence. Rows are applied in order to a single fresh
/// [`CoreFamilyRelayPacer`]; the state carried between them is the point.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayPacerVector {
    pub name: String,
    pub now_ms: i64,
    pub expected_wait_ms: i64,
}

/// One jitter derivation. `expected_jitter_ms` is what BLAKE2b under
/// [`FAMILY_RELAY_JITTER_CONTEXT`] produces; if a platform ever disagrees the
/// binding is marshalling the byte array wrong.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayJitterVector {
    pub name: String,
    pub identity_public_bytes: Vec<u8>,
    pub expected_jitter_ms: u64,
}

/// One rerun decision.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayRerunVector {
    pub name: String,
    pub pending_requested: bool,
    pub can_sync: bool,
    pub backoff_remaining_ms: i64,
    pub expected: CoreRelayRerunAction,
}

/// One health fold.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayHealthVector {
    pub name: String,
    pub fault: Option<CoreRelayFault>,
    pub own_relay_succeeded: bool,
    pub any_relay_succeeded: bool,
    pub expected: CoreRelayPassHealth,
}

fn backoff_vector(
    name: &str,
    retry_after_ms: u64,
    consecutive_rate_limits: u32,
    jitter_ms: u64,
    expected_delay_ms: u64,
) -> CoreRelayBackoffVector {
    CoreRelayBackoffVector {
        name: name.to_string(),
        retry_after_ms,
        consecutive_rate_limits,
        jitter_ms,
        expected_delay_ms,
    }
}

/// The 429 curve, including the cases that used to be asserted separately in
/// Kotlin and Swift.
#[uniffi::export]
pub fn core_family_relay_backoff_vectors() -> Vec<CoreRelayBackoffVector> {
    vec![
        // The deployed curve with no advertised window worth honouring.
        backoff_vector("first-429-base-window", 0, 1, 0, 1_000),
        backoff_vector("second-429-doubles", 0, 2, 0, 2_000),
        backoff_vector("third-429-doubles-again", 0, 3, 0, 4_000),
        backoff_vector("seventh-429-reaches-the-cap", 0, 7, 0, 60_000),
        // Beyond the exponent clamp nothing may keep growing.
        backoff_vector("hundredth-429-still-capped", 1_000, 100, 0, 60_000),
        backoff_vector("saturating-count-still-capped", 0, u32::MAX, 0, 60_000),
        // Retry-After is a floor: it wins while it is larger, and loses once
        // the exponential passes it. Neither ever shortens the other.
        backoff_vector("retry-after-floor-beats-base", 15_000, 1, 0, 15_000),
        backoff_vector("exponential-beats-a-small-retry-after", 1_000, 6, 0, 32_000),
        backoff_vector(
            "retry-after-above-the-cap-still-wins",
            90_000,
            100,
            0,
            90_000,
        ),
        // A zero streak is treated as the first refusal, not as no refusal.
        backoff_vector("zero-count-reads-as-the-first", 0, 0, 0, 1_000),
        // Jitter is added after the floor, never inside it, so it can only
        // ever lengthen a window.
        backoff_vector("jitter-adds-on-top-of-the-floor", 15_000, 1, 999, 15_999),
        backoff_vector("jitter-adds-on-top-of-the-cap", 0, 7, 1_000, 61_000),
        // Saturating arithmetic at the boundary.
        backoff_vector("absurd-retry-after-saturates", u64::MAX, 1, 1_000, u64::MAX),
    ]
}

/// A pacer sequence: two requests inside one instant, one that only partly
/// waits out the interval, and one that arrives after the reservation has
/// lapsed. Applied in order to one pacer.
#[uniffi::export]
pub fn core_family_relay_pacer_vectors() -> Vec<CoreRelayPacerVector> {
    vec![
        CoreRelayPacerVector {
            name: "first-request-goes-now".to_string(),
            now_ms: 10_000,
            expected_wait_ms: 0,
        },
        CoreRelayPacerVector {
            name: "second-request-waits-a-full-interval".to_string(),
            now_ms: 10_000,
            expected_wait_ms: 500,
        },
        CoreRelayPacerVector {
            name: "third-request-waits-out-the-queue-not-the-interval".to_string(),
            now_ms: 10_250,
            expected_wait_ms: 750,
        },
        CoreRelayPacerVector {
            name: "a-lapsed-reservation-costs-nothing".to_string(),
            now_ms: 12_000,
            expected_wait_ms: 0,
        },
        CoreRelayPacerVector {
            name: "and-paces-again-from-there".to_string(),
            now_ms: 12_000,
            expected_wait_ms: 500,
        },
    ]
}

/// Jitter draws. Distinct identities must draw distinct offsets far more often
/// than not, every offset must sit inside the window, and the same bytes must
/// always draw the same offset — including across a restart, which is what
/// "stable" means here.
#[uniffi::export]
pub fn core_family_relay_jitter_vectors() -> Vec<CoreRelayJitterVector> {
    vec![
        CoreRelayJitterVector {
            name: "empty-identity".to_string(),
            identity_public_bytes: Vec::new(),
            expected_jitter_ms: core_family_relay_jitter_ms(Vec::new()),
        },
        CoreRelayJitterVector {
            name: "single-byte".to_string(),
            identity_public_bytes: vec![1],
            expected_jitter_ms: core_family_relay_jitter_ms(vec![1]),
        },
        CoreRelayJitterVector {
            name: "reversed-bytes-are-a-different-identity".to_string(),
            identity_public_bytes: vec![4, 3, 2, 1],
            expected_jitter_ms: core_family_relay_jitter_ms(vec![4, 3, 2, 1]),
        },
        CoreRelayJitterVector {
            name: "thirty-two-byte-user-id".to_string(),
            identity_public_bytes: (0u8..32).collect(),
            expected_jitter_ms: core_family_relay_jitter_ms((0u8..32).collect()),
        },
        CoreRelayJitterVector {
            name: "high-bytes".to_string(),
            identity_public_bytes: vec![0xff; 32],
            expected_jitter_ms: core_family_relay_jitter_ms(vec![0xff; 32]),
        },
    ]
}

/// Rerun decisions, including the storm case (#222) that the rule exists for.
#[uniffi::export]
pub fn core_family_relay_rerun_vectors() -> Vec<CoreRelayRerunVector> {
    vec![
        CoreRelayRerunVector {
            name: "pending-nudge-outside-any-window-runs".to_string(),
            pending_requested: true,
            can_sync: true,
            backoff_remaining_ms: 0,
            expected: CoreRelayRerunAction::RunAgain,
        },
        CoreRelayRerunVector {
            name: "an-elapsed-window-is-no-window".to_string(),
            pending_requested: true,
            can_sync: true,
            backoff_remaining_ms: -5_000,
            expected: CoreRelayRerunAction::RunAgain,
        },
        CoreRelayRerunVector {
            name: "one-millisecond-of-window-still-defers".to_string(),
            pending_requested: true,
            can_sync: true,
            backoff_remaining_ms: 1,
            expected: CoreRelayRerunAction::ScheduleRateLimitRetry,
        },
        CoreRelayRerunVector {
            name: "the-storm-case-defers".to_string(),
            pending_requested: true,
            can_sync: true,
            backoff_remaining_ms: 30_000,
            expected: CoreRelayRerunAction::ScheduleRateLimitRetry,
        },
        CoreRelayRerunVector {
            name: "no-nudge-releases-the-worker".to_string(),
            pending_requested: false,
            can_sync: true,
            backoff_remaining_ms: 0,
            expected: CoreRelayRerunAction::Stop,
        },
        CoreRelayRerunVector {
            name: "no-nudge-inside-a-window-also-releases".to_string(),
            pending_requested: false,
            can_sync: true,
            backoff_remaining_ms: 9_999,
            expected: CoreRelayRerunAction::Stop,
        },
        CoreRelayRerunVector {
            name: "a-nudge-is-dropped-when-syncing-is-impossible".to_string(),
            pending_requested: true,
            can_sync: false,
            backoff_remaining_ms: 0,
            expected: CoreRelayRerunAction::Stop,
        },
        CoreRelayRerunVector {
            name: "impossible-and-rate-limited-still-stops".to_string(),
            pending_requested: true,
            can_sync: false,
            backoff_remaining_ms: 30_000,
            expected: CoreRelayRerunAction::Stop,
        },
    ]
}

fn health_vector(
    name: &str,
    fault: Option<CoreRelayFault>,
    own_relay_succeeded: bool,
    any_relay_succeeded: bool,
    expected: CoreRelayPassHealth,
) -> CoreRelayHealthVector {
    CoreRelayHealthVector {
        name: name.to_string(),
        fault,
        own_relay_succeeded,
        any_relay_succeeded,
        expected,
    }
}

/// The pass health fold, including the expiry-in-grace case that a green check
/// used to hide.
#[uniffi::export]
pub fn core_family_relay_health_vectors() -> Vec<CoreRelayHealthVector> {
    vec![
        health_vector("clean-pass", None, true, true, CoreRelayPassHealth::Ok),
        health_vector(
            "own-relay-failed-unstructured",
            None,
            false,
            true,
            CoreRelayPassHealth::Failing,
        ),
        health_vector(
            "nothing-answered",
            None,
            false,
            false,
            CoreRelayPassHealth::Failing,
        ),
        // Mailbox-level faults outrank a successful poll: relayd serves
        // fetches while refusing posts.
        health_vector(
            "quota-full-beats-a-successful-poll",
            Some(CoreRelayFault::MailboxFull),
            true,
            true,
            CoreRelayPassHealth::QuotaFull,
        ),
        health_vector(
            "oversized-envelope-beats-a-successful-poll",
            Some(CoreRelayFault::MessageTooLarge),
            true,
            true,
            CoreRelayPassHealth::MessageTooLarge,
        ),
        health_vector(
            "rate-limited-beats-a-successful-poll",
            Some(CoreRelayFault::RateLimited),
            true,
            true,
            CoreRelayPassHealth::RateLimited,
        ),
        // The seven-day read-only grace window is not health, and it is not
        // the same state as a pass past the window either: the mailbox still
        // answered, so queued mail is still arriving.
        health_vector(
            "expiry-in-grace-beats-a-successful-poll",
            Some(CoreRelayFault::PassExpired),
            true,
            true,
            CoreRelayPassHealth::ExpiredReadOnly,
        ),
        health_vector(
            "expiry-past-the-grace-window",
            Some(CoreRelayFault::PassExpired),
            false,
            false,
            CoreRelayPassHealth::Expired,
        ),
        // Another family's relay answering proves nothing about ours. Only our
        // own mailbox answering means our queued mail is still coming in.
        health_vector(
            "expiry-with-only-someone-elses-relay-answering",
            Some(CoreRelayFault::PassExpired),
            false,
            true,
            CoreRelayPassHealth::Expired,
        ),
        // Suspension and token rejection keep the older precedence, because
        // relayd refuses every operation for both.
        health_vector(
            "suspension-cannot-co-occur-with-a-good-poll",
            Some(CoreRelayFault::PassSuspended),
            true,
            true,
            CoreRelayPassHealth::Ok,
        ),
        health_vector(
            "suspension",
            Some(CoreRelayFault::PassSuspended),
            false,
            false,
            CoreRelayPassHealth::Suspended,
        ),
        health_vector(
            "token-rejected",
            Some(CoreRelayFault::TokenRejected),
            false,
            false,
            CoreRelayPassHealth::TokenRejected,
        ),
        health_vector(
            "outage-is-plain-failure",
            Some(CoreRelayFault::Outage),
            false,
            false,
            CoreRelayPassHealth::Failing,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_network_permission_states_the_whole_matrix() {
        use CoreRelayNetworkVerdict::{DeferredConstrained, DeferredRoaming, Permitted};
        use CoreRelayRoaming::{No, Unknown, Yes};

        // Every (roaming, constrained, override) combination, with the
        // expected verdict written out by hand. A table that recomputes the
        // policy instead of stating it agrees with itself and pins nothing.
        let cases = [
            // A path we know is roaming waits until the user opts in.
            (Yes, false, false, DeferredRoaming),
            (Yes, true, false, DeferredRoaming),
            // The override is honored; a constrained path still holds the
            // carried lane back, because that is a separate signal.
            (Yes, false, true, Permitted),
            (Yes, true, true, DeferredConstrained),
            // A path we know is not roaming is never deferred for roaming.
            (No, false, false, Permitted),
            (No, true, false, DeferredConstrained),
            (No, false, true, Permitted),
            (No, true, true, DeferredConstrained),
            // iOS, where roaming is unknowable and therefore never inferred:
            // Shore Pass keeps working on cellular at home, and the system's
            // own Data Roaming setting is what stops roaming spend.
            (Unknown, false, false, Permitted),
            (Unknown, true, false, DeferredConstrained),
            (Unknown, false, true, Permitted),
            (Unknown, true, true, DeferredConstrained),
        ];

        for (roaming, constrained, user_allows_roaming, expected) in cases {
            assert_eq!(
                core_relay_network_permitted(roaming, constrained, user_allows_roaming),
                expected,
                "roaming={roaming:?} constrained={constrained} override={user_allows_roaming}",
            );
        }
    }

    // -----------------------------------------------------------------------
    // Vectors: the same tables the two shells consume
    // -----------------------------------------------------------------------

    #[test]
    fn backoff_vectors_hold() {
        for vector in core_family_relay_backoff_vectors() {
            assert_eq!(
                core_family_relay_backoff_delay_ms(
                    vector.retry_after_ms,
                    vector.consecutive_rate_limits,
                    vector.jitter_ms,
                ),
                vector.expected_delay_ms,
                "backoff vector {}",
                vector.name
            );
        }
    }

    #[test]
    fn pacer_vectors_hold_as_one_sequence() {
        let pacer = CoreFamilyRelayPacer::new();
        for vector in core_family_relay_pacer_vectors() {
            assert_eq!(
                pacer.reserve(vector.now_ms),
                vector.expected_wait_ms,
                "pacer vector {}",
                vector.name
            );
        }
    }

    #[test]
    fn jitter_vectors_hold() {
        for vector in core_family_relay_jitter_vectors() {
            assert_eq!(
                core_family_relay_jitter_ms(vector.identity_public_bytes.clone()),
                vector.expected_jitter_ms,
                "jitter vector {}",
                vector.name
            );
        }
    }

    #[test]
    fn rerun_vectors_hold() {
        for vector in core_family_relay_rerun_vectors() {
            assert_eq!(
                core_relay_rerun_action(
                    vector.pending_requested,
                    vector.can_sync,
                    vector.backoff_remaining_ms,
                ),
                vector.expected,
                "rerun vector {}",
                vector.name
            );
        }
    }

    #[test]
    fn health_vectors_hold() {
        for vector in core_family_relay_health_vectors() {
            assert_eq!(
                core_relay_pass_health(
                    vector.fault,
                    vector.own_relay_succeeded,
                    vector.any_relay_succeeded,
                ),
                vector.expected,
                "health vector {}",
                vector.name
            );
        }
    }

    // -----------------------------------------------------------------------
    // Pacing
    // -----------------------------------------------------------------------

    #[test]
    fn pacer_caps_a_phone_at_two_requests_per_second() {
        let pacer = CoreFamilyRelayPacer::new();
        // Ten requests fired in the same instant are spread across the
        // interval rather than all issued at once.
        let waits: Vec<i64> = (0..10).map(|_| pacer.reserve(0)).collect();
        assert_eq!(
            waits,
            vec![0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500, 4_000, 4_500]
        );
    }

    #[test]
    fn pacer_survives_a_clock_that_runs_backwards() {
        // Both shells feed a monotonic clock, so this should be unreachable;
        // if it ever happens the wait is bounded by the reservation already
        // held, not by the size of the rollback, and nothing panics.
        let pacer = CoreFamilyRelayPacer::new();
        assert_eq!(pacer.reserve(1_000_000), 0);
        assert_eq!(pacer.reserve(0), 1_000_500);
        // And the pacer keeps working afterwards rather than wedging.
        assert_eq!(pacer.reserve(2_000_000), 0);
    }

    #[test]
    fn pacer_saturates_on_an_absurd_clock() {
        let pacer = CoreFamilyRelayPacer::new();
        assert_eq!(pacer.reserve(i64::MAX), 0);
        // The next reservation saturated rather than wrapping into the past.
        assert_eq!(pacer.reserve(i64::MAX), 0);
        assert_eq!(pacer.reserve(0), i64::MAX);
    }

    #[test]
    fn a_zero_interval_pacer_never_waits() {
        let pacer = CoreFamilyRelayPacer::with_interval_ms(0);
        assert_eq!(pacer.reserve(5), 0);
        assert_eq!(pacer.reserve(5), 0);
        // A negative interval is clamped, not run backwards.
        let clamped = CoreFamilyRelayPacer::with_interval_ms(-10_000);
        assert_eq!(clamped.reserve(5), 0);
        assert_eq!(clamped.reserve(5), 0);
    }

    // -----------------------------------------------------------------------
    // Backoff
    // -----------------------------------------------------------------------

    #[test]
    fn repeated_rate_limits_widen_and_a_clean_pass_recovers() {
        let identity = vec![7u8; 32];
        let backoff = CoreFamilyRelayBackoff::new();
        let jitter = core_family_relay_jitter_ms(identity.clone());

        let first = backoff.on_rate_limited(1_000, identity.clone());
        let second = backoff.on_rate_limited(1_000, identity.clone());
        assert_eq!(first, 1_000 + jitter);
        assert_eq!(second, 2_000 + jitter);
        assert!(second > first, "a second refusal must widen the window");
        assert_eq!(backoff.consecutive_rate_limits(), 2);

        backoff.on_successful_pass();
        assert_eq!(backoff.consecutive_rate_limits(), 0);
        assert_eq!(
            backoff.on_rate_limited(1_000, identity),
            first,
            "a completed pass must clear the punishment already served"
        );
    }

    #[test]
    fn a_family_of_phones_recovers_on_staggered_deadlines() {
        // The reason jitter exists: three phones refused at the same instant
        // must not all come back at the same instant.
        let identities: Vec<Vec<u8>> = vec![vec![1], vec![2], vec![3]];
        let clients: Vec<CoreFamilyRelayBackoff> = identities
            .iter()
            .map(|_| CoreFamilyRelayBackoff::new())
            .collect();

        let first: Vec<u64> = clients
            .iter()
            .zip(&identities)
            .map(|(client, identity)| client.on_rate_limited(1_000, identity.clone()))
            .collect();
        assert!(
            first.iter().all(|delay| (1_000..=2_000).contains(delay)),
            "every phone stays inside the floor plus one jitter window: {first:?}"
        );
        let distinct: std::collections::BTreeSet<u64> = first.iter().copied().collect();
        assert_eq!(distinct.len(), 3, "phones must not wake in lockstep");

        let second: Vec<u64> = clients
            .iter()
            .zip(&identities)
            .map(|(client, identity)| client.on_rate_limited(1_000, identity.clone()))
            .collect();
        assert!(second.iter().all(|delay| *delay >= 2_000));

        for client in &clients {
            client.on_successful_pass();
        }
        let recovered: Vec<u64> = clients
            .iter()
            .zip(&identities)
            .map(|(client, identity)| client.on_rate_limited(1_000, identity.clone()))
            .collect();
        assert_eq!(recovered, first);
    }

    #[test]
    fn jitter_is_stable_bounded_and_identity_specific() {
        let identity: Vec<u8> = (0u8..32).collect();
        assert_eq!(
            core_family_relay_jitter_ms(identity.clone()),
            core_family_relay_jitter_ms(identity.clone()),
            "the offset must not move between calls, or a restart jumps the queue"
        );

        let mut reversed = identity.clone();
        reversed.reverse();
        assert_ne!(
            core_family_relay_jitter_ms(identity),
            core_family_relay_jitter_ms(reversed),
            "byte order must matter, or two phones can share an offset"
        );

        // Every draw is inside the advertised window, and the window is
        // actually used rather than collapsing onto a few values.
        let mut seen = std::collections::BTreeSet::new();
        for byte in 0u8..=255 {
            let jitter = core_family_relay_jitter_ms(vec![byte; 32]);
            assert!(jitter <= FAMILY_RELAY_JITTER_WINDOW_MS);
            seen.insert(jitter);
        }
        assert!(
            seen.len() > 200,
            "256 identities should spread across the window, got {} distinct offsets",
            seen.len()
        );
    }

    #[test]
    fn jitter_length_frames_its_input() {
        // Without the length prefix these two would hash identical bytes.
        assert_ne!(
            core_family_relay_jitter_ms(vec![0x01, 0x02]),
            core_family_relay_jitter_ms(vec![0x01, 0x02, 0x00]),
        );
    }

    #[test]
    fn the_cap_getter_reports_the_constant() {
        assert_eq!(
            core_family_relay_backoff_cap_ms(),
            FAMILY_RELAY_BACKOFF_CAP_MS
        );
    }

    // -----------------------------------------------------------------------
    // Health fold
    // -----------------------------------------------------------------------

    #[test]
    fn worse_fault_keeps_the_persistent_condition_in_either_order() {
        let folded = core_worse_relay_fault(
            Some(core_worse_relay_fault(None, CoreRelayFault::RateLimited)),
            CoreRelayFault::MailboxFull,
        );
        assert_eq!(folded, CoreRelayFault::MailboxFull);

        let reversed = core_worse_relay_fault(
            Some(core_worse_relay_fault(None, CoreRelayFault::MailboxFull)),
            CoreRelayFault::RateLimited,
        );
        assert_eq!(reversed, CoreRelayFault::MailboxFull);
    }

    #[test]
    fn worse_fault_is_idempotent_and_takes_the_first_observation() {
        assert_eq!(
            core_worse_relay_fault(None, CoreRelayFault::Outage),
            CoreRelayFault::Outage,
            "the first observation wins over nothing, whatever its rank"
        );
        assert_eq!(
            core_worse_relay_fault(
                Some(CoreRelayFault::MailboxFull),
                CoreRelayFault::MailboxFull
            ),
            CoreRelayFault::MailboxFull
        );
    }
}
