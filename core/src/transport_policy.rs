use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::{CoreCarriedCursor, DigestEntry};

/// FC6: recover from mutex poisoning instead of propagating it as a panic.
/// `Mutex::lock` returns `Err` if some earlier locker panicked while
/// holding the lock; the stdlib's default advice is to treat the protected
/// data as possibly inconsistent, but every `Mutex` in this file only ever
/// guards a plain `HashMap` of local scheduling state (peer routes, backoff
/// timers, LAN health) -- there is no multi-step invariant that a panic
/// mid-update could leave torn in a way that matters here. Without this, the
/// first panic under any of these locks (a bug, an unexpected input) would
/// poison the mutex permanently: every later call from the UniFFI boundary
/// would itself panic natively, turning one bug into a crash loop until the
/// process restarts.
trait PoisonRecoverable<T> {
    fn lock_recoverable(&self) -> MutexGuard<'_, T>;
}

impl<T> PoisonRecoverable<T> for Mutex<T> {
    fn lock_recoverable(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub const DEFAULT_INITIAL_BACKOFF_MS: i64 = 5_000;
pub const DEFAULT_MAX_BACKOFF_MS: i64 = 5 * 60_000;
pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 6;
pub const DEFAULT_LAN_HEALTH_TIMEOUT_MS: i64 = 20_000;
pub const DEFAULT_LAN_HEALTH_MAX_TIMEOUTS: u32 = 3;

#[uniffi::export]
pub fn digest_is_expected_chat_id(digest_chat_id: Vec<u8>, hello_user_id: Option<Vec<u8>>) -> bool {
    hello_user_id.is_some_and(|id| id == digest_chat_id)
}

/// A DIGEST whose `chat_id` is a group this device shares with the link
/// peer. Old clients never reach this helper: they drop those frames via
/// [`digest_is_expected_chat_id`] (group id ≠ HELLO user id).
#[uniffi::export]
pub fn digest_is_shared_group(
    digest_chat_id: Vec<u8>,
    hello_user_id: Option<Vec<u8>>,
    own_user_id: Vec<u8>,
    group: crate::Group,
) -> bool {
    hello_user_id.is_some_and(|peer| {
        group.id == digest_chat_id
            && group.member_user_ids.iter().any(|member| member == &peer)
            && group
                .member_user_ids
                .iter()
                .any(|member| member == &own_user_id)
    })
}

/// Bounds of the periodic re-digest interval (D8). A link that stays up past a
/// jittered point in this window re-runs the digest exchange, so a message that
/// arrived (or a receipt that was authored) after the one-shot connect-time
/// digest still converges without waiting for a reconnect.
pub const REDIGEST_MIN_INTERVAL_MS: i64 = 3 * 60_000;
pub const REDIGEST_MAX_INTERVAL_MS: i64 = 5 * 60_000;

/// Max peers that may offer foreign-carry frames in one multi-peer spray pass
/// (G3). A busy desk can have 10+ simultaneous BLE links; walking the full
/// carry store to every peer at once is a self-DoS. [`CoreCarriedOfferGate`]
/// enforces this for both shells; [`may_start_carried_offer`] is the bare
/// predicate it is built on.
pub const MAX_CONCURRENT_CARRIED_OFFERS: u32 = 2;

/// Whether another peer may begin a foreign-carry offer given how many
/// offers are already in flight this pass.
#[uniffi::export]
pub fn may_start_carried_offer(active_offers: u32) -> bool {
    active_offers < MAX_CONCURRENT_CARRIED_OFFERS
}

/// How long one shared foreign-carry allowance lasts (G3). Short enough that a
/// peer denied its turn gets another chance within a single encounter, long
/// enough that one connection burst -- several links coming up at once as a
/// phone walks into range of a busy desk -- counts as one event rather than as
/// N independent chances to walk the carry store.
pub const CARRIED_OFFER_EPOCH_MS: i64 = 5_000;

/// The epoch length the shells construct [`CoreCarriedOfferGate`] with.
/// Exported as a function because UniFFI has no constants: both shells read it
/// from here so the window cannot drift between the platforms.
#[uniffi::export]
pub fn core_carried_offer_epoch_ms() -> i64 {
    CARRIED_OFFER_EPOCH_MS
}

/// A claim on one of the epoch's [`MAX_CONCURRENT_CARRIED_OFFERS`] slots. Hand
/// it back to [`CoreCarriedOfferGate::commit`] once the offer actually went
/// out, or to [`CoreCarriedOfferGate::release`] when the plan came out empty,
/// so the slot returns to the pool for another peer in the same epoch.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreCarriedOfferReservation {
    pub id: i64,
    pub epoch_start_ms: i64,
    pub logical_peer_id: Option<String>,
}

#[derive(Default)]
struct CarriedOfferEpoch {
    /// Distinguishes "no epoch yet" from "an epoch that started at 0", so a
    /// caller whose clock legitimately reads 0 is not stuck resetting.
    initialized: bool,
    epoch_start_ms: i64,
    offers_this_epoch: u32,
    next_reservation_id: i64,
    /// Reservation id -> the logical peer it was taken for, for the ones not
    /// yet committed. Only those can be released.
    uncommitted: HashMap<i64, Option<String>>,
    offered_logical_peers: HashSet<String>,
}

/// Atomically reserves the shared foreign-carry allowance for one short epoch.
///
/// This is the concurrency gate in front of every lane that offers *third
/// party* traffic: the HELLO drain and the digest spray. A busy desk can hold
/// ten simultaneous links, and each of them independently deciding to walk the
/// carry store is a self-DoS that queues live mail behind courier traffic on
/// all of them at once. At most [`MAX_CONCURRENT_CARRIED_OFFERS`] peers may
/// start such an offer per [`CARRIED_OFFER_EPOCH_MS`] window, and at most one
/// offer per *logical peer* -- so a phone reachable at two Bluetooth addresses,
/// or one that reconnects mid-epoch, cannot claim both slots for itself.
///
/// Reservations are taken *before* a plan is built, because the point is to
/// bound how many peers do the walk at all. A plan that comes out empty is
/// [`Self::release`]d, which frees the slot and clears the logical-peer mark,
/// since nothing was actually offered to that peer.
///
/// This gates *offering* only. It never removes a carried row and never acks
/// anything: a deferred peer simply gets its offer on a later round, and a
/// carried copy is still retired only on digest-proof of receipt.
#[derive(uniffi::Object)]
pub struct CoreCarriedOfferGate {
    epoch_ms: i64,
    state: Mutex<CarriedOfferEpoch>,
}

#[uniffi::export]
impl CoreCarriedOfferGate {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self::with_epoch_ms(CARRIED_OFFER_EPOCH_MS)
    }

    /// `epoch_ms` is clamped to at least 1ms: a zero-length epoch would roll on
    /// every call and defeat the cap entirely.
    #[uniffi::constructor]
    pub fn with_epoch_ms(epoch_ms: i64) -> Self {
        Self {
            epoch_ms: epoch_ms.max(1),
            state: Mutex::new(CarriedOfferEpoch::default()),
        }
    }

    pub fn epoch_ms(&self) -> i64 {
        self.epoch_ms
    }

    /// Claims a slot, or `None` when this epoch's allowance is spent or this
    /// logical peer already had its offer. `logical_peer_id` is the peer's
    /// UserID hex, never a link address -- deduplicating on the address is what
    /// let one phone with two roles take both slots.
    ///
    /// A backwards clock jump starts a fresh epoch rather than parking the lane
    /// until the clock catches up.
    pub fn try_reserve(
        &self,
        now_ms: i64,
        logical_peer_id: Option<String>,
    ) -> Option<CoreCarriedOfferReservation> {
        let mut state = self.state.lock_recoverable();
        self.roll_epoch_if_needed(&mut state, now_ms);
        if !may_start_carried_offer(state.offers_this_epoch) {
            return None;
        }
        if let Some(peer) = &logical_peer_id {
            if state.offered_logical_peers.contains(peer) {
                return None;
            }
        }
        state.next_reservation_id = if state.next_reservation_id == i64::MAX {
            0
        } else {
            state.next_reservation_id + 1
        };
        let id = state.next_reservation_id;
        state.offers_this_epoch += 1;
        state.uncommitted.insert(id, logical_peer_id.clone());
        if let Some(peer) = &logical_peer_id {
            state.offered_logical_peers.insert(peer.clone());
        }
        Some(CoreCarriedOfferReservation {
            id,
            epoch_start_ms: state.epoch_start_ms,
            logical_peer_id,
        })
    }

    /// The offer went out. The slot stays spent for the rest of the epoch and
    /// the peer stays marked, so neither can be claimed again until it rolls.
    pub fn commit(&self, reservation: CoreCarriedOfferReservation) {
        let mut state = self.state.lock_recoverable();
        if reservation.epoch_start_ms == state.epoch_start_ms {
            state.uncommitted.remove(&reservation.id);
        }
    }

    /// Nothing was offered after all, so return the slot to this epoch's pool
    /// and unmark the peer. A reservation from an epoch that has since rolled,
    /// or one already committed or released, is ignored -- crediting a slot
    /// back twice would let a third peer through.
    pub fn release(&self, reservation: CoreCarriedOfferReservation) {
        let mut state = self.state.lock_recoverable();
        if reservation.epoch_start_ms != state.epoch_start_ms
            || state.uncommitted.remove(&reservation.id).is_none()
        {
            return;
        }
        state.offers_this_epoch = state.offers_this_epoch.saturating_sub(1);
        if let Some(peer) = &reservation.logical_peer_id {
            state.offered_logical_peers.remove(peer);
        }
    }
}

impl CoreCarriedOfferGate {
    fn roll_epoch_if_needed(&self, state: &mut CarriedOfferEpoch, now_ms: i64) {
        let rolled = !state.initialized
            || now_ms < state.epoch_start_ms
            || now_ms.saturating_sub(state.epoch_start_ms) >= self.epoch_ms;
        if !rolled {
            return;
        }
        state.initialized = true;
        state.epoch_start_ms = now_ms;
        state.offers_this_epoch = 0;
        state.uncommitted.clear();
        state.offered_logical_peers.clear();
    }
}

impl Default for CoreCarriedOfferGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod carried_offer_gate_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Barrier;

    #[test]
    fn concurrent_digests_atomically_reserve_only_two_offers() {
        // Ported from the Kotlin gate's test: Android's BLE, LAN and relay
        // receive paths all reach this from different threads at once.
        let gate = Arc::new(CoreCarriedOfferGate::with_epoch_ms(5_000));
        let threads = 16;
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..4)
                        .filter(|_| gate.try_reserve(1_000, None).is_some())
                        .count()
                })
            })
            .collect();
        let granted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(granted, MAX_CONCURRENT_CARRIED_OFFERS as usize);
    }

    #[test]
    fn empty_plan_releases_but_committed_offer_counts_until_next_epoch() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(100);
        let empty = gate.try_reserve(1_000, None).unwrap();
        let sent = gate.try_reserve(1_000, None).unwrap();
        assert!(gate.try_reserve(1_000, None).is_none());

        gate.release(empty);
        let replacement = gate.try_reserve(1_000, None);
        assert!(replacement.is_some());
        gate.commit(sent);
        gate.commit(replacement.unwrap());
        assert!(gate.try_reserve(1_099, None).is_none());
        assert!(gate.try_reserve(1_100, None).is_some());
    }

    #[test]
    fn duplicate_addresses_for_one_logical_peer_get_one_offer_per_epoch() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(100);
        let first = gate.try_reserve(1_000, Some("alice".to_string())).unwrap();
        gate.commit(first);
        assert!(gate.try_reserve(1_000, Some("alice".to_string())).is_none());
        assert!(gate.try_reserve(1_000, Some("bob".to_string())).is_some());
        assert!(gate.try_reserve(1_100, Some("alice".to_string())).is_some());
    }

    #[test]
    fn released_empty_offer_does_not_block_same_logical_peer() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(100);
        let empty = gate.try_reserve(1_000, Some("alice".to_string())).unwrap();
        gate.release(empty);
        assert!(gate.try_reserve(1_000, Some("alice".to_string())).is_some());
    }

    #[test]
    fn a_committed_reservation_cannot_be_released_for_a_second_slot() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(100);
        let sent = gate.try_reserve(1_000, None).unwrap();
        let other = gate.try_reserve(1_000, None).unwrap();
        gate.commit(sent.clone());
        // A double release (or a release after commit) must not credit the
        // epoch a slot it never got back.
        gate.release(sent.clone());
        gate.release(sent);
        assert!(gate.try_reserve(1_000, None).is_none());
        gate.release(other);
        assert!(gate.try_reserve(1_000, None).is_some());
    }

    #[test]
    fn a_backwards_clock_jump_rolls_the_epoch_instead_of_parking_the_lane() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(5_000);
        gate.commit(gate.try_reserve(100_000, None).unwrap());
        gate.commit(gate.try_reserve(100_000, None).unwrap());
        assert!(gate.try_reserve(100_000, None).is_none());
        assert!(
            gate.try_reserve(50_000, None).is_some(),
            "a clock correction must not wedge foreign carry for the rest of \
             the old epoch"
        );
    }

    #[test]
    fn a_stale_reservation_from_a_rolled_epoch_is_ignored() {
        let gate = CoreCarriedOfferGate::with_epoch_ms(100);
        let stale = gate.try_reserve(1_000, Some("alice".to_string())).unwrap();
        // New epoch: two fresh slots, and the old reservation belongs to none
        // of them.
        assert!(gate.try_reserve(2_000, Some("bob".to_string())).is_some());
        gate.release(stale);
        assert!(gate.try_reserve(2_000, Some("carol".to_string())).is_some());
        assert!(gate.try_reserve(2_000, Some("dave".to_string())).is_none());
    }
}

#[cfg(test)]
mod concurrent_offer_tests {
    use super::*;

    #[test]
    fn may_start_carried_offer_caps_family_scale_spray() {
        assert!(may_start_carried_offer(0));
        assert!(may_start_carried_offer(MAX_CONCURRENT_CARRIED_OFFERS - 1));
        assert!(!may_start_carried_offer(MAX_CONCURRENT_CARRIED_OFFERS));
        assert!(!may_start_carried_offer(MAX_CONCURRENT_CARRIED_OFFERS + 5));
    }

    #[test]
    fn old_clients_drop_a_group_scoped_digest() {
        let peer = vec![0xAA; 16];
        let group_id = vec![0xBB; 16];
        assert!(!digest_is_expected_chat_id(group_id, Some(peer)));
    }

    #[test]
    fn new_clients_accept_a_digest_for_a_shared_group() {
        let peer = vec![0xAA; 16];
        let me = vec![0xCC; 16];
        let group = crate::Group {
            id: vec![0xBB; 16],
            name: "family".into(),
            member_user_ids: vec![peer.clone(), me.clone()],
            key: vec![0xDD; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        assert!(digest_is_shared_group(
            group.id.clone(),
            Some(peer.clone()),
            me.clone(),
            group.clone(),
        ));
        assert!(!digest_is_shared_group(
            group.id.clone(),
            Some(vec![0xEE; 16]),
            me,
            group,
        ));
    }
}

/// How long the foreign-carry lane of the digest spray waits, after walking
/// this device's whole carry queue once, before it walks it again *from the
/// top*.
///
/// The walk itself is paced by a per-round byte budget and resumed by a
/// cursor, so a courier converges: each re-digest offers the next page, and
/// eventually a page reaches the tail with nothing new in it. Re-walking from
/// the top immediately after that would put the link straight back to
/// re-offering rows the peer has already refused, which is the churn the
/// cursor exists to end. A long-lived link does eventually re-walk, because a
/// frame can be lost in the link's FIFO on a disconnect mid-write and only a
/// fresh pass would find it again; half an hour is far longer than the 3-5
/// minute re-digest, so the steady state of a converged pair is quiet.
///
/// What this interval does *not* gate is mail that arrived after the walk
/// finished. A completed walk keeps its tail cursor
/// ([`CoreMeshRouterState::record_carried_progress`]), and rounds inside the
/// cooldown resume from it, so anything enqueued since is offered on the very
/// next re-digest while everything already refused stays behind the cursor.
pub const CARRIED_REWALK_MIN_INTERVAL_MS: i64 = 30 * 60_000;

/// How long a logical peer's carry-offering state is kept once it stops being
/// used.
///
/// The state is deliberately not dropped on disconnect: one phone shows up
/// under many BLE addresses and rotates them, and restarting the walk per
/// address is what multiplied a single peer's backlog offer. But nothing else
/// removed it either, so a device that meets a busy fleet accumulated one
/// entry per user id it had ever handshaken with, for the life of the process.
///
/// A day is chosen because it is far longer than anything the state is useful
/// for and short enough to bound the map by the peers actually met recently.
/// Past [`CARRIED_REWALK_MIN_INTERVAL_MS`] the only thing an idle entry still
/// decides is where a re-walk resumes, and after this long the honest answer
/// is "from the top" -- which is exactly what a peer with no entry gets. So
/// expiry costs at most one extra full pass toward a peer not seen in a day,
/// and it can never suppress an offer.
pub const LOGICAL_CARRY_STATE_TTL_MS: i64 = 24 * 60 * 60_000;

/// Whether a long-lived link is due to re-run its digest exchange (D8).
///
/// The interval is jittered per link across `[REDIGEST_MIN_INTERVAL_MS,
/// REDIGEST_MAX_INTERVAL_MS]` using `jitter_seed` (e.g. a hash of the peer
/// address) so many simultaneously-established links don't all re-digest on the
/// same tick. Digests are idempotent, so an early or extra exchange is
/// harmless -- this only bounds how often a quiet-but-live link bothers.
///
/// `last_digest_at_ms` is when this link last ran a digest (0 if it never has,
/// which is due immediately). A `last_digest_at_ms` in the future (clock skew)
/// simply isn't due yet.
#[uniffi::export]
pub fn should_redigest(now_ms: i64, last_digest_at_ms: i64, jitter_seed: u64) -> bool {
    let span = (REDIGEST_MAX_INTERVAL_MS - REDIGEST_MIN_INTERVAL_MS) as u64;
    let jitter = (jitter_seed % (span + 1)) as i64;
    let interval = REDIGEST_MIN_INTERVAL_MS + jitter;
    now_ms.saturating_sub(last_digest_at_ms) >= interval
}

#[uniffi::export]
pub fn digest_through_lamport_for_sender(
    entries: Vec<DigestEntry>,
    sender_user_id: Vec<u8>,
) -> u64 {
    entries
        .into_iter()
        .find(|entry| entry.sender_user_id == sender_user_id)
        .map_or(0, |entry| entry.through_lamport)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreTransport {
    Central,
    Peripheral,
    Lan,
}

impl CoreTransport {
    fn priority(self, local_user_id: Option<&[u8]>, peer_user_id: &[u8]) -> u8 {
        match self {
            Self::Lan => 30,
            // Both phones can simultaneously hold the two inverse BLE roles.
            // Elect the same physical direction at both ends from authenticated
            // identities: the lexicographically smaller user is central and
            // the larger user is peripheral. If local identity has not been
            // installed yet, preserve a deterministic central-first fallback.
            Self::Central if local_user_id.is_none_or(|local| local < peer_user_id) => 20,
            Self::Peripheral if local_user_id.is_some_and(|local| local > peer_user_id) => 20,
            Self::Central | Self::Peripheral => 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreTransportRoute {
    pub transport: CoreTransport,
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreIdentifiedRoute {
    pub transport: CoreTransport,
    pub address: String,
    pub user_id: Vec<u8>,
}

#[derive(Clone)]
struct Peer {
    transport: CoreTransport,
    connected_sequence: u64,
    user_id: Option<Vec<u8>>,
    /// From HELLO2; `None` = peer never sent one (a pre-HELLO2 build).
    capabilities: Option<u32>,
    /// When this *link* last put a DIGEST on the wire, for the D8 re-digest
    /// cadence. Link state, not logical-peer state: a fresh connection has
    /// never exchanged one and is due immediately, which is what makes a
    /// reconnect converge. `None` = never.
    last_digest_at_ms: Option<i64>,
    /// Hidden-kind envelope msg_ids already sprayed to this peer during this
    /// link session — the once-per-session bound for peers that can't ack
    /// hidden kinds. Cleared on a fresh legacy HELLO (new handshake) and
    /// dropped with the peer on disconnect.
    hidden_offered: std::collections::HashSet<Vec<u8>>,
    /// This link was admitted as one of THIS person's own devices
    /// (`specs/multi-device-v1.md` §10 step 5), not as somebody's peer.
    ///
    /// It carries the roster notice and the HELLOs that precede it, and
    /// nothing else. A missing `user_id` does not say that on its own —
    /// [`CoreMeshRouterState::relay_routes`] deliberately floods every *not
    /// yet* identified link, and "not yet" is exactly what this link never is
    /// — so the fact is recorded rather than inferred. Two things follow: the
    /// epidemic fanout skips it, and no HELLO can turn it into a route. Both
    /// matter for the same reason: after a removal, the device still holding
    /// this person's agreement key is the device that was removed.
    own_device: bool,
}

/// Carry offering is encounter state for an authenticated logical peer, not
/// link state. Android commonly has central + peripheral links (and rotating
/// BLE addresses) for one phone at once. Keying these cursors by address made
/// every link restart at the queue head and multiplied a single peer's offer
/// by its address count. Retaining this small state across link reconnects
/// also lets the next address resume the walk; the normal cooldown still
/// schedules a full safety re-walk for frames lost from a transport FIFO.
#[derive(Clone, Default)]
struct LogicalCarryState {
    carried_cursor: Option<CoreCarriedCursor>,
    carried_walk_done_at_ms: Option<i64>,
    /// Targeted HELLO drain and foreign digest spray select different row
    /// sets, so their cursors remain disjoint even though both are peer-keyed.
    targeted_carried_cursor: Option<CoreCarriedCursor>,
    targeted_carried_walk_done_at_ms: Option<i64>,
    /// Last round (either lane) that used this entry, for the
    /// [`LOGICAL_CARRY_STATE_TTL_MS`] sweep. `None` on an entry a handshake
    /// created but no round has touched yet: it holds nothing but defaults,
    /// so the sweep may drop it and lose no progress.
    last_used_ms: Option<i64>,
}

/// Cursor to record when a walk reaches the tail having offered no row of its
/// own to resume behind -- an empty queue, or one whose every eligible row was
/// already excluded. `now_ms` is the walk's own clock and carried rows are
/// stamped with the same clock when enqueued, so this resumes exactly at "what
/// arrives from here on". The empty `msg_id` is the low end of the blob order
/// the keyset compares against, so a row landing on this very millisecond is
/// still included.
fn tail_cursor_at(now_ms: i64) -> CoreCarriedCursor {
    CoreCarriedCursor {
        received_at: now_ms,
        msg_id: Vec::new(),
    }
}

/// Resolve one lane's stored progress into what it should do this round, and
/// record the transition. Both carry lanes share the rule; only the pair of
/// fields they read differs.
///
/// Deciding a full re-walk *clears* `walk_done_at_ms`, which is what makes the
/// cooldown a cooldown rather than a permanent state. Without that clear, a
/// long-lived link never re-walked at all: every round inside the cooldown
/// resumed from the tail, found nothing, reported `exhausted` again, and
/// [`CoreMeshRouterState::record_carried_progress`] re-stamped `done_at` to
/// *now* -- so on a link re-digesting every 3-5 minutes the 30-minute window
/// was renewed forever and the safety pass for a frame lost in a transport
/// FIFO could never come due. Only a pass that actually started from the top
/// re-arms the window when it completes.
fn lane_from(
    walk_done_at_ms: &mut Option<i64>,
    cursor: Option<&CoreCarriedCursor>,
    now_ms: i64,
) -> CoreCarriedLane {
    match *walk_done_at_ms {
        // A `done_at` in the future (clock skew) reads as not-yet-due, the
        // same direction `should_redigest` errs in: worst case the full
        // re-walk waits longer on a link that is still offering new arrivals
        // from its tail cursor every round.
        Some(done_at) if now_ms.saturating_sub(done_at) < CARRIED_REWALK_MIN_INTERVAL_MS => {
            match cursor {
                Some(tail) => CoreCarriedLane {
                    skip: false,
                    after: Some(tail.clone()),
                },
                None => CoreCarriedLane {
                    skip: true,
                    after: None,
                },
            }
        }
        Some(_) => {
            *walk_done_at_ms = None;
            CoreCarriedLane {
                skip: false,
                after: None,
            }
        }
        None => CoreCarriedLane {
            skip: false,
            after: cursor.cloned(),
        },
    }
}

/// Touch `user_id`'s carry state (creating it if absent) and drop every other
/// peer's state that no round has used within [`LOGICAL_CARRY_STATE_TTL_MS`].
///
/// Sweeping here, on the paths that already hold the lock and already know the
/// clock, keeps the map bounded without a timer: a device that is meeting
/// peers is exactly the device whose map is growing. A clock that jumped
/// backwards reads as "used in the future", which keeps the entry -- erring
/// toward remembering progress rather than re-offering a backlog.
fn touch_and_sweep<'a>(
    carry: &'a mut HashMap<Vec<u8>, LogicalCarryState>,
    user_id: &[u8],
    now_ms: i64,
) -> &'a mut LogicalCarryState {
    carry.retain(|id, state| {
        id.as_slice() == user_id
            || state
                .last_used_ms
                .is_some_and(|used| now_ms.saturating_sub(used) < LOGICAL_CARRY_STATE_TTL_MS)
    });
    let state = carry.entry(user_id.to_vec()).or_default();
    state.last_used_ms = Some(now_ms);
    state
}

/// What the foreign-carry lane should do on this link right now
/// ([`CoreMeshRouterState::carried_lane_for`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreCarriedLane {
    /// Offer no carried frames at all this round: the walk is complete, still
    /// inside its re-walk cooldown, and has no tail to resume behind. A
    /// completed walk that *does* know its tail sets this `false` and resumes
    /// there instead, so new arrivals never wait out the cooldown.
    pub skip: bool,
    /// Resume point to hand to the spray plan. `None` is a fresh full pass.
    pub after: Option<CoreCarriedCursor>,
}

#[derive(uniffi::Object)]
pub struct CoreMeshRouterState {
    peers: Mutex<HashMap<String, Peer>>,
    logical_carry: Mutex<HashMap<Vec<u8>, LogicalCarryState>>,
    local_user_id: Mutex<Option<Vec<u8>>>,
    next_connected_sequence: AtomicU64,
}

impl Default for CoreMeshRouterState {
    fn default() -> Self {
        Self::new()
    }
}

#[uniffi::export]
impl CoreMeshRouterState {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            logical_carry: Mutex::new(HashMap::new()),
            local_user_id: Mutex::new(None),
            next_connected_sequence: AtomicU64::new(0),
        }
    }

    /// Install the identity used for symmetric BLE-role election. For two
    /// authenticated users, the smaller user id selects its central route and
    /// the larger selects the inverse peripheral route, so both endpoints pick
    /// the same physical connection rather than crossing over and duplicating
    /// every frame on the two links.
    pub fn set_local_user_id(&self, user_id: Vec<u8>) {
        *self.local_user_id.lock_recoverable() = Some(user_id);
    }

    pub fn on_connected(&self, address: String, transport: CoreTransport) {
        self.insert_peer(address, transport, false);
    }

    /// A link that proved it belongs to one of THIS person's own devices
    /// (`specs/multi-device-v1.md` §10 step 5), registered so frames can be
    /// written to it and marked so that nothing treats it as a peer.
    ///
    /// Separate from [`Self::on_connected`] because "no user id yet" and "never
    /// a route" are different facts that happen to look alike. The epidemic
    /// fanout floods every link of the first kind — that is what makes gossip
    /// work on a link whose HELLO has not landed — and a device this person
    /// has just removed must not be on the receiving end of that. Nor may it
    /// claim a contact's user id in a HELLO and take over that contact's
    /// route: [`Self::on_hello`] refuses one here.
    pub fn on_own_device_connected(&self, address: String, transport: CoreTransport) {
        self.insert_peer(address, transport, true);
    }

    pub fn on_disconnected(&self, address: String) {
        self.peers.lock_recoverable().remove(&address);
    }

    pub fn on_hello(&self, address: String, user_id: Vec<u8>) -> bool {
        let mut peers = self.peers.lock_recoverable();
        let Some(peer) = peers.get_mut(&address) else {
            return false;
        };
        // A link admitted as one of this person's own devices is not a route
        // and cannot become one by saying so. Without this, a removed phone —
        // which still holds the agreement key that admits it — could name a
        // contact in a HELLO and win that contact's elected route.
        if peer.own_device {
            return false;
        }
        if peer.user_id.as_ref().is_some_and(|known| *known != user_id) {
            return false;
        }
        peer.user_id = Some(user_id.clone());
        // A fresh legacy HELLO is a new handshake, so the once-per-session
        // hidden-kind spray bound resets and this session gets one new offer.
        // The carry walk deliberately does not reset: it belongs to the
        // authenticated user, so a second address or reconnect must not
        // multiply the same backlog offer. Its cooldown provides the eventual
        // full re-walk needed for link-FIFO loss.
        peer.hidden_offered.clear();
        drop(peers);
        self.logical_carry
            .lock_recoverable()
            .entry(user_id)
            .or_default();
        true
    }

    /// HELLO2 follow-up: same identity-consistency rule as [`Self::on_hello`],
    /// plus the peer's capability bits. Order-tolerant with the legacy HELLO
    /// (which is sent first but could be processed either side of this).
    pub fn on_hello2(&self, address: String, user_id: Vec<u8>, capabilities: u32) -> bool {
        let mut peers = self.peers.lock_recoverable();
        let Some(peer) = peers.get_mut(&address) else {
            return false;
        };
        // Same rule as [`Self::on_hello`], for the same reason.
        if peer.own_device {
            return false;
        }
        if peer.user_id.as_ref().is_some_and(|known| *known != user_id) {
            return false;
        }
        peer.user_id = Some(user_id);
        peer.capabilities = Some(capabilities);
        true
    }

    /// Whether this peer's DELIVERED watermark can be trusted to advance past
    /// *this one* hidden spray kind — the bit for the kind
    /// ([`crate::protocol::hidden_ack_capability`]), and only that bit.
    ///
    /// `false` for pre-HELLO2 builds, which advertise nothing at all: they
    /// process hidden kinds fine but never move their watermark past them, so
    /// re-sprays toward them are bounded to once per link session instead of
    /// once per digest. `true` for any kind that is not a hidden spray kind,
    /// which every build stores and acks the ordinary way.
    ///
    /// Asked per kind rather than as one all-or-nothing mask. T23 wrote the
    /// mask version because bit 1 alone could not answer "will this peer ack
    /// kind 9", and that was right — but the fix belongs on the kind, not on
    /// the peer. Under a mask, adding [`crate::protocol::CAP_ROSTER_GOSSIP`]
    /// makes every phone in today's fleet read as not-capable and demotes the
    /// five kinds it *does* ack honestly, on every link, until the whole fleet
    /// updates. Under this test the deployed fleet's advertisements keep
    /// meaning what they meant: friend requests, profile syncs, directories and
    /// relay-change notices stay on the watermark, and only a gossiped roster
    /// takes the conservative once-per-session path toward a build that
    /// predates kind 21. The envelope is still offered on every fresh link
    /// session either way, and the direct and relay paths are untouched.
    pub fn peer_acks_hidden_kind(&self, address: String, kind: u8) -> bool {
        let Some(required) = crate::protocol::hidden_ack_capability(kind) else {
            return true;
        };
        self.peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.capabilities)
            .is_some_and(|caps| caps & required == required)
    }

    /// Every hidden spray kind this peer will ack — [`Self::peer_acks_hidden_kind`]
    /// asked once for each, for the callers that must hand the whole answer to
    /// a plan builder rather than ask envelope by envelope.
    pub fn peer_acked_hidden_kinds(&self, address: String) -> Vec<u8> {
        crate::protocol::HIDDEN_SPRAY_KINDS
            .into_iter()
            .filter(|kind| self.peer_acks_hidden_kind(address.clone(), *kind))
            .collect()
    }

    pub fn hidden_offered_for(&self, address: String) -> Vec<Vec<u8>> {
        self.peers
            .lock_recoverable()
            .get(&address)
            .map(|peer| peer.hidden_offered.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn record_hidden_offered(&self, address: String, msg_ids: Vec<Vec<u8>>) {
        if msg_ids.is_empty() {
            return;
        }
        let mut peers = self.peers.lock_recoverable();
        if let Some(peer) = peers.get_mut(&address) {
            peer.hidden_offered.extend(msg_ids);
        }
    }

    /// Where this link's foreign-carry lane should resume, or whether to sit
    /// this round out entirely.
    ///
    /// Three states, in order:
    /// * mid-walk -- resume after the last row offered;
    /// * walked, still in cooldown -- resume after the *tail* the walk reached,
    ///   so this round offers only what has been enqueued since and never
    ///   re-offers a refused row. This is what keeps a message that arrives
    ///   during the cooldown from waiting it out: the lane used to sit the
    ///   whole half hour out, so a courier that had converged with a peer and
    ///   then picked up new mail for them held it, on a live link, until the
    ///   re-walk came due;
    /// * walked, cooldown elapsed -- a fresh *full* pass (`after: None`),
    ///   deliberately not a resume. A write accepted by the transport is not a
    ///   frame the peer received; a link that dropped mid-write lost whatever
    ///   was still queued behind it, and only a pass from the top finds those
    ///   rows again. Anything the peer really does hold it advertises in its
    ///   digest, so the re-walk excludes it in SQL and costs nothing.
    ///
    /// A completed walk with no tail cursor at all still skips the round: with
    /// no resume point, the only alternative is a full pass, which is the churn
    /// the cooldown exists to prevent.
    ///
    /// An unknown address reads as a fresh pass: the caller is about to spray
    /// down a link this state has no record of, and offering is always safe.
    pub fn carried_lane_for(&self, address: String, now_ms: i64) -> CoreCarriedLane {
        let user_id = self
            .peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.user_id.clone());
        let Some(user_id) = user_id else {
            return CoreCarriedLane {
                skip: false,
                after: None,
            };
        };
        let mut carry = self.logical_carry.lock_recoverable();
        let peer = touch_and_sweep(&mut carry, &user_id, now_ms);
        let cursor = peer.carried_cursor.clone();
        lane_from(&mut peer.carried_walk_done_at_ms, cursor.as_ref(), now_ms)
    }

    /// Record what the carried lane just offered down this link: `next` is the
    /// plan's `next_carried_cursor` and `exhausted` its `carried_exhausted`.
    ///
    /// Reaching the tail starts the cooldown and *keeps* a cursor there, so
    /// rounds inside the cooldown offer only rows enqueued behind the tail and
    /// the re-walk when the cooldown ends still starts from the top
    /// ([`Self::carried_lane_for`]). A page that stopped on the budget just
    /// advances the cursor. A round that offered nothing without reaching the
    /// tail -- the lane's zero-budget off switch -- changes nothing, so the
    /// next round reconsiders exactly the same page.
    ///
    /// The kept cursor is offering bookkeeping only: it decides what is
    /// re-offered and never what is removed. Carried mail is still dropped
    /// only on digest-proof of receipt, eviction, or expiry, and a cursor
    /// pointing at a row that has since been removed stays valid because the
    /// keyset compares `(received_at, msg_id)` values rather than positions.
    pub fn record_carried_progress(
        &self,
        address: String,
        next: Option<CoreCarriedCursor>,
        exhausted: bool,
        now_ms: i64,
    ) {
        let user_id = self
            .peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.user_id.clone());
        let Some(user_id) = user_id else {
            return;
        };
        let mut carry = self.logical_carry.lock_recoverable();
        let peer = touch_and_sweep(&mut carry, &user_id, now_ms);
        if exhausted {
            peer.carried_cursor = Some(next.unwrap_or_else(|| tail_cursor_at(now_ms)));
            // A continuation round that merely re-confirms the tail must not
            // renew a cooldown that is already running; only the pass that
            // started it -- or a fresh full pass -- arms the window.
            peer.carried_walk_done_at_ms.get_or_insert(now_ms);
        } else if next.is_some() {
            peer.carried_cursor = next;
            peer.carried_walk_done_at_ms = None;
        }
    }

    /// Where the targeted HELLO carried drain should resume (G2), same three
    /// states as [`Self::carried_lane_for`] but on a disjoint cursor.
    pub fn targeted_carried_lane_for(&self, address: String, now_ms: i64) -> CoreCarriedLane {
        let user_id = self
            .peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.user_id.clone());
        let Some(user_id) = user_id else {
            return CoreCarriedLane {
                skip: false,
                after: None,
            };
        };
        let mut carry = self.logical_carry.lock_recoverable();
        let peer = touch_and_sweep(&mut carry, &user_id, now_ms);
        let cursor = peer.targeted_carried_cursor.clone();
        lane_from(
            &mut peer.targeted_carried_walk_done_at_ms,
            cursor.as_ref(),
            now_ms,
        )
    }

    /// Record progress of a targeted HELLO carried drain (G2).
    pub fn record_targeted_carried_progress(
        &self,
        address: String,
        next: Option<CoreCarriedCursor>,
        exhausted: bool,
        now_ms: i64,
    ) {
        let user_id = self
            .peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.user_id.clone());
        let Some(user_id) = user_id else {
            return;
        };
        let mut carry = self.logical_carry.lock_recoverable();
        let peer = touch_and_sweep(&mut carry, &user_id, now_ms);
        if exhausted {
            peer.targeted_carried_cursor = Some(next.unwrap_or_else(|| tail_cursor_at(now_ms)));
            peer.targeted_carried_walk_done_at_ms.get_or_insert(now_ms);
        } else if next.is_some() {
            peer.targeted_carried_cursor = next;
            peer.targeted_carried_walk_done_at_ms = None;
        }
    }

    pub fn user_id_for(&self, address: String) -> Option<Vec<u8>> {
        self.peers
            .lock_recoverable()
            .get(&address)
            .and_then(|peer| peer.user_id.clone())
    }

    pub fn transport_for(&self, address: String) -> Option<CoreTransport> {
        self.peers
            .lock_recoverable()
            .get(&address)
            .map(|peer| peer.transport)
    }

    pub fn connected_routes(&self) -> Vec<CoreTransportRoute> {
        self.peers
            .lock_recoverable()
            .iter()
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect()
    }

    /// Every live link admitted as one of THIS person's own devices
    /// ([`Self::on_own_device_connected`]), address-sorted.
    ///
    /// Such a link is deliberately absent from [`Self::identified_routes`] --
    /// it has no user id and never becomes a route -- and that absence is what
    /// left it outside every periodic pass the shells run over their links. A
    /// link nothing probes is a link nothing closes: a half-open one held its
    /// socket for the whole Wi-Fi join, carried no frames, and (being a live
    /// connection) told the LAN transport it was not lonely enough to search.
    /// That is the state an approver sat in while it failed to tell a phone it
    /// had removed.
    ///
    /// So the shells get a way to name these links: to heartbeat them like any
    /// other LAN link, and to re-offer §10 step 5's roster on them rather than
    /// only at the instant a HELLO2 arrives.
    pub fn own_device_links(&self) -> Vec<CoreTransportRoute> {
        let peers = self.peers.lock_recoverable();
        let mut routes: Vec<_> = peers
            .iter()
            .filter(|(_, peer)| peer.own_device)
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect();
        drop(peers);
        routes.sort_by(|a, b| a.address.cmp(&b.address));
        routes
    }

    pub fn identified_routes(&self) -> Vec<CoreIdentifiedRoute> {
        self.peers
            .lock_recoverable()
            .iter()
            .filter_map(|(address, peer)| {
                peer.user_id.as_ref().map(|user_id| CoreIdentifiedRoute {
                    transport: peer.transport,
                    address: address.clone(),
                    user_id: user_id.clone(),
                })
            })
            .collect()
    }

    /// One selected route per authenticated logical peer. LAN wins over BLE;
    /// the two BLE roles use the symmetric identity election documented in
    /// [`Self::set_local_user_id`]. Equal-ranked links keep the oldest live
    /// connection, making address rotation sticky until the incumbent drops.
    pub fn selected_identified_routes(&self) -> Vec<CoreIdentifiedRoute> {
        let peers = self.peers.lock_recoverable();
        let local_user_id = self.local_user_id.lock_recoverable();
        let mut selected: HashMap<Vec<u8>, (&String, &Peer)> = HashMap::new();
        for (address, peer) in peers.iter() {
            let Some(user_id) = peer.user_id.as_ref() else {
                continue;
            };
            let replace = selected.get(user_id).is_none_or(|(_, current)| {
                route_precedes(peer, current, local_user_id.as_deref(), user_id)
            });
            if replace {
                selected.insert(user_id.clone(), (address, peer));
            }
        }
        let mut routes: Vec<_> = selected
            .into_iter()
            .map(|(user_id, (address, peer))| CoreIdentifiedRoute {
                transport: peer.transport,
                address: address.clone(),
                user_id,
            })
            .collect();
        routes.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        routes
    }

    /// Whether this authenticated address is the elected application-data
    /// route for its logical peer. Other live links remain available for
    /// exact-link handshake/control replies but are quarantined from bulk
    /// drains and periodic fanout.
    pub fn is_selected_route(&self, address: String) -> bool {
        self.selected_identified_routes()
            .iter()
            .any(|route| route.address == address)
    }

    /// Epidemic fanout plan: one route per authenticated user plus every
    /// not-yet-identified link. If the source is identified, exclude all of
    /// that user's physical routes so a frame cannot echo through its other
    /// BLE role or rotated address.
    ///
    /// A link admitted as one of this person's own devices
    /// ([`Self::on_own_device_connected`]) is in neither set. It has no user
    /// id, but it is not *not yet* identified either — it is a carrier for the
    /// §10 step 5 roster notice and nothing else, so flooding it would hand a
    /// device this person may have just removed a live feed of every envelope
    /// this phone sends or relays.
    pub fn relay_routes(&self, except_address: Option<String>) -> Vec<CoreTransportRoute> {
        let excluded_user = {
            let peers = self.peers.lock_recoverable();
            except_address
                .as_ref()
                .and_then(|address| peers.get(address))
                .and_then(|peer| peer.user_id.clone())
        };
        let selected_addresses: HashSet<_> = self
            .selected_identified_routes()
            .into_iter()
            .filter(|route| excluded_user.as_ref() != Some(&route.user_id))
            .map(|route| route.address)
            .collect();
        let peers = self.peers.lock_recoverable();
        let mut routes: Vec<_> = peers
            .iter()
            .filter(|(address, peer)| {
                if peer.own_device {
                    false
                } else if peer.user_id.is_some() {
                    selected_addresses.contains(*address)
                } else {
                    except_address.as_ref() != Some(*address)
                }
            })
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect();
        routes.sort_by(|a, b| a.address.cmp(&b.address));
        routes
    }

    pub fn connected_user_count(&self) -> u32 {
        self.peers
            .lock_recoverable()
            .values()
            .filter_map(|peer| peer.user_id.clone())
            .collect::<HashSet<_>>()
            .len() as u32
    }

    pub fn route_for(&self, user_id: Vec<u8>) -> Option<CoreTransportRoute> {
        self.routes_for(user_id).into_iter().next()
    }

    pub fn routes_for(&self, user_id: Vec<u8>) -> Vec<CoreTransportRoute> {
        let peers = self.peers.lock_recoverable();
        let local_user_id = self.local_user_id.lock_recoverable();
        let mut matching: Vec<_> = peers
            .iter()
            .filter(|(_, peer)| peer.user_id.as_ref() == Some(&user_id))
            .collect();
        matching.sort_by(|(address_a, peer_a), (address_b, peer_b)| {
            route_ordering(
                peer_a,
                address_a,
                peer_b,
                address_b,
                local_user_id.as_deref(),
                &user_id,
            )
        });
        matching
            .into_iter()
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect()
    }

    pub fn helloed_user_ids(&self) -> Vec<Vec<u8>> {
        self.peers
            .lock_recoverable()
            .values()
            .filter_map(|peer| peer.user_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn clear_transports(&self, transports: Vec<CoreTransport>) {
        self.peers
            .lock_recoverable()
            .retain(|_, peer| !transports.contains(&peer.transport));
    }

    pub fn clear(&self) {
        self.peers.lock_recoverable().clear();
        self.logical_carry.lock_recoverable().clear();
    }
}

/// The one place a peer row is created, shared by [`CoreMeshRouterState::
/// on_connected`] and [`CoreMeshRouterState::on_own_device_connected`].
///
/// Outside the `#[uniffi::export]` block deliberately: `uniffi` exports every
/// method in an exported `impl`, `pub` or not, and the frozen mobile ABI must
/// not grow an entry point that lets a shell mint a peer row of either kind
/// with the flag of its choosing. The two named doors are the whole surface.
impl CoreMeshRouterState {
    fn insert_peer(&self, address: String, transport: CoreTransport, own_device: bool) {
        self.peers.lock_recoverable().insert(
            address,
            Peer {
                transport,
                connected_sequence: self.next_connected_sequence.fetch_add(1, Ordering::Relaxed),
                user_id: None,
                capabilities: None,
                last_digest_at_ms: None,
                hidden_offered: std::collections::HashSet::new(),
                own_device,
            },
        );
    }
}

/// Encounter-planning accessors used by [`crate::session::mesh_meet`].
///
/// Deliberately outside the `#[uniffi::export]` block: the encounter planner
/// is core-internal until the shell adapters land, and the frozen mobile ABI
/// should not grow entry points nothing on the wire uses yet. Extending the
/// one router (rule: never a second one) is what keeps digest cadence, carry
/// cursors and route election reading the same peer record.
impl CoreMeshRouterState {
    /// Capability bits this link's peer advertised in HELLO2, or `None` for a
    /// pre-HELLO2 build.
    pub fn peer_capabilities_for(&self, address: &str) -> Option<u32> {
        self.peers
            .lock_recoverable()
            .get(address)
            .and_then(|peer| peer.capabilities)
    }

    /// Whether this link should put a DIGEST on the wire now (D8).
    ///
    /// A link that has never digested is due immediately -- that is the whole
    /// point of a fresh session, and it is why the marker lives on the link
    /// rather than on the logical peer (unlike the carry cursors, which
    /// deliberately survive address rotation so a reconnect cannot multiply a
    /// backlog offer: a digest is one small frame, a carry walk is megabytes).
    /// After that the [`should_redigest`] window applies, jittered from the
    /// peer's *identity* rather than from a clock or an address, so the whole
    /// fleet does not re-digest on the same tick and one pair's phase is
    /// stable across reconnects and address rotation.
    ///
    /// An address this state does not know reads as due: offering a digest is
    /// always safe, and refusing one is how a link goes quiet forever.
    pub fn digest_due_for(&self, address: &str, now_ms: i64) -> bool {
        let peers = self.peers.lock_recoverable();
        let Some(peer) = peers.get(address) else {
            return true;
        };
        let Some(last) = peer.last_digest_at_ms else {
            return true;
        };
        let seed = peer
            .user_id
            .as_deref()
            .map_or_else(|| jitter_seed(address.as_bytes()), jitter_seed);
        should_redigest(now_ms, last, seed)
    }

    /// A DIGEST for this link just went out; start its re-digest window.
    pub fn record_digest_sent(&self, address: &str, now_ms: i64) {
        if let Some(peer) = self.peers.lock_recoverable().get_mut(address) {
            peer.last_digest_at_ms = Some(now_ms);
        }
    }
}

/// FNV-1a over an identity, for jitter that is deterministic per peer and
/// carries no clock or address in it (determinism rule: identity-derived
/// jitter, never `rand`).
fn jitter_seed(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn route_precedes(
    candidate: &Peer,
    current: &Peer,
    local_user_id: Option<&[u8]>,
    peer_user_id: &[u8],
) -> bool {
    let candidate_priority = candidate.transport.priority(local_user_id, peer_user_id);
    let current_priority = current.transport.priority(local_user_id, peer_user_id);
    candidate_priority > current_priority
        || (candidate_priority == current_priority
            && candidate.connected_sequence < current.connected_sequence)
}

fn route_ordering(
    a: &Peer,
    address_a: &str,
    b: &Peer,
    address_b: &str,
    local_user_id: Option<&[u8]>,
    peer_user_id: &[u8],
) -> std::cmp::Ordering {
    b.transport
        .priority(local_user_id, peer_user_id)
        .cmp(&a.transport.priority(local_user_id, peer_user_id))
        .then_with(|| a.connected_sequence.cmp(&b.connected_sequence))
        .then_with(|| address_a.cmp(address_b))
}

#[uniffi::export]
/// Return exactly the first route supplied by the caller.
///
/// # Contract
///
/// `routes` must come from [`CoreMeshRouterState::routes_for`], which has
/// already applied LAN preference, authenticated symmetric BLE-role election,
/// and sticky connection age. This helper deliberately does not re-sort:
/// without both user ids it cannot reproduce the identity-dependent BLE
/// election. `frame_size` remains in the ABI for compatibility with clients
/// that predate the single-route policy.
pub fn core_transport_send_plan(
    routes: Vec<CoreTransportRoute>,
    frame_size: u32,
) -> Vec<CoreTransportRoute> {
    let _ = frame_size;
    // A logical peer gets exactly one application-data route; disconnecting
    // it makes the next preference-ordered call naturally fail over.
    routes.into_iter().take(1).collect()
}

#[derive(Clone, Copy)]
struct BackoffState {
    consecutive_failures: u32,
    next_eligible_at_ms: i64,
}

#[derive(uniffi::Object)]
pub struct CoreReconnectBackoffTracker {
    initial_backoff_ms: i64,
    max_backoff_ms: i64,
    max_consecutive_failures: u32,
    give_up_probe_ms: i64,
    state: Mutex<HashMap<String, BackoffState>>,
}

/// Exponential per-address reconnect backoff. Once an address exceeds
/// `max_consecutive_failures` it is "given up": attempts continue, but only
/// at the slow fixed `give_up_probe_ms` cadence. Give-up must never be a
/// permanent refusal: transient radio failures (2026-07-24, live two-phone
/// evidence: GATT connect storms while Wi-Fi tears down) can burn the whole
/// failure budget against a peer's *current* advertisement address, which
/// then stays valid for many more minutes — a permanent refusal wedges the
/// link on both sides until Bluetooth itself is cycled. A slow probe caps
/// the cost of a truly stale address at one attempt per interval while
/// guaranteeing a live peer is relinked within roughly one probe interval
/// of the radio settling.
#[uniffi::export]
impl CoreReconnectBackoffTracker {
    #[uniffi::constructor]
    pub fn new(
        initial_backoff_ms: i64,
        max_backoff_ms: i64,
        max_failures: u32,
        give_up_probe_ms: i64,
    ) -> Self {
        Self {
            initial_backoff_ms: initial_backoff_ms.max(0),
            max_backoff_ms: max_backoff_ms.max(0),
            max_consecutive_failures: max_failures,
            give_up_probe_ms: give_up_probe_ms.max(0),
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn can_attempt(&self, address: String, now_ms: i64) -> bool {
        self.state
            .lock_recoverable()
            .get(&address)
            .is_none_or(|state| now_ms >= state.next_eligible_at_ms)
    }

    /// True once the address is past the consecutive-failure budget and in
    /// slow-probe mode. Informational (logging/diagnostics) — callers must
    /// not use it to stop retrying; `can_attempt`/`retry_delay_ms` already
    /// encode the probe cadence.
    pub fn is_given_up(&self, address: String) -> bool {
        self.failure_count(address) >= self.max_consecutive_failures
    }

    pub fn failure_count(&self, address: String) -> u32 {
        self.state
            .lock_recoverable()
            .get(&address)
            .map_or(0, |state| state.consecutive_failures)
    }

    pub fn retry_delay_ms(&self, address: String, now_ms: i64) -> Option<i64> {
        self.state
            .lock_recoverable()
            .get(&address)
            .map(|state| state.next_eligible_at_ms.saturating_sub(now_ms).max(0))
    }

    pub fn record_failure(&self, address: String, now_ms: i64) -> u32 {
        let mut states = self.state.lock_recoverable();
        let failures = states
            .get(&address)
            .map_or(1, |state| state.consecutive_failures.saturating_add(1));
        let backoff = if failures >= self.max_consecutive_failures {
            self.give_up_probe_ms
        } else {
            let multiplier = 1_i64
                .checked_shl(failures.saturating_sub(1).min(20))
                .unwrap_or(i64::MAX);
            self.initial_backoff_ms
                .saturating_mul(multiplier)
                .min(self.max_backoff_ms)
        };
        states.insert(
            address,
            BackoffState {
                consecutive_failures: failures,
                next_eligible_at_ms: now_ms.saturating_add(backoff),
            },
        );
        failures
    }

    pub fn record_success(&self, address: String) {
        self.state.lock_recoverable().remove(&address);
    }
    pub fn clear(&self) {
        self.state.lock_recoverable().clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreLanHealthAction {
    Send,
    Wait,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreLanHealthDecision {
    pub action: CoreLanHealthAction,
    pub nonce: Option<u64>,
}

#[derive(Clone, Copy)]
struct LanLinkState {
    pending_nonce: Option<u64>,
    sent_at_ms: i64,
    consecutive_timeouts: u32,
}

#[derive(uniffi::Object)]
pub struct CoreLanHealthTracker {
    timeout_ms: i64,
    max_consecutive_timeouts: u32,
    links: Mutex<HashMap<String, LanLinkState>>,
}

#[uniffi::export]
impl CoreLanHealthTracker {
    #[uniffi::constructor]
    pub fn new(timeout_ms: i64, max_timeouts: u32) -> Self {
        Self {
            timeout_ms: timeout_ms.max(0),
            max_consecutive_timeouts: max_timeouts,
            links: Mutex::new(HashMap::new()),
        }
    }

    pub fn next(&self, address: String, now_ms: i64, nonce: u64) -> CoreLanHealthDecision {
        let mut links = self.links.lock_recoverable();
        let Some(current) = links.get(&address).copied() else {
            links.insert(
                address,
                LanLinkState {
                    pending_nonce: Some(nonce),
                    sent_at_ms: now_ms,
                    consecutive_timeouts: 0,
                },
            );
            return health_decision(CoreLanHealthAction::Send, Some(nonce));
        };
        if current.pending_nonce.is_none() {
            links.insert(
                address,
                LanLinkState {
                    pending_nonce: Some(nonce),
                    sent_at_ms: now_ms,
                    ..current
                },
            );
            return health_decision(CoreLanHealthAction::Send, Some(nonce));
        }
        if now_ms.saturating_sub(current.sent_at_ms) < self.timeout_ms {
            return health_decision(CoreLanHealthAction::Wait, None);
        }
        let failures = current.consecutive_timeouts.saturating_add(1);
        if failures >= self.max_consecutive_timeouts {
            links.remove(&address);
            return health_decision(CoreLanHealthAction::Close, None);
        }
        links.insert(
            address,
            LanLinkState {
                pending_nonce: Some(nonce),
                sent_at_ms: now_ms,
                consecutive_timeouts: failures,
            },
        );
        health_decision(CoreLanHealthAction::Send, Some(nonce))
    }

    pub fn response(&self, address: String, nonce: u64, now_ms: i64) -> Option<i64> {
        let mut links = self.links.lock_recoverable();
        let current = links.get(&address).copied()?;
        if current.pending_nonce != Some(nonce) {
            return None;
        }
        links.insert(
            address,
            LanLinkState {
                pending_nonce: None,
                sent_at_ms: 0,
                consecutive_timeouts: 0,
            },
        );
        Some(now_ms.saturating_sub(current.sent_at_ms).max(0))
    }

    pub fn remove(&self, address: String) {
        self.links.lock_recoverable().remove(&address);
    }
    pub fn clear(&self) {
        self.links.lock_recoverable().clear();
    }
}

fn health_decision(action: CoreLanHealthAction, nonce: Option<u64>) -> CoreLanHealthDecision {
    CoreLanHealthDecision { action, nonce }
}

/// How long a failover "resume sync" fan-out waits for the rest of a radio
/// event's disconnects to land before it runs — see
/// [`CoreFailoverResumeDebounce`].
///
/// Sized from field evidence (2026-08-07 capture): when several BLE links die
/// in one radio event, the per-link disconnect callbacks arrived spread over
/// roughly 240ms. A window that outlasts that whole spread is what makes the
/// difference between resuming onto a route whose own death is still in
/// flight and resuming onto whatever route actually survived. 300ms is the
/// smallest round number above the observed spread; it is deliberately not
/// much larger, because this delay is added to every genuine failover before
/// bulk sync continues.
pub const FAILOVER_RESUME_WINDOW_MS: i64 = 300;

/// The default [`CoreFailoverResumeDebounce`] window, exported so both shells
/// read one number instead of each hardcoding its own copy.
#[uniffi::export]
pub fn core_failover_resume_window_ms() -> i64 {
    FAILOVER_RESUME_WINDOW_MS
}

/// Leading-edge, per-logical-peer debounce for the failover resume fan-out
/// (Android `MeshService.resumeLogicalPeerSync`, iOS
/// `MeshController.resumeLogicalPeerSync`).
///
/// The bug this exists for (2026-08-07): resuming ran *synchronously inside*
/// the BLE disconnect callback, so when several centrals dropped in one radio
/// event the very first callback immediately queued a multi-KB carry drain
/// plus a digest onto the peer's sibling route — a route whose own
/// disconnect callback was still ~100ms away. The notification was rejected
/// outright and the sibling link was torn down as a send failure, which is a
/// worse outcome than simply waiting: the frames were wasted, the teardown
/// was attributed to the wrong cause, and the sync had to happen again anyway
/// once the real route was elected.
///
/// Semantics are deliberately *leading-edge with no extension*: the first
/// request for a key arms a window and every further request inside that
/// window is absorbed into it, so a burst of `n` disconnects for one logical
/// peer produces exactly one resume, and that resume is guaranteed to run
/// within one window of the burst's *start* rather than being pushed further
/// out by each new event. A trailing/extending debounce could starve a resume
/// indefinitely under a steady disconnect stream, which for a mesh whose
/// whole job is to keep syncing is the worse failure.
///
/// Coalescing is per key (the peer's UserID hex), never global: two different
/// peers failing over in the same radio event each get their own resume.
///
/// Callers must pass the [`CoreFailoverResumeArm::token`] they were handed back
/// to [`Self::fired`]. Without it, a timer finishing at the exact moment a new
/// window is armed for the same peer would clear the *new* window's marker, and
/// the next disconnect would arm (and run) a third resume — the very
/// duplication this object exists to prevent. The token makes "the window I
/// armed is over" exact rather than "some window for this peer is over".
#[derive(uniffi::Object)]
pub struct CoreFailoverResumeDebounce {
    window_ms: i64,
    state: Mutex<FailoverResumeState>,
}

/// The caller's half of an armed window: schedule the resume `delay_ms` out,
/// then hand `token` back to [`CoreFailoverResumeDebounce::fired`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CoreFailoverResumeArm {
    pub delay_ms: i64,
    pub token: i64,
}

#[derive(Default)]
struct FailoverResumeState {
    /// key -> the window currently armed for it.
    armed: HashMap<String, ArmedResumeWindow>,
    /// Monotonically increasing, so a token is never confused with an older
    /// window's token for the same key.
    last_token: i64,
}

#[derive(Clone, Copy)]
struct ArmedResumeWindow {
    armed_at_ms: i64,
    token: i64,
}

#[uniffi::export]
impl CoreFailoverResumeDebounce {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self::with_window_ms(FAILOVER_RESUME_WINDOW_MS)
    }

    #[uniffi::constructor]
    pub fn with_window_ms(window_ms: i64) -> Self {
        Self {
            window_ms: window_ms.max(0),
            state: Mutex::new(FailoverResumeState::default()),
        }
    }

    pub fn window_ms(&self) -> i64 {
        self.window_ms
    }

    /// Asks whether this failover should arm a timer. `Some(arm)` means the
    /// caller owns the window and must schedule the resume `arm.delay_ms` out,
    /// calling [`Self::fired`] with `arm.token` when the timer runs; `None`
    /// means an already armed window will cover this request, so the caller
    /// does nothing.
    ///
    /// A marker older than the window is treated as lost rather than as
    /// permanently pending (a cancelled timer, a process-lifecycle hiccup, or
    /// a clock jump backwards): it re-arms. Without that, one dropped timer
    /// would silently disable failover resume for that peer forever, which is
    /// exactly the class of silent-permanent-failure this file's other
    /// trackers are written to avoid.
    ///
    /// `now_ms` must come from the *same* clock the caller's timer runs on --
    /// a monotonic one on both shells (`SystemClock.elapsedRealtime()` /
    /// `DispatchTime.now()`). Measuring the window on the wall clock while the
    /// timer counts down on a monotonic one lets an NTP correction desynchronise
    /// the two and produce a second resume for one burst.
    pub fn request(&self, key: String, now_ms: i64) -> Option<CoreFailoverResumeArm> {
        let mut state = self.state.lock_recoverable();
        if let Some(existing) = state.armed.get(&key) {
            let elapsed = now_ms.saturating_sub(existing.armed_at_ms);
            if elapsed >= 0 && elapsed < self.window_ms {
                return None;
            }
        }
        state.last_token = state.last_token.wrapping_add(1);
        let token = state.last_token;
        state.armed.insert(
            key,
            ArmedResumeWindow {
                armed_at_ms: now_ms,
                token,
            },
        );
        Some(CoreFailoverResumeArm {
            delay_ms: self.window_ms,
            token,
        })
    }

    /// The timer armed as `token` for `key` just ran; that window is over, so
    /// the next failover for this peer arms a fresh one. Call this *before*
    /// doing the resume work so a disconnect arriving during that work starts a
    /// new window instead of being swallowed.
    ///
    /// A stale token (the window was already replaced or cancelled) is ignored:
    /// clearing someone else's marker is what would under-coalesce.
    pub fn fired(&self, key: String, token: i64) {
        let mut state = self.state.lock_recoverable();
        if state.armed.get(&key).map(|window| window.token) == Some(token) {
            state.armed.remove(&key);
        }
    }

    /// Drops whatever window is pending for `key` without running it (the peer
    /// went away entirely, so no token is at hand and none is wanted).
    pub fn cancel(&self, key: String) {
        self.state.lock_recoverable().armed.remove(&key);
    }

    pub fn is_pending(&self, key: String) -> bool {
        self.state.lock_recoverable().armed.contains_key(&key)
    }

    /// Forgets every pending window, e.g. on a full mesh stop.
    pub fn clear(&self) {
        self.state.lock_recoverable().armed.clear();
    }
}

impl Default for CoreFailoverResumeDebounce {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_recovers_from_a_poisoned_mutex_instead_of_crash_looping() {
        // FC6: before lock_recoverable, every method here did
        // `self.peers.lock().unwrap()` -- one panic while holding the lock
        // would poison the Mutex forever, and every later call would itself
        // panic. Simulate that first panic (inside catch_unwind so the test
        // process survives) and confirm a later call still succeeds.
        let router = CoreMeshRouterState::new();
        router.on_connected("ble".into(), CoreTransport::Central);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = router.peers.lock().unwrap();
            panic!("FC6 test: simulated panic while holding the peers mutex");
        }));
        assert!(panicked.is_err(), "the closure above should have panicked");
        assert!(router.peers.is_poisoned());

        // A later call through the normal API must recover the guard and
        // succeed, not panic -- and the state from before the panic (the
        // "ble" peer) must still be there since the panic never touched it.
        assert!(router.on_hello("ble".into(), vec![1]));
        assert_eq!(router.connected_user_count(), 1);
    }

    #[test]
    fn hello2_records_capabilities_and_hello_resets_the_session_offer_set() {
        let router = CoreMeshRouterState::new();
        router.on_connected("ble".into(), CoreTransport::Central);

        // Pre-HELLO2 peer: no capabilities recorded, so no hidden kind at all.
        assert!(router.on_hello("ble".into(), vec![1; 16]));
        assert!(router.peer_acked_hidden_kinds("ble".into()).is_empty());
        // A kind that is not a hidden spray kind needs no bit from anyone.
        assert!(router.peer_acks_hidden_kind("ble".into(), crate::KIND_TEXT));

        // Offers recorded for the session are returned...
        router.record_hidden_offered("ble".into(), vec![vec![9; 16]]);
        assert_eq!(router.hidden_offered_for("ble".into()), vec![vec![9; 16]]);

        // ...and a fresh legacy HELLO (new handshake) resets them.
        assert!(router.on_hello("ble".into(), vec![1; 16]));
        assert!(router.hidden_offered_for("ble".into()).is_empty());

        // A peer that advertises only the pre-kind-9 bit is truthful about
        // kinds 3/5/6/7 and is trusted for exactly those; it still drops a
        // relay-change notice, so kind 9 is not trusted.
        assert!(router.on_hello2(
            "ble".into(),
            vec![1; 16],
            crate::protocol::CAP_ACKS_HIDDEN_KINDS
        ));
        assert!(router.peer_acks_hidden_kind("ble".into(), crate::KIND_FRIEND_REQUEST));
        assert!(!router.peer_acks_hidden_kind("ble".into(), crate::KIND_RELAY_UPDATE));
        assert!(!router.peer_acks_hidden_kind("ble".into(), crate::KIND_ROSTER_GOSSIP));

        // **The deployed fleet.** Today's phones advertise the two older bits
        // and mean them; they have never heard of kind 21. They must keep the
        // watermark for everything they really do ack, and take the
        // once-per-session bound on the gossiped roster alone.
        let legacy = crate::protocol::CAP_ACKS_HIDDEN_KINDS
            | crate::protocol::CAP_RELAY_UPDATE
            | crate::protocol::CAP_MULTI_DEVICE;
        assert!(router.on_hello2("ble".into(), vec![1; 16], legacy));
        assert_eq!(
            router.peer_acked_hidden_kinds("ble".into()),
            vec![
                crate::KIND_FRIEND_REQUEST,
                crate::KIND_PROFILE_SYNC,
                crate::KIND_FRIEND_DIRECTORY,
                crate::KIND_INTRODUCED_FRIEND_REQUEST,
                crate::KIND_RELAY_UPDATE,
            ]
        );
        assert!(!router.peer_acks_hidden_kind("ble".into(), crate::KIND_ROSTER_GOSSIP));

        // HELLO2 sets capabilities; identity consistency still enforced.
        assert!(router.on_hello2(
            "ble".into(),
            vec![1; 16],
            crate::protocol::core_own_capabilities()
        ));
        assert_eq!(
            router.peer_acked_hidden_kinds("ble".into()).len(),
            crate::protocol::HIDDEN_SPRAY_KINDS.len()
        );
        // WPT: unknown capability bits (including reserved CAP_MULTI_DEVICE)
        // are stored, not rejected. Known-bit checks still see the bits they
        // care about.
        let future_caps = crate::protocol::core_own_capabilities()
            | crate::protocol::CAP_MULTI_DEVICE
            | (1 << 31);
        assert!(router.on_hello2("ble".into(), vec![1; 16], future_caps));
        assert_eq!(
            router.peer_acked_hidden_kinds("ble".into()).len(),
            crate::protocol::HIDDEN_SPRAY_KINDS.len()
        );
        assert!(!router.on_hello2("ble".into(), vec![2; 16], 1));

        // Disconnect drops the whole peer record, offers included.
        router.on_disconnected("ble".into());
        assert!(router.hidden_offered_for("ble".into()).is_empty());
        assert!(router.peer_acked_hidden_kinds("ble".into()).is_empty());
    }

    #[test]
    fn a_live_link_re_walks_after_the_cooldown_instead_of_renewing_it_forever() {
        // The cooldown used to be re-stamped by every round that re-confirmed
        // the tail, so on a link re-digesting every few minutes the 30-minute
        // safety re-walk was pushed out forever and a frame lost in a
        // transport FIFO could never be found again.
        let router = CoreMeshRouterState::new();
        router.on_connected("ble".into(), CoreTransport::Central);
        assert!(router.on_hello("ble".into(), vec![7; 16]));
        let now = 1_700_000_000_000_i64;
        let tail = CoreCarriedCursor {
            received_at: now,
            msg_id: vec![1; 16],
        };

        // Round 0 walks to the tail and starts the cooldown.
        router.record_carried_progress("ble".into(), Some(tail.clone()), true, now);

        // Rounds every 5 minutes for an hour: each resumes from the tail,
        // finds nothing, and reports exhaustion again.
        let round_ms = 5 * 60_000;
        let mut full_passes = 0;
        for round in 1..=12 {
            let at = now + round * round_ms;
            let lane = router.carried_lane_for("ble".into(), at);
            assert!(!lane.skip);
            if lane.after.is_none() {
                full_passes += 1;
            }
            router.record_carried_progress("ble".into(), None, true, at);
        }
        assert_eq!(
            full_passes, 2,
            "one re-walk per 30-minute cooldown -- not zero (the old renewal              bug) and not one per round (which is the churn the cooldown              exists to prevent)"
        );
    }

    #[test]
    fn the_carried_lane_resumes_mid_walk_and_parks_once_it_reaches_the_tail() {
        let router = CoreMeshRouterState::new();
        let cursor = |n: u8| CoreCarriedCursor {
            received_at: n as i64 * 1_000,
            msg_id: vec![n; 16],
        };
        let now = 1_700_000_000_000_i64;

        // A link this state has never seen: offer, from the top.
        assert_eq!(
            router.carried_lane_for("ble".into(), now),
            CoreCarriedLane {
                skip: false,
                after: None
            }
        );

        router.on_connected("ble".into(), CoreTransport::Central);
        assert!(router.on_hello("ble".into(), vec![1; 16]));
        assert_eq!(
            router.carried_lane_for("ble".into(), now),
            CoreCarriedLane {
                skip: false,
                after: None
            },
            "a fresh session starts its walk at the top"
        );

        // Mid-walk: each round resumes where the last one stopped.
        router.record_carried_progress("ble".into(), Some(cursor(1)), false, now);
        assert_eq!(
            router.carried_lane_for("ble".into(), now + 1),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(1))
            }
        );
        router.record_carried_progress("ble".into(), Some(cursor(2)), false, now + 1);
        assert_eq!(
            router.carried_lane_for("ble".into(), now + 2),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(2))
            }
        );

        // A round that offered nothing without reaching the tail (the
        // zero-budget off switch) leaves the walk exactly where it was.
        router.record_carried_progress("ble".into(), None, false, now + 3);
        assert_eq!(
            router.carried_lane_for("ble".into(), now + 4),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(2))
            }
        );

        // Tail reached: the cooldown starts, and rounds inside it resume from
        // the tail so only mail enqueued since is offered.
        router.record_carried_progress("ble".into(), Some(cursor(3)), true, now + 5);
        let done_at = now + 5;
        assert_eq!(
            router.carried_lane_for("ble".into(), done_at + CARRIED_REWALK_MIN_INTERVAL_MS - 1),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(3))
            },
            "inside the cooldown: offer only what landed behind the tail"
        );

        // Cooldown elapsed: a fresh FULL pass, not a resume -- frames lost in
        // a link's FIFO are only found again from the top.
        assert_eq!(
            router.carried_lane_for("ble".into(), done_at + CARRIED_REWALK_MIN_INTERVAL_MS),
            CoreCarriedLane {
                skip: false,
                after: None
            }
        );

        // A fresh handshake for the same logical peer does not reset the walk
        // mid-cooldown. A rotating address must not multiply its offers.
        router.record_carried_progress("ble".into(), Some(cursor(4)), true, done_at);
        assert!(router.on_hello("ble".into(), vec![1; 16]));
        assert_eq!(
            router.carried_lane_for("ble".into(), done_at + 1),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(4))
            },
            "a new handshake for one user shares its completed logical lane"
        );

        // Disconnect/reconnect under a rotated address also retains progress.
        // The eventual cooldown re-walk is the safety net for a frame that was
        // accepted into the old link's FIFO but lost at teardown.
        router.on_disconnected("ble".into());
        router.on_connected("rotated".into(), CoreTransport::Peripheral);
        assert!(router.on_hello("rotated".into(), vec![1; 16]));
        assert_eq!(
            router.carried_lane_for("rotated".into(), done_at + 2),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor(4))
            },
            "reconnect does not restart a completed logical-peer walk"
        );
        // A progress report for a link that is already gone is a no-op, not a
        // resurrected peer entry.
        router.record_carried_progress("ble".into(), Some(cursor(6)), false, done_at);
        assert_eq!(router.connected_user_count(), 1);
    }

    /// One carry queue, ordered the way the store orders it.
    fn queue_rows(start_received_at: i64, count: u32) -> Vec<CoreCarriedCursor> {
        (0..count)
            .map(|index| CoreCarriedCursor {
                received_at: start_received_at + index as i64,
                msg_id: index.to_be_bytes().to_vec(),
            })
            .collect()
    }

    /// Stand-in for the store's budgeted keyset page: rows ordered by
    /// `(received_at, msg_id)`, everything at or before `after` skipped, at
    /// most `page` rows returned. `exhausted` is false only when the page
    /// stopped on the budget, which is exactly the store's rule.
    fn page_after(
        rows: &[CoreCarriedCursor],
        after: Option<&CoreCarriedCursor>,
        page: usize,
    ) -> (Vec<CoreCarriedCursor>, bool) {
        let key = |cursor: &CoreCarriedCursor| (cursor.received_at, cursor.msg_id.clone());
        let mut remaining: Vec<CoreCarriedCursor> = rows
            .iter()
            .filter(|row| after.is_none_or(|resume| key(row) > key(resume)))
            .cloned()
            .collect();
        remaining.sort_by_key(&key);
        let exhausted = remaining.len() <= page;
        remaining.truncate(page);
        (remaining, exhausted)
    }

    /// Run one foreign-carry round against `rows` and return what it offered.
    fn carry_round(
        router: &CoreMeshRouterState,
        address: &str,
        rows: &[CoreCarriedCursor],
        page: usize,
        now_ms: i64,
    ) -> Vec<CoreCarriedCursor> {
        let lane = router.carried_lane_for(address.to_string(), now_ms);
        if lane.skip {
            return Vec::new();
        }
        let (offered, exhausted) = page_after(rows, lane.after.as_ref(), page);
        router.record_carried_progress(
            address.to_string(),
            offered.last().cloned(),
            exhausted,
            now_ms,
        );
        offered
    }

    #[test]
    fn a_second_full_offer_cycle_on_one_address_offers_only_the_new_mail() {
        // Field shape: a courier and a peer stay linked at the same address
        // while the courier walks a 220-row backlog to the tail, then picks up
        // another 220 rows for the same peer. The second cycle must offer the
        // new rows promptly and must not re-offer the first 220 -- and the
        // walk must not simply stop for the half-hour cooldown either, which
        // is what held new mail on a live link.
        let router = CoreMeshRouterState::new();
        router.on_connected("ble".into(), CoreTransport::Central);
        assert!(router.on_hello("ble".into(), vec![1; 16]));

        let now = 1_700_000_000_000_i64;
        let first: Vec<CoreCarriedCursor> = queue_rows(now, 220);
        let mut clock = now;
        let mut offered = Vec::new();
        for _ in 0..40 {
            clock += 1_000;
            let round = carry_round(&router, "ble", &first, 20, clock);
            if round.is_empty() {
                break;
            }
            offered.extend(round);
        }
        assert_eq!(offered, first, "the first cycle walks all 220 rows once");
        let done_at = clock;

        // 220 more rows land for the same peer while the link stays up.
        let mut queue = first.clone();
        queue.extend(queue_rows(done_at + 1, 220));

        let mut second = Vec::new();
        for _ in 0..40 {
            clock += 1_000;
            assert!(
                clock - done_at < CARRIED_REWALK_MIN_INTERVAL_MS,
                "the whole second cycle must run inside the cooldown"
            );
            let round = carry_round(&router, "ble", &queue, 20, clock);
            if round.is_empty() {
                break;
            }
            second.extend(round);
        }
        assert_eq!(
            second,
            queue[220..].to_vec(),
            "the second cycle offers exactly the new rows, none of the first 220"
        );

        // And a converged pair goes quiet again: nothing new, nothing offered.
        clock += 1_000;
        assert!(carry_round(&router, "ble", &queue, 20, clock).is_empty());

        // The cooldown still buys the safety re-walk from the top, so a row
        // lost in a link's FIFO is eventually found again.
        let lane = router.carried_lane_for("ble".into(), clock + CARRIED_REWALK_MIN_INTERVAL_MS);
        assert_eq!(
            lane,
            CoreCarriedLane {
                skip: false,
                after: None
            }
        );
    }

    #[test]
    fn one_user_behind_many_rotating_addresses_walks_its_backlog_once() {
        // Field shape: one phone, seen under a long sequence of rotating BLE
        // addresses, one short link each. Keying the walk by logical peer is
        // what makes the backlog advance across those links instead of
        // restarting -- and the tail cursor must survive the rotation too, so
        // the address the peer happens to wear when new mail arrives does not
        // decide whether it gets offered.
        let router = CoreMeshRouterState::new();
        let alice = vec![1; 16];
        let now = 1_700_000_000_000_i64;
        let rows = queue_rows(now, 220);

        let mut clock = now;
        let mut offered = Vec::new();
        for rotation in 0..30 {
            let address = format!("ble-{rotation}");
            router.on_connected(address.clone(), CoreTransport::Central);
            assert!(router.on_hello(address.clone(), alice.clone()));
            clock += 1_000;
            offered.extend(carry_round(&router, &address, &rows, 20, clock));
            router.on_disconnected(address);
        }
        assert_eq!(
            offered, rows,
            "each address continues the same logical walk, offering every row exactly once"
        );

        // New mail, and yet another address: it is offered on that address's
        // first round rather than waiting out the cooldown.
        let mut queue = rows.clone();
        queue.extend(queue_rows(clock + 1, 5));
        router.on_connected("ble-fresh".into(), CoreTransport::Central);
        assert!(router.on_hello("ble-fresh".into(), alice));
        clock += 1_000;
        assert_eq!(
            carry_round(&router, "ble-fresh", &queue, 20, clock),
            queue[220..].to_vec(),
        );
    }

    #[test]
    fn carry_state_for_a_peer_not_seen_for_a_day_is_swept() {
        let router = CoreMeshRouterState::new();
        let now = 1_700_000_000_000_i64;
        let cursor = CoreCarriedCursor {
            received_at: now,
            msg_id: vec![9; 16],
        };
        for index in 0..8_u8 {
            let address = format!("ble-{index}");
            router.on_connected(address.clone(), CoreTransport::Central);
            assert!(router.on_hello(address.clone(), vec![index; 16]));
            router.record_carried_progress(address.clone(), Some(cursor.clone()), false, now);
            router.on_disconnected(address);
        }
        assert_eq!(router.logical_carry.lock_recoverable().len(), 8);

        // A round for one more peer, a day later, sweeps the idle entries but
        // keeps the peer it is serving.
        let later = now + LOGICAL_CARRY_STATE_TTL_MS;
        router.on_connected("ble-new".into(), CoreTransport::Central);
        assert!(router.on_hello("ble-new".into(), vec![200; 16]));
        router.record_carried_progress("ble-new".into(), Some(cursor.clone()), false, later);
        let carry = router.logical_carry.lock_recoverable();
        assert_eq!(carry.len(), 1);
        assert!(carry.contains_key(&vec![200; 16]));
        drop(carry);

        // Sweeping only ever costs a full re-walk; it never suppresses one.
        assert_eq!(
            router.carried_lane_for("ble-new".into(), later + 1),
            CoreCarriedLane {
                skip: false,
                after: Some(cursor)
            }
        );
    }

    #[test]
    fn a_peer_seen_inside_the_ttl_keeps_its_progress() {
        let router = CoreMeshRouterState::new();
        let now = 1_700_000_000_000_i64;
        let cursor = CoreCarriedCursor {
            received_at: now,
            msg_id: vec![9; 16],
        };
        router.on_connected("idle".into(), CoreTransport::Central);
        assert!(router.on_hello("idle".into(), vec![1; 16]));
        router.record_carried_progress("idle".into(), Some(cursor.clone()), false, now);

        router.on_connected("busy".into(), CoreTransport::Central);
        assert!(router.on_hello("busy".into(), vec![2; 16]));
        router.record_carried_progress(
            "busy".into(),
            Some(cursor.clone()),
            false,
            now + LOGICAL_CARRY_STATE_TTL_MS - 1,
        );

        assert_eq!(
            router
                .carried_lane_for("idle".into(), now + LOGICAL_CARRY_STATE_TTL_MS - 1)
                .after,
            Some(cursor),
            "a peer still inside the retention window resumes where it stopped"
        );
    }

    #[test]
    fn duplicate_links_share_foreign_and_targeted_carry_progress() {
        let router = CoreMeshRouterState::new();
        let alice = vec![1; 16];
        let foreign = CoreCarriedCursor {
            received_at: 1_000,
            msg_id: vec![7; 16],
        };
        let targeted = CoreCarriedCursor {
            received_at: 2_000,
            msg_id: vec![8; 16],
        };
        router.on_connected("central".into(), CoreTransport::Central);
        assert!(router.on_hello("central".into(), alice.clone()));
        router.on_connected("peripheral".into(), CoreTransport::Peripheral);
        assert!(router.on_hello("peripheral".into(), alice));

        router.record_carried_progress("central".into(), Some(foreign.clone()), false, 10);
        router.record_targeted_carried_progress(
            "central".into(),
            Some(targeted.clone()),
            false,
            10,
        );

        assert_eq!(
            router.carried_lane_for("peripheral".into(), 11).after,
            Some(foreign),
        );
        assert_eq!(
            router
                .targeted_carried_lane_for("peripheral".into(), 11)
                .after,
            Some(targeted),
        );
    }

    #[test]
    fn redigest_waits_for_the_jittered_interval() {
        // seed 0 -> the minimum interval (3 min).
        assert!(!should_redigest(REDIGEST_MIN_INTERVAL_MS - 1, 0, 0));
        assert!(should_redigest(REDIGEST_MIN_INTERVAL_MS, 0, 0));
    }

    #[test]
    fn redigest_jitter_never_leaves_the_configured_window() {
        for seed in [0u64, 1, 7, 42, 1_000, u64::MAX] {
            // Just before the max bound: not every seed is due yet...
            let before_max = should_redigest(REDIGEST_MAX_INTERVAL_MS - 1, 0, seed);
            // ...but by the max bound, every seed must be due.
            assert!(should_redigest(REDIGEST_MAX_INTERVAL_MS, 0, seed));
            // And none is due before the min bound.
            assert!(!should_redigest(REDIGEST_MIN_INTERVAL_MS - 1, 0, seed));
            let _ = before_max;
        }
    }

    #[test]
    fn redigest_measures_from_the_last_digest() {
        let last = 10_000_000i64;
        assert!(!should_redigest(
            last + REDIGEST_MIN_INTERVAL_MS - 1,
            last,
            0
        ));
        assert!(should_redigest(last + REDIGEST_MAX_INTERVAL_MS, last, 0));
    }

    #[test]
    fn redigest_not_due_when_last_digest_is_in_the_future() {
        // Clock skew: last_digest_at ahead of now must not trigger a redigest.
        assert!(!should_redigest(0, 60_000, 0));
    }

    #[test]
    fn router_rejects_identity_changes_and_prefers_lan() {
        let router = CoreMeshRouterState::new();
        router.set_local_user_id(vec![0]);
        router.on_connected("ble".into(), CoreTransport::Central);
        router.on_connected("lan".into(), CoreTransport::Lan);
        assert!(router.on_hello("ble".into(), vec![1]));
        assert_eq!(router.route_for(vec![1]).unwrap().address, "ble");
        assert!(router.on_hello("lan".into(), vec![1]));
        assert!(!router.on_hello("lan".into(), vec![2]));
        assert_eq!(router.route_for(vec![1]).unwrap().address, "lan");
        assert_eq!(router.connected_user_count(), 1);
    }

    #[test]
    fn installing_local_identity_can_flip_an_existing_dual_role_peer() {
        let router = CoreMeshRouterState::new();
        let local = vec![2; 16];
        let peer = vec![1; 16];
        router.on_connected("central".into(), CoreTransport::Central);
        router.on_connected("peripheral".into(), CoreTransport::Peripheral);
        assert!(router.on_hello("central".into(), peer.clone()));
        assert!(router.on_hello("peripheral".into(), peer.clone()));
        assert_eq!(
            router.route_for(peer.clone()).unwrap().address,
            "central",
            "missing local identity uses the documented central-first fallback"
        );

        router.set_local_user_id(local);
        assert_eq!(
            router.route_for(peer).unwrap().address,
            "peripheral",
            "the larger local identity elects the inverse BLE role"
        );
    }

    #[test]
    fn send_plan_uses_exactly_one_elected_route_for_every_frame_size() {
        let routes = vec![
            CoreTransportRoute {
                transport: CoreTransport::Lan,
                address: "lan".into(),
            },
            CoreTransportRoute {
                transport: CoreTransport::Central,
                address: "ble".into(),
            },
        ];
        assert_eq!(
            core_transport_send_plan(routes.clone(), 100),
            vec![routes[0].clone()]
        );
        assert_eq!(
            core_transport_send_plan(routes.clone(), 9_000),
            vec![routes[0].clone()]
        );
    }

    #[test]
    fn authenticated_ids_elect_the_same_ble_connection_at_both_ends() {
        let alice = vec![1; 16];
        let bob = vec![2; 16];

        let alice_router = CoreMeshRouterState::new();
        alice_router.set_local_user_id(alice.clone());
        alice_router.on_connected("alice-central".into(), CoreTransport::Central);
        alice_router.on_connected("alice-peripheral".into(), CoreTransport::Peripheral);
        assert!(alice_router.on_hello("alice-central".into(), bob.clone()));
        assert!(alice_router.on_hello("alice-peripheral".into(), bob.clone()));

        let bob_router = CoreMeshRouterState::new();
        bob_router.set_local_user_id(bob.clone());
        bob_router.on_connected("bob-central".into(), CoreTransport::Central);
        bob_router.on_connected("bob-peripheral".into(), CoreTransport::Peripheral);
        assert!(bob_router.on_hello("bob-central".into(), alice.clone()));
        assert!(bob_router.on_hello("bob-peripheral".into(), alice.clone()));

        assert_eq!(
            alice_router.route_for(bob).unwrap().transport,
            CoreTransport::Central,
            "smaller authenticated user elects its central half"
        );
        assert_eq!(
            bob_router.route_for(alice).unwrap().transport,
            CoreTransport::Peripheral,
            "larger authenticated user elects the inverse half of that link"
        );
    }

    #[test]
    fn selected_route_is_sticky_across_rotation_and_fails_over_on_disconnect() {
        let router = CoreMeshRouterState::new();
        let local = vec![1; 16];
        let peer = vec![2; 16];
        router.set_local_user_id(local);
        router.on_connected("old-central".into(), CoreTransport::Central);
        assert!(router.on_hello("old-central".into(), peer.clone()));
        router.on_connected("rotated-central".into(), CoreTransport::Central);
        assert!(router.on_hello("rotated-central".into(), peer.clone()));
        router.on_connected("peripheral".into(), CoreTransport::Peripheral);
        assert!(router.on_hello("peripheral".into(), peer.clone()));

        assert_eq!(
            router.route_for(peer.clone()).unwrap().address,
            "old-central"
        );
        assert!(router.is_selected_route("old-central".into()));
        assert!(!router.is_selected_route("rotated-central".into()));
        assert_eq!(router.selected_identified_routes().len(), 1);

        router.on_disconnected("old-central".into());
        assert_eq!(
            router.route_for(peer.clone()).unwrap().address,
            "rotated-central",
            "the next oldest preferred-role address takes over"
        );
        router.on_disconnected("rotated-central".into());
        assert_eq!(
            router.route_for(peer).unwrap().address,
            "peripheral",
            "the inverse BLE role remains a bounded fallback"
        );
    }

    #[test]
    fn relay_plan_selects_one_route_per_user_and_excludes_the_source_user() {
        let router = CoreMeshRouterState::new();
        let local = vec![1; 16];
        let alice = vec![2; 16];
        let bob = vec![3; 16];
        router.set_local_user_id(local);
        for (address, transport, user_id) in [
            ("alice-central", CoreTransport::Central, Some(alice.clone())),
            (
                "alice-peripheral",
                CoreTransport::Peripheral,
                Some(alice.clone()),
            ),
            ("alice-lan", CoreTransport::Lan, Some(alice.clone())),
            ("bob", CoreTransport::Central, Some(bob)),
            ("unknown", CoreTransport::Peripheral, None),
        ] {
            router.on_connected(address.into(), transport);
            if let Some(user_id) = user_id {
                assert!(router.on_hello(address.into(), user_id));
            }
        }

        let all = router.relay_routes(None);
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|route| route.address == "alice-lan"));
        assert!(all.iter().any(|route| route.address == "bob"));
        assert!(all.iter().any(|route| route.address == "unknown"));

        let excluding_alice = router.relay_routes(Some("alice-peripheral".into()));
        assert_eq!(excluding_alice.len(), 2);
        assert!(!excluding_alice
            .iter()
            .any(|route| route.address.starts_with("alice-")));
    }

    /// §10 step 5: the link to a device of this person's own carries the
    /// roster notice and nothing else. It is not a route, and unlike an
    /// ordinary unidentified link it is not a flood target either -- the
    /// device still holding this person's agreement key after a removal is the
    /// device that was removed, and it must not be fed the person's traffic.
    #[test]
    fn relay_plan_skips_a_link_to_one_of_this_persons_own_devices() {
        let router = CoreMeshRouterState::new();
        let alice = vec![2; 16];
        router.on_connected("alice".into(), CoreTransport::Lan);
        assert!(router.on_hello("alice".into(), alice));
        router.on_connected("stranger".into(), CoreTransport::Central);
        router.on_own_device_connected("sibling".into(), CoreTransport::Lan);

        let all = router.relay_routes(None);
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|route| route.address == "alice"));
        // Still flooded: an identified peer's HELLO may simply not have
        // landed yet, which is the case this arm exists for.
        assert!(all.iter().any(|route| route.address == "stranger"));
        assert!(!all.iter().any(|route| route.address == "sibling"));

        // Excluding the own-device link as a source changes nothing: it was
        // never in the plan.
        assert_eq!(router.relay_routes(Some("sibling".into())).len(), 2);
    }

    /// A removed phone still holds the agreement key that admits it as one of
    /// this person's own devices. Naming a contact in a HELLO must not win it
    /// that contact's route.
    #[test]
    fn an_own_device_link_cannot_become_a_route_by_saying_so() {
        let router = CoreMeshRouterState::new();
        let contact = vec![7; 16];
        router.on_own_device_connected("sibling".into(), CoreTransport::Lan);

        assert!(!router.on_hello("sibling".into(), contact.clone()));
        assert!(!router.on_hello2("sibling".into(), contact.clone(), 0xffff_ffff));
        assert!(router.route_for(contact).is_none());
        assert!(router.selected_identified_routes().is_empty());
        assert_eq!(router.connected_user_count(), 0);
        // Still a live transport: the notice has to reach it somehow.
        assert_eq!(
            router.transport_for("sibling".into()),
            Some(CoreTransport::Lan)
        );
    }

    /// The shells cannot heartbeat or re-offer a roster on a link they cannot
    /// name, and an own-device link is in none of the route accessors.
    #[test]
    fn own_device_links_are_nameable_without_becoming_routes() {
        let router = CoreMeshRouterState::new();
        let contact = vec![9; 16];
        router.on_own_device_connected("sibling-b".into(), CoreTransport::Lan);
        router.on_own_device_connected("sibling-a".into(), CoreTransport::Lan);
        router.on_connected("friend".into(), CoreTransport::Lan);
        assert!(router.on_hello("friend".into(), contact));

        assert_eq!(
            router
                .own_device_links()
                .into_iter()
                .map(|route| route.address)
                .collect::<Vec<_>>(),
            vec!["sibling-a".to_string(), "sibling-b".to_string()]
        );
        assert!(router
            .identified_routes()
            .iter()
            .all(|route| route.address == "friend"));

        router.on_disconnected("sibling-a".into());
        assert_eq!(
            router
                .own_device_links()
                .into_iter()
                .map(|route| route.address)
                .collect::<Vec<_>>(),
            vec!["sibling-b".to_string()]
        );
    }

    #[test]
    fn reconnect_backoff_doubles_then_probes_slowly_after_give_up() {
        let tracker = CoreReconnectBackoffTracker::new(10, 40, 3, 100);
        assert!(tracker.can_attempt("peer".into(), 0));
        assert_eq!(tracker.record_failure("peer".into(), 0), 1);
        assert_eq!(tracker.retry_delay_ms("peer".into(), 3), Some(7));
        assert_eq!(tracker.record_failure("peer".into(), 10), 2);
        // Third failure exhausts the budget: given up, but only demoted to
        // the slow probe cadence — never refused forever (the live wedge:
        // transient failures against a still-advertised address).
        assert_eq!(tracker.record_failure("peer".into(), 30), 3);
        assert!(tracker.is_given_up("peer".into()));
        assert!(!tracker.can_attempt("peer".into(), 129));
        assert_eq!(tracker.retry_delay_ms("peer".into(), 30), Some(100));
        assert!(tracker.can_attempt("peer".into(), 130));
        // A failed probe re-arms the probe interval, not the exponential.
        assert_eq!(tracker.record_failure("peer".into(), 130), 4);
        assert!(!tracker.can_attempt("peer".into(), 229));
        assert!(tracker.can_attempt("peer".into(), 230));
        // A probe that finally connects clears everything.
        tracker.record_success("peer".into());
        assert!(!tracker.is_given_up("peer".into()));
        assert!(tracker.can_attempt("peer".into(), 230));
    }

    #[test]
    fn lan_health_tracks_matching_responses_and_closes() {
        let tracker = CoreLanHealthTracker::new(10, 2);
        assert_eq!(
            tracker.next("a".into(), 0, 1).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 5, 2).action,
            CoreLanHealthAction::Wait
        );
        assert_eq!(tracker.response("a".into(), 1, 7), Some(7));
        assert_eq!(
            tracker.next("a".into(), 8, 3).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 18, 4).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 28, 5).action,
            CoreLanHealthAction::Close
        );
    }

    #[test]
    fn failover_resume_window_outlasts_the_observed_disconnect_burst() {
        // The 2026-08-07 capture showed one radio event's disconnect
        // callbacks spread over ~240ms. The window has to outlast that or the
        // resume still fans out into a link whose death is in flight.
        assert!(core_failover_resume_window_ms() > 240);
        assert_eq!(core_failover_resume_window_ms(), FAILOVER_RESUME_WINDOW_MS);
        assert_eq!(
            CoreFailoverResumeDebounce::new().window_ms(),
            FAILOVER_RESUME_WINDOW_MS
        );
    }

    #[test]
    fn failover_resume_coalesces_a_burst_into_one_armed_window() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        // First disconnect of the burst arms the window.
        let arm = debounce.request("peer".into(), 1_000).expect("armed");
        assert_eq!(arm.delay_ms, 300);
        // The sibling links dying over the next 240ms are absorbed: exactly
        // one resume runs for the whole radio event.
        assert_eq!(debounce.request("peer".into(), 1_100), None);
        assert_eq!(debounce.request("peer".into(), 1_240), None);
        assert!(debounce.is_pending("peer".into()));
        // The window is not extended by the later events -- it still expires
        // one window after the burst *started*.
        assert_eq!(debounce.request("peer".into(), 1_299), None);
        debounce.fired("peer".into(), arm.token);
        assert!(!debounce.is_pending("peer".into()));
        // A later failover for the same peer is a new burst, not a repeat.
        assert!(debounce.request("peer".into(), 1_400).is_some());
    }

    #[test]
    fn failover_resume_coalesces_per_peer_not_globally() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        assert!(debounce.request("peer-a".into(), 0).is_some());
        // Two peers failing over in the same radio event each get a resume.
        assert!(debounce.request("peer-b".into(), 10).is_some());
        assert_eq!(debounce.request("peer-a".into(), 20), None);
    }

    #[test]
    fn failover_resume_rearms_when_a_pending_window_was_lost() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        assert!(debounce.request("peer".into(), 0).is_some());
        // A timer that never fired must not disable this peer's resume
        // forever; once the window has elapsed the next failover re-arms.
        assert!(debounce.request("peer".into(), 300).is_some());
        // Same for a clock that jumped backwards.
        assert!(debounce.request("peer".into(), 100).is_some());
    }

    #[test]
    fn failover_resume_ignores_a_stale_fired_token() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        // Timer A is armed at 0 and re-armed as timer B at exactly the window
        // boundary, before A's message has been dispatched.
        let first = debounce.request("peer".into(), 0).expect("armed");
        let second = debounce.request("peer".into(), 300).expect("re-armed");
        assert_ne!(first.token, second.token);

        // Timer A now runs. Clearing B's marker here is what used to let a
        // third disconnect arm a third window inside one burst.
        debounce.fired("peer".into(), first.token);
        assert!(debounce.is_pending("peer".into()));
        assert_eq!(debounce.request("peer".into(), 310), None);

        // B's own firing is honoured.
        debounce.fired("peer".into(), second.token);
        assert!(!debounce.is_pending("peer".into()));
    }

    #[test]
    fn failover_resume_tokens_are_never_reused() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        let mut seen = HashSet::new();
        for i in 0..10 {
            let arm = debounce.request("peer".into(), i * 1_000).expect("armed");
            assert!(seen.insert(arm.token), "token {} reused", arm.token);
            debounce.fired("peer".into(), arm.token);
        }
    }

    #[test]
    fn failover_resume_cancel_and_clear_drop_pending_windows() {
        let debounce = CoreFailoverResumeDebounce::with_window_ms(300);
        assert!(debounce.request("peer".into(), 0).is_some());
        debounce.cancel("peer".into());
        assert!(debounce.request("peer".into(), 10).is_some());
        debounce.clear();
        assert!(!debounce.is_pending("peer".into()));
        assert!(debounce.request("peer".into(), 20).is_some());
    }
}
